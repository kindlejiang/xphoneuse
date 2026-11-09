//! The iPhone's files: the media area, over AFC.
//!
//! `com.apple.afc` is a stock lockdown service - the door iTunes, Finder and
//! every phone manager use to reach `/var/mobile/Media`: the camera roll under
//! `DCIM`, `Downloads`, and the folders apps share. Unlike the screen, HID and
//! pasteboard services it is **not** served by the developer image, so none of
//! this needs Developer Mode, a mounted image, or the streaming tunnel - it
//! works on a phone this app can only watch. (The RSD variant of the same
//! service, `com.apple.afc.shim.remote`, exists and does need Developer Mode;
//! it is the fallback if the stock door ever closes.)
//!
//! What it cannot reach is everything outside that chroot: no `/`, no app
//! containers. That is a property of the protocol, not a shortcut taken here.
//!
//! Two shapes live in this module, for two different reasons:
//!
//! * **Browsing** goes through a long-lived session ([`session`]) - a listing
//!   costs one round trip per entry (`list_dir` returns names only, so every
//!   name needs a `get_file_info`), and paying a fresh lockdown handshake for
//!   each of them would make the window crawl.
//! * **Transfers** ([`download`], [`upload`]) open their own connection. An
//!   `AfcClient` hands out a descriptor that borrows it, so a listing and a
//!   transfer cannot share one anyway - and this way a multi-minute download
//!   never blocks the window, and cancelling one is just a flag.
//!
//! The session registry holds **strong** references, unlike the tunnel's: an
//! idle AFC connection costs nothing, and the Python CLI is one process per
//! call - with weak handles, `files` followed by `pull` would each pay a new
//! handshake for no reason at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use idevice::services::afc::errors::AfcError;
use idevice::services::afc::opcode::AfcFopenMode;
use idevice::services::afc::{AfcClient, FileInfo};
use idevice::{IdeviceError, IdeviceService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use xphoneuse_core::files::{self as fs, FsDirectory, FsEntry};

/// How long a listing may take on the device before the caller gives up.
///
/// Generous on purpose: a directory of a few thousand photos is one round trip
/// per entry, and the answer is still worth waiting for. A dead connection is
/// caught by the socket, not by this.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// One transfer request, which is also what `MAX_TRANSFER` in the vendored
/// crate uses - so a chunk is one AFC request in each direction.
const CHUNK: usize = 1024 * 1024;

/// The file service, as the app and the MCP server see it.
pub struct IosFsSession {
    tx: mpsc::Sender<Command>,
}

enum Command {
    List {
        path: String,
        reply: oneshot::Sender<Result<FsDirectory, String>>,
    },
    Stat {
        path: String,
        reply: oneshot::Sender<Result<FsEntry, String>>,
    },
    Exists {
        path: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
}

impl IosFsSession {
    /// The entries of `path`, sorted the way a file manager sorts.
    pub async fn list(&self, path: &str) -> Result<FsDirectory, String> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Command::List {
                path: path.to_string(),
                reply,
            })
            .await
            .map_err(|_| "the file session is no longer running".to_string())?;
        wait(answer).await
    }

    /// What one path is, without listing its directory.
    pub async fn stat(&self, path: &str) -> Result<FsEntry, String> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Command::Stat {
                path: path.to_string(),
                reply,
            })
            .await
            .map_err(|_| "the file session is no longer running".to_string())?;
        wait(answer).await
    }

    /// Whether anything is at `path`.
    pub async fn exists(&self, path: &str) -> Result<bool, String> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Command::Exists {
                path: path.to_string(),
                reply,
            })
            .await
            .map_err(|_| "the file session is no longer running".to_string())?;
        wait(answer).await
    }
}

async fn wait<T>(answer: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
    match tokio::time::timeout(REPLY_TIMEOUT, answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("the file session ended before it answered".to_string()),
        Err(_) => Err(format!(
            "the phone did not answer within {}s",
            REPLY_TIMEOUT.as_secs()
        )),
    }
}

fn registry() -> &'static Mutex<HashMap<String, Arc<IosFsSession>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<IosFsSession>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The session for this device, opening one if there is none (or if the last
/// one died with the connection).
pub async fn session(udid: &str) -> Result<Arc<IosFsSession>, String> {
    if let Some(existing) = registry().lock().expect("files registry").get(udid).cloned() {
        if !existing.tx.is_closed() {
            return Ok(existing);
        }
        registry().lock().expect("files registry").remove(udid);
    }
    let session = Arc::new(start(udid).await?);
    registry()
        .lock()
        .expect("files registry")
        .insert(udid.to_string(), session.clone());
    Ok(session)
}

