use xphoneuse_core::{adb, control, macro_engine, scrcpy};
use xphoneuse_core::scrcpy::StreamSettings;
use xphoneuse_ios::{device as ios_device, screenshot as ios_screenshot, uxplay as ios_uxplay};
use crate::audio::{self, AudioHandle};
use crate::ios;
use crate::state::{AppState, ScrcpySession};
use crate::video::{self, FrameEvent};
use base64::Engine;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_aptabase::EventTracker;

/// Report the phone that just connected to Aptabase - platform, model and OS
/// version - so the devices actually in use can be analysed later without
/// guessing from serials. Best effort: analytics never fails a connection.
fn report_device_connected(app: &AppHandle, props: serde_json::Map<String, serde_json::Value>) {
    let _ = app.track_event("device_connected", Some(serde_json::Value::Object(props)));
}

/// The project website: "learn more" and help links point here.
///
/// The site lived on xphoneuse.aovik.com while its filing was pending and now
/// answers on xphoneuse.com; the old host 301s here, so both keep working and
/// this constant only has to follow the canonical host.
const HOMEPAGE_URL: &str = "https://xphoneuse.com";
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
#[serde(tag = "platform", rename_all = "lowercase")]
pub enum PlatformDevice {
    Android(adb::Device),
    Ios(ios_device::IosDevice),
}

#[tauri::command]
pub async fn list_devices() -> Result<Vec<PlatformDevice>, String> {
    // An adb that cannot run is not an empty device list: it is reported (the
    // window asks `adb_status` and says so) and logged, rather than swallowed.
    let android = match adb::list_devices().await {
        Ok(devices) => devices,
        Err(e) => {
            eprintln!("[adb] listing Android devices failed: {e:#}");
            Vec::new()
        }
    };
    let ios = ios_device::list_devices().await.unwrap_or_default();

    let mut devices: Vec<PlatformDevice> = android
        .into_iter()
        .map(PlatformDevice::Android)
        .collect();
    devices.extend(ios.into_iter().map(PlatformDevice::Ios));

    Ok(devices)
}

/// Disconnect any active session (Android or iOS).
pub async fn disconnect_any_session(state: &AppState) {
    {
        let mut session = state.session.lock().await;
        if let Some(s) = session.take() {
            s.shutdown.notify_one();
            scrcpy::stop_server(&s.device_serial, 27183).await;
        }
    }
    let ios_session = state.ios_session.lock().await.take();
    if let Some(mut s) = ios_session {
        s.shutdown.notify_one();
        if let Some(mut uxplay) = s.uxplay.take() {
            // uxplay_stop joins the server thread: run it on a blocking thread
            let _ = tokio::task::spawn_blocking(move || uxplay.stop()).await;
        }
        if let Some(tunnel) = s.tunnel {
            tunnel.stop();
        }
    }
}

#[tauri::command]
pub async fn connect_device(
    app: AppHandle,
    serial: String,
    on_frame: Channel<FrameEvent>,
    settings: StreamSettings,
    state: State<'_, AppState>,
) -> Result<(u32, u32), String> {
    disconnect_any_session(&state).await;

    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("Failed to get resource dir: {}", e))?;
    let server_path = resource_dir
        .join("resources")
        .join("scrcpy-server-v2.7");
    let server_path_str = server_path.to_string_lossy().to_string();

    let port: u16 = 27183;

    let (streams, mut server_process) =
        scrcpy::start_server(&serial, &server_path_str, port, &settings)
            .await
            .map_err(|e| format!("Failed to start scrcpy server: {}", e))?;

    // What the fleet looks like, for later analysis (Aptabase): type, model
    // and OS in one adb round-trip. A failure here must not touch the session.
    {
        let props = adb::shell_output(
            &serial,
            "getprop ro.product.manufacturer; getprop ro.product.model; getprop ro.build.version.release",
        )
        .await
        .unwrap_or_default();
        let mut lines = props.lines().map(str::trim);
        let manufacturer = lines.next().unwrap_or_default();
        let model = lines.next().unwrap_or_default();
        let os_version = lines.next().unwrap_or_default();
        let mut map = serde_json::Map::new();
        map.insert("platform".into(), serde_json::json!("android"));
        map.insert("manufacturer".into(), serde_json::json!(manufacturer));
        map.insert("model".into(), serde_json::json!(model));
        map.insert("os_version".into(), serde_json::json!(os_version));
        report_device_connected(&app, map);
    }

    let shutdown = Arc::new(tokio::sync::Notify::new());
    let control_socket = Arc::new(Mutex::new(streams.control_socket));

    let audio_handle = if let Some(audio_socket) = streams.audio_socket {
        let handle = AudioHandle::new()
            .map_err(|e| format!("Failed to init audio: {}", e))?;
        let handle = Arc::new(handle);
        let audio_shutdown = shutdown.clone();
        let audio_ref = handle.clone();
        tokio::spawn(async move {
            audio::stream_audio(audio_socket, audio_ref, audio_shutdown).await;
        });
        Some(handle)
    } else {
        None
    };

    let session = ScrcpySession {
        device_serial: serial.clone(),
        control_socket: control_socket.clone(),
        screen_width: streams.screen_width,
        screen_height: streams.screen_height,
        shutdown: shutdown.clone(),
        audio: audio_handle,
    };

    let width = streams.screen_width;
    let height = streams.screen_height;

    {
        let mut s = state.session.lock().await;
        *s = Some(session);
    }

    let session_arc = state.session.clone();
    let serial_clone = serial.clone();

    let video_codec = if settings.video_codec == "h265" {
        video::VideoCodec::H265
    } else {
        video::VideoCodec::H264
    };

    tokio::spawn(async move {
        video::stream_video(streams.video_socket, on_frame, shutdown.clone(), video_codec).await;
        scrcpy::stop_server(&serial_clone, port).await;
        let _ = server_process.kill().await;
        let mut s = session_arc.lock().await;
        *s = None;
    });

    Ok((width, height))
}

/// Whether packaged builds write their log to a file. Off by default; the
/// marker file is read at startup, so a change takes effect on the next run.
#[tauri::command]
pub async fn file_logging_enabled() -> bool {
    crate::logging_marker().is_file()
}

/// Turn the file log on or off. The redirect happens at startup - Rust looks
/// up the stderr handle on the first write - so this only records the choice.
#[tauri::command]
pub async fn set_file_logging(enabled: bool) -> Result<bool, String> {
    let marker = crate::logging_marker();
    if enabled {
        if let Some(dir) = marker.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&marker, b"1").map_err(|e| format!("write {}: {e}", marker.display()))?;
    } else {
        let _ = std::fs::remove_file(&marker);
    }
    Ok(enabled)
}

/// Where the log lands, for the Settings line that tells a user what to send.
#[tauri::command]
pub async fn file_log_path() -> String {
    crate::log_dir().join("xphoneuse.log").display().to_string()
}

#[tauri::command]
pub async fn disconnect_device(state: State<'_, AppState>) -> Result<(), String> {
    disconnect_any_session(&state).await;
    Ok(())
}

/// Connect to an iOS device: starts a headless uxplay AirPlay receiver and
/// forwards the mirrored H.264 stream to `on_frame`. The iPhone user starts
/// mirroring from Control Center once the server is advertised.
///
/// Primary path is the uxplay.dll FFI session; falls back to the legacy
/// uxplay.exe subprocess (old upstream builds with `-vrtp`) when the DLL
/// is not available.
///
/// Also detects the AC636N BLE-HID bridge (CDC-ACM) and, when present,
/// attaches it to the session so the iPhone becomes controllable.
///
/// `screen_mode` ("airplay" | "tunnel") and `input_mode` ("bridge" |
/// "tunnel") select the transports; the default is the USB tunnel on iOS
/// >= 27, AirPlay + bridge below that. The tunnel (USB, Developer Mode)
/// needs no user action on the phone — unlike AirPlay, which
/// still requires the user to start mirroring from Control Center. When the
/// tunnel cannot start, the AirPlay/bridge paths run as a fallback and the
/// result's `screen` field carries an "-fallback" suffix so the frontend can
/// toast it. Input has no fallback: the bridge is only probed when the input
/// mode is "bridge", so a failed tunnel leaves `input` = "none" (view-only).
///
/// Returns `{ hid, port, input_available, screen, input }`.
/// True when the device's iOS version supports the CoreDevice tunnel path
/// (screen stream + HID input), which needs iOS >= 27. An unreadable version
/// errs on the permissive side; a failed tunnel start still falls back.
async fn ios_tunnel_supported(udid: &str) -> bool {
    ios_version_of(udid)
        .await
        .and_then(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()))
        .is_none_or(|major| major >= 27)
}

/// The device's iOS version as usbmuxd reports it (`16.7.12`), which is what
/// picks the developer image a device wants.
async fn ios_version_of(udid: &str) -> Option<String> {
    ios_device::list_devices()
        .await
        .ok()?
        .into_iter()
        .find(|d| d.udid == udid)
        .map(|d| d.ios_version)
}

