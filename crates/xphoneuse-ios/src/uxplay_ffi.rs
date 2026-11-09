//! FFI binding for the "XPhoneUse" fork of uxplay (vendor/uxplay).
//!
//! The fork builds uxplay as a shared library (`uxplay.dll` on Windows)
//! with GStreamer and local rendering removed. Mirror video is delivered
//! as decrypted Annex-B NAL data through the C ABI in `uxplay_api.h`.
//! The library is built with MinGW-64 (MSYS2) and loaded dynamically by
//! the MSVC host app; only the three `extern "C"` entry points cross the
//! ABI boundary, plus our own callback function pointers (C calling
//! convention both ways, no C++ objects cross the boundary).

use anyhow::{Result, anyhow};
use libloading::Library;
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};

/// Events delivered from the uxplay server thread(s) to the host app.
#[derive(Debug, Clone)]
pub enum UxplayEvent {
    /// Decrypted access unit: Annex-B NALs (00 00 00 01 delimited),
    /// timestamped with the stream clock in nanoseconds.
    VideoData {
        data: Vec<u8>,
        nal_count: i32,
        timestamp_ns: u64,
    },
    /// Video stream was flushed (new client / reset); pending frames are stale.
    VideoFlush,
    /// Source resolution reported by the client.
    VideoSize { width: f32, height: f32 },
    /// Mirror codec: 0 = H.264, 1 = H.265.
    VideoCodec { codec: i32 },
    /// A client connected / started mirroring. `client_ip` is the client's
    /// source address ("" when unknown).
    Connected {
        device_id: String,
        client_name: String,
        client_ip: String,
    },
    /// All clients disconnected / server stopped.
    Disconnected,
    /// Server log line. level: 0=error, 1=info, 2=debug.
    Log { level: i32, message: String },
}

/// Capacity of the callback event queue. Video frames use `try_send`, so
/// a slow consumer degrades to frame drops instead of stalling the
/// server threads.
const EVENT_QUEUE_CAPACITY: usize = 64;

// --- C ABI types (must match uxplay_api.h) -------------------------------

type CbVideoData =
    unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, u64);
type CbVideoFlush = unsafe extern "C" fn(*mut c_void);
type CbVideoSize = unsafe extern "C" fn(*mut c_void, f32, f32);
type CbVideoCodec = unsafe extern "C" fn(*mut c_void, c_int);
type CbConnected = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char);
type CbDisconnected = unsafe extern "C" fn(*mut c_void);
type CbLog = unsafe extern "C" fn(*mut c_void, c_int, *const c_char);

#[repr(C)]
struct CCallbacks {
    user_data: *mut c_void,
    on_video_data: Option<CbVideoData>,
    on_video_flush: Option<CbVideoFlush>,
    on_video_size: Option<CbVideoSize>,
    on_video_codec: Option<CbVideoCodec>,
    on_connected: Option<CbConnected>,
    on_disconnected: Option<CbDisconnected>,
    on_log: Option<CbLog>,
}

#[repr(C)]
struct CConfig {
    server_name: *const c_char,
    max_fps: c_int,
    nohold: c_int,
    keyfile: *const c_char,
}

// --- trampolines ----------------------------------------------------------

unsafe fn sender_from<'a>(user: *mut c_void) -> Option<&'a SyncSender<UxplayEvent>> {
    if user.is_null() {
        None
    } else {
        Some(&*(user as *const SyncSender<UxplayEvent>))
    }
}

unsafe extern "C" fn on_video_data(
    user: *mut c_void,
    data: *const u8,
    data_len: c_int,
    nal_count: c_int,
    timestamp_ns: u64,
) {
    if data.is_null() || data_len <= 0 {
        return;
    }
    let Some(sender) = sender_from(user) else { return };
    let bytes = std::slice::from_raw_parts(data, data_len as usize).to_vec();
    // Frame drop on overload is intentional (video backpressure).
    let _ = sender.try_send(UxplayEvent::VideoData {
        data: bytes,
        nal_count,
        timestamp_ns,
    });
}

unsafe extern "C" fn on_video_flush(user: *mut c_void) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::VideoFlush);
    }
}

unsafe extern "C" fn on_video_size(user: *mut c_void, width: f32, height: f32) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::VideoSize { width, height });
    }
}

unsafe extern "C" fn on_video_codec(user: *mut c_void, codec: c_int) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::VideoCodec { codec });
    }
}

unsafe fn c_char_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

unsafe extern "C" fn on_connected(
    user: *mut c_void,
    device_id: *const c_char,
    client_name: *const c_char,
    client_ip: *const c_char,
) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::Connected {
            device_id: c_char_to_string(device_id),
            client_name: c_char_to_string(client_name),
            client_ip: c_char_to_string(client_ip),
        });
    }
}

unsafe extern "C" fn on_disconnected(user: *mut c_void) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::Disconnected);
    }
}

unsafe extern "C" fn on_log(user: *mut c_void, level: c_int, message: *const c_char) {
    if let Some(sender) = sender_from(user) {
        let _ = sender.send(UxplayEvent::Log {
            level,
            message: c_char_to_string(message),
        });
    }
}

// --- session --------------------------------------------------------------

