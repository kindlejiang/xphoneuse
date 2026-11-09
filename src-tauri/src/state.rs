use crate::audio::AudioHandle;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct ScrcpySession {
    pub device_serial: String,
    pub control_socket: Arc<Mutex<TcpStream>>,
    pub screen_width: u32,
    pub screen_height: u32,
    pub shutdown: Arc<tokio::sync::Notify>,
    pub audio: Option<Arc<AudioHandle>>,
}

pub struct IosSession {
    pub udid: String,
    pub shutdown: Arc<tokio::sync::Notify>,
    /// FFI uxplay session (primary path); `None` when using the legacy
    /// uxplay.exe subprocess fallback.
    pub uxplay: Option<xphoneuse_ios::uxplay_ffi::UxplaySession>,
    /// BLE-HID bridge over CDC-ACM (AC636N). `Some` makes the session
    /// controllable (touch/keyboard/buttons); `None` = view-only.
    pub hid: Option<Arc<tokio::sync::Mutex<xphoneuse_ios::hid::HidSession>>>,
    /// CoreDevice tunnel over USB (iOS 17+, Developer Mode). Provides HID
    /// input when the input mode is "tunnel" and, when `screen` is
    /// "tunnel", the live video stream as well. `None` when neither mode
    /// selected it or it failed to start (AirPlay/bridge fallback).
    pub tunnel: Option<Arc<xphoneuse_ios::tunnel::TunnelHandle>>,
    /// Which transport actually provides the screen: "airplay" or "tunnel".
    pub screen: String,
}

pub struct McpServerState {
    pub cancel: Option<CancellationToken>,
    pub port: u16,
}

pub struct AppState {
    pub session: Arc<Mutex<Option<ScrcpySession>>>,
    pub ios_session: Arc<Mutex<Option<IosSession>>>,
    pub mcp: Arc<Mutex<McpServerState>>,
    /// AgentInput (iOS text-input keyboard) client. Lazily connected; used
    /// for non-ASCII text, which the BLE-HID bridge cannot type.
    pub agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
    /// Local TCP forward piping the AgentInput client's traffic to the phone
    /// over USB (usbmuxd), so no shared LAN is needed. `Some` exactly when
    /// the agent_input client is in USB mode. Lock order: `agent_input` before
    /// `agent_input_forward` wherever both are held.
    pub agent_input_forward: Arc<std::sync::Mutex<Option<xphoneuse_ios::usb_forward::UsbForward>>>,
    /// Live AirPlay frame plumbing for the embedded MCP server: the server
    /// requests a frame, the webview replies with the decoded canvas.
    pub mcp_frames: xphoneuse_mcp::MirrorFrames,
    /// How non-ASCII text is delivered to an iPhone by default (Settings).
    /// Shared with the embedded MCP server, which reads it per call.
    pub unicode_text_default: Arc<std::sync::Mutex<xphoneuse_ios::text_input::TextInputMethod>>,
    /// The conversation copilot's in-memory conversation. Never written to
    /// disk; cleared on a device switch.
    pub copilot: crate::copilot::SharedCopilot,
    /// The iOS location session, when one is running. The simulation lives
    /// only as long as this handle - dropping it closes the channel and the
    /// phone goes back to its real position (see src/location.rs). Kept
    /// together with the udid it belongs to, so switching devices cannot
    /// inherit the previous phone's session.
    pub location: Arc<tokio::sync::Mutex<Option<(String, xphoneuse_ios::location::LocationSession)>>>,
    /// Set by `stop_macro` to cut a playback short - the loop button's own
    /// stop, and Ctrl+E. Cleared by `play_macro` when a run starts, so a press
    /// between two rounds of a loop cannot kill the next one.
    pub macro_stop: Arc<std::sync::atomic::AtomicBool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
            ios_session: Arc::new(Mutex::new(None)),
            mcp: Arc::new(Mutex::new(McpServerState {
                cancel: None,
                port: 7070,
            })),
            agent_input: Arc::new(std::sync::Mutex::new(None)),
            agent_input_forward: Arc::new(std::sync::Mutex::new(None)),
            mcp_frames: xphoneuse_mcp::MirrorFrames::default(),
            unicode_text_default: Arc::new(std::sync::Mutex::new(
                xphoneuse_ios::text_input::TextInputMethod::default(),
            )),
            copilot: Arc::new(std::sync::Mutex::new(
                crate::copilot::CopilotState::default(),
            )),
            macro_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            location: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}