/// Where downloaded developer images are kept: one directory per bundle build,
/// under the app's data directory so they survive app updates and are shared
/// by every device that needs one.
pub fn developer_images_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .map(|d| d.join("developer-images"))
        .unwrap_or_else(|_| std::env::temp_dir().join("xphoneuse-developer-images"))
}

/// Where Android's platform-tools (adb) come from, for the window to offer
/// when the machine has none.
const PLATFORM_TOOLS_URL: &str = "https://developer.android.com/tools/releases/platform-tools";

/// Open that page. A command of its own rather than a general "open this URL",
/// so the window cannot ask the OS to open anything it likes.
#[tauri::command]
pub async fn open_platform_tools(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(PLATFORM_TOOLS_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

/// The author's X account, for the English half of the contact block (the
/// Chinese one shows the WeChat code). Like the platform-tools page, the URL
/// is fixed here rather than passed in from the window.
const X_ACCOUNT_URL: &str = "https://x.com/kindle_jiang";

#[tauri::command]
pub async fn open_x_account(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(X_ACCOUNT_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Whether the adb this process would run is usable, and where it came from.
///
/// The device list cannot say this by itself: no adb and no cable both look
/// like "no devices", and only one of them is the user's to fix.
#[derive(Serialize)]
pub struct AdbStatus {
    /// True when `adb version` ran.
    pub available: bool,
    /// The binary that was tried, for the message when it did not work.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[tauri::command]
pub async fn adb_status() -> AdbStatus {
    match adb::adb_available().await {
        Ok(()) => AdbStatus {
            available: true,
            path: adb::adb_location().await,
            error: None,
        },
        Err(error) => {
            eprintln!("[adb] {error}");
            AdbStatus {
                available: false,
                path: adb::adb_location().await,
                error: Some(error),
            }
        }
    }
}

/// Report a preparation that failed, so the UI can word it itself when it has
/// a sentence for it (`developer-mode-off` asks the user to flip the switch,
/// and the dialog answers questions the error text cannot).
fn report_image_failure(app: &AppHandle, error: &str, code: Option<&'static str>) {
    use tauri::Emitter;
    let _ = app.emit(
        "ios-image-progress",
        ImageProgressEvent {
            stage: "failed",
            got: None,
            total: None,
            seconds: None,
            build: None,
            error: Some(error.to_string()),
            code,
        },
    );
}

/// iOS 16 and older have no tunnel whose failure could tell us their developer
/// image is missing: their screenshots come from the classic `screenshotr`
/// service, and the app's own AirPlay preview needs them from the moment the
/// mirror opens. So the question is asked here, before anything starts, while
/// the user is still on the device list and can be told what is about to
/// happen.
///
/// A device that is merely busy or unplugged is not one that needs an image, so
/// a probe that fails for any other reason is not held against it - the capture
/// path reports that when it comes to it.
async fn ensure_classic_image(udid: &str, version: &str) -> Result<(), String> {
    match xphoneuse_ios::screenshot::classic_service_ready(udid).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            eprintln!("[ddi] {udid}: the classic screenshot service is not there; preparing its developer image");
            xphoneuse_ios::developer_image::ensure(
                udid,
                xphoneuse_ios::developer_image::route_for(version),
            )
            .await
            .map(|r| eprintln!("[ddi] {udid}: image {} ready", r.id))
            .map_err(|e| {
                // The UI answers this one (the dialog), but the log should
                // still say which question was asked and what came back.
                eprintln!("[ddi] {udid}: preparing the developer image failed: {e}");
                e.to_string()
            })
        }
        Err(e) => {
            eprintln!("[ddi] {udid}: classic screenshot service probe failed: {e:#}");
            Ok(())
        }
    }
}

/// The same preflight for iOS 27+, which is asked when the mirror will *not*
/// use the tunnel: the screen, HID, screenshot and pasteboard services come
/// from its developer image, and a device that is missing one of them would
/// mirror over AirPlay happily and fail at the first screenshot.
///
/// A probe that fails for any other reason (a device that is unplugged, or
/// locked) is not held against it - the connect carries on, and the capture
/// path reports what it finds. A preparation that cannot finish *is* held
/// against it: what it means is Developer Mode being off, and that is the
/// user's question to answer before a mirror opens whose screenshots would
/// quietly fail (the same treatment the tunnel and iOS 16 paths give it).
async fn ensure_tunnel_services(udid: &str, version: &str) -> Result<(), String> {
    match xphoneuse_ios::tunnel::services_ready(udid).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            eprintln!("[ddi] {udid}: the tunnel's services are not there; preparing the developer image");
            xphoneuse_ios::developer_image::ensure(
                udid,
                xphoneuse_ios::developer_image::route_for(version),
            )
            .await
            .map(|r| eprintln!("[ddi] {udid}: image {} ready", r.id))
            .map_err(|e| {
                eprintln!("[ddi] {udid}: preparing the developer image failed: {e}");
                e.to_string()
            })
        }
        Err(e) => {
            eprintln!("[ddi] {udid}: tunnel service probe failed: {e}");
            Ok(())
        }
    }
}

/// Make the Developer Mode row visible in the device's Settings.
///
/// The row is hidden on a device that has never been used for development, so
/// the manual route the dialog hands over to can end with the user staring at
/// a Settings screen that does not have the switch on it at all. Called as
/// soon as the app has a reason to think the switch is needed, and harmless
/// otherwise.
#[tauri::command]
pub async fn reveal_ios_developer_mode(udid: String) -> Result<(), String> {
    xphoneuse_ios::developer_image::reveal_developer_mode(&udid).await
}

/// Turn the device's Developer Mode on, because the user said so.
///
/// It restarts the phone (iOS 18+ immediately, older iOS on the next restart),
/// which is why this is a command of its own rather than something the connect
/// or screenshot paths do on their own initiative: the window shows a warning
/// and a countdown first, and this runs only if the user lets it.
#[tauri::command]
pub async fn enable_ios_developer_mode(udid: String) -> Result<(), String> {
    eprintln!("[ddi] {udid}: turning Developer Mode on at the user's request");
    xphoneuse_ios::developer_image::enable_developer_mode(&udid).await
}

/// Turn preparation progress into the UI's line. The download is the part that
/// takes time and the part worth a percentage; the rest is a stage name. One
/// event per percent is plenty - a 16 MB download arrives in thousands of
/// chunks, and each one as an event would swamp the IPC for nothing.
pub fn image_progress_sink(
    app: AppHandle,
) -> impl Fn(xphoneuse_ios::developer_image::ImageProgress) + Send + Sync + 'static {
    let last_report = std::sync::atomic::AtomicU64::new(0);
    move |p: xphoneuse_ios::developer_image::ImageProgress| {
        use xphoneuse_ios::developer_image::ImageProgress;
        use tauri::Emitter;
        if let ImageProgress::Downloading { got, total } = p {
            let step = total.map(|t| t / 100).unwrap_or(256 * 1024).max(1);
            let previous = last_report.swap(got, std::sync::atomic::Ordering::Relaxed);
            if got != total.unwrap_or(0) && got / step == previous / step {
                return;
            }
        }
        let _ = app.emit("ios-image-progress", ImageProgressEvent::from(p));
    }
}

/// One frame of "the device is missing its developer image", for the UI.
#[derive(Clone, Serialize)]
struct ImageProgressEvent {
    /// "checking" | "downloading" | "mounting" | "developer-mode" | "done" |
    /// "failed"
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    got: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    /// Countdown for the "developer-mode" stage, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// A stable name for the failures the UI words itself ("developer-mode-off",
    /// "developer-mode-enabled"); anything else is passed through as `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
}

impl From<xphoneuse_ios::developer_image::ImageProgress> for ImageProgressEvent {
    fn from(p: xphoneuse_ios::developer_image::ImageProgress) -> Self {
        use xphoneuse_ios::developer_image::ImageProgress as P;
        let empty = |stage| Self { stage, got: None, total: None, seconds: None, build: None, error: None, code: None };
        match p {
            P::Checking => empty("checking"),
            P::Downloading { got, total } => {
                Self { stage: "downloading", got: Some(got), total, ..empty("downloading") }
            }
            P::Mounting { sent, total } => Self {
                stage: "mounting",
                got: Some(sent),
                total: Some(total),
                ..empty("mounting")
            },
            P::Mounted { id, .. } => Self { stage: "done", build: Some(id), ..empty("done") },
        }
    }
}

