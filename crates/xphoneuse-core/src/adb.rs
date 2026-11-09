use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_resource_dir(path: PathBuf) {
    let _ = RESOURCE_DIR.set(path);
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub serial: String,
    pub model: String,
    pub state: String,
}

fn adb_binary_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}

/// The adb candidates, best first: an SDK install, then one on PATH, and only
/// then the copy shipped with the app.
///
/// The bundle is the last resort on purpose. adb talks to a *server* on port
/// 5037, and two versions of it fight over that port - each one kills the
/// other's server, which shows up as "could not read ok from ADB Server /
/// failed to start daemon". A machine that already has adb (Android Studio,
/// a PATH install) has to keep using it, or the app breaks the tooling that
/// was working.
fn adb_candidates() -> Vec<PathBuf> {
    let binary = adb_binary_name();
    let mut candidates = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(&home).join("Library/Android/sdk/platform-tools").join(binary));
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local_app_data).join("Android/Sdk/platform-tools").join(binary));
    }
    if let Ok(android_home) = std::env::var("ANDROID_HOME") {
        candidates.push(PathBuf::from(&android_home).join("platform-tools").join(binary));
    }
    // Whatever `adb` means to this process - the PATH resolution happens when
    // it is run, so this candidate is judged by running it like the others.
    candidates.push(PathBuf::from(binary));
    if let Some(dir) = RESOURCE_DIR.get() {
        candidates.push(dir.join("resources").join(binary));
    }
    // Absolute paths only beyond this point: a bare name that does not exist
    // is not something `exists()` can answer.
    candidates
}

static RESOLVED: OnceLock<PathBuf> = OnceLock::new();

/// Serialises the probe below. Two callers commonly arrive together - the
/// window asks for `adb_status` and `list_devices` in the same tick - and
/// without this they both find `RESOLVED` empty and each run the probe, so the
/// app spawns one adb per caller instead of one per run.
static RESOLVE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn runs(candidate: &Path) -> bool {
    if candidate.is_absolute() && !candidate.exists() {
        return false;
    }
    let mut cmd = Command::new(candidate);
    cmd.arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // A GUI app that starts a console program gets a console window for it
    // unless it says otherwise, so without this the probe flashed one on
    // screen at every start (2026-09-17).
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The adb this process uses, decided once: the first candidate that actually
/// runs. Cached, so the probe costs one spawn per app run rather than one per
/// command.
async fn adb_path() -> PathBuf {
    if let Some(path) = RESOLVED.get() {
        return path.clone();
    }
    let _probe = RESOLVE_LOCK.lock().await;
    // Whoever held the lock may have decided it while we waited.
    if let Some(path) = RESOLVED.get() {
        return path.clone();
    }
    for candidate in adb_candidates() {
        if runs(&candidate).await {
            let _ = RESOLVED.set(candidate.clone());
            return candidate;
        }
    }
    // Nothing runs. Keep the bare name so the failure is a spawn error, which
    // is what `adb_available` explains.
    let fallback = PathBuf::from(adb_binary_name());
    let _ = RESOLVED.set(fallback.clone());
    fallback
}

/// Where adb was found, for a UI that has to explain itself when it is not.
pub async fn adb_location() -> String {
    adb_path().await.display().to_string()
}

/// Whether adb can actually be run, with a sentence to show the user when it
/// cannot.
///
/// Every Android verb needs adb, and without this the only symptom is an empty
/// device list - which reads as "no phone plugged in" and sends people looking
/// at cables instead of at their machine. Candidates are tried best-first and
/// the app ships its own as the last resort, so this failing means nothing on
/// the machine could be run at all.
pub async fn adb_available() -> Result<(), String> {
    let path = adb_path().await;
    match adb_command().await.arg("version").output().await {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "{} is there but would not run: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) if path.as_os_str() == adb_binary_name() => Err(format!(
            "no adb found: looked for an Android SDK (platform-tools) install, the copy \
             shipped with the app, and one on PATH ({e})"
        )),
        Err(e) => Err(format!("{} could not be run: {e}", path.display())),
    }
}

/// Whether adb's own output says its server is not there (or was just killed
/// by another adb's server), rather than the command itself being wrong.
///
/// Callers pass **both** streams: adb reports daemon trouble on stdout
/// (`* failed to start daemon`) and device trouble on stderr, so a check of
/// one alone misses half of it - which is how a failed push came out as a bare
/// "failed:" with no reason at all.
fn is_daemon_trouble(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("failed to start daemon")
        || lower.contains("cannot connect to daemon")
        || lower.contains("could not read ok from adb server")
        || lower.contains("failed to check server version")
}