/// An active uxplay server session. Consuming the struct stops the server
/// (blocks until the server thread exits, so call from a blocking context).
pub struct UxplaySession {
    // Keep the library loaded for the whole session.
    _lib: Library,
    stop_fn: unsafe extern "C" fn(),
    started: bool,
    receiver: Option<Receiver<UxplayEvent>>,
    // Boxed so the C side can hold a stable pointer as user_data. Never
    // read directly - dropped only after the server thread was joined.
    #[allow(dead_code)]
    sender: Box<SyncSender<UxplayEvent>>,
}

impl UxplaySession {
    /// Start a headless AirPlay mirroring server named `name`. The iPhone
    /// connects from Control Center → Screen Mirroring → `name`.
    pub fn start(name: &str, max_fps: u32) -> Result<Self> {
        let path = uxplay_dll_path();
        let lib = load_library(&path)?;

        let start_fn: libloading::Symbol<
            unsafe extern "C" fn(*const CConfig, *const CCallbacks) -> c_int,
        > = unsafe { lib.get(b"uxplay_start\0") }
            .map_err(|e| anyhow!("uxplay_start not found in {}: {}", path.display(), e))?;

        let stop_fn: libloading::Symbol<unsafe extern "C" fn()> =
            unsafe { lib.get(b"uxplay_stop\0") }
                .map_err(|e| anyhow!("uxplay_stop not found in {}: {}", path.display(), e))?;
        // Copy the pointer out so the borrow of `lib` ends before we move it.
        let stop_fn_ptr: unsafe extern "C" fn() = *stop_fn;

        let (sender, receiver) = mpsc::sync_channel::<UxplayEvent>(EVENT_QUEUE_CAPACITY);
        // Box first: the C side keeps this heap address as user_data for the
        // whole session, so the pointer must not move (and must outlive the
        // server thread - guaranteed by the Drop order in `stop()`).
        let sender = Box::new(sender);

        let callbacks = CCallbacks {
            user_data: sender.as_ref() as *const SyncSender<UxplayEvent> as *mut c_void,
            on_video_data: Some(on_video_data),
            on_video_flush: Some(on_video_flush),
            on_video_size: Some(on_video_size),
            on_video_codec: Some(on_video_codec),
            on_connected: Some(on_connected),
            on_disconnected: Some(on_disconnected),
            on_log: Some(on_log),
        };

        let name_c = CString::new(name)
            .map_err(|e| anyhow!("invalid server name: {}", e))?;
        let config = CConfig {
            server_name: name_c.as_ptr(),
            max_fps: max_fps as c_int,
            nohold: 1,
            keyfile: std::ptr::null(),
        };

        let ret = unsafe { start_fn(&config, &callbacks) };
        if ret != 0 {
            return Err(anyhow!("uxplay_start failed with code {}", ret));
        }

        Ok(Self {
            _lib: lib,
            stop_fn: stop_fn_ptr,
            started: true,
            receiver: Some(receiver),
            sender,
        })
    }

    /// Take the event receiver (must be drained continuously while the
    /// session lives, otherwise the fixed-size queue fills up and video
    /// frames are dropped).
    pub fn take_receiver(&mut self) -> Option<Receiver<UxplayEvent>> {
        self.receiver.take()
    }

    /// Stop the server and join its thread. Blocks; call from a blocking
    /// context (e.g. `tokio::task::spawn_blocking`). Idempotent.
    pub fn stop(&mut self) {
        if self.started {
            unsafe { (self.stop_fn)() };
            self.started = false;
            self.receiver.take();
        }
    }
}

impl Drop for UxplaySession {
    fn drop(&mut self) {
        // Best effort: never leave the server thread running past the
        // session's lifetime (e.g. app exit without an explicit stop).
        self.stop();
    }
}

/// Load the FFI library. On Windows, the MinGW dependency DLLs (libglib,
/// libplist, ...) sit next to uxplay.dll; LoadLibraryW consults PATH for
/// dependencies, so prepend the DLL's directory to PATH before loading.
fn load_library(path: &PathBuf) -> Result<Library> {
    #[cfg(windows)]
    {
        if let Some(dir) = path.parent() {
            let dir = dir.to_string_lossy().into_owned();
            let old = std::env::var("PATH").unwrap_or_default();
            if !old.split(';').any(|p| p == dir) {
                std::env::set_var("PATH", format!("{};{}", dir, old));
            }
        }
    }
    let lib = unsafe { Library::new(path) }
        .map_err(|e| anyhow!("failed to load {}: {:?}", path.display(), e))?;
    Ok(lib)
}

/// Path to the FFI library, searched in the same places as the uxplay
/// executable (resources dir first, then PATH).
pub fn uxplay_dll_path() -> PathBuf {
    let binary = if cfg!(windows) {
        "uxplay.dll"
    } else if cfg!(target_os = "macos") {
        // CMake: SHARED + OUTPUT_NAME uxplay + PREFIX "" → uxplay.dylib
        "uxplay.dylib"
    } else {
        "libuxplay.so"
    };
    if let Some(dir) = crate::uxplay::RESOURCE_DIR.get() {
        let bundled = dir.join("resources").join(binary);
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(binary)
}

/// True when the FFI library is available (primary iOS mirroring path).
/// Callers fall back to the legacy uxplay.exe subprocess when false.
pub fn available() -> bool {
    let path = uxplay_dll_path();
    path.is_absolute() && path.exists()
}