/// Start the tunnel, and when it fails because the device is not offering the
/// services the developer image provides, prepare that image and try once
/// more.
///
/// This is the whole of the "iOS update broke my mirror" fix: an iOS update
/// invalidates the mounted developer image, the display/HID/screenshot/
/// pasteboard services vanish with it, and the tunnel start then fails with
/// "device does not offer com.apple.coredevice.displayservice". Nothing here
/// costs anything on a device whose image is fine - the failure is what
/// triggers the work.
async fn start_tunnel(
    app: &AppHandle,
    udid: &str,
    want_frames: bool,
) -> anyhow::Result<Arc<xphoneuse_ios::tunnel::TunnelHandle>> {
    let first = match xphoneuse_ios::tunnel::TunnelHandle::start(udid, want_frames).await {
        Err(e) if xphoneuse_ios::tunnel::is_missing_service_error(&format!("{e:#}")) => e,
        other => return other,
    };

    eprintln!("[ddi] {udid}: the device is not offering the tunnel's services; preparing the developer image");
    let route = xphoneuse_ios::developer_image::route_for(&ios_version_of(udid).await.unwrap_or_default());
    match xphoneuse_ios::developer_image::ensure(udid, route).await {
        Ok(result) => {
            eprintln!(
                "[ddi] {udid}: developer image {} ready (downloaded={})",
                result.id, result.downloaded
            );
        }
        Err(e) => {
            let code = matches!(e, xphoneuse_ios::developer_image::EnsureError::DeveloperModeOff)
                .then_some("developer-mode-off");
            report_image_failure(app, &e.to_string(), code);
            return Err(anyhow::anyhow!(
                "the device does not offer the tunnel's services and preparing its developer image failed: {e} (tunnel said: {first:#})"
            ));
        }
    }

    xphoneuse_ios::tunnel::TunnelHandle::start(udid, want_frames)
        .await
        .map_err(|e| anyhow::anyhow!("the developer image was mounted but the tunnel still failed: {e:#}"))
}

#[derive(Serialize)]
pub struct ConnectIosResult {
    pub hid: bool,
    pub port: Option<String>,
    /// Whether ANY input transport is attached (bridge or tunnel).
    pub input_available: bool,
    /// Screen transport actually in use: "airplay" | "tunnel" |
    /// "airplay-fallback".
    pub screen: String,
    /// Input transport actually in use: "bridge" | "tunnel" | "none".
    pub input: String,
}

#[tauri::command]
pub async fn connect_ios_device(
    app: AppHandle,
    udid: String,
    on_frame: Channel<FrameEvent>,
    screen_mode: Option<String>,
    input_mode: Option<String>,
    state: State<'_, AppState>,
) -> Result<ConnectIosResult, String> {
    let t_connect = std::time::Instant::now();
    eprintln!("[ios-connect] [{}] connect_ios_device({udid}) invoked", ts());
    let t = std::time::Instant::now();
    disconnect_any_session(&state).await;
    eprintln!("[ios-connect] [{}] disconnect_any_session: {:?}", ts(), t.elapsed());

    // Verify the device is currently visible to usbmuxd (pairing is not
    // required for AirPlay, but the device must be connected via USB)
    let t = std::time::Instant::now();
    ios_device::provider_for(&udid)
        .await
        .map_err(|e| format!("iOS device not available: {}", e))?;
    eprintln!("[ios-connect] [{}] provider_for: {:?}", ts(), t.elapsed());

    // Device family and OS for later analysis (Aptabase). `list_devices` is
    // what the device list polls, so its answer is cached and cheap here.
    if let Ok(devices) = ios_device::list_devices().await {
        if let Some(device) = devices.iter().find(|d| d.udid == udid) {
            let mut map = serde_json::Map::new();
            map.insert("platform".into(), serde_json::json!("ios"));
            map.insert("model".into(), serde_json::json!(device.product_type));
            map.insert("os_version".into(), serde_json::json!(device.ios_version));
            map.insert("connection_type".into(), serde_json::json!(device.connection_type));
            report_device_connected(&app, map);
        }
    }

    let shutdown = Arc::new(tokio::sync::Notify::new());

    // CoreDevice tunnel (USB, iOS 27+, Developer Mode): screen and/or input
    // transport. Falls back to AirPlay (screen) when it cannot start. It is
    // the default on supported devices; below iOS 27 any tunnel request is
    // overridden to the AirPlay / BLE-HID bridge paths.
    let tunnel_supported = ios_tunnel_supported(&udid).await;
    let mut screen_mode = screen_mode
        .unwrap_or_else(|| if tunnel_supported { "tunnel" } else { "airplay" }.to_string());
    let mut input_mode = input_mode
        .unwrap_or_else(|| if tunnel_supported { "tunnel" } else { "bridge" }.to_string());
    if !tunnel_supported && (screen_mode == "tunnel" || input_mode == "tunnel") {
        eprintln!(
            "[ios-connect] [{}] tunnel not supported on this iOS version; using AirPlay + bridge",
            ts()
        );
        if screen_mode == "tunnel" {
            screen_mode = "airplay".into();
        }
        if input_mode == "tunnel" {
            input_mode = "bridge".into();
        }
    }
    let want_tunnel_screen = screen_mode == "tunnel";
    let want_tunnel_input = input_mode == "tunnel";
    let mut screen_actual = screen_mode.clone();
    let mut input_actual = input_mode.clone();

    // Every iPhone is asked the same question before its mirror opens: can it
    // offer what the transport it is about to use needs? The answer can be a
    // download, or a restart the user has to agree to, and both belong on the
    // device list rather than behind a mirror that cannot take a picture.
    let version = ios_version_of(&udid).await.unwrap_or_default();
    if !tunnel_supported {
        ensure_classic_image(&udid, &version).await?;
    } else if !want_tunnel_screen && !want_tunnel_input {
        // When the tunnel *is* the transport, starting it is that question
        // (and `start_tunnel` prepares the image when it comes up short).
        // With AirPlay chosen instead, nothing would ask - and the services
        // the image provides are what the screenshots and the preview need.
        ensure_tunnel_services(&udid, &version).await?;
    }

    // Detect the BLE-HID bridge only when the input mode actually wants it:
    // probing every COM port costs tens of seconds when the bridge is absent,
    // which the tunnel input mode must not pay. UART_HID_PORT (e.g. COM10)
    // pins a port; otherwise probe all serial ports with the encrypted AUTH
    // handshake.
    let t = std::time::Instant::now();
    let hid = if want_tunnel_input {
        None
    } else if let Ok(name) = std::env::var("UART_HID_PORT") {
        xphoneuse_ios::hid::HidSession::connect(&name).ok()
    } else {
        tokio::task::spawn_blocking(xphoneuse_ios::hid::detect_first)
            .await
            .ok()
            .flatten()
    };
    eprintln!(
        "[ios-connect] [{}] hid detect (found={}, mode={input_mode}): {:?}",
        ts(),
        hid.is_some(),
        t.elapsed()
    );
    let hid_port = hid.as_ref().map(|s| s.port_name().to_string());
    let hid = hid.map(|s| Arc::new(tokio::sync::Mutex::new(s)));

    let mut tunnel: Option<Arc<xphoneuse_ios::tunnel::TunnelHandle>> = None;
    if want_tunnel_screen || want_tunnel_input {
        let t = std::time::Instant::now();
        let started = start_tunnel(&app, &udid, want_tunnel_screen).await;
        match started {
            Ok(handle) => {
                tunnel = Some(handle);
                eprintln!(
                    "[ios-connect] [{}] tunnel started: {:?} (input_mode={input_mode}, screen_mode={screen_mode})",
                    ts(),
                    t.elapsed()
                );
            }
            Err(e) if format!("{e:#}").contains(xphoneuse_ios::developer_image::DEVELOPER_MODE_OFF_MARK) => {
                // Not a tunnel problem to fall back from: the image cannot be
                // installed until the user turns Developer Mode on, and an
                // AirPlay session would open a mirror that cannot show
                // anything without the phone being told to start mirroring -
                // the dialog is the only useful thing to show. Handing the
                // error out is what raises it, and the user continues from
                // there into a tunnel that works.
                eprintln!("[ios-connect] [{}] {e:#}", ts());
                return Err(format!("{e:#}"));
            }
            Err(e) => {
                eprintln!(
                    "[ios-connect] [{}] tunnel start failed after {:?}, falling back: {e:#}",
                    ts(),
                    t.elapsed()
                );
                if want_tunnel_screen {
                    screen_actual = "airplay-fallback".into();
                }
                if want_tunnel_input {
                    // No bridge was probed for this mode, so there is no
                    // input transport at all — the session is view-only.
                    input_actual = "none".into();
                }
            }
        }
    }
    let input_available = hid.is_some() || tunnel.is_some();

    // Tunnel screen path: no uxplay at all — the display stream IS the
    // mirror. No Control Center interaction needed on the phone.
    if want_tunnel_screen {
        if let Some(t) = tunnel.clone() {
            let frames = t
                .subscribe_frames()
                .await
                .map_err(|e| format!("tunnel video unavailable: {e}"))?;

            {
                let mut s = state.ios_session.lock().await;
                *s = Some(crate::state::IosSession {
                    udid: udid.clone(),
                    shutdown: shutdown.clone(),
                    uxplay: None,
                    hid: hid.clone(),
                    tunnel: Some(t),
                    screen: "tunnel".into(),
                });
            }

            autoconfigure_agent_input_usb(&state, &udid, &on_frame);

            let session_arc = state.ios_session.clone();
            let this_shutdown = shutdown.clone();

            tokio::spawn(async move {
                ios::stream_ios_video_tunnel(frames, on_frame, shutdown).await;
                // Same ptr-eq session guard as the AirPlay paths: a stale
                // task from a previous session must not stop the new one.
                let mut s = session_arc.lock().await;
                let is_ours = s
                    .as_ref()
                    .map(|x| Arc::ptr_eq(&x.shutdown, &this_shutdown))
                    == Some(true);
                let session = if is_ours { s.take() } else { None };
                drop(s);
                if let Some(session) = session {
                    if let Some(tunnel) = session.tunnel {
                        tunnel.stop();
                    }
                }
            });

            eprintln!(
                "[ios-connect] [{}] connect_ios_device (tunnel screen) returning after {:?}",
                ts(),
                t_connect.elapsed()
            );
            return Ok(ConnectIosResult {
                hid: hid.is_some(),
                port: hid_port,
                input_available,
                screen: "tunnel".into(),
                input: if want_tunnel_input { "tunnel".into() } else { input_actual },
            });
        }
    }

    if xphoneuse_ios::uxplay_ffi::available() {
        let t = std::time::Instant::now();
        let mut session = xphoneuse_ios::uxplay_ffi::UxplaySession::start("XPhoneUse", 60)
            .map_err(|e| format!("Failed to start uxplay: {}", e))?;
        eprintln!(
            "[ios-connect] [{}] uxplay start: {:?} (connect_ios_device total: {:?})",
            ts(),
            t.elapsed(),
            t_connect.elapsed()
        );
        let receiver = session
            .take_receiver()
            .ok_or_else(|| "uxplay event receiver unavailable".to_string())?;

        {
            let mut s = state.ios_session.lock().await;
            *s = Some(crate::state::IosSession {
                udid: udid.clone(),
                shutdown: shutdown.clone(),
                uxplay: Some(session),
                hid: hid.clone(),
                // Headless tunnel when only the input mode is "tunnel".
                tunnel: tunnel.clone(),
                screen: "airplay".into(),
            });
        }

        let t = std::time::Instant::now();
        autoconfigure_agent_input_usb(&state, &udid, &on_frame);
        eprintln!(
            "[ios-connect] [{}] agent_input usb autoconfig: {:?}",
            ts(),
            t.elapsed()
        );

        let session_arc = state.ios_session.clone();
        let this_shutdown = shutdown.clone();
        let agent_input = state.agent_input.clone();

        tokio::spawn(async move {
            ios::stream_ios_video_ffi(receiver, on_frame, shutdown, agent_input).await;

            // Take ownership of the session and stop it outside async context.
            // Match on the session's shutdown Notify (unique per session), NOT
            // the udid: on reconnecting the same iPhone, a stale task from the
            // previous session would otherwise steal and stop the new session.
            let mut s = session_arc.lock().await;
            let is_ours = s
                .as_ref()
                .map(|x| Arc::ptr_eq(&x.shutdown, &this_shutdown))
                == Some(true);
            let session = if is_ours { s.take() } else { None };
            drop(s);
            if let Some(mut session) = session {
                if let Some(mut uxplay) = session.uxplay.take() {
                    let t = std::time::Instant::now();
                    let _ = tokio::task::spawn_blocking(move || uxplay.stop()).await;
                    eprintln!(
                        "[ios-connect] [{}] stream task cleanup, uxplay.stop: {:?}",
                        ts(),
                        t.elapsed()
                    );
                }
            }
        });

        eprintln!(
            "[ios-connect] [{}] connect_ios_device returning after {:?} (hid={}, tunnel={})",
            ts(),
            t_connect.elapsed(),
            hid.is_some(),
            tunnel.is_some()
        );
        return Ok(ConnectIosResult {
            hid: hid.is_some(),
            port: hid_port,
            input_available,
            screen: screen_actual.clone(),
            input: input_actual.clone(),
        });
    }

    // Fallback: legacy uxplay.exe subprocess streaming RTP over localhost
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind RTP socket: {}", e))?;
    let port = socket
        .local_addr()
        .map_err(|e| format!("Failed to get RTP port: {}", e))?
        .port();

    let process = ios_uxplay::UxplayProcess::spawn(port, "XPhoneUse", 60)
        .map_err(|e| format!("Failed to start uxplay: {}", e))?;

    {
        let mut s = state.ios_session.lock().await;
        *s = Some(crate::state::IosSession {
            udid: udid.clone(),
            shutdown: shutdown.clone(),
            uxplay: None,
            hid: hid.clone(),
            tunnel: tunnel.clone(),
            screen: "airplay".into(),
        });
    }

    let t = std::time::Instant::now();
    autoconfigure_agent_input_usb(&state, &udid, &on_frame);
    eprintln!(
        "[ios-connect] [{}] agent_input usb autoconfig: {:?}",
        ts(),
        t.elapsed()
    );

    let session_arc = state.ios_session.clone();
    let this_shutdown = shutdown.clone();

    tokio::spawn(async move {
        ios::stream_ios_video(socket, process.child, on_frame, shutdown).await;
        // Match on the session's shutdown Notify (unique per session), NOT the
        // udid — see the FFI path above.
        let mut s = session_arc.lock().await;
        if s.as_ref().map(|x| Arc::ptr_eq(&x.shutdown, &this_shutdown)) == Some(true) {
            *s = None;
        }
    });

    eprintln!(
        "[ios-connect] [{}] connect_ios_device returning after {:?} (hid={}, tunnel={})",
        ts(),
        t_connect.elapsed(),
        hid.is_some(),
        tunnel.is_some()
    );
    Ok(ConnectIosResult {
        hid: hid.is_some(),
        port: hid_port,
        input_available,
        screen: screen_actual,
        input: input_actual,
    })
}

