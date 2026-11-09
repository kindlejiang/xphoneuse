//! The file manager window's backend.
//!
//! Everything the window does to the phone lands here: list a directory,
//! copy a file in either direction, hand a local file to the OS to be dragged
//! out. The work itself is in the crates - `xphoneuse_core::files` for what is
//! common and for Android, `xphoneuse_ios::files` for the iPhone - and this
//! layer is the dispatch between them plus the window's own concerns:
//! progress events, cancellation, and where a download lands.
//!
//! The dispatch is the same one the MCP server makes (see
//! `xphoneuse_list_files` there): Android first, because a session on the
//! phone in front of the user wins over a view-only iOS one, and the location
//! may say which phone it means.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use xphoneuse_core::files as fs;
use xphoneuse_core::{adb, files};

use crate::state::AppState;

/// Which phone an operation acts on.
enum Target {
    Android(String),
    Ios(String),
}

impl Target {
    fn platform(&self) -> &'static str {
        match self {
            Target::Android(_) => fs::PLATFORM_ANDROID,
            Target::Ios(_) => fs::PLATFORM_IOS,
        }
    }

    fn device(&self) -> &str {
        match self {
            Target::Android(serial) => serial,
            Target::Ios(udid) => udid,
        }
    }
}

/// The connected device a file operation would act on.
async fn target_for(state: &AppState, wanted: Option<&str>) -> Result<Target, String> {
    let android = {
        let session = state.session.lock().await;
        session.as_ref().map(|session| session.device_serial.clone())
    };
    let ios = {
        let session = state.ios_session.lock().await;
        session.as_ref().map(|session| session.udid.clone())
    };
    match wanted {
        Some(fs::PLATFORM_ANDROID) => android.map(Target::Android).ok_or_else(|| {
            "that location is for an Android phone, and none is connected".to_string()
        }),
        Some(fs::PLATFORM_IOS) => ios.map(Target::Ios).ok_or_else(|| {
            "that location is for an iPhone, and none is connected".to_string()
        }),
        _ => match (android, ios) {
            (Some(serial), _) => Ok(Target::Android(serial)),
            (None, Some(udid)) => Ok(Target::Ios(udid)),
            (None, None) => Err("No device connected".to_string()),
        },
    }
}

/// The phone an operation acts on, and where in it - from a location that may
/// or may not name its platform, and may not exist at all (the window's first
/// call asks for nothing and gets the root).
async fn resolve(state: &AppState, input: Option<&str>) -> Result<(Target, String), String> {
    let named = input.and_then(fs::platform_of);
    let target = target_for(state, named).await?;
    let path = match input {
        Some(raw) => fs::parse_location(raw, target.platform())?.1,
        None => fs::default_path(target.platform()).to_string(),
    };
    Ok((target, path))
}

/// What the window is told while a transfer runs.
#[derive(Clone, serde::Serialize)]
pub struct FmProgress {
    /// The id the window handed in, so a row can find its own transfer.
    pub id: String,
    /// `"download"` or `"upload"`.
    pub direction: String,
    pub name: String,
    pub transferred: u64,
    pub total: u64,
    /// `"running"`, `"done"`, `"failed"` or `"cancelled"`.
    pub state: String,
    /// Where a finished download landed, so the row can be dragged out.
    pub local_path: Option<String>,
    pub error: Option<String>,
}

/// What a finished transfer produced.
#[derive(Clone, serde::Serialize)]
pub struct FmTransfer {
    /// The file's name on the phone.
    pub name: String,
    /// Where it ended up, as a location.
    pub location: String,
    /// The local copy, for a download - what a drag would hand the OS.
    pub local_path: Option<String>,
    pub size: u64,
}

/// Sends progress to the window, at most once every 100 ms.
///
/// The transfer itself runs at megabytes per second, so every chunk is a
/// hundred IPC messages a second - enough to make the window stutter, and
/// more than any progress bar needs. The last one always goes out.
#[derive(Clone)]
struct Progress {
    app: AppHandle,
    id: String,
    direction: &'static str,
    name: String,
    last: Instant,
}

impl Progress {
    fn new(app: &AppHandle, id: &str, direction: &'static str, name: &str) -> Self {
        Self {
            app: app.clone(),
            id: id.to_string(),
            direction,
            name: name.to_string(),
            last: Instant::now() - Duration::from_secs(1),
        }
    }