/// The reason an adb run failed, from whichever stream carries it.
fn failure_text(stdout: &str, stderr: &str) -> String {
    let (out, err) = (stdout.trim(), stderr.trim());
    match (out.is_empty(), err.is_empty()) {
        (false, true) => out.to_string(),
        (true, false) => err.to_string(),
        (false, false) => format!("{err} ({out})"),
        (true, true) => "adb said nothing and exited with an error".to_string(),
    }
}

async fn adb_command() -> Command {
    let mut cmd = Command::new(adb_path().await);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

async fn run_adb(args: &[&str]) -> Result<Vec<u8>> {
    let output = adb_command()
        .await
        .args(args)
        .output()
        .await
        .map_err(|e| anyhow!("Failed to run adb: {}", e))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // "failed to start daemon" / "cannot connect to daemon" is what a
        // server that another adb just killed looks like. Starting it again is
        // the documented fix, and one retry separates that from a real failure
        // (no device, no driver, adb missing).
        if is_daemon_trouble(&stdout) || is_daemon_trouble(&stderr) {
            let started = adb_command().await.arg("start-server").output().await;
            if matches!(&started, Ok(o) if o.status.success()) {
                let retry = adb_command().await.args(args).output().await;
                if let Ok(retry) = retry {
                    if retry.status.success() {
                        return Ok(retry.stdout);
                    }
                    let (out, err) = (
                        String::from_utf8_lossy(&retry.stdout).to_string(),
                        String::from_utf8_lossy(&retry.stderr).to_string(),
                    );
                    return Err(anyhow!(
                        "adb {} failed: {}",
                        args.join(" "),
                        failure_text(&out, &err)
                    ));
                }
            }
        }
        return Err(anyhow!(
            "adb {} failed: {}",
            args.join(" "),
            failure_text(&stdout, &stderr)
        ));
    }
    Ok(output.stdout)
}

async fn run_adb_text(args: &[&str]) -> Result<String> {
    let stdout = run_adb(args).await?;
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

pub async fn list_devices() -> Result<Vec<Device>> {
    let output = run_adb_text(&["devices", "-l"]).await?;
    let mut devices = Vec::new();

    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let serial = parts[0].to_string();
        let state = parts[1].to_string();
        let model = parts
            .iter()
            .find(|p| p.starts_with("model:"))
            .map(|p| p.trim_start_matches("model:").to_string())
            .unwrap_or_else(|| serial.clone());

        devices.push(Device {
            serial,
            model,
            state,
        });
    }
    Ok(devices)
}

pub async fn push_file(serial: &str, local: &str, remote: &str) -> Result<()> {
    run_adb(&["-s", serial, "push", local, remote]).await?;
    Ok(())
}

/// Copy a file off the device.
///
/// `-a` keeps the original mtime, which is the difference between a photo
/// that sorts by when it was taken and one that sorts by when it was copied.
/// Both paths are arguments rather than shell text, so nothing here needs
/// quoting: the device never sees a shell.
pub async fn pull_file(serial: &str, remote: &str, local: &str) -> Result<()> {
    run_adb(&["-s", serial, "pull", "-a", remote, local]).await?;
    Ok(())
}

pub async fn forward_port(serial: &str, local_port: u16, remote: &str) -> Result<()> {
    run_adb(&[
        "-s",
        serial,
        "forward",
        &format!("tcp:{}", local_port),
        remote,
    ])
    .await?;
    Ok(())
}

pub async fn remove_forward(serial: &str, local_port: u16) -> Result<()> {
    let _ = run_adb(&[
        "-s",
        serial,
        "forward",
        "--remove",
        &format!("tcp:{}", local_port),
    ])
    .await;
    Ok(())
}