/// Send one input command line to the active iOS session: through the
/// CoreDevice tunnel when its input is ready (input mode "tunnel"), else
/// through the BLE-HID bridge as an encrypted command. Errors when neither
/// transport is present.
async fn ios_hid_send(state: &AppState, cmd: String) -> Result<(), String> {
    // Tunnel first: the same bridge command string is parsed into a HID
    // verb (see xphoneuse_ios::tunnel::parse_bridge_command) and executed
    // over CoreDevice.
    let tunnel = {
        let s = state.ios_session.lock().await;
        s.as_ref().and_then(|s| s.tunnel.clone())
    };
    if let Some(t) = tunnel {
        if t.input_ready() {
            let verb = xphoneuse_ios::tunnel::parse_bridge_command(&cmd)
                .map_err(|e| format!("cannot route {cmd:?} over the tunnel: {e}"))?;
            return t.input(verb).await;
        }
        // Auth gate still open (stream started <300ms ago): fall through to
        // the bridge when one exists, so early inputs are not lost.
    }

    let hid = {
        let s = state.ios_session.lock().await;
        match s.as_ref() {
            Some(s) => s
                .hid
                .clone()
                .ok_or_else(|| "Input control is not supported for this iOS session (no HID bridge detected)".to_string())?,
            None => return Err("Not connected".to_string()),
        }
    };
    let reply = tokio::task::spawn_blocking(move || {
        let mut guard = hid.blocking_lock();
        guard.send(&cmd)
    })
    .await
    .map_err(|e| format!("HID task failed: {e}"))?
    .map_err(|e| format!("HID bridge error: {e}"))?;

    if let Some(err) = reply.strip_prefix("ERR") {
        return Err(xphoneuse_ios::hid::map_hid_error(err));
    }
    Ok(())
}

/// True when an iOS session with a usable input transport (HID bridge or
/// ready CoreDevice tunnel) is active, i.e. controllable.
async fn ios_hid_active(state: &AppState) -> bool {
    state
        .ios_session
        .lock()
        .await
        .as_ref()
        .map(|s| s.hid.is_some() || s.tunnel.as_ref().is_some_and(|t| t.input_ready()))
        .unwrap_or(false)
}

