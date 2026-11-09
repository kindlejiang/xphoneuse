pub mod server;

use std::sync::Arc;
use anyhow::Result;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

/// One frame handed over by the host webview.
#[derive(Clone, Debug)]
pub struct MirrorFrame {
    /// Base64 PNG of the mirror canvas.
    pub data: String,
    /// What the webview saw while grabbing it (how many canvases the document
    /// has, whether this one is attached and visible, which element is on
    /// screen). Reported back to the caller, so a stale picture says why
    /// instead of leaving the user to guess.
    pub diag: String,
}

/// Live-mirror frame plumbing shared with the host app. The MCP server asks
/// the host for a frame (the app relays the request to its webview, which
/// grabs the decoded AirPlay canvas); the host delivers the base64 PNG (or
/// `None` when mirroring is not running) via [`MirrorFrames::deliver`].
#[derive(Clone, Default)]
pub struct MirrorFrames {
    /// Host callback: start a frame request with the given id.
    pub request: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Pending replies keyed by request id.
    replies: Arc<std::sync::Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<Option<MirrorFrame>>>>>,
}

impl MirrorFrames {
    /// Deliver a reply for a pending request (no-op for unknown ids).
    pub fn deliver(&self, id: u64, data: Option<String>, diag: Option<String>) {
        if let Some(tx) = self.replies.lock().unwrap().remove(&id) {
            let _ = tx.send(data.map(|data| MirrorFrame {
                data,
                diag: diag.unwrap_or_default(),
            }));
        }
    }

    /// Request a live frame and wait for the reply. `None` when no host is
    /// attached, the host is not mirroring, or the reply times out.
    pub async fn get(&self, timeout: std::time::Duration) -> Option<MirrorFrame> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let request = self.request.as_ref()?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.replies.lock().unwrap().insert(id, tx);
        request(id);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(data)) => data,
            _ => {
                self.replies.lock().unwrap().remove(&id);
                None
            }
        }
    }
}

/// The host app's current iOS session, as seen by the embedded MCP server.
/// `hid` is the same handle the app UI uses, so CLI input and app input
/// drive the same BLE-HID bridge; `tunnel` is the app's CoreDevice tunnel
/// (USB) when the input/screen mode selected it, sharing the same lifecycle.
pub struct IosHandle {
    pub udid: String,
    pub hid: Option<Arc<tokio::sync::Mutex<xphoneuse_ios::hid::HidSession>>>,
    pub tunnel: Option<Arc<xphoneuse_ios::tunnel::TunnelHandle>>,
}

/// The host app's current Android (scrcpy) session. `control_socket` is the
/// same handle the app UI uses, so CLI input and app input share it.
pub struct AndroidHandle {
    pub serial: String,
    pub control_socket: Arc<tokio::sync::Mutex<tokio::net::TcpStream>>,
    pub screen_width: u32,
    pub screen_height: u32,
}

/// Host-app integration hooks. Only set when the server is embedded in the
/// XPhoneUse app; the standalone binary uses [`HostHooks::default`] and the
/// server runs its own uxplay/HID/scrcpy sessions.
#[derive(Clone, Default)]
pub struct HostHooks {
    /// Live mirror frames from the host app's webview.
    pub frames: MirrorFrames,
    /// Ask the host app to connect an iOS device. The app switches its UI
    /// to the mirror screen and streams the video itself; fire-and-forget -
    /// poll [`HostHooks::ios_session`] for the result.
    pub ios_connect: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// The host app's current iOS session, if any.
    pub ios_session: Option<Arc<dyn Fn() -> Option<IosHandle> + Send + Sync>>,
    /// Ask the host app to connect an Android device (same flow as iOS).
    pub android_connect: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// The host app's current Android session, if any.
    pub android_session: Option<Arc<dyn Fn() -> Option<AndroidHandle> + Send + Sync>>,
    /// How the app is configured to deliver non-ASCII text to an iPhone
    /// (Settings). Read at call time, so changing the setting takes effect
    /// without restarting the server. Unset = the clipboard default.
    pub text_input_method: Option<Arc<dyn Fn() -> xphoneuse_ios::text_input::TextInputMethod + Send + Sync>>,
}

pub async fn start_sse_server(
    port: u16,
    scrcpy_server_path: Option<String>,
    cancel: CancellationToken,
    agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
    hooks: HostHooks,
) -> Result<()> {
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel.child_token());

    // One shared connection state for every MCP session the server accepts:
    // rmcp builds a handler per session, and short-lived clients (the CLI)
    // must see each other's connect/disconnect.
    let shared = std::sync::Arc::new(server::Shared::default());
    // Embedded in the app: follow the app's own device session, so tools work
    // the moment the app is connected rather than the moment someone calls
    // `connect_device` (see `Shared::follow_host`).
    if hooks.ios_session.is_some() || hooks.android_session.is_some() {
        let follower = shared.clone();
        let following = hooks.clone();
        let follow_cancel = cancel.child_token();
        tokio::spawn(async move { follower.follow_host(following, follow_cancel).await });
    }
    let service = StreamableHttpService::new(
        move || {
            Ok(server::XPhoneUseMcp::with_shared(
                scrcpy_server_path.clone(),
                agent_input.clone(),
                hooks.clone(),
                shared.clone(),
            ))
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    eprintln!("[mcp] SSE server listening on http://{}/mcp", listener.local_addr()?);

    let ct = cancel.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;

    Ok(())
}
