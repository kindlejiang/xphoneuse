use xphoneuse_core::macro_engine::{self, MacroEvent, MacroRecorder};
use xphoneuse_core::{adb, accessibility, control, files, scrcpy};
use xphoneuse_core::scrcpy::StreamSettings;
use xphoneuse_ios::text_input::{self, TextCapabilities, TextInputMethod, TextRoute};
use xphoneuse_ios::{device as ios_device, screenshot as ios_screenshot, uxplay as ios_uxplay};
use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

struct Session {
    device_serial: String,
    control_socket: Arc<Mutex<TcpStream>>,
    screen_width: u32,
    screen_height: u32,
    shutdown: Arc<tokio::sync::Notify>,
    /// Whether this server started the scrcpy session itself. Sessions
    /// adopted from the host app are views on the app's session and must
    /// not stop scrcpy on disconnect - the app owns that lifecycle.
    owned: bool,
}

/// An iOS session: uxplay is advertising an AirPlay receiver that the user
/// has to start mirroring to from the iPhone. `ffi` is the primary path
/// (uxplay.dll); `process` the legacy exe fallback. When the BLE-HID bridge
/// (CDC-ACM) is attached, the session is controllable through it.
/// `tunnel` is a view on the host app's CoreDevice tunnel (adopted session)
/// — it is never stopped here, the app owns that lifecycle.
struct IosSession {
    udid: String,
    shutdown: Arc<tokio::sync::Notify>,
    ffi: Option<xphoneuse_ios::uxplay_ffi::UxplaySession>,
    process: Option<ios_uxplay::UxplayProcess>,
    hid: Option<Arc<tokio::sync::Mutex<xphoneuse_ios::hid::HidSession>>>,
    tunnel: Option<Arc<xphoneuse_ios::tunnel::TunnelHandle>>,
}

/// Stop an iOS session: signal shutdown, stop the FFI session on a blocking
/// thread (uxplay_stop joins its server thread), kill the exe child if any.
/// The tunnel (host-app-owned) is deliberately not stopped.
async fn stop_ios_session(mut s: IosSession) {
    s.shutdown.notify_one();
    if let Some(mut ffi) = s.ffi.take() {
        let _ = tokio::task::spawn_blocking(move || ffi.stop()).await;
    }
    if let Some(mut process) = s.process.take() {
        let _ = process.child.kill().await;
    }
}

#[derive(Clone)]
pub struct XPhoneUseMcp {
    scrcpy_server_path: Option<String>,
    session: Arc<Mutex<Option<Session>>>,
    ios_session: Arc<Mutex<Option<IosSession>>>,
    /// See [`Shared::host_opt_out`].
    host_opt_out: Arc<std::sync::atomic::AtomicBool>,
    macros: Arc<Mutex<HashMap<String, macro_engine::Macro>>>,
    recorder: Arc<Mutex<Option<MacroRecorder>>>,
    /// Shared with the host app: AgentInput client for non-ASCII text input
    /// on iOS (the BLE-HID bridge is printable-ASCII only).
    agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
    /// Live AirPlay frames from the host app's webview, when embedded.
    host: crate::HostHooks,
    tool_router: ToolRouter<Self>,
}

fn default_platform() -> String {
    "android".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectParams {
    #[schemars(description = "Device identifier from xphoneuse_list_devices (adb serial for Android, UDID for iOS)")]
    pub serial: String,
    #[serde(default = "default_platform")]
    #[schemars(description = "Device platform: 'android' (default) or 'ios'.")]
    pub platform: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ButtonParams {
    #[schemars(description = "Button: home, back, recents, power, volume_up, volume_down")]
    pub button: String,
}

/// Enter is pressed unless the caller says otherwise: in chat apps Enter is
/// the send button, and "send this message" is the ordinary task. Drafting is
/// the opt-in (`enter: false`).
fn enter_default() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TextParams {
    #[schemars(description = "Text to type on the device")]
    pub text: String,
    #[schemars(
        description = "Press Enter after typing (default true - in chat apps Enter sends the \
                       message, which is the ordinary task; pass false to only put the text \
                       in the field)"
    )]
    #[serde(default = "enter_default")]
    pub enter: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnicodeTextParams {
    #[schemars(description = "Text to type - any Unicode, including Chinese, emoji and newlines")]
    pub text: String,
    #[schemars(description = "How to deliver non-ASCII text on iOS. Left out, the app's own \
        setting decides. 'clipboard' replaces the device clipboard and presses Cmd+V: it needs \
        nothing installed but may show the system Allow-Paste alert, which someone has to tap. \
        'agentinput' types through the AgentInput keyboard app: no alerts and the clipboard is \
        untouched, but that keyboard has to be active on the phone - when the phone's keyboard \
        reads \"AgentInput\", the user has it installed, so use this. Ignored on Android, which \
        types any text directly.")]
    #[serde(default)]
    pub method: Option<String>,
    #[schemars(
        description = "Press Enter after typing (default true - in chat apps Enter sends the \
                       message, which is the ordinary task; pass false to only put the text \
                       in the field)"
    )]
    #[serde(default = "enter_default")]
    pub enter: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TouchParams {
    #[schemars(description = "Touch action: down, up, move")]
    pub action: String,
    #[schemars(description = "X position (0.0 to 1.0)")]
    pub x: f64,
    #[schemars(description = "Y position (0.0 to 1.0)")]
    pub y: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TapParams {
    #[schemars(description = "X position (0.0 to 1.0)")]
    pub x: f64,
    #[schemars(description = "Y position (0.0 to 1.0)")]
    pub y: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LongTapParams {
    #[schemars(description = "X position (0.0 to 1.0)")]
    pub x: f64,
    #[schemars(description = "Y position (0.0 to 1.0)")]
    pub y: f64,
    #[schemars(description = "Press duration in milliseconds (default 800, clamped to 100-10000)")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScrollParams {
    #[schemars(description = "X position (0.0 to 1.0)")]
    pub x: f64,
    #[schemars(description = "Y position (0.0 to 1.0)")]
    pub y: f64,
    #[schemars(description = "Horizontal scroll amount (-1.0 to 1.0)")]
    pub dx: f64,
    #[schemars(description = "Vertical scroll amount (-1.0 to 1.0, negative = scroll down)")]
    pub dy: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WifiEnableParams {
    #[schemars(description = "Device serial number to enable WiFi debugging on")]
    pub serial: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddressParams {
    #[schemars(description = "Device address in ip:port format (e.g. 192.168.1.100:5555)")]
    pub address: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SerialParams {
    #[schemars(description = "Device serial number")]
    pub serial: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellParams {
    #[schemars(description = "Shell command to run on the device")]
    pub command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFilesParams {
    #[schemars(
        description = "Where to look, as `platform:/path` (`android:/sdcard/DCIM`, `ios:/DCIM`) or a \
                       bare path for the connected phone. Omit to start at the phone's files \
                       (`/sdcard` on Android; on iOS the media area, `/var/mobile/Media`)."
    )]
    pub path: Option<String>,
    #[schemars(description = "Case-insensitive substring matched against entry names.")]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PullFileParams {
    #[schemars(
        description = "The file to copy off the phone: `platform:/path`, or a bare path for the \
                       connected phone (`/sdcard/DCIM/123.mp4`). A folder cannot be pulled - list \
                       it and pull the files."
    )]
    pub path: String,
    #[schemars(
        description = "Absolute local directory to save it into, on the machine running the app. The \
                       file keeps its name; a name already there becomes \"name (1).ext\" rather \
                       than overwriting it."
    )]
    pub dest: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PushFileParams {
    #[schemars(
        description = "The local file to send, as an absolute path on the machine running the app."
    )]
    pub host_path: String,
    #[schemars(
        description = "Directory on the phone to put it in (`android:/sdcard/Download`, `ios:/`), or \
                       a bare path for the connected phone. Omit for the phone's files root. A name \
                       already there becomes \"name (1).ext\". On iOS the file lands in the media \
                       area, which the phone's own apps (Photos, Files, WeChat) cannot see - \
                       pushing there is only useful for getting the file back later."
    )]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAppsParams {
    #[schemars(
        description = "Case-insensitive substring matched against the app identifier and, on iOS, its \
                       display name (\"tencent\", \"微信\"). Omit to list everything, which is long."
    )]
    pub filter: Option<String>,
    #[schemars(
        description = "\"user\" (default: the apps someone installed), \"system\" (the platform's \
                       own), or \"all\"."
    )]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwipeParams {
    #[schemars(description = "Start X position (0.0 to 1.0)")]
    pub from_x: f64,
    #[schemars(description = "Start Y position (0.0 to 1.0)")]
    pub from_y: f64,
    #[schemars(description = "End X position (0.0 to 1.0)")]
    pub to_x: f64,
    #[schemars(description = "End Y position (0.0 to 1.0)")]
    pub to_y: f64,
    #[schemars(description = "Duration in milliseconds (default 300)")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenUrlParams {
    #[schemars(description = "URL to open")]
    pub url: String,
    #[serde(default)]
    #[schemars(description = "Browser app package (default com.android.chrome). Ignored when use_system_handler is true.")]
    pub browser_package: Option<String>,
    #[serde(default)]
    #[schemars(description = "If true, use the system default URL handler (often the Google app for google.com). If false, opens in browser_package (Chrome by default).")]
    pub use_system_handler: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchAppParams {
    #[schemars(
        description = "App identifier: the package name on Android (com.android.chrome), the bundle id                        on iOS (com.tencent.xin)"
    )]
    pub package: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindOnScreenParams {
    #[schemars(description = "Search by text content (case-insensitive substring match)")]
    pub text: Option<String>,
    #[schemars(description = "Search by content description (case-insensitive substring match)")]
    pub content_desc: Option<String>,
    #[schemars(description = "Search by resource ID (case-insensitive substring match)")]
    pub resource_id: Option<String>,
    #[schemars(description = "Search by class name, e.g. 'Button', 'EditText', 'TextView'")]
    pub class_name: Option<String>,
    #[schemars(description = "Only return clickable elements")]
    pub clickable_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MacroNameParams {
    #[schemars(description = "Name of the macro")]
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MacroPlayParams {
    #[schemars(description = "Name of the macro to play")]
    pub name: String,
    #[schemars(description = "Number of times to repeat (default: 1)")]
    pub repeat: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    #[schemars(
        description = "Longest side of the returned image in pixels (default 1568, which is what vision \
                       models scale to anyway). Pass 0 to keep the capture's own resolution."
    )]
    pub max_dimension: Option<u32>,
    #[schemars(
        description = "'jpeg' (default) is small enough to look at without filling the context; 'png' is \
                       lossless - pass it when the pixels themselves matter (comparison, fine text)."
    )]
    pub format: Option<String>,
    #[schemars(description = "JPEG quality, 1-100 (default 80). Ignored for png.")]
    pub quality: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OcrParams {
    #[schemars(
        description = "Path to an image file to read (PNG or JPEG), on the machine running the app. \
                       Omit it to capture the connected device's screen instead."
    )]
    pub path: Option<String>,
    #[schemars(
        description = "'text' (default) returns the recognized lines joined by newlines; 'json' returns \
                       {width, height, lines: [{text, confidence, bbox: [x, y, w, h]}]} with the boxes in \
                       image pixels."
    )]
    pub format: Option<String>,
    #[schemars(description = "Drop lines recognized with less confidence than this (0-1, default 0.5)")]
    pub min_confidence: Option<f32>,
}