    fn send(&mut self, transferred: u64, total: u64) {
        let now = Instant::now();
        if transferred < total && now.duration_since(self.last) < Duration::from_millis(100) {
            return;
        }
        self.last = now;
        self.emit(transferred, total, "running", None, None);
    }

    fn finish(&self, transferred: u64, total: u64, state: &str, local: Option<&str>, error: Option<&str>) {
        self.emit(
            transferred,
            total,
            state,
            local.map(str::to_string),
            error.map(str::to_string),
        );
    }

    fn emit(&self, transferred: u64, total: u64, state: &str, local: Option<String>, error: Option<String>) {
        let _ = self.app.emit_to(
            crate::FILES_WINDOW_LABEL,
            "fm-progress",
            FmProgress {
                id: self.id.clone(),
                direction: self.direction.to_string(),
                name: self.name.clone(),
                transferred,
                total,
                state: state.to_string(),
                local_path: local,
                error,
            },
        );
    }
}

/// The cancellation flag for a transfer, by the id the window chose.
///
/// Keyed by the window's own id rather than a counter so the "cancel" click
/// always finds the transfer it was looking at. Entries are removed when the
/// transfer ends - see [`finish_transfer`].
fn cancel_flag(id: &str) -> Arc<AtomicBool> {
    static FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    let flags = FLAGS.get_or_init(|| Mutex::new(HashMap::new()));
    flags
        .lock()
        .expect("transfer registry")
        .entry(id.to_string())
        .or_default()
        .clone()
}

fn finish_transfer(id: &str) {
    static FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    let flags = FLAGS.get_or_init(|| Mutex::new(HashMap::new()));
    flags.lock().expect("transfer registry").remove(id);
}

/// Where a download lands.
///
/// Two ways in, because the window asks two different questions. An explicit
/// `dest_path` (what the save dialog returned) is honoured exactly - the user
/// said where. A `dest_dir` (or nothing) means "a copy over here, please",
/// which is the drag-and-drop prefetch: that one must not interrupt the
/// gesture with a dialog, so it numbers a collision instead of asking.
async fn destination(
    app: &AppHandle,
    dest_path: Option<String>,
    dest_dir: Option<String>,
    name: &str,
) -> Result<PathBuf, String> {
    if let Some(path) = dest_path.map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("could not use {}: {e}", parent.display()))?;
        }
        return Ok(path);
    }
    let dir = match dest_dir.map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir,
        None => app
            .path()
            .document_dir()
            .map_err(|e| format!("no Documents folder to save into: {e}"))?,
    };
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("could not use {}: {e}", dir.display()))?;
    files::unique_local_path(&dir, name)
}

/// A directory on the phone, with everything the window needs to draw around
/// it - roots, breadcrumbs, the way up.
#[tauri::command]
pub async fn fm_list(
    state: State<'_, AppState>,
    location: Option<String>,
) -> Result<fs::FsListing, String> {
    let (target, path) = resolve(state.inner(), location.as_deref()).await?;
    let directory = match &target {
        Target::Android(serial) => files::list_dir(serial, &path).await?,
        Target::Ios(udid) => {
            xphoneuse_ios::files::session(udid)
                .await?
                .list(&path)
                .await?
        }
    };
    Ok(files::build_listing(
        target.platform(),
        target.device(),
        &path,
        directory,
    ))
}