/// The session if one is open, without opening one. For callers that only want
/// to know whether this device has been browsed (a status line, a disconnect).
pub fn active(udid: &str) -> Option<Arc<IosFsSession>> {
    registry().lock().ok()?.get(udid).cloned()
}

/// Forget this device's session. The connection itself closes when the last
/// handle to it goes away.
pub fn close(udid: &str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(udid);
    }
}

/// Connect and start answering. The connection is opened here rather than in
/// the actor so that a phone which cannot be reached says so to the caller,
/// with the failure in hand.
async fn start(udid: &str) -> Result<IosFsSession, String> {
    let client = connect(udid).await?;
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(run(client, rx));
    Ok(IosFsSession { tx })
}

/// One AFC connection over lockdown.
async fn connect(udid: &str) -> Result<AfcClient, String> {
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| format!("{e:#}"))?;
    AfcClient::connect(provider.as_ref())
        .await
        .map_err(|e| connect_error(&e, udid))
}

/// What went wrong, in words the user can act on.
///
/// The device's own states are worth naming: a locked iPhone refuses the
/// session, an untrusted one never gets that far, and both look like a generic
/// failure if the text is passed through raw.
fn connect_error(error: &IdeviceError, udid: &str) -> String {
    match error {
        IdeviceError::DeviceLocked | IdeviceError::PasswordProtected => {
            "the iPhone is locked - unlock it and try again".to_string()
        }
        IdeviceError::InvalidHostID | IdeviceError::UserDeniedPairing => format!(
            "this computer is not trusted by {udid} - plug it in, unlock it and tap Trust"
        ),
        IdeviceError::ServiceNotFound => {
            "the phone would not open its file service (is it unlocked?)".to_string()
        }
        other => format!("the phone's file service could not be reached: {other}"),
    }
}

/// Whether an error means the connection is gone (and the session with it),
/// rather than that this particular path or operation is not allowed.
///
/// The distinction matters: a typo'd path must not throw away a working
/// session, and a dropped cable must not leave one that answers every
/// following call with the same failure.
fn is_connection_lost(error: &IdeviceError) -> bool {
    matches!(
        error,
        IdeviceError::Socket(_)
            | IdeviceError::Timeout
            | IdeviceError::NoEstablishedConnection
            | IdeviceError::NotEnoughBytes(..)
            | IdeviceError::Afc(AfcError::ServiceNotConnected)
    )
}

/// A path-shaped failure, in words.
fn path_error(error: &IdeviceError, path: &str) -> String {
    match error {
        IdeviceError::Afc(AfcError::ObjectNotFound) => format!("there is nothing at {path}"),
        IdeviceError::Afc(AfcError::PermDenied) => format!("the phone refused access to {path}"),
        IdeviceError::Afc(AfcError::ObjectIsDir) => format!("{path} is a folder"),
        IdeviceError::Afc(AfcError::NotEnoughData) => {
            format!("the phone stopped sending {path} before it was complete")
        }
        other => other.to_string(),
    }
}

/// Owns the connection and answers commands until the handle is dropped.
async fn run(mut client: AfcClient, mut rx: mpsc::Receiver<Command>) {
    while let Some(command) = rx.recv().await {
        match command {
            Command::List { path, reply } => {
                let result = list_dir(&mut client, &path).await;
                let lost = matches!(&result, Err(e) if is_connection_lost(e));
                let _ = reply.send(result.map_err(|e| path_error(&e, &path)));
                if lost {
                    break;
                }
            }
            Command::Stat { path, reply } => {
                let result = stat_path(&mut client, &path).await;
                let lost = matches!(&result, Err(e) if is_connection_lost(e));
                let _ = reply.send(result.map_err(|e| path_error(&e, &path)));
                if lost {
                    break;
                }
            }
            Command::Exists { path, reply } => {
                // "nothing there" is an answer here, not a failure - which is
                // why this is decided on the typed error and not on the text
                // the caller would have seen.
                let result = match stat_path(&mut client, &path).await {
                    Ok(_) => Ok(true),
                    Err(IdeviceError::Afc(AfcError::ObjectNotFound)) => Ok(false),
                    Err(e) => Err(e),
                };
                let lost = matches!(&result, Err(e) if is_connection_lost(e));
                let _ = reply.send(result.map_err(|e| path_error(&e, &path)));
                if lost {
                    break;
                }
            }
        }
    }
}

