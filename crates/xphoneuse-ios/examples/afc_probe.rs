//! Probe: the phone's files, over AFC.
//!
//! AFC (`com.apple.afc`) is the stock lockdown service iTunes, Finder and
//! every "phone manager" use to reach `/var/mobile/Media` - the media area:
//! the camera roll under `DCIM`, `Downloads`, and the folders apps share.
//! Unlike the screen, HID and pasteboard services it is **not** served by the
//! developer image, so this door needs no Developer Mode and works on a phone
//! this app can only watch.
//!
//! This probe exists to answer, on the real device, the four questions the
//! file manager's design rests on:
//!
//!   1. does the classic lockdown route still hand out AFC on iOS 27,
//!   2. what is the root (the expectation is `/var/mobile/Media`),
//!   3. what does one listing cost (`list_dir` returns names only, so a
//!      listing is one round trip plus one `get_file_info` per entry),
//!   4. is the media area writable.
//!
//! Usage:
//!   cargo run -p xphoneuse-ios --example afc_probe -- probe
//!   cargo run -p xphoneuse-ios --example afc_probe -- ls [PATH]
//!   cargo run -p xphoneuse-ios --example afc_probe -- get REMOTE LOCAL
//!   cargo run -p xphoneuse-ios --example afc_probe -- put LOCAL REMOTE_DIR
//!
//! The device comes from `XPHONEUSE_UDID`, or the test iPhone by default.

use std::time::Instant;

use idevice::services::afc::opcode::AfcFopenMode;
use idevice::services::afc::AfcClient;
use idevice::IdeviceService;

const DEFAULT_UDID: &str = "00008140-0000000000000000";

/// Where the write round trip lands. The root on purpose: if the media area
/// is writable anywhere it is writable here, and a failure reads cleanly.
const PROBE_FILE: &str = "/xphoneuse-afc-probe.txt";

#[tokio::main]
async fn main() {
    let udid = std::env::var("XPHONEUSE_UDID").unwrap_or_else(|_| DEFAULT_UDID.to_string());
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "probe".to_string());

    let started = Instant::now();
    let mut client = match connect(&udid).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("[afc] connect failed after {:?}: {e}", started.elapsed());
            std::process::exit(1);
        }
    };
    eprintln!("[afc] connected in {:?}", started.elapsed());

    let result = match command.as_str() {
        "probe" => probe(&mut client).await,
        "ls" => ls(&mut client, &args.next().unwrap_or_else(|| "/".to_string())).await,
        "get" => match (args.next(), args.next()) {
            (Some(remote), Some(local)) => get(&mut client, &remote, &local).await,
            _ => Err("usage: afc_probe get REMOTE LOCAL".to_string()),
        },
        "rm" => {
            let Some(remote) = args.next() else {
                return eprintln!("usage: afc_probe rm REMOTE") .into();
            };
            match client.remove(remote.clone()).await {
                Ok(()) => eprintln!("[afc] removed {remote}"),
                Err(e) => eprintln!("[afc] remove {remote} failed: {e}"),
            }
            Ok(())
        }
        "put" => match (args.next(), args.next()) {
            (Some(local), Some(dir)) => put(&mut client, &local, &dir).await,
            _ => Err("usage: afc_probe put LOCAL REMOTE_DIR".to_string()),
        },
        other => Err(format!("unknown command {other:?}; try probe, ls, get, put")),
    };
    if let Err(e) = result {
        eprintln!("[afc] failed: {e}");
        std::process::exit(1);
    }
}

async fn connect(udid: &str) -> Result<AfcClient, String> {
    let provider = xphoneuse_ios::device::provider_for(udid)
        .await
        .map_err(|e| format!("{e:#}"))?;
    AfcClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("{e} (is the device unlocked and paired with this computer?)"))
}