/// Copy a file off the phone into a directory on this computer.
#[tauri::command]
pub async fn fm_download(
    app: AppHandle,
    state: State<'_, AppState>,
    location: String,
    id: String,
    dest_path: Option<String>,
    dest_dir: Option<String>,
) -> Result<FmTransfer, String> {
    let (target, path) = resolve(state.inner(), Some(&location)).await?;
    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    let local = destination(&app, dest_path, dest_dir, &name).await?;
    let cancel = cancel_flag(&id);
    let mut progress = Progress::new(&app, &id, "download", &name);

    let outcome = match &target {
        Target::Android(serial) => {
            let info = files::stat(serial, &path).await;
            let info = match info {
                Ok(info) => info,
                Err(e) => {
                    finish_transfer(&id);
                    progress.finish(0, 0, "failed", None, Some(&e));
                    return Err(e);
                }
            };
            if info.kind == "dir" {
                finish_transfer(&id);
                let e = format!("{path} is a folder - open it and copy the files");
                progress.finish(0, 0, "failed", None, Some(&e));
                return Err(e);
            }
            let part = local.with_file_name(format!(
                "{}.part",
                local.file_name().and_then(|n| n.to_str()).unwrap_or("download")
            ));
            // adb says nothing until it is finished, so the size of the file
            // being written is what says how far along it is.
            let watcher = {
                let mut watcher_progress = progress.clone();
                let part = part.clone();
                let total = info.size;
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let size = tokio::fs::metadata(&part)
                            .await
                            .map(|meta| meta.len())
                            .unwrap_or(0);
                        watcher_progress.send(size.min(total), total);
                    }
                })
            };
            let result = adb::pull_file(serial, &path, &part.display().to_string()).await;
            watcher.abort();
            match result {
                Ok(()) => match tokio::fs::rename(&part, &local).await {
                    Ok(()) => Ok((local, info.size)),
                    Err(e) => Err(format!("could not finish {}: {e}", local.display())),
                },
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    Err(format!("{e}"))
                }
            }
        }
        Target::Ios(udid) => {
            let mut on_chunk = |transferred: u64, total: u64| progress.send(transferred, total);
            xphoneuse_ios::files::download(udid, &path, &local, &cancel, &mut on_chunk).await
        }
    };

    finish_transfer(&id);
    match outcome {
        Ok((local, size)) => {
            let location = fs::format_location(target.platform(), &path);
            progress.finish(size, size, "done", Some(&local.display().to_string()), None);
            Ok(FmTransfer {
                name,
                location,
                local_path: Some(local.display().to_string()),
                size,
            })
        }
        Err(e) => {
            let cancelled = cancelled(&e, &cancel);
            progress.finish(0, 0, if cancelled { "cancelled" } else { "failed" }, None, Some(&e));
            Err(e)
        }
    }
}