/// Switch the iOS input transport online, without touching the video.
///
/// * `"tunnel"` — start (or adopt) a headless CoreDevice tunnel and attach it
///   to the running session; `ios_hid_send` starts routing through it.
/// * `"bridge"` — stop a headless tunnel and go back to the BLE-HID bridge.
///
/// Switching the SCREEN transport needs a reconnect; the frontend handles
/// that with its existing silent-reconnect flow rather than calling this.
#[tauri::command]
pub async fn set_ios_transport_modes(
    app: AppHandle,
    input_mode: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (udid, has_tunnel, screen) = {
        let s = state.ios_session.lock().await;
        let s = s.as_ref().ok_or("Not connected")?;
        (
            s.udid.clone(),
            s.tunnel.is_some(),
            s.screen.clone(),
        )
    };

    match input_mode.as_str() {
        "tunnel" => {
            // The tunnel path needs iOS >= 27; the Settings UI disables the
            // option there, this keeps other callers honest.
            if !ios_tunnel_supported(&udid).await {
                return Err("USB tunnel needs iOS 27+".to_string());
            }
            if has_tunnel {
                return Ok("tunnel".to_string());
            }
            let t = start_tunnel(&app, &udid, false)
                .await
                .map_err(|e| format!("tunnel start failed: {e:#}"))?;
            let mut s = state.ios_session.lock().await;
            if let Some(s) = s.as_mut() {
                s.tunnel = Some(t);
            }
            Ok("tunnel".to_string())
        }
        "bridge" => {
            // The tunnel cannot be stopped while it provides the video.
            if screen == "tunnel" {
                return Err(
                    "The tunnel is currently providing the screen; reconnect with AirPlay screen mode to use the Bluetooth bridge".to_string(),
                );
            }
            let tunnel = {
                let mut s = state.ios_session.lock().await;
                s.as_mut().and_then(|s| s.tunnel.take())
            };
            if let Some(t) = tunnel {
                t.stop();
            }
            // Tunnel input mode skips COM probing at connect time, so the
            // bridge has to be brought up on demand here. This can take tens
            // of seconds when no bridge is attached.
            let has_hid = {
                let s = state.ios_session.lock().await;
                s.as_ref().is_some_and(|s| s.hid.is_some())
            };
            if has_hid {
                return Ok("bridge".to_string());
            }
            let hid = if let Ok(name) = std::env::var("UART_HID_PORT") {
                xphoneuse_ios::hid::HidSession::connect(&name).ok()
            } else {
                tokio::task::spawn_blocking(xphoneuse_ios::hid::detect_first)
                    .await
                    .ok()
                    .flatten()
            };
            match hid {
                Some(h) => {
                    let mut s = state.ios_session.lock().await;
                    if let Some(s) = s.as_mut() {
                        s.hid = Some(Arc::new(tokio::sync::Mutex::new(h)));
                    }
                    Ok("bridge".to_string())
                }
                None => Err(
                    "no BLE-HID bridge found on any serial port - switch back to the USB tunnel or reconnect".to_string(),
                ),
            }
        }
        other => Err(format!("unknown input mode: {other:?}")),
    }
}

#[tauri::command]
pub async fn set_muted(muted: bool, state: State<'_, AppState>) -> Result<(), String> {
    if state.session.lock().await.is_none() && state.ios_session.lock().await.is_some() {
        return Err("Mute is not supported for iOS devices (audio is not forwarded)".to_string());
    }
    let session = state.session.lock().await;
    let session = session.as_ref().ok_or("Not connected")?;
    if let Some(audio) = &session.audio {
        if muted {
            audio.sink.set_volume(0.0);
        } else {
            audio.sink.set_volume(1.0);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn send_touch(
    action: String,
    x: f64,
    y: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            let px = (x * session.screen_width as f64) as u32;
            let py = (y * session.screen_height as f64) as u32;
            return control::inject_touch(
                &session.control_socket,
                &action,
                px,
                py,
                session.screen_width as u16,
                session.screen_height as u16,
            )
            .await
            .map_err(|e| e.to_string());
        }
    }
    // iOS: route through the BLE-HID bridge (normalized → 1920 logical)
    let cmd = xphoneuse_ios::hid::map_touch(&action, x, y).map_err(|e| e.to_string())?;
    ios_hid_send(&state, cmd).await
}

#[tauri::command]
pub async fn send_key(
    keycode: u32,
    action: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            return control::inject_keycode(&session.control_socket, &action, keycode, 0, 0)
                .await
                .map_err(|e| e.to_string());
        }
    }
    let cmd = xphoneuse_ios::hid::map_key(keycode).map_err(|e| e.to_string())?;
    ios_hid_send(&state, cmd).await
}

#[tauri::command]
pub async fn send_text(
    text: String,
    enter: Option<bool>,
    method: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let enter = enter.unwrap_or(false);
    // An explicit method wins; otherwise the app setting decides how
    // non-ASCII text leaves the building (Settings → iOS connection).
    let method = match method.as_deref() {
        Some(m) => xphoneuse_ios::text_input::TextInputMethod::parse(m)?,
        None => *state.unicode_text_default.lock().unwrap(),
    };
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            // Android types any Unicode through scrcpy; the method is moot.
            if !text.is_empty() {
                control::send_text(&session.control_socket, &text)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if enter {
                // scrcpy's inject_text cannot carry a newline - press the key.
                return control::inject_keycode(&session.control_socket, "press", 66, 0, 0)
                    .await
                    .map_err(|e| e.to_string());
            }
            return Ok(());
        }
    }

    ios_deliver_text(&state, &text, enter, method).await
}

/// What the current iOS session can do with text.
async fn ios_text_capabilities(state: &AppState) -> xphoneuse_ios::text_input::TextCapabilities {
    use xphoneuse_ios::text_input::TextCapabilities;
    let s = state.ios_session.lock().await;
    let session = s.as_ref();
    TextCapabilities {
        tunnel: session
            .and_then(|s| s.tunnel.as_ref())
            .is_some_and(|t| t.input_ready()),
        // The bridge specifically: `ios_hid_active` is also true for a
        // tunnel-only session, which cannot take a bridge route.
        bridge: session.is_some_and(|s| s.hid.is_some()),
        agent_input: state.agent_input.lock().unwrap().is_some(),
    }
}

/// Carry out one planned route on iOS.
async fn ios_exec_text_route(
    state: &AppState,
    route: xphoneuse_ios::text_input::TextRoute,
    text: &str,
    enter: bool,
) -> Result<(), String> {
    use xphoneuse_ios::text_input::TextRoute;
    match route {
        // The tunnel picks for itself: it types what the HID keyboard can
        // carry and pastes the rest.
        TextRoute::TunnelKeys | TextRoute::TunnelPaste => {
            if !text.is_empty() {
                let tunnel = {
                    let s = state.ios_session.lock().await;
                    s.as_ref().and_then(|s| s.tunnel.clone())
                }
                .ok_or_else(|| "No iOS tunnel is attached".to_string())?;
                tunnel
                    .input(xphoneuse_ios::tunnel::HidVerb::Text { text: text.to_string() })
                    .await?;
            }
            if enter {
                ios_hid_send(
                    state,
                    xphoneuse_ios::hid::map_key(66).map_err(|e| e.to_string())?,
                )
                .await?;
            }
            Ok(())
        }
        TextRoute::Bridge => {
            let mut cmds = xphoneuse_ios::hid::map_text(text);
            if enter {
                if let Ok(cmd) = xphoneuse_ios::hid::map_key(66) {
                    cmds.push(cmd);
                }
            }
            if cmds.is_empty() {
                return Ok(()); // nothing printable to send
            }
            for cmd in cmds {
                ios_hid_send(state, cmd).await?;
            }
            Ok(())
        }
        TextRoute::AgentInput => {
            let text = text.to_string();
            agent_input_send(state, move |c| {
                if !text.is_empty() {
                    c.send_text(&text)?;
                }
                if enter {
                    // WeChat and similar apps need a moment to commit the
                    // text insertion; an immediate "\n" is treated as a
                    // literal newline instead of the send key.
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    c.enter()?;
                }
                Ok(())
            })
            .await
        }
    }
}

/// Deliver text to an iPhone: plan the route (the policy lives in
/// `xphoneuse_ios::text_input`, shared with the MCP server), then run it.
///
/// AgentInput dials lazily, so "configured" only turns into "reachable" here.
/// A failed dial means the keyboard is not installed or not the active one,
/// and nothing has been typed yet - which is what makes falling back to the
/// clipboard safe. A failure *after* that point is reported as-is: retrying
/// over another transport could type the message twice.
async fn ios_deliver_text(
    state: &AppState,
    text: &str,
    enter: bool,
    method: xphoneuse_ios::text_input::TextInputMethod,
) -> Result<(), String> {
    use xphoneuse_ios::text_input::{self, TextCapabilities, TextRoute};

    let caps = ios_text_capabilities(state).await;
    let plan = text_input::plan_text_delivery(text, method, caps)?;
    if plan.route != TextRoute::AgentInput {
        return ios_exec_text_route(state, plan.route, text, enter).await;
    }

    if let Err(e) = text_input::ensure_agent_input(&state.agent_input).await {
        let fallback = text_input::plan_text_delivery(
            text,
            method,
            TextCapabilities { agent_input: false, ..caps },
        )?;
        println!(
            "[ios] AgentInput unreachable ({e}); falling back to {}",
            fallback.route.as_str()
        );
        return ios_exec_text_route(state, fallback.route, text, enter).await;
    }
    ios_exec_text_route(state, TextRoute::AgentInput, text, enter).await
}