/// One directory: names, then a stat per entry, with the cost of each visible.
async fn ls(client: &mut AfcClient, path: &str) -> Result<(), String> {
    let started = Instant::now();
    let names = client
        .list_dir(path.to_string())
        .await
        .map_err(|e| e.to_string())?;
    eprintln!(
        "[afc] list_dir {path}: {} entries (including . and ..) in {:?}",
        names.len(),
        started.elapsed()
    );
    for name in names
        .iter()
        .filter(|n| !matches!(n.as_str(), "." | ".."))
        .take(20)
    {
        let full = join(path, name);
        let stat = Instant::now();
        match client.get_file_info(full.clone()).await {
            Ok(info) => println!(
                "{:>10}  {:<8}  {}  ({:?})",
                if info.st_ifmt == "S_IFDIR" {
                    "<dir>".to_string()
                } else {
                    info.size.to_string()
                },
                info.st_ifmt,
                full,
                stat.elapsed()
            ),
            Err(e) => println!("{:>10}  ?         {}  ({e})", "?", full),
        }
    }
    Ok(())
}

async fn probe(client: &mut AfcClient) -> Result<(), String> {
    match client.get_device_info().await {
        Ok(info) => eprintln!(
            "[afc] device: {} total={} free={} block={}",
            info.model, info.total_bytes, info.free_bytes, info.block_size
        ),
        Err(e) => eprintln!("[afc] device info failed: {e}"),
    }

    // The root: `/` in AFC is the media mount, so this is the answer to
    // "what can the user see".
    let started = Instant::now();
    let root = client.list_dir("/".to_string()).await.map_err(|e| {
        format!("root listing failed: {e} (the phone may be locked, or AFC is not being served)")
    })?;
    eprintln!("[afc] root in {:?}: {root:?}", started.elapsed());

    // One directory in, with per-entry stat timings: this is the cost model
    // for a listing in the UI.
    for candidate in ["/DCIM", "/Downloads", "/PhotoData"] {
        let names = match client.list_dir(candidate.to_string()).await {
            Ok(names) => names,
            Err(e) => {
                eprintln!("[afc] {candidate}: {e}");
                continue;
            }
        };
        let real: Vec<&String> = names
            .iter()
            .filter(|n| !matches!(n.as_str(), "." | ".."))
            .collect();
        if real.is_empty() {
            eprintln!("[afc] {candidate}: empty");
            continue;
        }
        eprintln!("[afc] {candidate}: {} entries", real.len());
        let mut first_file: Option<String> = None;
        for name in real.iter().take(6) {
            let full = join(candidate, name);
            let stat = Instant::now();
            match client.get_file_info(full.clone()).await {
                Ok(info) => {
                    eprintln!(
                        "[afc]   {:>9}  {:<8}  {}  stat in {:?}",
                        if info.st_ifmt == "S_IFDIR" {
                            "<dir>".to_string()
                        } else {
                            info.size.to_string()
                        },
                        info.st_ifmt,
                        full,
                        stat.elapsed()
                    );
                    if info.st_ifmt != "S_IFDIR" && first_file.is_none() {
                        first_file = Some(full);
                    }
                }
                Err(e) => eprintln!("[afc]   stat {full} failed: {e}"),
            }
        }
        // A real read: the first 4 KiB of a real file, which is what a
        // download is made of.
        if let Some(path) = first_file.or_else(|| real.first().map(|n| join(candidate, n))) {
            let read = Instant::now();
            match client.open(path.clone(), AfcFopenMode::RdOnly).await {
                Ok(mut file) => {
                    match file.read_n(4096).await {
                        Ok(bytes) => eprintln!(
                            "[afc] read {} bytes of {path} in {:?} (starts {:02x?})",
                            bytes.len(),
                            read.elapsed(),
                            &bytes[..bytes.len().min(8)]
                        ),
                        Err(e) => eprintln!("[afc] read {path} failed: {e}"),
                    }
                    let _ = file.close().await;
                }
                Err(e) => eprintln!("[afc] open {path} failed: {e}"),
            }
        }
        break;
    }

    // Write round trip. WrOnly truncates and creates, which is what an upload
    // wants; the probe removes what it wrote so the phone is left as found.
    let payload = b"xphoneuse afc probe\n";
    let write = Instant::now();
    match client.open(PROBE_FILE.to_string(), AfcFopenMode::WrOnly).await {
        Ok(mut file) => {
            let wrote = file.write_entire(payload).await;
            let closed = file.close().await;
            match (wrote, closed) {
                (Ok(()), Ok(())) => eprintln!(
                    "[afc] wrote {} bytes to {PROBE_FILE} in {:?}",
                    payload.len(),
                    write.elapsed()
                ),
                (wrote, closed) => {
                    return Err(format!("write failed: write={wrote:?} close={closed:?}"));
                }
            }
        }
        Err(e) => {
            return Err(format!(
                "open for write failed: {e} (the media area is supposed to be writable)"
            ));
        }
    }
    match client.get_file_info(PROBE_FILE.to_string()).await {
        Ok(info) => eprintln!(
            "[afc] stat after write: size={} ifmt={}",
            info.size, info.st_ifmt
        ),
        Err(e) => eprintln!("[afc] stat after write failed: {e}"),
    }
    match client.open(PROBE_FILE.to_string(), AfcFopenMode::RdOnly).await {
        Ok(mut file) => {
            match file.read_entire().await {
                Ok(bytes) if bytes == payload => eprintln!("[afc] read back matches"),
                Ok(bytes) => eprintln!("[afc] read back differs: {:?}", String::from_utf8_lossy(&bytes)),
                Err(e) => eprintln!("[afc] read back failed: {e}"),
            }
            let _ = file.close().await;
        }
        Err(e) => eprintln!("[afc] reopen failed: {e}"),
    }
    match client.remove(PROBE_FILE.to_string()).await {
        Ok(()) => eprintln!("[afc] removed {PROBE_FILE}"),
        Err(e) => eprintln!("[afc] remove failed: {e} (the phone now has a stray probe file)"),
    }
    Ok(())
}