pub async fn shell(serial: &str, cmd: &str) -> Result<tokio::process::Child> {
    let child = adb_command().await
        .args(["-s", serial, "shell", cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn adb shell: {}", e))?;
    Ok(child)
}

/// Run a shell command and give back its stdout as text.
///
/// The one-liner every caller of [`shell`] was writing for itself (spawn,
/// wait, lossy-decode). `cmd` is passed as a single argv entry, so the device
/// runs it through its own shell - pipes, `&&` and quoting all work as they do
/// in `adb shell` by hand. A non-zero exit is an error, with stderr in the
/// message, because a command that failed silently is worse than one that
/// failed loudly.
pub async fn shell_output(serial: &str, cmd: &str) -> Result<String> {
    let child = shell(serial, cmd).await?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow!("adb shell {cmd:?} failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        bail!("adb shell {cmd:?} exited with {}: {detail}", output.status);
    }
    Ok(stdout)
}

pub async fn reverse(serial: &str, remote: &str, local_port: u16) -> Result<()> {
    run_adb(&[
        "-s",
        serial,
        "reverse",
        remote,
        &format!("tcp:{}", local_port),
    ])
    .await?;
    Ok(())
}

pub async fn remove_reverse(serial: &str, remote: &str) -> Result<()> {
    let _ = run_adb(&["-s", serial, "reverse", "--remove", remote]).await;
    Ok(())
}

pub async fn kill_scrcpy_server(serial: &str) {
    let _ = run_adb(&[
        "-s",
        serial,
        "shell",
        "pkill -f com.genymobile.scrcpy.Server",
    ])
    .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
}

pub async fn exec_out_screencap(serial: &str) -> Result<Vec<u8>> {
    run_adb(&["-s", serial, "exec-out", "screencap", "-p"]).await
}

pub async fn dump_ui_hierarchy(serial: &str) -> Result<String> {
    let output =
        run_adb_text(&["-s", serial, "exec-out", "uiautomator", "dump", "/dev/tty"]).await?;

    if let Some(end) = output.rfind("</hierarchy>") {
        let xml = &output[..end + "</hierarchy>".len()];
        if let Some(start) = xml.find('<') {
            return Ok(xml[start..].to_string());
        }
    }

    Err(anyhow!("Failed to parse UI hierarchy output"))
}

pub async fn kill_server() {
    let mut cmd = Command::new(adb_path().await);
    cmd.args(["kill-server"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.output().await;
}

pub async fn connect_device(address: &str) -> Result<()> {
    let output = run_adb_text(&["connect", address]).await?;
    if output.contains("connected") || output.contains("already connected") {
        Ok(())
    } else {
        Err(anyhow!("Failed to connect: {}", output.trim()))
    }
}

pub async fn tcpip(serial: &str, port: u16) -> Result<()> {
    run_adb(&["-s", serial, "tcpip", &port.to_string()]).await?;
    Ok(())
}

pub async fn disconnect_device(address: &str) -> Result<()> {
    run_adb(&["disconnect", address]).await?;
    Ok(())
}

pub async fn get_device_ip(serial: &str) -> Result<Option<String>> {
    if let Ok(output) = run_adb_text(&["-s", serial, "shell", "ip", "route"]).await {
        for line in output.lines() {
            if line.contains("wlan") {
                if let Some(idx) = line.find("src ") {
                    if let Some(ip) = line[idx + 4..].split_whitespace().next() {
                        return Ok(Some(ip.to_string()));
                    }
                }
            }
        }
    }

    if let Ok(output) = run_adb_text(&[
        "-s", serial, "shell", "ip", "-f", "inet", "addr", "show", "wlan0",
    ])
    .await
    {
        for line in output.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("inet ") {
                if let Some(ip) = rest.split('/').next() {
                    return Ok(Some(ip.to_string()));
                }
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// adb is chatty on stdout and complains on stderr, and both have to be
    /// read: a push that failed because the daemon would not start came out as
    /// a bare "failed:" with nothing after it (2026-09-17), because the only
    /// stream that was read held no text at all.
    #[test]
    fn a_failure_says_why_whichever_stream_carried_it() {
        assert_eq!(failure_text("", "device offline"), "device offline");
        assert_eq!(
            failure_text("* daemon not running; starting now at tcp:5037", ""),
            "* daemon not running; starting now at tcp:5037"
        );
        assert_eq!(
            failure_text("some stdout", "some stderr"),
            "some stderr (some stdout)"
        );
        assert!(!failure_text("", "").trim().is_empty());
    }

    /// The daemon-trouble test is what triggers the one retry, and it has to
    /// see adb's stdout form of it.
    #[test]
    fn daemon_trouble_is_recognised_on_either_stream() {
        assert!(is_daemon_trouble(
            "* daemon not running; starting now at tcp:5037\ncould not read ok from ADB Server\n\
             * failed to start daemon"
        ));
        assert!(is_daemon_trouble("adb.exe: failed to check server version: cannot connect to daemon"));
        assert!(!is_daemon_trouble("adb: device offline"));
        assert!(!is_daemon_trouble("adb: failed to install"));
    }
}