/// Connection state shared by every service instance the HTTP server
/// creates. rmcp instantiates the handler once per MCP session, so without
/// this a `connect_device` from one short-lived session (e.g. a CLI
/// invocation) would be invisible to the next.
#[derive(Default)]
pub struct Shared {
    pub session: Arc<Mutex<Option<Session>>>,
    pub ios_session: Arc<Mutex<Option<IosSession>>>,
    /// Set by `disconnect_device`: the client let go of the session on
    /// purpose, and [`Shared::follow_host`] must not hand it back two seconds
    /// later. Cleared by an explicit `connect_device`.
    pub host_opt_out: Arc<std::sync::atomic::AtomicBool>,
    pub macros: Arc<Mutex<HashMap<String, macro_engine::Macro>>>,
    pub recorder: Arc<Mutex<Option<MacroRecorder>>>,
}

impl Shared {
    /// Keep this state in step with the host app's own session.
    ///
    /// The app owns the device session and publishes it through
    /// [`crate::HostHooks`]; everything here works on a view of it. That view
    /// used to be filled in only by `connect_device`, so restarting the app -
    /// which is how you pick up a new build - left every tool answering "No
    /// device connected" while the window was plainly mirroring the phone, and
    /// the cure (a `connect` call nobody knew to make) is exactly what a
    /// person cannot guess. Following the app removes the question.
    ///
    /// Runs until `cancel` fires. A standalone server has no hooks, and this
    /// does nothing there.
    pub async fn follow_host(&self, hooks: crate::HostHooks, cancel: tokio_util::sync::CancellationToken) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {}
            }

            if self.host_opt_out.load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }

            if let Some(get) = &hooks.ios_session {
                let handle = get();
                let mut ours = self.ios_session.lock().await;
                let same = match (ours.as_ref(), handle.as_ref()) {
                    (None, None) => true,
                    (Some(ours), Some(handle)) => ours.udid == handle.udid,
                    _ => false,
                };
                match handle {
                    Some(handle) if !same => {
                        // A view, not an owner: `shutdown` is ours alone and
                        // the AirPlay receiver / tunnel stay the app's.
                        *ours = Some(IosSession {
                            udid: handle.udid,
                            shutdown: Arc::new(tokio::sync::Notify::new()),
                            ffi: None,
                            process: None,
                            hid: handle.hid,
                            tunnel: handle.tunnel,
                        });
                    }
                    None if ours.is_some() => *ours = None,
                    _ => {}
                }
            }

            if let Some(get) = &hooks.android_session {
                let handle = get();
                let mut ours = self.session.lock().await;
                let same = match (ours.as_ref(), handle.as_ref()) {
                    (None, None) => true,
                    (Some(ours), Some(handle)) => ours.device_serial == handle.serial,
                    _ => false,
                };
                match handle {
                    Some(handle) if !same => {
                        // `owned: false` - the app started scrcpy, and this
                        // must not stop it (see the field's docs).
                        *ours = Some(Session {
                            device_serial: handle.serial,
                            control_socket: handle.control_socket,
                            screen_width: handle.screen_width,
                            screen_height: handle.screen_height,
                            shutdown: Arc::new(tokio::sync::Notify::new()),
                            owned: false,
                        });
                    }
                    None if ours.is_some() => *ours = None,
                    _ => {}
                }
            }
        }
    }
}

impl XPhoneUseMcp {
    pub fn new(scrcpy_server_path: Option<String>) -> Self {
        Self::with_agent_input(scrcpy_server_path, Arc::default(), crate::HostHooks::default())
    }

    pub fn with_agent_input(
        scrcpy_server_path: Option<String>,
        agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
        host: crate::HostHooks,
    ) -> Self {
        Self::with_shared(scrcpy_server_path, agent_input, host, Arc::default())
    }

    pub fn with_shared(
        scrcpy_server_path: Option<String>,
        agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
        host: crate::HostHooks,
        shared: Arc<Shared>,
    ) -> Self {
        let tool_router = Self::tool_router();
        Self {
            scrcpy_server_path,
            session: shared.session.clone(),
            ios_session: shared.ios_session.clone(),
            host_opt_out: shared.host_opt_out.clone(),
            macros: shared.macros.clone(),
            recorder: shared.recorder.clone(),
            agent_input,
            host,
            tool_router,
        }
    }

    /// When only an iOS session is active, adb-only tools cannot run.
    async fn ios_input_error(&self) -> Option<String> {
        if self.session.lock().await.is_none() && self.ios_session.lock().await.is_some() {
            Some("Not available for iOS sessions (Android-only tool)".to_string())
        } else {
            None
        }
    }