/// `list_dir` gives names only, so each one is described separately.
///
/// One entry that cannot be described does not sink the listing: it comes back
/// as an `other` row with a warning, which is more use than an error that
/// hides the other thousand photos. A connection that dies, though, ends the
/// whole answer.
async fn list_dir(client: &mut AfcClient, path: &str) -> Result<FsDirectory, IdeviceError> {
    let names = client.list_dir(path.to_string()).await?;
    let mut directory = FsDirectory {
        entries: Vec::new(),
        truncated: 0,
        warnings: Vec::new(),
    };
    for name in names {
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        if directory.entries.len() >= fs::MAX_ENTRIES {
            directory.truncated += 1;
            continue;
        }
        let full = join(path, &name);
        match client.get_file_info(full.clone()).await {
            Ok(info) => directory.entries.push(entry_of(&name, &full, &info)),
            Err(e) if is_connection_lost(&e) => return Err(e),
            Err(_) => {
                directory
                    .warnings
                    .push(format!("could not read the properties of {name}"));
                directory.entries.push(FsEntry {
                    name,
                    location: fs::format_location(fs::PLATFORM_IOS, &full),
                    kind: "other".to_string(),
                    size: 0,
                    modified: None,
                });
            }
        }
    }
    fs::sort_entries(&mut directory.entries);
    Ok(directory)
}

async fn stat_path(client: &mut AfcClient, path: &str) -> Result<FsEntry, IdeviceError> {
    let info = client.get_file_info(path.to_string()).await?;
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    Ok(entry_of(&name, path, &info))
}

fn entry_of(name: &str, full: &str, info: &FileInfo) -> FsEntry {
    FsEntry {
        name: name.to_string(),
        location: fs::format_location(fs::PLATFORM_IOS, full),
        kind: fs::kind_of(&info.st_ifmt).to_string(),
        size: info.size as u64,
        // Before 1970 is what "the device did not say" looks like here.
        modified: Some(info.modified.and_utc().timestamp()).filter(|t| *t > 0),
    }
}