async fn get(client: &mut AfcClient, remote: &str, local: &str) -> Result<(), String> {
    let info = client
        .get_file_info(remote.to_string())
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("[afc] {remote} is {} bytes", info.size);
    let started = Instant::now();
    let mut file = client
        .open(remote.to_string(), AfcFopenMode::RdOnly)
        .await
        .map_err(|e| e.to_string())?;
    let bytes = file.read_entire().await.map_err(|e| e.to_string())?;
    file.close().await.map_err(|e| e.to_string())?;
    std::fs::write(local, &bytes).map_err(|e| e.to_string())?;
    let secs = started.elapsed().as_secs_f64().max(0.001);
    eprintln!(
        "[afc] {} bytes -> {local} in {:?} ({:.1} MB/s)",
        bytes.len(),
        started.elapsed(),
        bytes.len() as f64 / 1e6 / secs
    );
    Ok(())
}

async fn put(client: &mut AfcClient, local: &str, dir: &str) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| e.to_string())?;
    let name = std::path::Path::new(local)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{local} has no file name"))?;
    let remote = join(dir, name);
    let started = Instant::now();
    let mut file = client
        .open(remote.clone(), AfcFopenMode::WrOnly)
        .await
        .map_err(|e| e.to_string())?;
    file.write_entire(&bytes).await.map_err(|e| e.to_string())?;
    file.close().await.map_err(|e| e.to_string())?;
    let back = client
        .get_file_info(remote.clone())
        .await
        .map_err(|e| e.to_string())?;
    eprintln!(
        "[afc] {} bytes -> {remote} in {:?} (device says {} bytes, left in place)",
        bytes.len(),
        started.elapsed(),
        back.size
    );
    Ok(())
}

/// AFC paths are absolute; the join is only interesting for the root.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}