    /// Send one input command line to the active iOS session: through the
    /// CoreDevice tunnel when its input is ready (same command grammar,
    /// parsed into HID verbs), else through the BLE-HID bridge.
    async fn ios_hid_send(&self, cmd: String) -> Result<String, String> {
        let tunnel = {
            let s = self.ios_session.lock().await;
            s.as_ref().and_then(|s| s.tunnel.clone())
        };
        if let Some(t) = tunnel {
            if t.input_ready() {
                let verb = xphoneuse_ios::tunnel::parse_bridge_command(&cmd)
                    .map_err(|e| format!("cannot route {cmd:?} over the tunnel: {e}"))?;
                return t.input(verb).await.map(|()| "OK".to_string());
            }
            // Auth gate still open: fall through to the bridge when present.
        }

        let hid = {
            let s = self.ios_session.lock().await;
            match s.as_ref() {
                Some(s) => s.hid.clone().ok_or_else(|| {
                    "Input control is not supported for this iOS session (no HID bridge detected)".to_string()
                })?,
                None => return Err("No device connected".to_string()),
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
        Ok(reply)
    }

    fn resolve_scrcpy_server(&self) -> Result<String, String> {
        if let Some(ref path) = self.scrcpy_server_path {
            return Ok(path.clone());
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("scrcpy-server-v2.7");
                if candidate.exists() {
                    return Ok(candidate.to_string_lossy().to_string());
                }
                let candidate = dir.join("resources").join("scrcpy-server-v2.7");
                if candidate.exists() {
                    return Ok(candidate.to_string_lossy().to_string());
                }
            }
        }

        Err("scrcpy-server not found. Pass --scrcpy-server <path> or set SCRCPY_SERVER_PATH".into())
    }

    /// Connect to an iOS device: verify it is visible to usbmuxd, then start
    /// a headless uxplay AirPlay receiver. The iPhone user starts mirroring
    /// from Control Center; the RTP stream is left undrained (view-only).
    async fn connect_ios(&self, udid: &str) -> String {
        {
            let mut session = self.session.lock().await;
            if let Some(s) = session.take() {
                s.shutdown.notify_one();
                scrcpy::stop_server(&s.device_serial, 27183).await;
            }
        }
        if let Some(s) = self.ios_session.lock().await.take() {
            stop_ios_session(s).await;
        }

        if let Err(e) = ios_device::provider_for(udid).await {
            return format!("iOS device not available: {}", e);
        }

        let shutdown = Arc::new(tokio::sync::Notify::new());

        // Embedded in the host app: delegate the connection to it. The app
        // runs the AirPlay receiver, renders the video, switches its UI to
        // the mirror screen, and shares its HID session - so the CLI and the
        // app operate the same phone. Adopt an existing session when the app
        // is already mirroring this device.
        if self.host.ios_session.is_some() {
            let get = self.host.ios_session.clone().unwrap();
            let already_connected =
                get().map(|h| h.udid == udid).unwrap_or(false);
            if !already_connected {
                if let Some(request) = self.host.ios_connect.clone() {
                    request(udid);
                }
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                if let Some(handle) = get() {
                    if handle.udid == udid {
                        let hid_attached = handle.hid.is_some();
                        let tunnel_attached = handle.tunnel.is_some();
                        *self.ios_session.lock().await = Some(IosSession {
                            udid: udid.to_string(),
                            shutdown,
                            ffi: None,
                            process: None,
                            hid: handle.hid,
                            tunnel: handle.tunnel,
                        });
                        let control = if tunnel_attached {
                            "controllable via the USB tunnel"
                        } else if hid_attached {
                            "controllable via the BLE-HID bridge"
                        } else {
                            "view-only - no input transport detected"
                        };
                        return format!(
                            "Connected to iOS device {} through the host app (its window is \
                             mirroring; {}). Ask the user to start AirPlay mirroring from the \
                             iPhone Control Center if the window shows no video.",
                            udid, control
                        );
                    }
                }
                if std::time::Instant::now() > deadline {
                    return format!(
                        "The host app did not connect to iOS device {} in time; \
                         is the XPhoneUse window open?",
                        udid
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }

        // Detect the BLE-HID bridge (CDC-ACM) so the session is controllable.
        let hid = if let Ok(name) = std::env::var("UART_HID_PORT") {
            xphoneuse_ios::hid::HidSession::connect(&name).ok()
        } else {
            tokio::task::spawn_blocking(xphoneuse_ios::hid::detect_first)
                .await
                .ok()
                .flatten()
        };
        let hid = hid.map(|s| Arc::new(tokio::sync::Mutex::new(s)));
        let hid_attached = hid.is_some();

        // Primary path: uxplay.dll FFI session (NAL events drained in the
        // background; MCP never consumes video).
        if xphoneuse_ios::uxplay_ffi::available() {
            // uxplay's server state is process-global: when the host app is
            // already mirroring, uxplay_start returns -2 ("already running").
            // Reuse that session instead of failing - screenshots come from
            // the live mirror frames either way.
            let mut session =
                match xphoneuse_ios::uxplay_ffi::UxplaySession::start("XPhoneUse", 60) {
                    Ok(s) => s,
                    Err(e) if e.to_string().contains("code -2") => {
                        *self.ios_session.lock().await = Some(IosSession {
                            udid: udid.to_string(),
                            shutdown,
                            ffi: None,
                            process: None,
                            hid: hid.clone(),
                            tunnel: None,
                        });
                        return if hid_attached {
                            format!(
                                "Connected to iOS device {} (reusing the AirPlay receiver already \
                                 running in the host app; controllable via the BLE-HID bridge).",
                                udid
                            )
                        } else {
                            format!(
                                "Connected to iOS device {} (reusing the AirPlay receiver already \
                                 running in the host app; view-only).",
                                udid
                            )
                        };
                    }
                    Err(e) => return format!("Failed to start uxplay: {}", e),
                };
            if let Some(receiver) = session.take_receiver() {
                let agent_input = self.agent_input.clone();
                std::thread::spawn(move || {
                    while let Ok(event) = receiver.recv() {
                        // Auto-configure AgentInput from the mirroring phone's
                        // IP when the user has not set one up manually.
                        if let xphoneuse_ios::uxplay_ffi::UxplayEvent::Connected {
                            ref client_ip, ..
                        } = event
                        {
                            let host = client_ip
                                .trim_start_matches('[')
                                .trim_start_matches("::ffff:")
                                .split('%')
                                .next()
                                .unwrap_or("")
                                .trim_end_matches(']')
                                .to_string();
                            if host.contains('.') {
                                let mut rb = agent_input.lock().unwrap();
                                if rb.is_none() {
                                    *rb = Some(xphoneuse_ios::agent_input::AgentInputClient::new(
                                        host,
                                        xphoneuse_ios::agent_input::DEFAULT_PORT,
                                        None,
                                    ));
                                }
                            }
                        }
                    }
                });
            }
            *self.ios_session.lock().await = Some(IosSession {
                udid: udid.to_string(),
                shutdown,
                ffi: Some(session),
                process: None,
                hid: hid.clone(),
                tunnel: None,
            });
        } else {
            // Fallback: legacy uxplay.exe subprocess (old upstream builds)
            let socket = match tokio::net::UdpSocket::bind("127.0.0.1:0").await {
                Ok(s) => s,
                Err(e) => return format!("Failed to bind RTP socket: {}", e),
            };
            let port = match socket.local_addr() {
                Ok(addr) => addr.port(),
                Err(e) => return format!("Failed to get RTP port: {}", e),
            };

            let process = match ios_uxplay::UxplayProcess::spawn(port, "XPhoneUse", 60) {
                Ok(p) => p,
                Err(e) => return format!("Failed to start uxplay: {}", e),
            };
            *self.ios_session.lock().await = Some(IosSession {
                udid: udid.to_string(),
                shutdown,
                ffi: None,
                process: Some(process),
                hid: hid.clone(),
                tunnel: None,
            });
        }

        if hid_attached {
            format!(
                "Connected to iOS device {} (controllable via the BLE-HID bridge). Ask the user \
                 to start AirPlay mirroring from the iPhone Control Center to \"XPhoneUse\". \
                 Make sure the iPhone is paired with the bridge over Bluetooth.",
                udid
            )
        } else {
            format!(
                "Connected to iOS device {} (view-only). Ask the user to start AirPlay mirroring \
                 from the iPhone Control Center to \"XPhoneUse\". xphoneuse_take_screenshot works; input tools \
                 need the BLE-HID bridge (not detected).",
                udid
            )
        }
    }

    async fn maybe_record(&self, event: MacroEvent) {
        if let Some(ref mut r) = *self.recorder.lock().await {
            r.record(event);
        }
    }

    async fn get_ui_elements(
        &self,
    ) -> Result<(Vec<accessibility::UiElement>, u32, u32), String> {
        let (serial, sw, sh) = {
            let session = self.session.lock().await;
            let s = session.as_ref().ok_or("No device connected")?;
            (s.device_serial.clone(), s.screen_width, s.screen_height)
        };

        let xml = adb::dump_ui_hierarchy(&serial)
            .await
            .map_err(|e| format!("Error: {}", e))?;

        let elements = accessibility::parse_ui_hierarchy(&xml, sw, sh)
            .map_err(|e| format!("Error parsing UI hierarchy: {}", e))?;

        Ok((elements, sw, sh))
    }

    /// The connected device's screen as PNG bytes, by whichever route works:
    /// Android's `screencap`, a live AirPlay frame from the host app, the
    /// CoreDevice tunnel, then DVT/classic. Shared by the screenshot and OCR
    /// tools so the two cannot drift apart.
    async fn capture_screen_png(&self) -> Result<Vec<u8>, rmcp::ErrorData> {
        Ok(self.capture_screen_png_with_source().await?.0)
    }

    /// The same capture, plus which of the three paths produced it.
    ///
    /// The source is worth reporting rather than hiding: the three are not
    /// equivalent (the mirror canvas is the decoded stream, the tunnel and DVT
    /// are device-side captures), and a screenshot that turns out to be stale
    /// is only diagnosable if the caller knows where it came from.
    async fn capture_screen_png_with_source(&self) -> Result<(Vec<u8>, String), rmcp::ErrorData> {
        // Android session first
        if let Some(session) = self.session.lock().await.as_ref() {
            return adb::exec_out_screencap(&session.device_serial)
                .await
                .map(|png| (png, "adb screencap".to_string()))
                .map_err(|e| rmcp::ErrorData::internal_error(format!("Screenshot failed: {}", e), None));
        }

        // iOS session (udid cloned out so the session lock is not held
        // across the awaits below). The tunnel's own frame age rides along:
        // comparing it with the canvas's paint age says whether a stale
        // picture is a dead RTP stream or a webview that stopped drawing.
        let (ios_udid, tunnel_age) = {
            let guard = self.ios_session.lock().await;
            match guard.as_ref() {
                Some(s) => (
                    Some(s.udid.clone()),
                    s.tunnel.as_ref().and_then(|t| t.frame_age()),
                ),
                None => (None, None),
            }
        };
        if let Some(udid) = ios_udid {
            // Prefer a frame from the host app's webview: it is the cheapest
            // path by far, and while the stream is healthy it is also the
            // most current - it is what the user is looking at.
            //
            // It is a decoded video frame, not a capture, and that matters
            // when a screenshot turns out to be of a screen the phone has
            // left: iOS sends no RTP while the screen is still, so the canvas
            // is not repainted for as long as nobody touches the phone - which
            // is normal, and the content is still right. What it cannot
            // survive is the stream dying, and the two look identical from
            // here. So the source is reported to the caller (see
            // `capture_screen_png_with_source`) and nothing is guessed; a
            // reproduction of a wrong image is what would settle it.
            if let Some(frame) = self.host.frames.get(std::time::Duration::from_secs(3)).await {
                let png = base64::engine::general_purpose::STANDARD
                    .decode(&frame.data)
                    .map_err(|e| {
                        rmcp::ErrorData::internal_error(
                            format!("Screenshot failed: malformed frame: {}", e),
                            None,
                        )
                    })?;
                let tunnel_note = match tunnel_age {
                    Some(age) => format!("tunnel last frame {}ms ago", age.as_millis()),
                    None => "tunnel has produced no frames".to_string(),
                };
                let source = if frame.diag.is_empty() {
                    format!("the app's mirror canvas ({})", tunnel_note)
                } else {
                    format!("the app's mirror canvas ({}, {})", frame.diag, tunnel_note)
                };
                return Ok((png, source));
            }
            // Then the CoreDevice tunnel one-shot capture. The service only
            // accepts "png" (probed on-device: jpeg/jpg/heic/tiff are all
            // rejected as invalid ScreenshotImageFormat), but at ~750ms it
            // still beats the DVT path by seconds. A timeout only abandons
            // the request; the tunnel actor keeps serving.
            let tunnel_attempt = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                xphoneuse_ios::tunnel::tunnel_screenshot(&udid, xphoneuse_ios::tunnel::ImageFormat::Png),
            )
            .await;
            if let Ok(Some(Ok(png))) = tunnel_attempt {
                return Ok((png, "the app's USB tunnel".to_string()));
            }
            // DVT (or classic on old iOS) as the last resort. It prepares a
            // missing developer image, but nothing here can turn Developer
            // Mode on - that restarts the phone, and an agent's screenshot is
            // not a good enough reason to reboot somebody's phone.
            return ios_screenshot::take_screenshot(&udid)
                .await
                .map(|png| (png, "a direct DVT capture".to_string()))
                .map_err(|e| rmcp::ErrorData::internal_error(format!("iOS screenshot failed: {}", e), None));
        }

        Err(rmcp::ErrorData::internal_error("No device connected", None))
    }

    /// Which phone a file request is about, and where in it.
    ///
    /// The platform comes from the location when it names one (`ios:/DCIM`
    /// while an Android phone is connected is a mistake worth reporting, not
    /// something to quietly reinterpret), and from the connected phone
    /// otherwise - a bare `/sdcard/DCIM` means "the phone in front of me".
    async fn resolve_location(&self, input: Option<&str>) -> Result<(FsTarget, String), String> {
        let named = input.and_then(files::platform_of);
        let target = self.fs_target(named).await?;
        let path = match input {
            Some(raw) => files::parse_location(raw, target.platform())?.1,
            None => files::default_path(target.platform()).to_string(),
        };
        Ok((target, path))
    }

    /// The connected device a file operation would act on. Android first, as
    /// everywhere else: a session on the phone in front of the user wins over
    /// a view-only iOS one.
    async fn fs_target(&self, wanted: Option<&str>) -> Result<FsTarget, String> {
        let android = self
            .session
            .lock()
            .await
            .as_ref()
            .map(|session| session.device_serial.clone());
        let ios = self
            .ios_session
            .lock()
            .await
            .as_ref()
            .map(|session| session.udid.clone());
        match wanted {
            Some(files::PLATFORM_ANDROID) => android.map(FsTarget::Android).ok_or_else(|| {
                "that location is for an Android phone, and none is connected".to_string()
            }),
            Some(files::PLATFORM_IOS) => ios.map(FsTarget::Ios).ok_or_else(|| {
                "that location is for an iPhone, and none is connected".to_string()
            }),
            _ => match (android, ios) {
                (Some(serial), _) => Ok(FsTarget::Android(serial)),
                (None, Some(udid)) => Ok(FsTarget::Ios(udid)),
                (None, None) => Err("No device connected".to_string()),
            },
        }
    }
}

/// The phone a file operation acts on. The two are not interchangeable: an
/// Android phone's `/sdcard` and an iPhone's media area have nothing to do
/// with each other beyond both being "the phone's files".
enum FsTarget {
    Android(String),
    Ios(String),
}

impl FsTarget {
    fn platform(&self) -> &'static str {
        match self {
            FsTarget::Android(_) => files::PLATFORM_ANDROID,
            FsTarget::Ios(_) => files::PLATFORM_IOS,
        }
    }

    fn device(&self) -> &str {
        match self {
            FsTarget::Android(serial) => serial,
            FsTarget::Ios(udid) => udid,
        }
    }
}

#[tool_router]
impl XPhoneUseMcp {
    #[tool(description = "List connected devices (Android via adb, iOS via usbmuxd). Each line is prefixed with the platform to pass to xphoneuse_connect_device.")]
    async fn xphoneuse_list_devices(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        match adb::list_devices().await {
            Ok(devices) => lines.extend(
                devices
                    .iter()
                    .map(|d| format!("android:{} - {} ({})", d.serial, d.model, d.state)),
            ),
            Err(e) => lines.push(format!("Android error: {}", e)),
        }

        match ios_device::list_devices().await {
            Ok(devices) => lines.extend(devices.iter().map(|d| {
                format!(
                    "ios:{} - {} (iOS {}, paired: {}, available: {})",
                    d.udid, d.name, d.ios_version, d.paired, d.available
                )
            })),
            Err(e) => lines.push(format!("iOS error: {}", e)),
        }

        if lines.is_empty() {
            "No devices connected".to_string()
        } else {
            lines.join("\n")
        }
    }

    #[tool(description = "Connect to a device. Android (default) starts scrcpy for full control; iOS starts a view-only AirPlay receiver (screenshots work, input tools do not).")]
    async fn xphoneuse_connect_device(&self, params: Parameters<ConnectParams>) -> String {
        let params = params.0;
        self.host_opt_out.store(false, std::sync::atomic::Ordering::Relaxed);

        if params.platform == "ios" {
            return self.connect_ios(&params.serial).await;
        }

        let server_path = match self.resolve_scrcpy_server() {
            Ok(p) => p,
            Err(e) => return e,
        };

        {
            let mut session = self.session.lock().await;
            if let Some(s) = session.take() {
                s.shutdown.notify_one();
                if s.owned {
                    scrcpy::stop_server(&s.device_serial, 27183).await;
                }
            }
        }
        if let Some(s) = self.ios_session.lock().await.take() {
            stop_ios_session(s).await;
        }

        // Embedded in the host app: delegate the connection to it. The app
        // starts scrcpy, switches its UI to the mirror screen, streams the
        // video, and shares its control socket - so the CLI and the app
        // operate the same device. Adopt an existing session when the app is
        // already mirroring this device.
        if self.host.android_session.is_some() {
            let get = self.host.android_session.clone().unwrap();
            let shutdown = Arc::new(tokio::sync::Notify::new());
            let already_connected = get().map(|h| h.serial == params.serial).unwrap_or(false);
            if !already_connected {
                if let Some(request) = self.host.android_connect.clone() {
                    request(&params.serial);
                }
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                if let Some(handle) = get() {
                    if handle.serial == params.serial {
                        *self.session.lock().await = Some(Session {
                            device_serial: handle.serial,
                            control_socket: handle.control_socket,
                            screen_width: handle.screen_width,
                            screen_height: handle.screen_height,
                            shutdown,
                            owned: false,
                        });
                        return format!(
                            "Connected to {} ({}x{}) through the host app (its window is \
                             mirroring the device)",
                            params.serial, handle.screen_width, handle.screen_height
                        );
                    }
                }
                if std::time::Instant::now() > deadline {
                    return format!(
                        "The host app did not connect to Android device {} in time; \
                         is the XPhoneUse window open?",
                        params.serial
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }

        let settings = StreamSettings {
            max_size: 0,
            max_fps: 1,
            video_bit_rate: 500000,
            video_codec: "h264".to_string(),
            audio: false,
        };

        let port: u16 = 27183;
        let (streams, mut server_process) =
            match scrcpy::start_server(&params.serial, &server_path, port, &settings).await {
                Ok(s) => s,
                Err(e) => return format!("Failed to start scrcpy: {}", e),
            };

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let control_socket = Arc::new(Mutex::new(streams.control_socket));

        let drain_shutdown = shutdown.clone();
        let mut video_socket = streams.video_socket;
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                tokio::select! {
                    result = video_socket.read(&mut buf) => {
                        match result {
                            Ok(0) | Err(_) => break,
                            _ => continue,
                        }
                    }
                    _ = drain_shutdown.notified() => break,
                }
            }
        });

        let cleanup_shutdown = shutdown.clone();
        let cleanup_serial = params.serial.clone();
        tokio::spawn(async move {
            cleanup_shutdown.notified().await;
            scrcpy::stop_server(&cleanup_serial, port).await;
            let _ = server_process.kill().await;
        });

        let text = format!(
            "Connected to {} ({}x{})",
            params.serial, streams.screen_width, streams.screen_height
        );

        let session = Session {
            device_serial: params.serial,
            control_socket,
            screen_width: streams.screen_width,
            screen_height: streams.screen_height,
            shutdown,
            owned: true,
        };

        *self.session.lock().await = Some(session);
        text
    }

    #[tool(description = "Disconnect from the current device (Android or iOS)")]
    async fn xphoneuse_disconnect_device(&self) -> String {
        // Before anything is taken apart: this was asked for, and the host
        // follower must not quietly hand the session back afterwards.
        self.host_opt_out.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut session = self.session.lock().await;
        if let Some(s) = session.take() {
            s.shutdown.notify_one();
            let owned = s.owned;
            let serial = s.device_serial.clone();
            if owned {
                scrcpy::stop_server(&serial, 27183).await;
            }
            let note = if owned { "" } else { " (host app keeps mirroring)" };
            return format!("Disconnected from {}{}", serial, note);
        }
        if let Some(s) = self.ios_session.lock().await.take() {
            let udid = s.udid.clone();
            stop_ios_session(s).await;
            return format!("Disconnected from iOS device {}", udid);
        }
        "No device connected".to_string()
    }

    #[tool(
        description = "Take a screenshot of the connected device (Android or iOS). The capture is a \
                       full-resolution PNG (e.g. 1179x2556), which is far more than a model needs to look \
                       at, so by default it comes back scaled to 1568px on its longest side and encoded as \
                       JPEG. Pass format=\"png\" for a lossless image, max_dimension=0 for the capture's \
                       own resolution, or quality to change the JPEG's."
    )]
    async fn xphoneuse_take_screenshot(
        &self,
        params: Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let (capture, source) = self.capture_screen_png_with_source().await?;
        let params = params.0;
        let (bytes, mime) = screenshot_for_model(
            &capture,
            params.max_dimension.unwrap_or(DEFAULT_MAX_DIMENSION),
            params.format.as_deref().unwrap_or("jpeg"),
            params.quality.unwrap_or(DEFAULT_JPEG_QUALITY),
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("Screenshot failed: {}", e), None))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        // The capture's own size, not the scaled one that is about to be sent:
        // it is what says whether the image is the whole screen.
        let size = image::load_from_memory(&capture)
            .map(|image| format!("{}x{}", image.width(), image.height()))
            .unwrap_or_else(|_| "an unknown size".to_string());
        Ok(CallToolResult::success(vec![
            rmcp::model::Content::text(format!("Captured {size} from {source}.")),
            rmcp::model::Content::image(b64, mime),
        ]))
    }

    #[tool(
        description = "Read text off the device screen with offline OCR (PaddleOCR, no network). Pass a local \
                       image path, or omit it to capture the connected device's screen first. Returns the \
                       recognized lines as plain text; format=\"json\" returns \
                       {width, height, lines:[{text, confidence, bbox:[x,y,w,h]}]} instead, with the boxes in \
                       image pixels (divide by width/height for the normalized coordinates xphoneuse_tap takes)."
    )]
    async fn xphoneuse_ocr(&self, params: Parameters<OcrParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let params = params.0;
        let path = params.path.as_deref().map(str::trim).filter(|path| !path.is_empty());
        let (png, source) = match path {
            Some(path) => (
                std::fs::read(path).map_err(|e| {
                    rmcp::ErrorData::internal_error(format!("OCR failed: could not read {}: {}", path, e), None)
                })?,
                path.to_string(),
            ),
            None => (self.capture_screen_png().await?, "the device screen".to_string()),
        };

        let image = xphoneuse_ocr::recognize_png(png)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(format!("OCR failed: {}", e), None))?;

        // The engine is built with its own threshold off, so this is the only
        // place lines are dropped.
        let min_confidence = params.min_confidence.unwrap_or(0.5);
        let lines: Vec<xphoneuse_ocr::OcrLine> = image
            .lines
            .into_iter()
            .filter(|line| line.confidence >= min_confidence)
            .collect();

        let text = if params.format.as_deref() == Some("json") {
            serde_json::json!({
                "width": image.width,
                "height": image.height,
                "lines": lines,
            })
            .to_string()
        } else if lines.is_empty() {
            format!("no text found in {}", source)
        } else {
            lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n")
        };
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(text)]))
    }

    #[tool(description = "Press a device button (home, back, recents, power, volume_up, volume_down)")]
    async fn xphoneuse_press_button(&self, params: Parameters<ButtonParams>) -> String {
        let params = params.0;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let keycode = match params.button.as_str() {
                    "home" => control::KEYCODE_HOME,
                    "back" => control::KEYCODE_BACK,
                    "recents" => control::KEYCODE_APP_SWITCH,
                    "power" => control::KEYCODE_POWER,
                    "volume_up" => control::KEYCODE_VOLUME_UP,
                    "volume_down" => control::KEYCODE_VOLUME_DOWN,
                    _ => return format!("Unknown button: {}", params.button),
                };
                if let Err(e) = control::inject_keycode(&session.control_socket, "down", keycode, 0, 0).await {
                    return format!("Error: {}", e);
                }
                if let Err(e) = control::inject_keycode(&session.control_socket, "up", keycode, 0, 0).await {
                    return format!("Error: {}", e);
                }
                self.maybe_record(MacroEvent::Button {
                    button: params.button.clone(),
                })
                .await;
                return format!("Pressed {}", params.button);
            }
        }
        // iOS: consumer key through the BLE-HID bridge. The app switcher has
        // no consumer key on iOS, so "recents" is synthesized as the system
        // gesture (slow drag up from the bottom edge, pause, release).
        if self.ios_session.lock().await.is_some() {
            if params.button == "recents" {
                for (cmd, delay) in xphoneuse_ios::hid::app_switcher_gesture() {
                    if let Err(e) = self.ios_hid_send(cmd).await {
                        return e;
                    }
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                self.maybe_record(MacroEvent::Button {
                    button: params.button.clone(),
                })
                .await;
                return format!("Pressed {}", params.button);
            }
            let cmd = match xphoneuse_ios::hid::map_button(&params.button) {
                Ok(c) => c,
                Err(e) => return e.to_string(),
            };
            return match self.ios_hid_send(cmd).await {
                Ok(_) => {
                    self.maybe_record(MacroEvent::Button {
                        button: params.button.clone(),
                    })
                    .await;
                    format!("Pressed {}", params.button)
                }
                Err(e) => e,
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Type text on the connected device")]
    async fn xphoneuse_send_text(&self, params: Parameters<TextParams>) -> String {
        let params = params.0;
        if let Some(result) = self.android_send_text(&params.text, params.enter).await {
            return match result {
                Ok(()) => {
                    self.maybe_record(MacroEvent::Text {
                        text: params.text.clone(),
                    })
                    .await;
                    format!("Typed: {}", params.text)
                }
                Err(e) => format!("Error: {}", e),
            };
        }
        // iOS: the route depends on what the session can do and on how the
        // app is configured to deliver non-ASCII text (see
        // xphoneuse_ios::text_input). `text` follows the app setting.
        if self.ios_session.lock().await.is_some() {
            let method = self.text_input_default();

            return match self.deliver_ios_text(&params.text, params.enter, method).await {
                Ok((_route, note)) => {
                    self.maybe_record(MacroEvent::Text {
                        text: params.text.clone(),
                    })
                    .await;
                    match note {
                        Some(n) => format!("Typed: {} ({n})", params.text),
                        None => format!("Typed: {}", params.text),
                    }
                }
                Err(e) => format!("Error: {}", e),
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Type text on iOS with an explicit delivery method. 'clipboard' (default) \
        puts the text on the device pasteboard and presses Cmd+V; 'agentinput' types it through \
        the AgentInput keyboard app, which must be installed and active. Only non-ASCII text \
        needs a method - ASCII is typed either way. Reports the route actually used, including \
        when the requested one was unavailable and a fallback ran. Use xphoneuse_send_text when \
        the app's configured default is fine, and for Android.")]
    async fn xphoneuse_unicode_text(&self, params: Parameters<UnicodeTextParams>) -> String {
        let params = params.0;

        // Android types Unicode directly through scrcpy; no method involved.
        if let Some(result) = self.android_send_text(&params.text, params.enter).await {
            return match result {
                Ok(()) => {
                    self.maybe_record(MacroEvent::Text {
                        text: params.text.clone(),
                    })
                    .await;
                    format!("Typed: {}", params.text)
                }
                Err(e) => format!("Error: {}", e),
            };
        }

        let method = match params.method.as_deref() {
            Some(m) => match TextInputMethod::parse(m) {
                Ok(m) => m,
                Err(e) => return format!("Error: {e}"),
            },
            None => self.text_input_default(),
        };

        if self.ios_session.lock().await.is_none() {
            return "No device connected".to_string();
        }

        match self.deliver_ios_text(&params.text, params.enter, method).await {
            Ok((route, note)) => {
                self.maybe_record(MacroEvent::Text {
                    text: params.text.clone(),
                })
                .await;
                let mut out = format!(
                    "Typed: {} (method: {}, via: {})",
                    params.text,
                    method.as_str(),
                    route.as_str()
                );
                if let Some(n) = note {
                    out.push_str(&format!(" ({n})"));
                }
                out
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Send text through AgentInput (blocking; the client re-dials itself).
    async fn agent_input_send(&self, text: String, enter: bool) -> Result<(), String> {
        let agent_input = self.agent_input.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = agent_input.lock().unwrap();
            let client = guard
                .as_mut()
                .ok_or_else(|| "AgentInput is not configured".to_string())?;
            if !text.is_empty() {
                client.send_text(&text)?;
            }
            if enter {
                // WeChat and similar apps need a moment to commit the text
                // insertion; an immediate "\n" is treated as a literal
                // newline instead of the send key.
                std::thread::sleep(std::time::Duration::from_millis(400));
                client.enter()?;
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("AgentInput task failed: {e}"))?
    }

    /// Type on Android. scrcpy's inject_text carries any Unicode, so the iOS
    /// delivery method does not apply. `None` means there is no Android
    /// session and the caller should look at iOS instead.
    async fn android_send_text(&self, text: &str, enter: bool) -> Option<Result<(), String>> {
        let session = self.session.lock().await;
        let session = session.as_ref()?;
        if !text.is_empty() {
            if let Err(e) = control::send_text(&session.control_socket, text).await {
                return Some(Err(e.to_string()));
            }
        }
        if enter {
            // scrcpy's inject_text cannot carry a newline - press the key.
            if let Err(e) = control::inject_keycode(&session.control_socket, "press", 66, 0, 0).await
            {
                return Some(Err(e.to_string()));
            }
        }
        Some(Ok(()))
    }

    /// What the current iOS session can do with text.
    async fn ios_text_capabilities(&self) -> TextCapabilities {
        let s = self.ios_session.lock().await;
        let session = s.as_ref();
        TextCapabilities {
            tunnel: session
                .and_then(|s| s.tunnel.as_ref())
                .is_some_and(|t| t.input_ready()),
            // The bridge specifically - not "any HID input", which would also
            // be true for a tunnel-only session.
            bridge: session.is_some_and(|s| s.hid.is_some()),
            agent_input: self.agent_input.lock().unwrap().is_some(),
        }
    }

    /// The method the app is configured to use (settings, default clipboard).
    fn text_input_default(&self) -> TextInputMethod {
        self.host
            .text_input_method
            .as_ref()
            .map(|f| f())
            .unwrap_or_default()
    }

    /// Carry out one planned route.
    async fn exec_text_route(&self, route: TextRoute, text: &str, enter: bool) -> Result<(), String> {
        match route {
            // One verb covers both: the tunnel types what the keyboard can
            // carry and pastes the rest.
            TextRoute::TunnelKeys | TextRoute::TunnelPaste => {
                if !text.is_empty() {
                    let tunnel = {
                        let s = self.ios_session.lock().await;
                        s.as_ref().and_then(|s| s.tunnel.clone())
                    }
                    .ok_or_else(|| "No iOS tunnel is attached".to_string())?;
                    let verb = xphoneuse_ios::tunnel::HidVerb::Text { text: text.to_string() };
                    tunnel.input(verb).await?;
                }
                if enter {
                    // K ENTER = the Return key (usage 0x28).
                    self.ios_hid_send("K ENTER".to_string()).await?;
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
                    return Err("Nothing printable to send (TEXT supports printable ASCII only)".to_string());
                }
                for cmd in cmds {
                    self.ios_hid_send(cmd).await?;
                }
                Ok(())
            }
            TextRoute::AgentInput => self.agent_input_send(text.to_string(), enter).await,
        }
    }

    /// Type `text` on the connected iPhone, returning the route that carried
    /// it and a note when that is not the route the caller asked for.
    async fn deliver_ios_text(
        &self,
        text: &str,
        enter: bool,
        method: TextInputMethod,
    ) -> Result<(TextRoute, Option<String>), String> {
        let caps = self.ios_text_capabilities().await;
        let plan = text_input::plan_text_delivery(text, method, caps)?;

        if plan.route != TextRoute::AgentInput {
            self.exec_text_route(plan.route, text, enter).await?;
            return Ok((plan.route, plan.note.map(str::to_string)));
        }

        // AgentInput dials lazily, so "configured" only becomes "reachable"
        // here. A failed dial means the keyboard is not installed or not the
        // active one - and nothing has been typed yet, which is what makes
        // falling back safe. (A failure *after* this point is reported as-is:
        // retrying elsewhere could type the message twice.)
        if let Err(e) = text_input::ensure_agent_input(&self.agent_input).await {
            let fallback_caps = TextCapabilities { agent_input: false, ..caps };
            let fallback = text_input::plan_text_delivery(text, method, fallback_caps)?;
            self.exec_text_route(fallback.route, text, enter).await?;
            let note = match fallback.note {
                Some(n) => format!("AgentInput unreachable ({e}) - {n}"),
                None => format!("AgentInput unreachable ({e}) - used {} instead", fallback.route.as_str()),
            };
            return Ok((fallback.route, Some(note)));
        }

        self.exec_text_route(TextRoute::AgentInput, text, enter).await?;
        Ok((TextRoute::AgentInput, plan.note.map(str::to_string)))
    }

    #[tool(description = "Tap at normalized coordinates (0.0-1.0) - one down+up in a single step. Use this for ordinary taps; use send_touch only for drags and long-presses.")]
    async fn xphoneuse_tap(&self, params: Parameters<TapParams>) -> String {
        let params = params.0;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let px = (params.x * session.screen_width as f64) as u32;
                let py = (params.y * session.screen_height as f64) as u32;
                if let Err(e) = control::tap(
                    &session.control_socket,
                    px,
                    py,
                    session.screen_width as u16,
                    session.screen_height as u16,
                )
                .await
                {
                    return format!("Error: {}", e);
                }
                // Record as a down/up pair so replays (and existing macros)
                // stay compatible with the Touch event.
                self.maybe_record(MacroEvent::Touch {
                    action: "down".to_string(),
                    x: params.x,
                    y: params.y,
                })
                .await;
                self.maybe_record(MacroEvent::Touch {
                    action: "up".to_string(),
                    x: params.x,
                    y: params.y,
                })
                .await;
                return format!("Tapped at ({:.2}, {:.2})", params.x, params.y);
            }
        }
        // iOS: the firmware's TAP command - locate + light touch in one step.
        if self.ios_session.lock().await.is_some() {
            let cmd = xphoneuse_ios::hid::map_tap(params.x, params.y);
            return match self.ios_hid_send(cmd).await {
                Ok(_) => {
                    // Record as a down/up pair, like the Android branch: a tap
                    // replayed as one Touch event would not be one.
                    self.maybe_record(MacroEvent::Touch {
                        action: "down".to_string(),
                        x: params.x,
                        y: params.y,
                    })
                    .await;
                    self.maybe_record(MacroEvent::Touch {
                        action: "up".to_string(),
                        x: params.x,
                        y: params.y,
                    })
                    .await;
                    format!("Tapped at ({:.2}, {:.2})", params.x, params.y)
                }
                Err(e) => e,
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Long-press at normalized coordinates (0.0-1.0): touch down, hold, then up. duration_ms sets the hold time (default 800, clamped to 100-10000).")]
    async fn xphoneuse_longtap(&self, params: Parameters<LongTapParams>) -> String {
        let p = params.0;
        let ms = p.duration_ms.unwrap_or(800).clamp(100, 10000);

        // Android: inject_touch down + hold + up. The session lock is taken
        // per step (not across the hold) so other commands keep flowing.
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let px = (p.x * session.screen_width as f64) as u32;
                let py = (p.y * session.screen_height as f64) as u32;
                let r = control::inject_touch(
                    &session.control_socket,
                    "down",
                    px,
                    py,
                    session.screen_width as u16,
                    session.screen_height as u16,
                )
                .await;
                if let Err(e) = r {
                    return format!("Error: {}", e);
                }
            } else if self.ios_session.lock().await.is_some() {
                let down = match xphoneuse_ios::hid::map_touch("down", p.x, p.y) {
                    Ok(c) => c,
                    Err(e) => return e.to_string(),
                };
                if let Err(e) = self.ios_hid_send(down).await {
                    return e;
                }
            } else {
                return "No device connected".to_string();
            }
        }
        self.maybe_record(MacroEvent::Touch {
            action: "down".to_string(),
            x: p.x,
            y: p.y,
        })
        .await;
        self.maybe_record(MacroEvent::Wait { ms }).await;
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let px = (p.x * session.screen_width as f64) as u32;
                let py = (p.y * session.screen_height as f64) as u32;
                let r = control::inject_touch(
                    &session.control_socket,
                    "up",
                    px,
                    py,
                    session.screen_width as u16,
                    session.screen_height as u16,
                )
                .await;
                if let Err(e) = r {
                    return format!("Error: {}", e);
                }
            } else if self.ios_session.lock().await.is_some() {
                let up = match xphoneuse_ios::hid::map_touch("up", p.x, p.y) {
                    Ok(c) => c,
                    Err(e) => return e.to_string(),
                };
                if let Err(e) = self.ios_hid_send(up).await {
                    return e;
                }
            } else {
                return "Error: session ended during the hold".to_string();
            }
        }
        self.maybe_record(MacroEvent::Touch {
            action: "up".to_string(),
            x: p.x,
            y: p.y,
        })
        .await;
        format!("Long-pressed at ({:.2}, {:.2}) for {}ms", p.x, p.y, ms)
    }

    #[tool(description = "Send a touch event (down/up/move) at normalized coordinates (0.0-1.0)")]
    async fn xphoneuse_send_touch(&self, params: Parameters<TouchParams>) -> String {
        let params = params.0;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let px = (params.x * session.screen_width as f64) as u32;
                let py = (params.y * session.screen_height as f64) as u32;
                if let Err(e) = control::inject_touch(
                    &session.control_socket,
                    &params.action,
                    px, py,
                    session.screen_width as u16,
                    session.screen_height as u16,
                ).await {
                    return format!("Error: {}", e);
                }
                self.maybe_record(MacroEvent::Touch {
                    action: params.action.clone(),
                    x: params.x,
                    y: params.y,
                })
                .await;
                return format!("Touch {} at ({:.2}, {:.2})", params.action, params.x, params.y);
            }
        }
        // iOS: MDOWN/M/MUP through the bridge (1920 logical plane)
        if self.ios_session.lock().await.is_some() {
            let cmd = match xphoneuse_ios::hid::map_touch(&params.action, params.x, params.y) {
                Ok(c) => c,
                Err(e) => return e.to_string(),
            };
            return match self.ios_hid_send(cmd).await {
                Ok(_) => {
                    self.maybe_record(MacroEvent::Touch {
                        action: params.action.clone(),
                        x: params.x,
                        y: params.y,
                    })
                    .await;
                    format!("Touch {} at ({:.2}, {:.2})", params.action, params.x, params.y)
                }
                Err(e) => e,
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Send a scroll event at normalized coordinates")]
    async fn xphoneuse_send_scroll(&self, params: Parameters<ScrollParams>) -> String {
        let params = params.0;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let px = (params.x * session.screen_width as f64) as u32;
                let py = (params.y * session.screen_height as f64) as u32;
                let sx = (params.dx * 120.0) as i16;
                let sy = (params.dy * 120.0) as i16;
                if let Err(e) = control::inject_scroll(
                    &session.control_socket,
                    px, py,
                    session.screen_width as u16,
                    session.screen_height as u16,
                    sx, sy,
                ).await {
                    return format!("Error: {}", e);
                }
                self.maybe_record(MacroEvent::Scroll {
                    x: params.x,
                    y: params.y,
                    dx: params.dx,
                    dy: params.dy,
                })
                .await;
                return format!("Scrolled at ({:.2}, {:.2}) by ({:.2}, {:.2})", params.x, params.y, params.dx, params.dy);
            }
        }
        // iOS: SCROLL drag through the bridge
        if self.ios_session.lock().await.is_some() {
            let cmd = xphoneuse_ios::hid::map_scroll(params.dx, params.dy);
            return match self.ios_hid_send(cmd).await {
                Ok(_) => {
                    self.maybe_record(MacroEvent::Scroll {
                        x: params.x,
                        y: params.y,
                        dx: params.dx,
                        dy: params.dy,
                    })
                    .await;
                    format!("Scrolled at ({:.2}, {:.2}) by ({:.2}, {:.2})", params.x, params.y, params.dx, params.dy)
                }
                Err(e) => e,
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Enable WiFi debugging on a USB-connected device and connect wirelessly")]
    async fn xphoneuse_wifi_enable(&self, params: Parameters<WifiEnableParams>) -> String {
        let params = params.0;
        let ip = match adb::get_device_ip(&params.serial).await {
            Ok(Some(ip)) => ip,
            Ok(None) => return "Device is not connected to WiFi".to_string(),
            Err(e) => return format!("Error: {}", e),
        };

        if let Err(e) = adb::tcpip(&params.serial, 5555).await {
            return format!("Error enabling tcpip: {}", e);
        }

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
            return format!("Could not connect to {}", addr);
        }

        format!("WiFi debugging enabled at {}", addr)
    }

    #[tool(description = "Connect to a device over WiFi by IP address")]
    async fn xphoneuse_wifi_connect(&self, params: Parameters<AddressParams>) -> String {
        match adb::connect_device(&params.0.address).await {
            Ok(_) => format!("Connected to {}", params.0.address),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Disconnect a WiFi-connected device")]
    async fn xphoneuse_wifi_disconnect(&self, params: Parameters<AddressParams>) -> String {
        match adb::disconnect_device(&params.0.address).await {
            Ok(_) => format!("Disconnected {}", params.0.address),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Get the WiFi IP address of a USB-connected device")]
    async fn xphoneuse_get_device_ip(&self, params: Parameters<SerialParams>) -> String {
        match adb::get_device_ip(&params.0.serial).await {
            Ok(Some(ip)) => format!("IP: {}", ip),
            Ok(None) => "Device is not connected to WiFi".to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Run an adb shell command on the connected device and return its output")]
    async fn xphoneuse_shell(&self, params: Parameters<ShellParams>) -> String {
        if let Some(e) = self.ios_input_error().await { return e; }
        let session = self.session.lock().await;
        let session = match session.as_ref() {
            Some(s) => s,
            None => return "No device connected".to_string(),
        };
        match adb::shell(&session.device_serial, &params.0.command).await {
            Ok(child) => {
                match child.wait_with_output().await {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if stderr.is_empty() {
                            stdout.to_string()
                        } else {
                            format!("{}\nSTDERR: {}", stdout, stderr)
                        }
                    }
                    Err(e) => format!("Error reading output: {}", e),
                }
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Perform a swipe gesture from one point to another at normalized coordinates (0.0-1.0)")]
    async fn xphoneuse_swipe(&self, params: Parameters<SwipeParams>) -> String {
        let params = params.0;
        {
            let session = self.session.lock().await;
            if let Some(session) = session.as_ref() {
                let (socket, w, h) = (session.control_socket.clone(), session.screen_width, session.screen_height);
                let from_px = (params.from_x * w as f64) as u32;
                let from_py = (params.from_y * h as f64) as u32;
                let to_px = (params.to_x * w as f64) as u32;
                let to_py = (params.to_y * h as f64) as u32;
                let steps = params.duration_ms.unwrap_or(300) / 16;

                if let Err(e) = control::inject_touch(
                    &socket, "down",
                    from_px, from_py, w as u16, h as u16,
                ).await {
                    return format!("Error: {}", e);
                }

                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    let cx = from_px as f64 + (to_px as f64 - from_px as f64) * t;
                    let cy = from_py as f64 + (to_py as f64 - from_py as f64) * t;
                    tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
                    if let Err(e) = control::inject_touch(
                        &socket, "move",
                        cx as u32, cy as u32, w as u16, h as u16,
                    ).await {
                        return format!("Error: {}", e);
                    }
                }

                if let Err(e) = control::inject_touch(
                    &socket, "up",
                    to_px, to_py, w as u16, h as u16,
                ).await {
                    return format!("Error: {}", e);
                }
                return format!(
                    "Swiped from ({:.2}, {:.2}) to ({:.2}, {:.2})",
                    params.from_x, params.from_y, params.to_x, params.to_y
                );
            }
        }
        // iOS: SWIPE through the bridge
        if self.ios_session.lock().await.is_some() {
            let cmd = xphoneuse_ios::hid::map_swipe(
                params.from_x, params.from_y, params.to_x, params.to_y,
                params.duration_ms.unwrap_or(300),
            );
            return match self.ios_hid_send(cmd).await {
                Ok(_) => format!(
                    "Swiped from ({:.2}, {:.2}) to ({:.2}, {:.2})",
                    params.from_x, params.from_y, params.to_x, params.to_y
                ),
                Err(e) => e,
            };
        }
        "No device connected".to_string()
    }

    #[tool(description = "Open a URL on the device. By default targets Chrome (com.android.chrome) so links open in a real browser; set use_system_handler true for the previous default-handler behavior.")]
    async fn xphoneuse_open_url(&self, params: Parameters<OpenUrlParams>) -> String {
        if let Some(e) = self.ios_input_error().await { return e; }
        let session = self.session.lock().await;
        let session = match session.as_ref() {
            Some(s) => s,
            None => return "No device connected".to_string(),
        };
        let p = &params.0;
        let cmd = if p.use_system_handler {
            format!("am start -a android.intent.action.VIEW -d '{}'", p.url)
        } else {
            let pkg = p
                .browser_package
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("com.android.chrome");
            if !pkg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            {
                return "Invalid browser_package (use letters, digits, dots, underscores)".to_string();
            }
            format!(
                "am start -a android.intent.action.VIEW -d '{}' -p {}",
                p.url, pkg
            )
        };
        match adb::shell(&session.device_serial, &cmd).await {
            Ok(mut child) => match child.wait().await {
                Ok(_) => format!("Opened {}", p.url),
                Err(e) => format!("Error: {}", e),
            },
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Launch an app by its identifier: the package name on Android (com.android.chrome), \
                       the bundle id on iOS (com.tencent.xin). On iOS the app comes to the foreground the \
                       same way tapping its icon does; it needs the device's developer image, which the \
                       app prepares on its own (and which needs Developer Mode - a device without it says \
                       so by name)."
    )]
    async fn xphoneuse_launch_app(&self, params: Parameters<LaunchAppParams>) -> String {
        let package = params.0.package.trim().to_string();
        if package.is_empty() {
            return "Error: an app identifier is required".to_string();
        }

        // Android first, as everywhere else: a session on the phone in front
        // of the user wins over a view-only iOS one.
        let session = self.session.lock().await;
        if let Some(session) = session.as_ref() {
            let cmd = format!(
                "monkey -p {} -c android.intent.category.LAUNCHER 1",
                package
            );
            return match adb::shell(&session.device_serial, &cmd).await {
                Ok(mut child) => match child.wait().await {
                    Ok(_) => format!("Launched {}", package),
                    Err(e) => format!("Error: {}", e),
                },
                Err(e) => format!("Error: {}", e),
            };
        }
        drop(session);

        // iOS: DVT process control - the only door there is, and the same hub
        // the screenshot path rides.
        let ios_udid = self
            .ios_session
            .lock()
            .await
            .as_ref()
            .map(|session| session.udid.clone());
        let Some(udid) = ios_udid else {
            return "No device connected".to_string();
        };
        match xphoneuse_ios::launch::launch_app_ready(&udid, &package).await {
            Ok(pid) => format!("Launched {} (pid {pid})", package),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "List the apps installed on the connected device, with the identifiers                        xphoneuse_launch_app takes. Android answers with package names (`pm list                        packages`); iOS answers with bundle id, display name and version (lockdown's                        installation proxy - no developer image or Developer Mode needed). Filter it: a                        full list is hundreds of lines."
    )]
    async fn xphoneuse_list_apps(&self, params: Parameters<ListAppsParams>) -> String {
        let params = params.0;
        let filter = params
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_lowercase);
        let kind = params.kind.as_deref().unwrap_or("user");

        // Android first, as everywhere else.
        let session = self.session.lock().await;
        if let Some(session) = session.as_ref() {
            let flag = match kind.to_ascii_lowercase().as_str() {
                "system" | "sys" => " -s",
                "all" | "any" => "",
                _ => " -3",
            };
            let child = match adb::shell(&session.device_serial, &format!("pm list packages{flag}")).await {
                Ok(child) => child,
                Err(e) => return format!("Error: {}", e),
            };
            let output = match child.wait_with_output().await {
                Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
                Err(e) => return format!("Error reading output: {}", e),
            };
            let all: Vec<&str> = output
                .lines()
                .filter_map(|line| line.strip_prefix("package:"))
                .map(str::trim)
                .filter(|package| !package.is_empty())
                .collect();
            let shown: Vec<&str> = all
                .iter()
                .copied()
                .filter(|package| match &filter {
                    Some(f) => package.to_lowercase().contains(f),
                    None => true,
                })
                .collect();
            let rows: Vec<(String, String, String)> = shown
                .iter()
                .map(|package| (package.to_string(), String::new(), String::new()))
                .collect();
            return render_app_list(&rows, all.len(), &filter, "Android");
        }
        drop(session);

        let ios_udid = self
            .ios_session
            .lock()
            .await
            .as_ref()
            .map(|session| session.udid.clone());
        let Some(udid) = ios_udid else {
            return "No device connected".to_string();
        };
        let apps = match xphoneuse_ios::apps::list_apps(&udid, kind).await {
            Ok(apps) => apps,
            Err(e) => return format!("Error: {}", e),
        };
        let total = apps.len();
        let rows: Vec<(String, String, String)> = apps
            .into_iter()
            .filter(|app| match &filter {
                Some(f) => {
                    app.bundle_id.to_lowercase().contains(f)
                        || app.name.to_lowercase().contains(f)
                }
                None => true,
            })
            .map(|app| (app.bundle_id, app.name, app.version))
            .collect();
        render_app_list(&rows, total, &filter, "iOS")
    }

    #[tool(
        description = "List the files in a directory on the phone. Android browses the whole of adb's \
                       world (`/sdcard`, `/`); iOS browses the media area - the camera roll under \
                       `/DCIM`, `Downloads` and the folders apps share - which is all AFC reaches \
                       (no app containers, no Developer Mode needed). Pass a location \
                       (`android:/sdcard/DCIM`, or a bare `/sdcard/DCIM` for the connected phone) or \
                       omit it for the phone's files root. Use this before xphoneuse_pull_file or to \
                       find somewhere to xphoneuse_push_file."
    )]
    async fn xphoneuse_list_files(&self, params: Parameters<ListFilesParams>) -> String {
        let params = params.0;
        let (target, path) = match self.resolve_location(params.path.as_deref()).await {
            Ok(pair) => pair,
            Err(e) => return format!("Error: {e}"),
        };
        let directory = match &target {
            FsTarget::Android(serial) => files::list_dir(serial, &path).await,
            FsTarget::Ios(udid) => match xphoneuse_ios::files::session(udid).await {
                Ok(session) => session.list(&path).await,
                Err(e) => return format!("Error: {e}"),
            },
        };
        let directory = match directory {
            Ok(directory) => directory,
            Err(e) => return format!("Error: {e}"),
        };
        let listing = files::build_listing(target.platform(), target.device(), &path, directory);
        let filter = params
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_lowercase);
        render_fs_listing(&listing, &filter)
    }

    #[tool(
        description = "Copy a file off the phone onto this computer. Give the file's location \
                       (`android:/sdcard/DCIM/123.mp4`, or a bare path for the connected phone) and an \
                       absolute local directory to save it into; it keeps its name, and a name already \
                       there is numbered rather than overwritten. Folders are not copied - list and \
                       pull the files."
    )]
    async fn xphoneuse_pull_file(&self, params: Parameters<PullFileParams>) -> String {
        let params = params.0;
        let dest = std::path::PathBuf::from(params.dest.trim());
        if !dest.is_absolute() {
            return format!(
                "Error: {} is not an absolute directory - say where on this computer the file should go",
                params.dest
            );
        }
        if let Err(e) = tokio::fs::create_dir_all(&dest).await {
            return format!("Error: could not use {}: {e}", dest.display());
        }
        let (target, path) = match self.resolve_location(Some(&params.path)).await {
            Ok(pair) => pair,
            Err(e) => return format!("Error: {e}"),
        };
        let name = path.rsplit('/').next().unwrap_or("file").to_string();

        match &target {
            FsTarget::Android(serial) => {
                let local = match files::unique_local_path(&dest, &name) {
                    Ok(local) => local,
                    Err(e) => return format!("Error: {e}"),
                };
                // Through a `.part` name like the iOS side: a killed transfer
                // must not leave a short file under the real name.
                let part = local.with_file_name(format!("{name}.part"));
                let result = adb::pull_file(serial, &path, &part.display().to_string()).await;
                match result {
                    Ok(()) => match tokio::fs::rename(&part, &local).await {
                        Ok(()) => {
                            let size = tokio::fs::metadata(&local)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0);
                            format!(
                                "Pulled {} ({}) to {}",
                                path,
                                files::human_size(size),
                                local.display()
                            )
                        }
                        Err(e) => format!("Error: could not finish {}: {e}", local.display()),
                    },
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&part).await;
                        format!("Error: {e}")
                    }
                }
            }
            FsTarget::Ios(udid) => {
                let local = match files::unique_local_path(&dest, &name) {
                    Ok(local) => local,
                    Err(e) => return format!("Error: {e}"),
                };
                let mut no_progress = |_: u64, _: u64| {};
                match xphoneuse_ios::files::download(
                    udid,
                    &path,
                    &local,
                    &std::sync::atomic::AtomicBool::new(false),
                    &mut no_progress,
                )
                .await
                {
                    Ok((local, size)) => format!(
                        "Pulled {} ({}) to {}",
                        path,
                        files::human_size(size),
                        local.display()
                    ),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
    }

    #[tool(
        description = "Send a file from this computer to the phone. `host_path` is an absolute path \
                       on this machine; `path` is the directory on the phone to put it in (a \
                       location, or a bare path for the connected phone; omitted = the phone's files \
                       root). A name already there is numbered rather than overwritten. On iOS the \
                       media area is reachable (`/`, `/DCIM`, `/Downloads`); app containers are not."
    )]
    async fn xphoneuse_push_file(&self, params: Parameters<PushFileParams>) -> String {
        let params = params.0;
        let local = std::path::PathBuf::from(params.host_path.trim());
        let info = match tokio::fs::metadata(&local).await {
            Ok(info) => info,
            Err(e) => return format!("Error: could not read {}: {e}", local.display()),
        };
        if info.is_dir() {
            return format!("Error: {} is a folder - send files", local.display());
        }
        let (target, dir) = match self.resolve_location(params.path.as_deref()).await {
            Ok(pair) => pair,
            Err(e) => return format!("Error: {e}"),
        };
        let name = match local.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => return format!("Error: {} has no file name", local.display()),
        };

        match &target {
            FsTarget::Android(serial) => {
                let remote = match files::unique_remote_path(serial, &dir, &name).await {
                    Ok(remote) => remote,
                    Err(e) => return format!("Error: {e}"),
                };
                match adb::push_file(serial, &local.display().to_string(), &remote).await {
                    Ok(()) => format!(
                        "Pushed {} ({}) to {}",
                        name,
                        files::human_size(info.len()),
                        remote
                    ),
                    Err(e) => format!("Error: {e}"),
                }
            }
            FsTarget::Ios(udid) => {
                let remote = match xphoneuse_ios::files::unique_remote_path(udid, &dir, &name).await {
                    Ok(remote) => remote,
                    Err(e) => return format!("Error: {e}"),
                };
                let mut no_progress = |_: u64, _: u64| {};
                match xphoneuse_ios::files::upload(
                    udid,
                    &local,
                    &remote,
                    &std::sync::atomic::AtomicBool::new(false),
                    &mut no_progress,
                )
                .await
                {
                    Ok(size) => format!(
                        "Pushed {} ({}) to {}",
                        name,
                        files::human_size(size),
                        remote
                    ),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
    }

    #[tool(description = "Get the UI accessibility tree of the current screen. Returns a hierarchical text representation of all UI elements with their properties and normalized coordinates (0.0-1.0). Use this to understand the screen layout before interacting. Coordinates can be used directly with xphoneuse_send_touch.")]
    async fn xphoneuse_get_ui_tree(&self) -> String {
        if let Some(e) = self.ios_input_error().await { return e; }
        match self.get_ui_elements().await {
            Ok((elements, sw, sh)) => {
                let tree = accessibility::format_tree(&elements, 0);
                format!("Screen: {}x{}\n{}", sw, sh, tree)
            }
            Err(e) => e,
        }
    }

    #[tool(description = "Search for UI elements on screen by text, content description, resource ID, or class name. Returns matching elements with normalized coordinates ready to use with xphoneuse_send_touch. Much more reliable than screenshot-based element discovery.")]
    async fn xphoneuse_find_on_screen(&self, params: Parameters<FindOnScreenParams>) -> String {
        if let Some(e) = self.ios_input_error().await { return e; }
        let p = params.0;
        if p.text.is_none()
            && p.content_desc.is_none()
            && p.resource_id.is_none()
            && p.class_name.is_none()
        {
            return "Provide at least one search parameter (text, content_desc, resource_id, or class_name)".to_string();
        }

        let elements = match self.get_ui_elements().await {
            Ok((e, _, _)) => e,
            Err(e) => return e,
        };

        let found = accessibility::find_elements(
            &elements,
            p.text.as_deref(),
            p.content_desc.as_deref(),
            p.resource_id.as_deref(),
            p.class_name.as_deref(),
            p.clickable_only.unwrap_or(false),
        );

        if found.is_empty() {
            "No elements found matching the search criteria".to_string()
        } else {
            serde_json::to_string_pretty(&found).unwrap_or_else(|e| format!("Error: {}", e))
        }
    }

    #[tool(description = "Start recording a macro. All subsequent actions (touch, text, scroll, button presses) will be recorded with timing until you call xphoneuse_macro_stop.")]
    async fn xphoneuse_macro_record(&self, params: Parameters<MacroNameParams>) -> String {
        let mut recorder = self.recorder.lock().await;
        if recorder.is_some() {
            return "Already recording. Call xphoneuse_macro_stop first.".to_string();
        }
        *recorder = Some(MacroRecorder::new(params.0.name.clone()));
        format!("Recording macro '{}'", params.0.name)
    }

    #[tool(description = "Stop recording and save the current macro")]
    async fn xphoneuse_macro_stop(&self) -> String {
        let mut recorder = self.recorder.lock().await;
        let r = match recorder.take() {
            Some(r) => r,
            None => return "No macro is being recorded".to_string(),
        };
        let m = r.finish();
        let name = m.name.clone();
        let count = m.events.len();
        self.macros.lock().await.insert(name.clone(), m);
        format!("Saved macro '{}' with {} events", name, count)
    }

    #[tool(description = "Play a recorded macro by name. Replays all recorded actions with original timing.")]
    async fn xphoneuse_macro_play(&self, params: Parameters<MacroPlayParams>) -> String {
        let m = {
            let macros = self.macros.lock().await;
            match macros.get(&params.0.name) {
                Some(m) => m.clone(),
                None => return format!("Macro '{}' not found", params.0.name),
            }
        };

        let repeat = params.0.repeat.unwrap_or(1).max(1);

        // Android path
        if self.session.lock().await.is_some() {
            let (socket, sw, sh) = {
                let session = self.session.lock().await;
                let session = match session.as_ref() {
                    Some(s) => s,
                    None => return "No device connected".to_string(),
                };
                (
                    session.control_socket.clone(),
                    session.screen_width,
                    session.screen_height,
                )
            };
            for i in 0..repeat {
                // An agent's playback runs to the end: `stop_macro` is the
                // app's own button, and there is no UI on this path to press
                // it from.
                if let Err(e) = macro_engine::play_events(&m.events, &socket, sw, sh, &|| false).await {
                    return format!("Playback error on repeat {}: {}", i + 1, e);
                }
            }
            return format!(
                "Played macro '{}' ({} events, {} time(s))",
                params.0.name,
                m.events.len(),
                repeat
            );
        }

        // iOS path: replay through the BLE-HID bridge
        if self.ios_session.lock().await.is_some() {
            // Wake a sleeping phone first, but only when it is actually
            // asleep: the only screen key iOS offers is power, and power is a
            // toggle, so pressing it on an awake phone would blank the screen
            // the macro is about to use. A stalled frame stream is the signal
            // (iOS stops streaming when it sleeps); a session that never
            // streams reports no age and gets no press.
            if let Some(t) = {
                let s = self.ios_session.lock().await;
                s.as_ref().and_then(|s| s.tunnel.clone())
            } {
                if t.frame_age().is_some_and(|age| age > std::time::Duration::from_secs(3)) {
                    if let Err(e) = self.ios_hid_send("KEY POWER".to_string()).await {
                        return format!("Error: could not wake the device: {e}");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                }
            }
            for i in 0..repeat {
                let mut prev_ts = 0u64;
                for timed in &m.events {
                    let sleep_ms = timed.timestamp_ms.saturating_sub(prev_ts);
                    if sleep_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
                    }
                    prev_ts = timed.timestamp_ms;
                    let cmd = match &timed.event {
                        MacroEvent::Touch { action, x, y } => {
                            match xphoneuse_ios::hid::map_touch(action, *x, *y) {
                                Ok(c) => c,
                                Err(e) => return format!("Playback error on repeat {}: {}", i + 1, e),
                            }
                        }
                        MacroEvent::Text { text } => {
                            // Same delivery policy as the text tools, so a
                            // recorded message goes back out the way it went
                            // out the first time - and a character the
                            // transport cannot carry is an error here rather
                            // than a silent drop.
                            let method = self.text_input_default();
                            if let Err(e) = self.deliver_ios_text(text, false, method).await {
                                return format!("Playback error on repeat {}: {}", i + 1, e);
                            }
                            continue;
                        }
                        MacroEvent::Key { keycode, .. } => {
                            match xphoneuse_ios::hid::map_key(*keycode) {
                                Ok(c) => c,
                                Err(e) => return format!("Playback error on repeat {}: {}", i + 1, e),
                            }
                        }
                        MacroEvent::Scroll { dx, dy, .. } => xphoneuse_ios::hid::map_scroll(*dx, *dy),
                        MacroEvent::Button { button } => {
                            // iOS has no consumer key for the app switcher, so
                            // `recents` replays as the same system gesture the
                            // live button press uses.
                            if button == "recents" {
                                for (cmd, delay) in xphoneuse_ios::hid::app_switcher_gesture() {
                                    if let Err(e) = self.ios_hid_send(cmd).await {
                                        return format!("Playback error on repeat {}: {}", i + 1, e);
                                    }
                                    if !delay.is_zero() {
                                        tokio::time::sleep(delay).await;
                                    }
                                }
                                continue;
                            }
                            match xphoneuse_ios::hid::map_button(button) {
                                Ok(c) => c,
                                Err(e) => return format!("Playback error on repeat {}: {}", i + 1, e),
                            }
                        }
                        MacroEvent::Wait { .. } => continue,
                    };
                    if let Err(e) = self.ios_hid_send(cmd).await {
                        return format!("Playback error on repeat {}: {}", i + 1, e);
                    }
                }
            }
            return format!(
                "Played macro '{}' ({} events, {} time(s))",
                params.0.name,
                m.events.len(),
                repeat
            );
        }

        "No device connected".to_string()
    }

    #[tool(description = "List all recorded macros")]
    async fn xphoneuse_macro_list(&self) -> String {
        let macros = self.macros.lock().await;
        if macros.is_empty() {
            "No macros recorded".to_string()
        } else {
            macros
                .iter()
                .map(|(name, m)| format!("{} ({} events)", name, m.events.len()))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    #[tool(description = "Rotate the device screen orientation")]
    async fn xphoneuse_rotate_device(&self) -> String {
        if self.session.lock().await.is_none() && self.ios_session.lock().await.is_some() {
            return "Rotation is not supported for iOS devices".to_string();
        }
        let session = self.session.lock().await;
        let session = match session.as_ref() {
            Some(s) => s,
            None => return "No device connected".to_string(),
        };
        match control::rotate_device(&session.control_socket).await {
            Ok(_) => "Device rotated".to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Delete a recorded macro")]
    async fn xphoneuse_macro_delete(&self, params: Parameters<MacroNameParams>) -> String {
        if self.macros.lock().await.remove(&params.0.name).is_some() {
            format!("Deleted macro '{}'", params.0.name)
        } else {
            format!("Macro '{}' not found", params.0.name)
        }
    }
}

// --- what a screenshot is worth to a model ---------------------------------
//
// The device hands over a full-resolution PNG (a 1179x2556 phone screen, a few
// megabytes of mostly flat colour). Nothing that reads it needs that: vision
// models scale to 1568px on the long side anyway, and as JPEG the same screen
// is a fraction of the size. Only what *leaves* the tools is touched - OCR and
// the copilot read the full-resolution capture, and `format = "png"` still
// returns one.

/// Longest side of a returned screenshot, in pixels.
const DEFAULT_MAX_DIMENSION: u32 = 1568;
/// JPEG quality of a returned screenshot.
const DEFAULT_JPEG_QUALITY: u8 = 80;

fn screenshot_for_model(
    capture: &[u8],
    max_dimension: u32,
    format: &str,
    quality: u8,
) -> Result<(Vec<u8>, &'static str), String> {
    use image::ImageEncoder;

    let as_png = format.eq_ignore_ascii_case("png");
    // The untouched capture, for a caller who asked for it exactly as it came:
    // re-encoding it would only lose a round trip.
    if as_png && max_dimension == 0 {
        return Ok((capture.to_vec(), "image/png"));
    }

    let image = image::load_from_memory(capture)
        .map_err(|e| format!("could not read the capture: {e}"))?;
    let longest = image.width().max(image.height());
    // `resize` also scales *up*, which is the last thing a small capture needs.
    let image = if max_dimension > 0 && longest > max_dimension {
        image.resize(
            max_dimension,
            max_dimension,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image
    };

    let mut out = Vec::new();
    if as_png {
        image
            .write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            ))
            .map_err(|e| format!("could not encode the capture: {e}"))?;
        Ok((out, "image/png"))
    } else {
        // JPEG carries no alpha: hand it RGB rather than let the fourth
        // channel be dropped (or the encode refused) further down.
        let rgb = image.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| format!("could not encode the capture: {e}"))?;
        Ok((out, "image/jpeg"))
    }
}

/// A directory as the model will read it: a header naming the place, one
/// entry per line, capped like the app list and for the same reason - a camera
/// roll is thousands of lines of context for the one photo that was wanted.
///
/// Directories carry a trailing `/` and no size, which is how `ls` has said it
/// for fifty years and how a model reads it without being told.
fn render_fs_listing(listing: &files::FsListing, filter: &Option<String>) -> String {
    const MAX_ROWS: usize = 150;
    let matching: Vec<&files::FsEntry> = listing
        .entries
        .iter()
        .filter(|entry| match filter {
            Some(f) => entry.name.to_lowercase().contains(f),
            None => true,
        })
        .collect();
    let where_ = format!("{} on {}", listing.location, listing.device);
    if matching.is_empty() {
        return match filter {
            Some(f) => format!(
                "Nothing in {where_} matches \"{f}\" ({} entries)",
                listing.entries.len()
            ),
            None => format!("{where_} is empty"),
        };
    }

    let dirs = matching.iter().filter(|e| e.kind == "dir").count();
    let mut out = match filter {
        Some(f) => format!(
            "{} of {} entries in {where_} match \"{f}\" ({dirs} folders):",
            matching.len(),
            listing.entries.len()
        ),
        None => format!(
            "{} entries in {where_} ({dirs} folders):",
            listing.entries.len()
        ),
    };
    for entry in matching.iter().take(MAX_ROWS) {
        let name = if entry.kind == "dir" {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };
        let size = if entry.kind == "dir" {
            "-".to_string()
        } else {
            files::human_size(entry.size)
        };
        let when = entry
            .modified
            .map(files::format_time)
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!("\n{name}\t{size}\t{when}"));
    }
    if matching.len() > MAX_ROWS {
        out.push_str(&format!(
            "\n... and {} more - narrow it with filter",
            matching.len() - MAX_ROWS
        ));
    }
    for warning in listing.warnings.iter().take(3) {
        out.push_str(&format!("\n({warning})"));
    }
    out
}

/// A listing as the model will read it: one app per line, capped.
///
/// The cap matters more than it looks: `pm list packages` on a phone with a
/// few hundred apps is a few hundred lines of context, and the caller who
/// wanted one of them did not mean to pay for all of them.
fn render_app_list(rows: &[(String, String, String)], total: usize, filter: &Option<String>, platform: &str) -> String {
    const MAX_ROWS: usize = 150;
    if rows.is_empty() {
        return match filter {
            Some(f) => format!("No app on this {platform} device matches \"{f}\" ({total} installed)"),
            None => format!("No apps listed ({total} installed)"),
        };
    }
    let mut out = match filter {
        Some(f) => format!("{} app(s) matching \"{f}\" of {total} ({platform}):", rows.len()),
        None => format!("{} app(s) ({platform}):", rows.len()),
    };
    for (id, name, version) in rows.iter().take(MAX_ROWS) {
        match (name.is_empty(), version.is_empty()) {
            // Android answers with identifiers alone, and often has no name
            // for an app at all (app extensions).
            (true, _) | (_, true) => out.push_str(&format!("
{id}	{name}")),
            (false, false) => out.push_str(&format!("
{id}	{name} {version}")),
        }
    }
    if rows.len() > MAX_ROWS {
        out.push_str(&format!(
            "
... and {} more - narrow it with filter",
            rows.len() - MAX_ROWS
        ));
    }
    out
}

#[cfg(test)]
mod screenshot_tests {
    use super::*;

    /// A 200x400 PNG, the shape of a phone screen, with something to compress.
    fn capture() -> Vec<u8> {
        let mut img = image::RgbaImage::new(200, 400);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x % 255) as u8, (y % 255) as u8, 40, 255]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode fixture");
        out
    }

    fn decoded(bytes: &[u8]) -> (u32, u32, image::ImageFormat) {
        let format = image::guess_format(bytes).expect("format");
        let img = image::load_from_memory(bytes).expect("decode");
        (img.width(), img.height(), format)
    }

    #[test]
    fn a_tall_capture_is_scaled_to_the_longest_side() {
        let (bytes, mime) = screenshot_for_model(&capture(), 100, "jpeg", 80).unwrap();
        assert_eq!(mime, "image/jpeg");
        let (w, h, format) = decoded(&bytes);
        assert_eq!((w, h), (50, 100), "200x400 scaled to a 100px long side");
        assert_eq!(format, image::ImageFormat::Jpeg);
    }

    #[test]
    fn a_capture_inside_the_limit_is_not_scaled_up() {
        let (bytes, _) = screenshot_for_model(&capture(), 1568, "jpeg", 80).unwrap();
        let (w, h, _) = decoded(&bytes);
        assert_eq!((w, h), (200, 400));
    }

    #[test]
    fn png_asks_for_lossless_and_keeps_the_size() {
        let (bytes, mime) = screenshot_for_model(&capture(), 0, "png", 80).unwrap();
        assert_eq!(mime, "image/png");
        // Untouched, byte for byte: no decoder, no re-encode.
        assert_eq!(bytes, capture());
    }

    /// Size is not asserted here: how the two codecs rank depends on the
    /// picture, and a synthetic one is not a screen. It is measured on real
    /// captures instead (`--nocapture` prints it).
    #[test]
    fn printing_the_sizes_of_a_real_capture() {
        let Some(path) = std::env::var_os("XPHONEUSE_TEST_SCREENSHOT") else {
            return;
        };
        let capture = std::fs::read(path).expect("read capture");
        for (format, max) in [("jpeg", DEFAULT_MAX_DIMENSION), ("png", DEFAULT_MAX_DIMENSION)] {
            let (bytes, _) = screenshot_for_model(&capture, max, format, DEFAULT_JPEG_QUALITY).unwrap();
            let (w, h, _) = decoded(&bytes);
            println!(
                "{format} at {max}px: {w}x{h}, {} KB (capture {} KB)",
                bytes.len() / 1024,
                capture.len() / 1024
            );
        }
    }
}

#[cfg(test)]
mod list_apps_tests {
    use super::*;

    fn rows(n: usize) -> Vec<(String, String, String)> {
        (0..n)
            .map(|i| (format!("com.example.{i}"), format!("App {i}"), "1.0".into()))
            .collect()
    }

    /// A phone with a few hundred apps is a few hundred lines of context, and
    /// the caller who wanted one of them did not mean to pay for all of them.
    #[test]
    fn a_long_listing_is_capped_and_says_how_many_are_left() {
        let out = render_app_list(&rows(200), 200, &None, "Android");
        assert_eq!(out.lines().count(), 1 + 150 + 1);
        assert!(out.starts_with("200 app(s) (Android):"));
        assert!(out.ends_with("... and 50 more - narrow it with filter"), "{out}");
    }

    #[test]
    fn a_filtered_listing_says_what_it_is_a_filter_of() {
        let out = render_app_list(&rows(3), 129, &Some("example".into()), "iOS");
        assert!(out.starts_with("3 app(s) matching \"example\" of 129 (iOS):"));
        assert!(out.contains("com.example.0	App 0 1.0"));
    }

    #[test]
    fn a_filter_that_matches_nothing_is_not_an_empty_string() {
        let out = render_app_list(&[], 129, &Some("wechat".into()), "iOS");
        assert!(out.contains("No app"), "{out}");
        assert!(out.contains("129 installed"), "{out}");
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for XPhoneUseMcp {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut caps = rmcp::model::ServerCapabilities::default();
        caps.tools = Some(rmcp::model::ToolsCapability::default());
        rmcp::model::ServerInfo::new(caps).with_instructions(
            "Android and iOS device control server. Use xphoneuse_list_devices to find devices \
             (lines are prefixed 'android:' or 'ios:'), xphoneuse_connect_device to establish a \
             session (set platform: 'ios' for iOS devices), then control devices with \
             xphoneuse_press_button, xphoneuse_send_text, xphoneuse_send_touch, \
             xphoneuse_send_scroll, and xphoneuse_take_screenshot. \
             On iOS, non-ASCII text (e.g. Chinese) is delivered either through the device \
             clipboard or through the AgentInput keyboard, whichever the app is configured \
             for; xphoneuse_unicode_text picks one explicitly. Use xphoneuse_get_ui_tree and \
             xphoneuse_find_on_screen for reliable UI element discovery on Android, and \
             xphoneuse_ocr to read text off the screen as text (offline OCR; on iOS it is the only \
             way to read the screen without spending image tokens). Use \
             xphoneuse_macro_record/xphoneuse_macro_stop/xphoneuse_macro_play for \
             automation recording and playback."
                .to_string(),
        )
    }
}