/// Run a blocking AgentInput operation on the shared client.
async fn agent_input_send<F>(state: &AppState, op: F) -> Result<(), String>
where
    F: FnOnce(&mut xphoneuse_ios::agent_input::AgentInputClient) -> Result<(), String> + Send + 'static,
{
    let agent_input = state.agent_input.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = agent_input.lock().unwrap();
        let client = guard
            .as_mut()
            .ok_or_else(|| "AgentInput is not configured".to_string())?;
        op(client)
    })
    .await
    .map_err(|e| format!("AgentInput task failed: {e}"))?
}

/// Configure and connect the AgentInput text-input keyboard. The connection
/// is validated immediately; later sends re-dial if the keyboard reopens.
///
/// `host: null` (or empty) means USB mode: a localhost forward is started
/// that pipes to the phone's AgentInput port through usbmuxd, so the phone
/// and computer do not need to be on the same LAN (only the USB cable).
#[tauri::command]
pub async fn set_agent_input(
    host: Option<String>,
    port: Option<u16>,
    pin: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let port = port.unwrap_or(xphoneuse_ios::agent_input::DEFAULT_PORT);
    let host = host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string);

    let forward = match &host {
        None => {
            let udid = {
                let s = state.ios_session.lock().await;
                s.as_ref().map(|s| s.udid.clone())
            }
            .ok_or("No iPhone connected - plug it in over USB first")?;
            Some(xphoneuse_ios::usb_forward::UsbForward::start(&udid, port)?)
        }
        Some(_) => None,
    };

    let mut client = xphoneuse_ios::agent_input::AgentInputClient::new(
        host.unwrap_or_else(|| "127.0.0.1".into()),
        forward.as_ref().map(|f| f.local_port).unwrap_or(port),
        pin,
    );
    let (mut client, res) = tokio::task::spawn_blocking(move || {
        let res = client.connect();
        (client, res)
    })
    .await
    .map_err(|e| format!("AgentInput task failed: {e}"))?;
    res?;
    let desc = if let Some(f) = forward {
        let local = f.local_port;
        {
            let mut rb = state.agent_input.lock().unwrap();
            *rb = Some(client);
            *state.agent_input_forward.lock().unwrap() = Some(f);
        }
        format!("AgentInput connected via USB (local port {local} -> phone:{port})")
    } else {
        let desc = format!("AgentInput connected at {}:{}", client.host(), client.port());
        {
            let mut rb = state.agent_input.lock().unwrap();
            *rb = Some(client);
            // A manual IP config replaces any previous USB forward.
            *state.agent_input_forward.lock().unwrap() = None;
        }
        desc
    };
    Ok(desc)
}

/// Forget the AgentInput configuration (and stop its USB forward, if any).
#[tauri::command]
pub async fn clear_agent_input(state: State<'_, AppState>) -> Result<(), String> {
    let mut rb = state.agent_input.lock().unwrap();
    *rb = None;
    *state.agent_input_forward.lock().unwrap() = None;
    Ok(())
}

/// Auto-configure AgentInput over USB when an iOS session starts: forward the
/// phone's AgentInput port through usbmuxd so non-English text works with no
/// configuration and no shared LAN. The client is lazy - it only dials when
/// text is first sent (the phone-side server exists only while the AgentInput
/// keyboard is active).
fn autoconfigure_agent_input_usb(state: &AppState, udid: &str, on_frame: &Channel<FrameEvent>) {
    let mut rb = state.agent_input.lock().unwrap();
    let mut fw = state.agent_input_forward.lock().unwrap();
    // Rebuild the forward when one exists from a previous session: it is
    // bound to that session's device. A manual IP config (no forward) is
    // left alone.
    if fw.is_some() || rb.is_none() {
        match xphoneuse_ios::usb_forward::UsbForward::start(udid, xphoneuse_ios::agent_input::DEFAULT_PORT) {
            Ok(f) => {
                let local_port = f.local_port;
                match rb.as_mut() {
                    // USB mode from a previous session: keep the PIN, just
                    // retarget the client at the new local port.
                    Some(client) => client.set_port(local_port),
                    None => {
                        *rb = Some(xphoneuse_ios::agent_input::AgentInputClient::new(
                            "127.0.0.1",
                            local_port,
                            None,
                        ));
                        let _ = on_frame.send(FrameEvent::AgentInput {
                            host: "USB".to_string(),
                        });
                        println!(
                            "[ios-connect] agent_input auto-configured via USB (local port {local_port})"
                        );
                    }
                }
                *fw = Some(f);
            }
            Err(e) => eprintln!("[ios-connect] agent_input USB forward failed: {e}"),
        }
    }
}

/// True when a AgentInput client is configured (not necessarily connected -
/// the phone-side server only exists while its keyboard is active).
#[tauri::command]
pub async fn get_agent_input_configured(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.agent_input.lock().unwrap().is_some())
}

/// How non-ASCII text is sent to an iPhone by default: "clipboard" (device
/// pasteboard + Cmd+V) or "agentinput" (the AgentInput keyboard). The MCP
/// server reads the same value through a host hook.
#[tauri::command]
pub async fn get_unicode_text_default(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.unicode_text_default.lock().unwrap().as_str().to_string())
}

#[tauri::command]
pub async fn set_unicode_text_default(
    method: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let parsed = xphoneuse_ios::text_input::TextInputMethod::parse(&method)?;
    *state.unicode_text_default.lock().unwrap() = parsed;
    Ok(parsed.as_str().to_string())
}

/// Whether the app starts with the system.
///
/// The OS is the source of truth: the login item lives in the registry (or a
/// LaunchAgent, or an autostart .desktop file), so this asks it rather than
/// keeping a copy that could drift from what the system will actually do.
#[tauri::command]
pub async fn autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// Turn starting-with-the-system on or off, and answer with the state the
/// system reports afterwards - not with what was asked for. A request the OS
/// refuses (macOS policy, a locked-down registry) has to show up as a switch
/// that did not move, rather than as one that did and lies.
#[tauri::command]
pub async fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let changed = if enabled { manager.enable() } else { manager.disable() };
    changed.map_err(|e| e.to_string())?;
    manager.is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_scroll(
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            let px = (x * session.screen_width as f64) as u32;
            let py = (y * session.screen_height as f64) as u32;
            let sx = (dx * 120.0) as i16;
            let sy = (dy * 120.0) as i16;
            return control::inject_scroll(
                &session.control_socket,
                px,
                py,
                session.screen_width as u16,
                session.screen_height as u16,
                sx,
                sy,
            )
            .await
            .map_err(|e| e.to_string());
        }
    }
    let cmd = xphoneuse_ios::hid::map_scroll(dx, dy);
    ios_hid_send(&state, cmd).await
}

/// Capture the current device's screen.
///
/// A capture can prepare a missing developer image as a side effect, but it can
/// never turn Developer Mode on: that restarts the phone, and this is called
/// from a timer (the AirPlay preview asks once a second) as much as from a
/// person. The window asks about the switch in its own dialog.
#[tauri::command]
pub async fn take_screenshot(state: State<'_, AppState>) -> Result<String, String> {
    let t0 = std::time::Instant::now();
    match take_screenshot_png(&state).await {
        Ok(png_data) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
            eprintln!(
                "[ios-screenshot] [{}] command total: {:?} (base64: {} chars)",
                ts(),
                t0.elapsed(),
                b64.len()
            );
            Ok(b64)
        }
        Err(e) => {
            eprintln!(
                "[ios-screenshot] [{}] command FAILED after {:?}: {e}",
                ts(),
                t0.elapsed()
            );
            Err(e)
        }
    }
}