/// Copy a file off the phone to exactly `dest` on this computer.
///
/// The caller picks the name (the app asks the user with a save dialog, the
/// MCP tool numbers a collision) - this side only writes. The bytes are
/// streamed (one chunk per AFC request) rather than read whole: a 2 GB video
/// is a normal thing to find in a camera roll, and reading it into memory to
/// write it out again is how a file manager runs out of it.
///
/// The copy lands in `<dest>.part` first and is renamed only when it is
/// complete, so a cancelled or failed transfer never leaves a half-written
/// file under a name the user will later trust. `progress` is called after
/// every chunk with (written, total).
pub async fn download(
    udid: &str,
    remote: &str,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(PathBuf, u64), String> {
    let mut client = connect(udid).await?;
    let info = client
        .get_file_info(remote.to_string())
        .await
        .map_err(|e| path_error(&e, remote))?;
    if fs::kind_of(&info.st_ifmt) == "dir" {
        return Err(format!("{remote} is a folder"));
    }
    let total = info.size as u64;
    let final_path = dest.to_path_buf();
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("could not use {}: {e}", parent.display()))?;
    }
    // The sibling `.part` name, not a replaced extension: `IMG.HEIC` must not
    // become `IMG.part` while it is in flight.
    let part = final_path.with_file_name(format!(
        "{}.part",
        final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));

    let mut source = client
        .open(remote.to_string(), AfcFopenMode::RdOnly)
        .await
        .map_err(|e| path_error(&e, remote))?;
    let mut sink = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("could not write {}: {e}", part.display()))?;

    let mut written = 0u64;
    let outcome: Result<(), String> = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("cancelled".to_string());
        }
        let want = CHUNK.min((total - written) as usize);
        if want == 0 {
            break Ok(());
        }
        match source.read_n(want).await {
            Ok(chunk) if !chunk.is_empty() => {
                if let Err(e) = sink.write_all(&chunk).await {
                    break Err(format!("could not write {}: {e}", part.display()));
                }
                written += chunk.len() as u64;
                progress(written, total);
            }
            Ok(_) => {
                break Err(format!(
                    "the phone stopped sending {remote} after {written} of {total} bytes"
                ))
            }
            Err(e) => break Err(path_error(&e, remote)),
        }
    };
    let _ = source.close().await;
    let _ = sink.flush().await;
    drop(sink);

    match outcome {
        Ok(()) => {
            tokio::fs::rename(&part, &final_path)
                .await
                .map_err(|e| format!("could not finish {}: {e}", final_path.display()))?;
            Ok((final_path, written))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

/// Copy a local file onto the phone at `remote`.
///
/// `WrOnly` creates and truncates, which is why callers pick the remote name
/// with [`xphoneuse_core::files::unique_remote_path`] first - nothing here
/// overwrites. A cancelled upload removes what it had written.
pub async fn upload(
    udid: &str,
    local: &Path,
    remote: &str,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<u64, String> {
    let total = std::fs::metadata(local)
        .map_err(|e| format!("could not read {}: {e}", local.display()))?
        .len();
    let mut source = tokio::fs::File::open(local)
        .await
        .map_err(|e| format!("could not read {}: {e}", local.display()))?;
    let mut client = connect(udid).await?;
    let mut target = client
        .open(remote.to_string(), AfcFopenMode::WrOnly)
        .await
        .map_err(|e| path_error(&e, remote))?;

    let mut buffer = vec![0u8; CHUNK];
    let mut written = 0u64;
    let outcome: Result<(), String> = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("cancelled".to_string());
        }
        let read = match source.read(&mut buffer).await {
            Ok(read) => read,
            Err(e) => break Err(format!("could not read {}: {e}", local.display())),
        };
        if read == 0 {
            break Ok(());
        }
        if let Err(e) = target.write_entire(&buffer[..read]).await {
            break Err(path_error(&e, remote));
        }
        written += read as u64;
        progress(written, total);
    };
    let closed = target.close().await;
    // A half-written file under the real name would be worse than no file at
    // all: the user would have to work out that it is truncated. Closing the
    // descriptor releases the client, so the cleanup can borrow it again.
    match outcome {
        Ok(()) => {
            closed.map_err(|e| path_error(&e, remote))?;
            Ok(written)
        }
        Err(e) => {
            let _ = client.remove(remote.to_string()).await;
            Err(e)
        }
    }
}

/// Whether a name on the phone is free, and if not, one that is.
pub async fn unique_remote_path(udid: &str, dir: &str, name: &str) -> Result<String, String> {
    let session = session(udid).await?;
    let dir = dir.trim_end_matches('/');
    let base = format!("{dir}/{name}");
    if !session.exists(&base).await? {
        return Ok(base);
    }
    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], Some(&name[index + 1..])),
        _ => (name, None),
    };
    for n in 1..1000u32 {
        let candidate = match extension {
            Some(ext) => format!("{dir}/{stem} ({n}).{ext}"),
            None => format!("{dir}/{stem} ({n})"),
        };
        if !session.exists(&candidate).await? {
            return Ok(candidate);
        }
    }
    Err(format!("{name} already exists 1000 times over in {dir}"))
}

/// The path of `name` inside `dir`, with the root handled.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root is the one directory whose join needs care.
    #[test]
    fn joining_handles_the_root() {
        assert_eq!(join("/", "DCIM"), "/DCIM");
        assert_eq!(join("/DCIM", "100APPLE"), "/DCIM/100APPLE");
        assert_eq!(join("/DCIM/", "100APPLE"), "/DCIM/100APPLE");
    }

    /// A lost connection and a wrong path are told apart, because only one of
    /// them should throw the session away.
    #[test]
    fn only_a_lost_connection_ends_a_session() {
        assert!(is_connection_lost(&IdeviceError::Timeout));
        assert!(is_connection_lost(&IdeviceError::NoEstablishedConnection));
        assert!(is_connection_lost(&IdeviceError::Afc(
            AfcError::ServiceNotConnected
        )));

        assert!(!is_connection_lost(&IdeviceError::Afc(AfcError::ObjectNotFound)));
        assert!(!is_connection_lost(&IdeviceError::Afc(AfcError::PermDenied)));
        assert!(!is_connection_lost(&IdeviceError::Afc(AfcError::ObjectIsDir)));
        assert!(!is_connection_lost(&IdeviceError::DeviceLocked));
    }

    /// The failures a user actually hits say what to do about them.
    #[test]
    fn failures_are_explained_in_words() {
        assert!(connect_error(&IdeviceError::DeviceLocked, "udid").contains("unlock"));
        assert!(connect_error(&IdeviceError::InvalidHostID, "udid").contains("Trust"));
        assert!(path_error(&IdeviceError::Afc(AfcError::ObjectNotFound), "/DCIM/x")
            .contains("/DCIM/x"));
        assert!(path_error(&IdeviceError::Afc(AfcError::PermDenied), "/x").contains("refused"));
        assert!(path_error(&IdeviceError::Afc(AfcError::ObjectIsDir), "/x").contains("folder"));
    }
}