/// Copy a local file onto the phone.
#[tauri::command]
pub async fn fm_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    directory: String,
    host_path: String,
    id: String,
) -> Result<FmTransfer, String> {
    let local = PathBuf::from(host_path.trim());
    let info = tokio::fs::metadata(&local)
        .await
        .map_err(|e| format!("could not read {}: {e}", local.display()))?;
    if info.is_dir() {
        return Err("folders cannot be sent yet - send the files inside it".to_string());
    }
    let name = local
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", local.display()))?
        .to_string();
    let (target, dir) = resolve(state.inner(), Some(&directory)).await?;
    let cancel = cancel_flag(&id);
    let mut progress = Progress::new(&app, &id, "upload", &name);

    let outcome = match &target {
        Target::Android(serial) => {
            match files::unique_remote_path(serial, &dir, &name).await {
                Ok(remote) => match adb::push_file(serial, &local.display().to_string(), &remote).await
                {
                    Ok(()) => {
                        progress.send(info.len(), info.len());
                        Ok((remote, info.len()))
                    }
                    Err(e) => Err(format!("{e}")),
                },
                Err(e) => Err(e),
            }
        }
        Target::Ios(udid) => {
            match xphoneuse_ios::files::unique_remote_path(udid, &dir, &name).await {
                Ok(remote) => {
                    let mut on_chunk =
                        |transferred: u64, total: u64| progress.send(transferred, total);
                    match xphoneuse_ios::files::upload(udid, &local, &remote, &cancel, &mut on_chunk)
                        .await
                    {
                        Ok(size) => Ok((remote, size)),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
    };

    finish_transfer(&id);
    match outcome {
        Ok((remote, size)) => {
            let location = fs::format_location(target.platform(), &remote);
            progress.finish(
                size,
                size,
                "done",
                Some(&local.display().to_string()),
                None,
            );
            Ok(FmTransfer {
                name,
                location,
                local_path: Some(local.display().to_string()),
                size,
            })
        }
        Err(e) => {
            let cancelled = cancelled(&e, &cancel);
            progress.finish(0, 0, if cancelled { "cancelled" } else { "failed" }, None, Some(&e));
            Err(e)
        }
    }
}

/// Stop a transfer that is running. The transfer notices on its next chunk
/// and removes what it had started writing.
#[tauri::command]
pub fn fm_cancel(id: String) -> Result<(), String> {
    cancel_flag(&id).store(true, Ordering::Relaxed);
    Ok(())
}

/// Whether a failure was the user pressing cancel.
fn cancelled(error: &str, flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed) || error.contains("cancelled")
}

/// Hand a file that is already on this computer to the operating system to be
/// dragged to wherever the user wants it.
///
/// This is the direction tauri has no API for: its drag-and-drop is an
/// incoming one (a drop *onto* a webview), and `start_dragging` moves the
/// window. CrabNebula's `drag` crate is what fills the gap, and it has two
/// requirements that shape the whole feature: the file must exist locally, and
/// the call has to happen while the mouse button is down. So a file that only
/// exists on the phone is downloaded first (the window offers a save button
/// and marks the row once it is local) and can be dragged afterwards.
///
/// `run_on_main_thread` is not decoration: on Windows the drag is an OLE modal
/// loop that belongs to the thread owning the window.
/// Answers with whether a drag actually ran. `Ok(false)` is the ordinary
/// "the button was already up" case: the file is here now, but the gesture
/// that asked for it is over, so the window says "drag it again" instead of
/// pretending something happened.
#[tauri::command]
pub async fn fm_start_drag(
    window: tauri::WebviewWindow,
    local_path: String,
) -> Result<bool, String> {
    let path = std::fs::canonicalize(&local_path)
        .map_err(|e| format!("{local_path} is not on this computer: {e}"))?;
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    // A drag has to begin while the button is held. This call often comes
    // after a fetch - the window starts one when a file that is not here yet
    // is dragged - and by then the hand may be off the button. Starting the
    // OLE drag anyway would drop the file wherever the cursor happens to be,
    // or wait for input that is never coming.
    if !left_button_down() {
        return Ok(false);
    }
    // The closure needs the window, and the call itself borrows it, so the
    // closure gets its own handle. The result comes back through a channel and
    // is awaited, so a drag that could not start is an error the window can
    // show - the alternative (logging it) is a gesture that silently does
    // nothing, which is indistinguishable from a broken feature.
    let drag_window = window.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    window
        .run_on_main_thread(move || {
            let window = drag_window;
            let icon = drag::Image::Raw(include_bytes!("../icons/128x128.png").to_vec());
            #[cfg(target_os = "linux")]
            let result = match window.gtk_window() {
                Ok(gtk_window) => drag::start_drag(
                    &gtk_window,
                    drag::DragItem::Files(vec![path]),
                    icon,
                    |_, _| {},
                    drag::Options {
                        mode: drag::DragMode::Copy,
                        ..Default::default()
                    },
                )
                .map_err(|e| e.to_string()),
                Err(e) => Err(format!("this window cannot start a drag: {e}")),
            };
            #[cfg(not(target_os = "linux"))]
            let result = drag::start_drag(
                &window,
                drag::DragItem::Files(vec![path]),
                icon,
                |_, _| {},
                drag::Options {
                    mode: drag::DragMode::Copy,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string());
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;

    // `start_drag` blocks the main thread until the drop happens, so this waits
    // for the gesture to be over. Off the async runtime, since that can be a
    // while.
    let outcome = tokio::task::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(600))
            .map_err(|_| "the drag never started".to_string())?
    })
    .await
    .map_err(|e| e.to_string())?;
    outcome?;
    Ok(true)
}

/// Whether the left mouse button is physically down right now.
///
/// The OS is the only one who knows: the webview's own mouseup does not arrive
/// when the pointer has left the window, which is exactly what happens while
/// someone drags toward the desktop.
#[cfg(target_os = "windows")]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    // The high bit means "down now" (the low bit is the since-boot history).
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16) & 0x8000 != 0 }
}

/// The other platforms start drags through their own toolkits, where a stale
/// button is not the same hazard; the drag simply does not begin.
#[cfg(not(target_os = "windows"))]
fn left_button_down() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancel press and a transfer that says it was cancelled are the same
    /// thing to the row that is showing it.
    #[test]
    fn cancellation_is_recognised_either_way() {
        let flag = AtomicBool::new(true);
        assert!(cancelled("anything", &flag));
        let flag = AtomicBool::new(false);
        assert!(cancelled("cancelled", &flag));
        assert!(!cancelled("the phone stopped sending", &flag));
    }
}