/// Wall-clock log timestamp (seconds.mmm since process start) so interleaved
/// log lines from concurrent calls can be ordered after the fact.
pub fn ts() -> String {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    let ms = start.elapsed().as_millis();
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

pub(crate) async fn take_screenshot_png(state: &AppState) -> Result<Vec<u8>, String> {
    // Clone what we need and release the session locks BEFORE the slow I/O,
    // so a long screenshot never blocks other commands (hid input, connect,
    // concurrent screenshot calls) waiting on the same mutex.
    let (android_serial, ios_udid) = {
        let session = state.session.lock().await;
        match session.as_ref() {
            Some(s) => (Some(s.device_serial.clone()), None),
            None => {
                drop(session);
                let ios = state.ios_session.lock().await;
                match ios.as_ref() {
                    Some(s) => (None, Some(s.udid.clone())),
                    None => (None, None),
                }
            }
        }
    };

    if let Some(serial) = android_serial {
        return adb::exec_out_screencap(&serial)
            .await
            .map_err(|e| e.to_string());
    }
    if let Some(udid) = ios_udid {
        return ios_screenshot::take_screenshot(&udid)
            .await
            .map_err(|e| e.to_string());
    }

    Err("Not connected".to_string())
}

#[tauri::command]
pub async fn press_button(button: String, state: State<'_, AppState>) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            let keycode = match button.as_str() {
                "home" => control::KEYCODE_HOME,
                "back" => control::KEYCODE_BACK,
                "recents" => control::KEYCODE_APP_SWITCH,
                "power" => control::KEYCODE_POWER,
                "volume_up" => control::KEYCODE_VOLUME_UP,
                "volume_down" => control::KEYCODE_VOLUME_DOWN,
                _ => return Err(format!("Unknown button: {}", button)),
            };
            control::inject_keycode(&session.control_socket, "down", keycode, 0, 0)
                .await
                .map_err(|e| e.to_string())?;
            return control::inject_keycode(&session.control_socket, "up", keycode, 0, 0)
                .await
                .map_err(|e| e.to_string());
        }
    }
    // iOS: consumer key through the bridge. HOME works; the app switcher
    // has no consumer key on iOS, so "recents" is synthesized as the system
    // gesture (slow drag up from the bottom edge, pause, release).
    if button == "recents" {
        for (cmd, delay) in xphoneuse_ios::hid::app_switcher_gesture() {
            ios_hid_send(&state, cmd).await?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        return Ok(());
    }
    let cmd = xphoneuse_ios::hid::map_button(&button).map_err(|e| e.to_string())?;
    ios_hid_send(&state, cmd).await
}

#[tauri::command]
pub async fn update_screen_size(width: u32, height: u32, state: State<'_, AppState>) -> Result<(), String> {
    let mut session = state.session.lock().await;
    let session = session.as_mut().ok_or("Not connected")?;
    session.screen_width = width;
    session.screen_height = height;
    Ok(())
}

#[tauri::command]
pub async fn rotate_device(state: State<'_, AppState>) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            return control::rotate_device(&session.control_socket)
                .await
                .map_err(|e| e.to_string());
        }
    }
    // The bridge has no orientation control; the mirror follows the phone.
    Err("Rotation is not supported for iOS devices".to_string())
}

#[tauri::command]
pub async fn wake_screen(state: State<'_, AppState>) -> Result<(), String> {
    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            let is_on = adb::shell(&session.device_serial, "dumpsys power | grep 'Display Power'")
                .await
                .map_err(|e| e.to_string())?
                .wait_with_output()
                .await
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("state=ON"))
                .unwrap_or(true);

            if !is_on {
                control::inject_keycode(
                    &session.control_socket,
                    "down",
                    control::KEYCODE_WAKEUP,
                    0,
                    0,
                )
                .await
                .map_err(|e| e.to_string())?;
                control::inject_keycode(
                    &session.control_socket,
                    "up",
                    control::KEYCODE_WAKEUP,
                    0,
                    0,
                )
                .await
                .map_err(|e| e.to_string())?;
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            return Ok(());
        }
    }
    // iOS has no "wake without toggling" primitive - the one HID key that
    // touches the screen state is the power button, and it is a toggle. A
    // blind press on a phone that is already awake puts it to sleep, which is
    // the opposite of what the caller wants (macro playback wakes first).
    //
    // The tunnel is the one thing that can tell us: iOS stops sending video
    // when the phone sleeps, so a stalled frame stream means the screen is
    // off. Anything else - a headless input-only tunnel, AirPlay, the BLE
    // bridge - gets no press at all, because guessing wrong costs the user
    // their screen.
    const AWAKE_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
    let tunnel = {
        let s = state.ios_session.lock().await;
        s.as_ref().and_then(|s| s.tunnel.clone())
    };
    let Some(tunnel) = tunnel else {
        return Ok(());
    };
    match tunnel.frame_age() {
        Some(age) if age < AWAKE_WINDOW => Ok(()), // awake: leave it alone
        Some(_) => {
            ios_hid_send(&state, "KEY POWER".to_string()).await?;
            // Give the screen time to come up before the caller types into it.
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            Ok(())
        }
        None => Ok(()), // never streamed: state unknown, do not toggle
    }
}

#[tauri::command]
pub async fn play_macro(
    events: Vec<macro_engine::TimedEvent>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // A stop left over from a previous round must not end this one before it
    // starts: the loop plays the same macro again and again, and Ctrl+E is
    // what ends it - not the next call.
    state.macro_stop.store(false, std::sync::atomic::Ordering::Relaxed);
    let stop = || state.macro_stop.load(std::sync::atomic::Ordering::Relaxed);

    {
        let session = state.session.lock().await;
        if let Some(session) = session.as_ref() {
            let (socket, sw, sh) = (
                session.control_socket.clone(),
                session.screen_width,
                session.screen_height,
            );
            return macro_engine::play_events(&events, &socket, sw, sh, &stop)
                .await
                .map_err(|e| e.to_string());
        }
    }

    // iOS: replay each event through the HID bridge (preserving timings).
    if !ios_hid_active(&state).await {
        return Err("Input control is not supported for this iOS session (no HID bridge detected)".to_string());
    }
    // Same as the Android path: a finger left down by a stop would stay down.
    let mut held: Option<(f64, f64)> = None;
    let mut prev_ts = 0u64;
    for timed in &events {
        if stop() {
            break;
        }
        let sleep_ms = timed.timestamp_ms.saturating_sub(prev_ts);
        if sleep_ms > 0 && !macro_engine::wait_or_stop(sleep_ms, &stop).await {
            break;
        }
        prev_ts = timed.timestamp_ms;
        match &timed.event {
            macro_engine::MacroEvent::Touch { action, x, y } => {
                held = if action == "up" { None } else { Some((*x, *y)) };
                let cmd = xphoneuse_ios::hid::map_touch(action, *x, *y).map_err(|e| e.to_string())?;
                ios_hid_send(&state, cmd).await?;
            }
            macro_engine::MacroEvent::Text { text } => {
                // Deliver it the way the app is configured to deliver text
                // (clipboard or AgentInput). The bridge grammar alone would
                // drop every non-ASCII character without saying so.
                let method = *state.unicode_text_default.lock().unwrap();
                ios_deliver_text(&state, text, false, method).await?;
            }
            macro_engine::MacroEvent::Key { keycode, .. } => {
                let cmd = xphoneuse_ios::hid::map_key(*keycode).map_err(|e| e.to_string())?;
                ios_hid_send(&state, cmd).await?;
            }
            macro_engine::MacroEvent::Scroll { dx, dy, .. } => {
                let cmd = xphoneuse_ios::hid::map_scroll(*dx, *dy);
                ios_hid_send(&state, cmd).await?;
            }
            macro_engine::MacroEvent::Button { button } => {
                // iOS has no consumer key for the app switcher, so `recents`
                // replays as the same system gesture the live press uses.
                if button == "recents" {
                    for (cmd, delay) in xphoneuse_ios::hid::app_switcher_gesture() {
                        ios_hid_send(&state, cmd).await?;
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                    }
                } else {
                    let cmd = xphoneuse_ios::hid::map_button(button).map_err(|e| e.to_string())?;
                    ios_hid_send(&state, cmd).await?;
                }
            }
            macro_engine::MacroEvent::Wait { .. } => {}
        }
    }
    if let Some((x, y)) = held {
        let cmd = xphoneuse_ios::hid::map_touch("up", x, y).map_err(|e| e.to_string())?;
        ios_hid_send(&state, cmd).await?;
    }
    Ok(())
}

/// Cut the macro playing now short at its next event.
///
/// Both the loop button and Ctrl+E land here. Doing nothing when nothing is
/// playing is deliberate: the key is bound globally and has to be harmless.
#[tauri::command]
pub fn stop_macro(state: State<'_, AppState>) {
    state.macro_stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn macro_path(dir: &str, name: &str) -> PathBuf {
    PathBuf::from(dir).join(format!("{}.json", sanitize_filename(name)))
}

#[derive(Serialize)]
pub struct MacroInfo {
    pub name: String,
    pub event_count: usize,
}

#[tauri::command]
pub async fn get_default_macros_dir(app: AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("macros");
    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn list_macro_files(dir: String) -> Result<Vec<MacroInfo>, String> {
    let path = PathBuf::from(&dir);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let order_path = path.join("_order.json");
    let order: Vec<String> = if order_path.exists() {
        let data = tokio::fs::read_to_string(&order_path)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|e| e.to_string())?;

    let mut macros: Vec<MacroInfo> = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".json") || fname == "_order.json" {
            continue;
        }
        let data = match tokio::fs::read_to_string(entry.path()).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        let m: macro_engine::Macro = match serde_json::from_str(&data) {
            Ok(m) => m,
            Err(_) => continue,
        };
        macros.push(MacroInfo {
            name: m.name,
            event_count: m.events.len(),
        });
    }

    if !order.is_empty() {
        macros.sort_by_key(|m| {
            order
                .iter()
                .position(|n| n == &m.name)
                .unwrap_or(usize::MAX)
        });
    }

    Ok(macros)
}

#[tauri::command]
pub async fn load_macro_file(
    dir: String,
    name: String,
) -> Result<macro_engine::Macro, String> {
    let path = macro_path(&dir, &name);
    let data = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read macro: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse macro: {}", e))
}

#[tauri::command]
pub async fn save_macro_file(
    dir: String,
    macro_data: macro_engine::Macro,
) -> Result<(), String> {
    let path = PathBuf::from(&dir);
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|e| e.to_string())?;

    let file_path = macro_path(&dir, &macro_data.name);
    let json = serde_json::to_string_pretty(&macro_data).map_err(|e| e.to_string())?;
    tokio::fs::write(&file_path, json)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_macro_file(dir: String, name: String) -> Result<(), String> {
    let path = macro_path(&dir, &name);
    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn rename_macro_file(
    dir: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    let old_path = macro_path(&dir, &old_name);
    if !old_path.exists() {
        return Err(format!("Macro '{}' not found", old_name));
    }

    let data = tokio::fs::read_to_string(&old_path)
        .await
        .map_err(|e| e.to_string())?;
    let mut m: macro_engine::Macro =
        serde_json::from_str(&data).map_err(|e| e.to_string())?;
    m.name = new_name.clone();

    let new_path = macro_path(&dir, &new_name);
    let json = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    tokio::fs::write(&new_path, json)
        .await
        .map_err(|e| e.to_string())?;

    if old_path != new_path {
        tokio::fs::remove_file(&old_path)
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[tauri::command]
pub async fn save_macros_order(dir: String, order: Vec<String>) -> Result<(), String> {
    let path = PathBuf::from(&dir);
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|e| e.to_string())?;

    let order_path = path.join("_order.json");
    let json = serde_json::to_string_pretty(&order).map_err(|e| e.to_string())?;
    tokio::fs::write(&order_path, json)
        .await
        .map_err(|e| e.to_string())
}

/// The default save location for screenshots and recordings when the user
/// has not picked one in Settings: the system Documents directory.
#[tauri::command]
pub async fn default_save_dir(app: AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .document_dir()
        .map_err(|e| format!("no Documents directory: {e}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn save_file(path: String, data: String) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;
    let file_path = PathBuf::from(&path);
    if let Some(parent) = file_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&file_path, bytes)
        .await
        .map_err(|e| format!("Failed to write file: {}", e))
}

#[tauri::command]
pub async fn wifi_connect(address: String) -> Result<(), String> {
    adb::connect_device(&address).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn wifi_disconnect(address: String) -> Result<(), String> {
    adb::disconnect_device(&address).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_device_ip(serial: String) -> Result<Option<String>, String> {
    adb::get_device_ip(&serial).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn wifi_enable(serial: String) -> Result<String, String> {
    let ip = adb::get_device_ip(&serial)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Device is not connected to WiFi. Connect it to the same network as this computer.".to_string())?;

    adb::tcpip(&serial, 5555)
        .await
        .map_err(|e| e.to_string())?;

    let addr = format!("{}:5555", ip);
    let mut connected = false;
    for _ in 0..5 {
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
        if adb::connect_device(&addr).await.is_ok() {
            connected = true;
            break;
        }
    }

    if !connected {
        return Err(format!("Could not connect to {} -- make sure both devices are on the same WiFi network", addr));
    }

    Ok(addr)
}

#[tauri::command]
pub async fn start_mcp_server(
    port: u16,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut mcp = state.mcp.lock().await;

    if mcp.cancel.is_some() {
        return Ok(());
    }

    let scrcpy_server_path = app
        .path()
        .resource_dir()
        .ok()
        .map(|dir| {
            dir.join("resources")
                .join("scrcpy-server-v2.7")
                .to_string_lossy()
                .to_string()
        });

    let ct = CancellationToken::new();
    mcp.cancel = Some(ct.clone());
    mcp.port = port;

    let app_for_frames = app.clone();
    let app_for_connect = app.clone();
    let app_for_session = app.clone();

    let agent_input = state.agent_input.clone();
    // Let the MCP server request live mirror frames: the hook emits to the
    // webview, whose listener replies via `mcp_frame_reply`.
    let mut hooks = xphoneuse_mcp::HostHooks::default();
    hooks.frames = state.mcp_frames.clone();
    hooks.frames.request = Some(Arc::new(move |id| {
        use tauri::Emitter;
        let _ = app_for_frames.emit("mcp-frame-request", id);
    }));
    // Ask the webview to run its normal connect flow: the app window
    // switches to the mirror screen and streams the video itself.
    hooks.ios_connect = Some(Arc::new(move |udid: &str| {
        use tauri::Emitter;
        let _ = app_for_connect.emit(
            "mcp-connect-device",
            serde_json::json!({ "platform": "ios", "serial": udid }),
        );
    }));
    // How the user configured iOS non-ASCII input, read at call time so a
    // settings change applies without restarting the MCP server.
    let method_state = app.clone();
    hooks.text_input_method = Some(Arc::new(move || {
        use tauri::Manager;
        let state = method_state.state::<AppState>();
        let guard = state
            .unicode_text_default
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }));
    // Expose the app's current iOS session (udid + shared HID handle) to
    // the MCP server. try_lock: the getter runs inside MCP async code.
    hooks.ios_session = Some(Arc::new(move || {
        use tauri::Manager;
        let state = app_for_session.state::<AppState>();
        state
            .ios_session
            .clone()
            .try_lock()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|s| xphoneuse_mcp::IosHandle {
                    udid: s.udid.clone(),
                    hid: s.hid.clone(),
                    tunnel: s.tunnel.clone(),
                })
            })
    }));
    // Same two hooks for Android (scrcpy): the shared control socket makes
    // CLI and app input operate the same device.
    let app_for_android_connect = app.clone();
    let app_for_android_session = app.clone();
    hooks.android_connect = Some(Arc::new(move |serial: &str| {
        use tauri::Emitter;
        let _ = app_for_android_connect.emit(
            "mcp-connect-device",
            serde_json::json!({ "platform": "android", "serial": serial }),
        );
    }));
    hooks.android_session = Some(Arc::new(move || {
        use tauri::Manager;
        let state = app_for_android_session.state::<AppState>();
        state
            .session
            .clone()
            .try_lock()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|s| xphoneuse_mcp::AndroidHandle {
                    serial: s.device_serial.clone(),
                    control_socket: s.control_socket.clone(),
                    screen_width: s.screen_width,
                    screen_height: s.screen_height,
                })
            })
    }));
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) =
                xphoneuse_mcp::start_sse_server(port, scrcpy_server_path, ct, agent_input, hooks).await
            {
                eprintln!("[mcp] server error: {}", e);
            }
        });
    });

    Ok(())
}

#[tauri::command]
pub async fn stop_mcp_server(state: State<'_, AppState>) -> Result<(), String> {
    let mut mcp = state.mcp.lock().await;
    if let Some(ct) = mcp.cancel.take() {
        ct.cancel();
    }
    Ok(())
}

/// Clear a media session the phone kept after an abrupt disconnect (USB
/// pulled). iOS holds the screen-capture session - and with it the camera -
/// until `stopMediaStream {stopAll}` arrives over a live link, so the device
/// list calls this for every iPhone it sees; it returns immediately when
/// there is nothing to clean or a live tunnel owns the device.
#[tauri::command]
pub async fn cleanup_ios_media_session(udid: String) -> Result<bool, String> {
    xphoneuse_ios::tunnel::cleanup_media_session(&udid).await
}

/// Frontend reply to an `mcp-frame-request` event: the base64 PNG of the
/// live mirror canvas, or `None` when mirroring is not running. `diag` is
/// what the webview observed while grabbing it (see `MirrorFrame::diag`).
#[tauri::command]
pub async fn mcp_frame_reply(
    id: u64,
    data: Option<String>,
    diag: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.mcp_frames.deliver(id, data, diag);
    Ok(())
}

#[tauri::command]
pub async fn get_mcp_status(state: State<'_, AppState>) -> Result<bool, String> {
    let mcp = state.mcp.lock().await;
    Ok(mcp.cancel.is_some())
}

/// Opens the project's website in the system browser (the welcome screen's
/// version link, the no-device hint and Settings -> Help & feedback all land
/// there). Taking no argument and holding no URL from the frontend keeps this
/// from becoming a generic "open anything" command.
#[tauri::command]
pub async fn open_homepage(app: AppHandle, path: Option<String>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // A path lets the app deep-link into a page ("zh/dongle" for the
    // Bluetooth dongle an older iPhone needs).
    let url = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{HOMEPAGE_URL}/{}", p.trim_start_matches('/')),
        None => HOMEPAGE_URL.to_string(),
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}
