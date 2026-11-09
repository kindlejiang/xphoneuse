//! CoreDevice tunnel transport for iOS: live screen, HID input and one-shot
//! screenshots, all multiplexed over a single CoreDeviceProxy tunnel (USB,
//! iOS 17+, Developer Mode required).
//!
//! Why this exists: the AirPlay mirror needs the user to open Control Center
//! and start screen mirroring by hand, and the DVT screenshot service is slow
//! and stalls. Over the tunnel we get the same things Xcode gets — a
//! displayservice HEVC media stream (the "live screen"), the CoreDevice HID
//! services (touch/keyboard/buttons) and the screencaptureservice one-shot
//! screenshot — with no user interaction and no AirPlay.
//!
//! One actor task owns the tunnel (AdapterHandle/RsdHandshake need `&mut` and
//! are not shareable) and serves three channels:
//!
//! * video RTP → [`TunnelFrame`] broadcast (always depacketized; jktcp
//!   buffers unread datagrams, so headless mode still drains the sockets),
//! * HID requests → lazily connected `universalhidservice` (touch) and
//!   `hid.indigo` (keyboard/buttons) clients,
//! * screenshot requests → a lazily connected `screencaptureservice` client.
//!
//! Two device quirks drive the design:
//!
//! * **HID auth gate**: synthetic touch/key events are silently dropped by
//!   backboardd unless a displayservice media stream is active. The stream is
//!   therefore ALWAYS started (even headless), and `input_ready` only flips
//!   ~300ms after the stream starts, when backboardd has re-matched the HID
//!   surfaces against the newly authenticated stream.
//! * **RTCP timeout**: the device stops encoding after ~20-25s without RTCP
//!   receiver reports, so the actor sends a liveness compound report every 5s
//!   to the observed RTP source port.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use idevice::core_device::hid::{
    ButtonState, IndigoHidClient, UniversalHidServiceClient, TOUCHSCREEN_STATE_CONTACT,
    TOUCHSCREEN_STATE_RELEASE,
};
use idevice::core_device::display_stream::{
    build_frame_ack, build_keyframe_request, build_liveness, build_rctl, build_screen_audio_offer,
    build_screen_video_offer_with_codec, build_start_audio_parameters,
    build_start_video_parameters, is_rtcp, AvcDepacketizer, CallInfoBlob, DisplayServiceClient,
    HevcDepacketizer, ReportBlock, RtpPacket, ScreenVideoCodec, SenderReport,
};
use idevice::core_device::{PasteboardServiceClient, ScreenCaptureServiceClient, GENERAL_PASTEBOARD};
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::provider::IdeviceProvider;
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::{AdapterHandle, UdpSocketHandle};
use idevice::{IdeviceService, ReadWrite, RemoteXpcClient, RsdService};
use tokio::sync::{mpsc, oneshot, Notify};

/// What we (the controller) support, NOT the device's mask. Device Hub sends
/// 140 for screen sharing; the device's larger mask produces an invalid config.
const CLIENT_SUPPORTED_FEATURES: u64 = 140;

/// Payload type the device negotiates for HEVC video (from a captured Xcode
/// session). H.264 would come as PT 123; we don't depacketize that yet.
const HEVC_PAYLOAD_TYPE: u8 = 100;
/// H.264 RTP payload type on the tunnel display stream.
const AVC_PAYLOAD_TYPE: u8 = 123;

/// RTCP liveness period. The device stops the encoder after ~20-25s without
/// receiver reports; 5s leaves ample margin.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Per-frame ACK + RCTL receiver-control report period. Apple's Device Hub
/// sends both roughly every 50ms (see the `rtcp` module docs: AVConference's
/// encoder uses them for reference/rate management). Without them the encoder
/// can go quiet after the initial keyframe even while the screen keeps
/// changing — the receiver looks dead to it, and a PLI alone does not bring
/// it back.
const FEEDBACK_INTERVAL: Duration = Duration::from_millis(50);

/// No RTP for this long while the socket is alive → warn + keyframe request.
const STALL_WARN: Duration = Duration::from_secs(10);

/// How long backboardd needs after stream start before synthetic HID events
/// dispatch as authenticated (~300ms, to be verified by tunnel_hid_probe).
const AUTH_GATE_DELAY: Duration = Duration::from_millis(300);

/// Upper bound for tunnel + RSD + media-stream start.
const BUILD_TIMEOUT: Duration = Duration::from_secs(20);

/// Bound on the graceful `stop_media_stream` during teardown (the device is
/// often already gone when we stop).
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Frame channel depth (~1-2s of video); overflow drops frames.
const FRAME_CHANNEL_CAP: usize = 120;

/// A partially-assembled access unit is flushed when no further RTP has
/// arrived for this long. The stream is change-driven: while the screen is
/// static (or off) the device sends nothing for many seconds, so waiting for
/// the next frame's AUD delimiter would pin the first frame — and with it
/// the decoder configuration — indefinitely. A live frame's packets arrive
/// within milliseconds, so 500ms of silence means the picture is complete.
const FRAME_FLUSH_AFTER: Duration = Duration::from_millis(500);

/// How often the actor checks for a flushable stale access unit.
const FLUSH_TICK: Duration = Duration::from_millis(100);

/// The bridge's logical touch plane width/height (see [`crate::hid`]). Bridge
/// command coordinates live on this plane; HID reports on 0..=65535.
const LOGICAL_PLANE: f64 = 1920.0;

type StreamClient = DisplayServiceClient<Box<dyn ReadWrite>>;
type TouchClient = UniversalHidServiceClient<Box<dyn ReadWrite>>;
type IndigoClient = IndigoHidClient<Box<dyn ReadWrite>>;
type CaptureClient = ScreenCaptureServiceClient<Box<dyn ReadWrite>>;
type PasteClient = PasteboardServiceClient<Box<dyn ReadWrite>>;

// Re-exported so downstream crates (xphoneuse-mcp) can request capture formats
// without depending on idevice directly.
pub use idevice::core_device::ImageFormat;

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// One reassembled video access unit (Annex-B bytes) from the display stream.
#[derive(Debug)]
pub enum TunnelFrame {
    AccessUnit {
        /// Whether this access unit contains a keyframe (decoder sync point).
        key: bool,
        /// Annex-B start-code-prefixed NAL units.
        data: Vec<u8>,
        /// Stream-relative presentation time in microseconds (90kHz RTP clock
        /// rebased to the first frame).
        timestamp_us: u64,
    },
    /// The display's true framebuffer size, sent once before any access units.
    /// The encoder pads the stream to whole macroblocks (e.g. 1179x2556 →
    /// 1184x2576 with black borders right/bottom); odd widths cannot be
    /// expressed by H.264 SPS cropping, so the renderer crops decoder output
    /// to this size instead. Measured from a screencaptureservice PNG's IHDR.
    VisibleSize {
        width: u32,
        height: u32,
    },
}

/// A touch phase for [`HidVerb::Touch`] (mirrors the bridge's MDOWN/M/MUP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Down,
    Move,
    Up,
}

/// One input gesture for the tunnel transport. Coordinates are normalized
/// 0..=1 like every other input path in the app; [`HidVerb::Scroll`] carries
/// the bridge's logical-plane drag units (±500 per wheel tick).
#[derive(Debug, Clone)]
pub enum HidVerb {
    Tap { x: f64, y: f64 },
    Touch { action: TouchAction, x: f64, y: f64 },
    Swipe { x1: f64, y1: f64, x2: f64, y2: f64, duration_ms: u64 },
    /// Drag at screen center by `(dx, dy)` logical-plane units (the bridge
    /// `SCROLL` semantics; direction/scale to be verified on device).
    Scroll { dx: f64, dy: f64 },
    /// HID Keyboard/Keypad usage (e.g. 0x28 = Return).
    Key { usage: u64 },
    /// Text input: printable ASCII is typed key by key over HID; anything
    /// else is delivered via the device pasteboard + Cmd+V (see
    /// [`TunnelActor::paste_text`]).
    Text { text: String },
    /// Caller-level button name: home / back / recents / power / volume_up /
    /// volume_down.
    Button { button: String },
}

// ---------------------------------------------------------------------------
// Handle + per-udid registry
// ---------------------------------------------------------------------------

enum TunnelRequest {
    Input { verb: HidVerb, reply: oneshot::Sender<Result<(), String>> },
    Screenshot { format: ImageFormat, reply: oneshot::Sender<Result<Vec<u8>, String>> },
}

/// A running tunnel for one device. Cheap to clone via [`Arc`]; the actor
/// task dies with [`TunnelHandle::stop`] or when the last handle drops.
pub struct TunnelHandle {
    pub udid: String,
    req_tx: mpsc::UnboundedSender<TunnelRequest>,
    kill: Arc<Notify>,
    input_ready: Arc<AtomicBool>,
    frames_rx: Mutex<Option<mpsc::Receiver<TunnelFrame>>>,
    /// Milliseconds from `stream_epoch` to the last frame handed to the
    /// subscriber; 0 until one is. Shared with the actor.
    last_frame_ms: Arc<AtomicU64>,
    stream_epoch: Instant,
}

impl TunnelHandle {
    /// Start (or adopt an already-running) tunnel for `udid`.
    ///
    /// `want_frames == false` selects headless mode: the media stream still
    /// runs (it holds the HID auth gate open) but frames are not
    /// depacketized or forwarded.
    pub async fn start(udid: &str, want_frames: bool) -> Result<Arc<TunnelHandle>> {
        if let Some(existing) = active_tunnel(udid) {
            return Ok(existing);
        }

        let provider = crate::device::provider_for(udid).await?;

        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (frames_tx, frames_rx) = mpsc::channel(FRAME_CHANNEL_CAP);
        let (built_tx, built_rx) = oneshot::channel();
        let kill = Arc::new(Notify::new());
        let input_ready = Arc::new(AtomicBool::new(false));
        let last_frame_ms = Arc::new(AtomicU64::new(0));
        let stream_epoch = Instant::now();

        let handle = Arc::new(TunnelHandle {
            udid: udid.to_string(),
            req_tx: req_tx.clone(),
            kill: kill.clone(),
            input_ready: input_ready.clone(),
            frames_rx: Mutex::new(Some(frames_rx)),
            last_frame_ms: last_frame_ms.clone(),
            stream_epoch,
        });
        // Insert before spawning so a concurrent start() adopts this handle
        // instead of racing a second tunnel. The registry holds a Weak: a
        // dropped handle must let the actor die (its Drop wakes the actor)
        // instead of being pinned alive by the registry.
        registry().lock().unwrap().insert(udid.to_string(), Arc::downgrade(&handle));

        tokio::spawn(run_tunnel_actor(
            udid.to_string(),
            provider,
            kill.clone(),
            req_rx,
            input_ready,
            frames_tx,
            want_frames,
            built_tx,
            Arc::downgrade(&handle),
            last_frame_ms,
            stream_epoch,
        ));

        let outcome = tokio::time::timeout(BUILD_TIMEOUT, built_rx).await;
        match outcome {
            Ok(Ok(Ok(()))) => Ok(handle),
            Ok(Ok(Err(e))) => {
                remove_tunnel(udid, &handle);
                Err(anyhow!("{e}"))
            }
            Ok(Err(_)) => {
                remove_tunnel(udid, &handle);
                Err(anyhow!("tunnel actor exited during startup"))
            }
            Err(_) => {
                remove_tunnel(udid, &handle);
                kill.notify_one();
                Err(anyhow!("tunnel build timed out after {BUILD_TIMEOUT:?}"))
            }
        }
    }

    /// Whether the HID auth gate is open (stream up + gate delay elapsed).
    /// Input before this returns `true` is silently dropped by the device.
    pub fn input_ready(&self) -> bool {
        self.input_ready.load(Ordering::Relaxed)
    }

    /// How long ago the last video frame arrived, or `None` when this handle
    /// has never produced one - a headless (input-only) tunnel, or a session
    /// that has not started streaming.
    ///
    /// iOS stops sending video when the phone sleeps, so a growing age is the
    /// only signal the app has that the screen went off. It matters because
    /// the one HID key that touches the screen state is the power button, and
    /// power is a *toggle*: pressing it on an awake phone puts it to sleep.
    pub fn frame_age(&self) -> Option<Duration> {
        let last = self.last_frame_ms.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        Some(self.stream_epoch.elapsed().saturating_sub(Duration::from_millis(last)))
    }

    /// Execute one input gesture. Errors come back as strings on purpose —
    /// they surface directly in app toasts and MCP tool results.
    pub async fn input(&self, verb: HidVerb) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.req_tx
            .send(TunnelRequest::Input { verb, reply: reply_tx })
            .map_err(|_| "tunnel closed".to_string())?;
        reply_rx.await.map_err(|_| "tunnel closed".to_string())?
    }

    /// Take a one-shot screenshot over the tunnel (Xcode's capture path).
    pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.req_tx
            .send(TunnelRequest::Screenshot { format, reply: reply_tx })
            .map_err(|_| "tunnel closed".to_string())?;
        reply_rx.await.map_err(|_| "tunnel closed".to_string())?
    }

    /// Take the frame receiver out of this handle. There is exactly one
    /// subscriber (the app's video pipeline task); a second call is an error.
    pub async fn subscribe_frames(&self) -> Result<mpsc::Receiver<TunnelFrame>, String> {
        self.frames_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "frames already subscribed".to_string())
    }

    /// Stop the tunnel and forget it. Safe to call multiple times.
    pub fn stop(&self) {
        remove_tunnel(&self.udid, self);
        self.kill.notify_one();
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        // Last handle gone → wake the actor so it tears the tunnel down
        // instead of streaming into the void.
        if Arc::strong_count(&self.kill) == 1 {
            self.kill.notify_one();
        }
    }
}

/// Devices whose streaming session ended without a clean stop - the USB cable
/// was pulled, so `stopMediaStream` never reached the phone. iOS keeps the
/// screen-capture session alive (and with it the camera preview goes black)
/// until a `stopMediaStream {stopAll}` arrives over a live connection, so the
/// next time the device is reachable the app clears it.
static DIRTY_SESSIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
/// Where the pending-cleanup set is persisted (the app's data directory).
/// It has to survive a restart: the phone stays captured until a live
/// connection stops it, and the user may well reopen the app before
/// plugging the cable back in.
static STATE_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

fn dirty_sessions() -> &'static Mutex<HashSet<String>> {
    DIRTY_SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

const CLEANUP_FILE: &str = "pending-media-cleanup";

/// Point the pending-cleanup set at a directory and load what an earlier run
/// left there (one UDID per line).
pub fn set_state_dir(dir: std::path::PathBuf) {
    if let Ok(text) = std::fs::read_to_string(dir.join(CLEANUP_FILE)) {
        let mut set = dirty_sessions().lock().unwrap();
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                set.insert(line.to_string());
            }
        }
    }
    let _ = STATE_DIR.set(dir);
}

fn persist_dirty() {
    let Some(dir) = STATE_DIR.get() else { return };
    let set = dirty_sessions().lock().unwrap();
    let file = dir.join(CLEANUP_FILE);
    if set.is_empty() {
        let _ = std::fs::remove_file(&file);
        return;
    }
    let text: String = set.iter().map(|udid| format!("{udid}\n")).collect();
    let _ = std::fs::write(file, text);
}

fn set_dirty(udid: &str) {
    dirty_sessions().lock().unwrap().insert(udid.to_string());
    persist_dirty();
}

fn clear_dirty(udid: &str) {
    dirty_sessions().lock().unwrap().remove(udid);
    persist_dirty();
}

/// True when `udid` may still have a media session running on the device
/// after an interrupted connection. Cheap: callers poll this per device.
pub fn needs_cleanup(udid: &str) -> bool {
    dirty_sessions().lock().unwrap().contains(udid)
}

/// Stop whatever media session the device kept from an interrupted
/// connection. Returns whether a stop was actually sent - false when there is
/// nothing to clean, or when a live tunnel owns the device (stopping from a
/// second connection would kill the stream it is serving).
pub async fn cleanup_media_session(udid: &str) -> Result<bool, String> {
    if !needs_cleanup(udid) {
        return Ok(false);
    }
    if active_tunnel(udid).is_some() {
        return Ok(false);
    }
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| e.to_string())?;
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .map_err(|e| format!("CoreDeviceProxy: {e:?}"))?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|e| format!("software tunnel: {e:?}"))?
        .to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|e| format!("RSD connect: {e:?}"))?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|e| format!("RSD handshake: {e:?}"))?;
    let mut client = connect_display_service(&mut adapter, &handshake).await?;
    client
        .stop_media_stream()
        .await
        .map_err(|e| format!("stopMediaStream: {e:?}"))?;
    clear_dirty(udid);
    eprintln!("[tunnel] {udid}: cleared a media session left by an abrupt disconnect");
    Ok(true)
}

fn registry() -> &'static Mutex<HashMap<String, Weak<TunnelHandle>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<TunnelHandle>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The running tunnel for `udid`, if any.
pub fn active_tunnel(udid: &str) -> Option<Arc<TunnelHandle>> {
    registry()
        .lock()
        .unwrap()
        .get(udid)
        .and_then(|weak| weak.upgrade())
}

/// One-shot screenshot through `udid`'s running tunnel, or `None` when there
/// is no tunnel (caller falls back to DVT).
pub async fn tunnel_screenshot(udid: &str, format: ImageFormat) -> Option<Result<Vec<u8>, String>> {
    let t = active_tunnel(udid)?;
    Some(t.screenshot(format).await)
}

/// Remove `handle`'s registry entry, but only if it is still the current one
/// (a later start() may have replaced it).
fn remove_tunnel(udid: &str, handle: &TunnelHandle) {
    let mut reg = registry().lock().unwrap();
    let is_ours = reg
        .get(udid)
        .and_then(|weak| weak.upgrade())
        .map(|cur| Arc::as_ptr(&cur) == handle as *const TunnelHandle)
        .unwrap_or(false);
    if is_ours {
        reg.remove(udid);
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
/// The display's true framebuffer size, via one screencaptureservice PNG.
///
/// The display stream encodes macroblock-aligned dimensions with black
/// padding on the right and bottom (the real 1179-pixel width is odd and
/// cannot be recovered via SPS cropping), so the renderer needs the exact
/// visible size to crop the borders away.
async fn probe_screen_size(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> Result<(u32, u32), String> {
    // Retried, because one failure costs the whole session its crop: with no
    // visible size the renderer cannot crop, and the encoder's macroblock
    // padding stays on screen as black borders down the right and bottom edges.
    // The padding is real and uneven - decoded off the wire on the 1179x2556
    // test device (2026-10-10): content 1178x2556 inside a coded 1184x2576,
    // i.e. 6 pixels on the right (the encoder drops the screen's odd last
    // column) and 20 at the bottom, with no SPS frame_cropping to recover
    // either number. A reconnect is exactly when the probe races something -
    // the previous session's capture client may still be holding
    // screencaptureservice - so a second attempt a moment later is worth the
    // delay.
    const ATTEMPTS: u32 = 3;
    let mut last = String::from("size probe was never attempted");
    for attempt in 1..=ATTEMPTS {
        match probe_screen_size_once(adapter, handshake).await {
            Ok(size) => return Ok(size),
            Err(e) => {
                eprintln!("[tunnel] size probe attempt {attempt}/{ATTEMPTS} failed: {e}");
                last = e;
                if attempt < ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            }
        }
    }
    Err(last)
}

async fn probe_screen_size_once(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> Result<(u32, u32), String> {
    let stream = open_rsd_stream(adapter, handshake, SVC_SCREEN_CAPTURE).await?;
    let mut client = CaptureClient::from_stream(stream)
        .await
        .map_err(|e| format!("screencaptureservice handshake: {e:?}"))?;
    let png = client
        .take_screenshot(None, ImageFormat::Png)
        .await
        .map_err(|e| format!("size probe take_screenshot: {e:?}"))?;
    parse_png_size(&png).ok_or_else(|| "size probe: not a PNG with an IHDR".to_string())
}

/// Width/height out of a PNG's IHDR chunk (bytes 16..24) — no decoding.
fn parse_png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

async fn run_tunnel_actor(
    udid: String,
    provider: Box<dyn IdeviceProvider>,
    kill: Arc<Notify>,
    mut req_rx: mpsc::UnboundedReceiver<TunnelRequest>,
    input_ready: Arc<AtomicBool>,
    frames_tx: mpsc::Sender<TunnelFrame>,
    want_frames: bool,
    built_tx: oneshot::Sender<std::result::Result<(), String>>,
    weak: Weak<TunnelHandle>,
    last_frame_ms: Arc<AtomicU64>,
    stream_epoch: Instant,
) {
    let build = async {
        // USB → lockdown → CDTunnel. Failing here almost always means
        // Developer Mode is off, iOS < 17, or the device just unplugged.
        let proxy = CoreDeviceProxy::connect(provider.as_ref())
            .await
            .map_err(|e| format!("CoreDeviceProxy: {e:?} (Developer Mode off? iOS 17+ required)"))?;
        let rsd_port = proxy.tunnel_info().server_rsd_port;
        let mut adapter = proxy
            .create_software_tunnel()
            .map_err(|e| format!("software tunnel: {e:?}"))?
            .to_async_handle();
        let stream = adapter
            .connect(rsd_port)
            .await
            .map_err(|e| format!("RSD connect: {e:?}"))?;
        let mut handshake = RsdHandshake::new(stream)
            .await
            .map_err(|e| format!("RSD handshake: {e:?}"))?;
        // True screen size before the stream starts: measured now, an IHDR
        // parse is cheap, and doing it after start_media_stream would block
        // the actor's RTP drain for the whole capture.
        let screen_size = if want_frames {
            let t0 = Instant::now();
            let r = probe_screen_size(&mut adapter, &handshake).await;
            match &r {
                Ok((w, h)) => eprintln!(
                    "[tunnel] {udid}: screen {}x{} (probe {:?})",
                    w,
                    h,
                    t0.elapsed()
                ),
                Err(e) => eprintln!("[tunnel] {udid}: size probe failed ({e}) — no crop"),
            }
            r.ok()
        } else {
            None
        };
        let session = start_media_stream(&mut adapter, &mut handshake).await?;
        Ok((adapter, handshake, session, screen_size))
    }
    .await;

    let (mut adapter, mut handshake, session, screen_size) = match build {
        Ok(v) => v,
        Err(e) => {
            input_ready.store(false, Ordering::Relaxed);
            if let Some(handle) = weak.upgrade() {
                remove_tunnel(&udid, &handle);
            }
            let _ = built_tx.send(Err(e));
            return;
        }
    };

    // Deliver the visible size before the main loop starts emitting access
    // units, so the decoder config can carry the crop from the very start.
    if let Some((width, height)) = screen_size {
        let _ = frames_tx.send(TunnelFrame::VisibleSize { width, height }).await;
    }

    // Hold the auth gate: once the stream is up, backboardd needs a moment
    // before it accepts our HID reports as authenticated.
    {
        let gate = input_ready.clone();
        tokio::spawn(async move {
            tokio::time::sleep(AUTH_GATE_DELAY).await;
            gate.store(true, Ordering::Relaxed);
        });
    }

    let mut actor = TunnelActor {
        adapter: &mut adapter,
        handshake: &mut handshake,
        stream: session.client,
        audio_udp: session.audio_udp,
        video_udp: session.video_udp,
        our_ssrc: session.our_ssrc,
        want_frames,
        frames_tx,
        last_frame_ms,
        stream_epoch,
        depack: None,
        pending: Vec::new(),
        pending_ts: None,
        device_rtp_port: None,
        device_ssrc: None,
        last_sr: None,
        last_rtp_at: Instant::now(),
        last_seq: 0,
        seen_marker: false,
        drop_until_marker: false,
        drop_next_au: false,
        ts_base: None,
        fir_seq: 0,
        frame_acks: Vec::new(),
        frames_since_report: 0,
        packets: 0,
        frames: 0,
        dropped_frames: 0,
        stall_reported: false,
        touch: None,
        indigo: None,
        capture: None,
        paste: None,
    };

    let _ = built_tx.send(Ok(()));
    eprintln!("[tunnel] {udid}: stream up, entering main loop");

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut flush_tick = tokio::time::interval(FLUSH_TICK);
    flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Starts after the stream is up, so the first report carries real state.
    let mut feedback_tick = tokio::time::interval(FEEDBACK_INTERVAL);
    feedback_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            req = req_rx.recv() => {
                let Some(req) = req else { break };
                match req {
                    TunnelRequest::Input { verb, reply } => {
                        let _ = reply.send(actor.execute(verb).await);
                    }
                    TunnelRequest::Screenshot { format, reply } => {
                        let _ = reply.send(actor.screenshot(format).await);
                    }
                }
            }
            _ = kill.notified() => break,
            dg = actor.video_udp.recv() => {
                match dg {
                    Ok(dg) => actor.on_video_datagram(dg.data, dg.source_port),
                    Err(e) => {
                        eprintln!("[tunnel] {udid}: video socket closed: {e}");
                        break;
                    }
                }
            }
            dg = actor.audio_udp.recv() => {
                // Audio RTP is always discarded (drain keeps jktcp's
                // buffering bounded; the port must stay bound regardless).
                let _ = dg;
            }
            _ = flush_tick.tick() => {
                actor.flush_stale_pending();
            }
            _ = keepalive.tick() => {
                actor.send_keepalive().await;
            }
            _ = feedback_tick.tick() => {
                actor.send_frame_feedback().await;
            }
        }
    }

    // Teardown: stop the stream best-effort, then wake anyone watching us.
    input_ready.store(false, Ordering::Relaxed);
    if let Some(handle) = weak.upgrade() {
        remove_tunnel(&udid, &handle);
    }
    // Mark before the attempt: when the cable has already been pulled, the
    // stop cannot reach the phone and the session (camera lock included)
    // survives there until cleanup_media_session runs over a live link. A
    // successful stop means there is nothing left to clean.
    set_dirty(&udid);
    if matches!(
        tokio::time::timeout(STOP_TIMEOUT, actor.stream.stop_media_stream()).await,
        Ok(Ok(_))
    ) {
        clear_dirty(&udid);
    }
    eprintln!(
        "[tunnel] {udid}: stopped after {} packets, {} frames ({} dropped)",
        actor.packets, actor.frames, actor.dropped_frames
    );
}

/// A started displayservice media session plus the sockets its ports are
/// bound to (dropping the sockets unbinds the ports and kills the stream).
struct MediaSession {
    client: StreamClient,
    audio_udp: UdpSocketHandle,
    video_udp: UdpSocketHandle,
    our_ssrc: u32,
}

// ---------------------------------------------------------------------------
// RSD service connection (bypassing RsdService::connect_rsd)
// ---------------------------------------------------------------------------

/// What a tunnel start says when the device is not offering one of the
/// services above. That is its own failure: those services come from the
/// mounted developer image, so the caller can fix it by mounting a current
/// one (see [`crate::developer_image`]) rather than giving up on the tunnel.
pub const MISSING_SERVICE_MARK: &str = "device does not offer ";

/// True when a tunnel error (formatted with `{:#}`) is that case.
pub fn is_missing_service_error(error: &str) -> bool {
    error.contains(MISSING_SERVICE_MARK)
}

const SVC_DISPLAY: &str = "com.apple.coredevice.displayservice";
const SVC_UNIVERSAL_HID: &str = "com.apple.coredevice.hid.universalhidservice";
const SVC_INDIGO_HID: &str = "com.apple.coredevice.hid.indigo";
const SVC_SCREEN_CAPTURE: &str = "com.apple.coredevice.screencaptureservice";
const SVC_PASTEBOARD: &str = "com.apple.coredevice.pasteboardservice";

/// Everything the tunnel is built on, and everything the mount is expected to
/// bring with it.
const NEEDED_SERVICES: [&str; 5] = [
    SVC_DISPLAY,
    SVC_UNIVERSAL_HID,
    SVC_INDIGO_HID,
    SVC_SCREEN_CAPTURE,
    SVC_PASTEBOARD,
];

/// Whether the device publishes the services the tunnel needs, without
/// starting a tunnel of its own.
///
/// The same question a tunnel start answers by failing (`MISSING_SERVICE_MARK`),
/// asked up front by callers that need it before deciding what to start - the
/// app asks it before opening a mirror whose transport will *not* be the
/// tunnel, where nothing else would notice the image is missing until a
/// screenshot came up short.
///
/// `Ok(false)` is the missing (or stale) developer image; every other failure
/// comes back as `Err`, so a device that is merely unplugged or locked is not
/// mistaken for one that needs work.
pub async fn services_ready(udid: &str) -> Result<bool, String> {
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| format!("{udid}: {e}"))?;
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .map_err(|e| format!("CoreDeviceProxy: {e:?}"))?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|e| format!("software tunnel: {e:?}"))?
        .to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|e| format!("RSD connect: {e:?}"))?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|e| format!("RSD handshake: {e:?}"))?;
    Ok(NEEDED_SERVICES
        .iter()
        .all(|service| handshake.services.contains_key(*service)))
}

/// Open a stream to one CoreDevice service on the existing tunnel. Bypasses
/// the `RsdService::connect_rsd` trait method: its RPITIT future is not
/// Send-bounded, which fails tokio::spawn's Send check ("implementation is
/// not general enough") — same workaround as screenshot.rs's
/// `build_dvt_server`.
pub(crate) async fn open_rsd_stream(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
    service: &str,
) -> Result<Box<dyn ReadWrite>, String> {
    let port = handshake
        .services
        .get(service)
        .map(|s| s.port)
        .ok_or_else(|| format!("{MISSING_SERVICE_MARK}{service}{}", what_it_does_offer(handshake)))?;
    let stream = adapter
        .connect(port)
        .await
        .map_err(|e| format!("connect {service} (port {port}): {e:?}"))?;
    Ok(Box::new(stream) as Box<dyn ReadWrite>)
}

/// What the device did offer, appended to "device does not offer <service>".
///
/// The two failure shapes want different fixes and look identical without
/// this: a device with no CoreDevice services at all is locked, unpaired, or
/// had Developer Mode reset (an OS update does that), while a device that
/// lists them but not the one asked for has renumbered or renamed something
/// and the app has to follow.
fn what_it_does_offer(handshake: &RsdHandshake) -> String {
    let mut coredevice: Vec<&str> = handshake
        .services
        .keys()
        .map(String::as_str)
        .filter(|name| name.starts_with("com.apple.coredevice."))
        .collect();
    coredevice.sort_unstable();
    if coredevice.is_empty() {
        return format!(
            " — RSD listed {} services, none of them com.apple.coredevice.*",
            handshake.services.len()
        );
    }
    format!(
        " — RSD listed {} services, among them {}",
        handshake.services.len(),
        coredevice.join(", ")
    )
}

/// Connect the displayservice control-plane client.
async fn connect_display_service(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> Result<StreamClient, String> {
    let stream = open_rsd_stream(adapter, handshake, SVC_DISPLAY).await?;
    let core = idevice::core_device::CoreDeviceServiceClient::new(stream)
        .await
        .map_err(|e| format!("displayservice handshake: {e:?}"))?;
    Ok(StreamClient::new(core))
}

/// Connect a raw RemoteXPC service (the two HID services).
async fn connect_xpc_service(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
    service: &str,
) -> Result<RemoteXpcClient<Box<dyn ReadWrite>>, String> {
    let stream = open_rsd_stream(adapter, handshake, service).await?;
    let mut xpc = RemoteXpcClient::new(stream)
        .await
        .map_err(|e| format!("{service} handshake: {e:?}"))?;
    xpc.do_handshake()
        .await
        .map_err(|e| format!("{service} do_handshake: {e:?}"))?;
    Ok(xpc)
}

/// Audio first, then video on the same clientSessionID — the DeviceHub
/// pattern the device expects (verified in tools/src/coredevice_stream.rs).
async fn start_media_stream(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> Result<MediaSession, String> {
    let mut client = connect_display_service(adapter, handshake).await?;

    // Clear anything the device still holds from a previous host or an
    // interrupted connection before starting ours (`stopAll`): a leftover
    // screen-capture session blocks the camera and can make the new
    // startMediaStream fail. Best effort - a fresh connection with no
    // session may answer with an error.
    if let Err(e) = client.stop_media_stream().await {
        eprintln!("[tunnel] pre-start stopMediaStream (best effort): {e:?}");
    }

    let audio_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(audio): {e:?}"))?;
    let video_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(video): {e:?}"))?;
    let receiver_ip = adapter.host_ip().to_string();
    let sender_ip = adapter.peer_ip().to_string();

    // VCCallInfoBlob describing this (host) endpoint — mirrors a captured
    // Device Hub offer the device accepts.
    let call_info = CallInfoBlob {
        call_id: 0,
        client_version: 1,
        device_type: "Mac17,7".into(),
        framework_version: "2205.3.1".into(),
        os_version: "25F71".into(),
        device_name: None,
        audio_device_uid: None,
    };

    let client_session_id = uuid::Uuid::new_v4();

    // Audio first to establish the screen-sharing session.
    let audio_call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let audio_offer =
        build_screen_audio_offer(&audio_call_id, &call_info).map_err(|e| format!("audio offer: {e:?}"))?;
    let audio_params = build_start_audio_parameters(
        &receiver_ip,
        audio_udp.local_port(),
        &sender_ip,
        50000,
        audio_offer,
        CLIENT_SUPPORTED_FEATURES,
        client_session_id,
    );
    client
        .start_media_stream(audio_params)
        .await
        .map_err(|e| format!("audio startMediaStream: {e:?}"))?;

    // Video on the same session. H.264 only: the Windows HEVC decode path
    // (MS extensions / DXVA) clips pictures taller than 720 rows to a default
    // 1280x720 output — broken beyond repair from our side — while H.264
    // decodes at full height in hardware AND software (Chromium's built-in
    // ffmpeg fallback), with no extension install required.
    let call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let our_ssrc = uuid::Uuid::new_v4().as_u128() as u32;
    let offer = build_screen_video_offer_with_codec(&call_id, &call_info, our_ssrc, ScreenVideoCodec::H264)
        .map_err(|e| format!("video offer: {e:?}"))?;
    let params = build_start_video_parameters(
        &receiver_ip,
        video_udp.local_port(),
        &sender_ip,
        50001,
        offer,
        CLIENT_SUPPORTED_FEATURES,
        1,
        client_session_id,
    );
    client
        .start_media_stream(params)
        .await
        .map_err(|e| format!("video startMediaStream: {e:?} (mediastream daemon wedged? reboot the device)"))?;

    Ok(MediaSession { client, audio_udp, video_udp, our_ssrc })
}

/// State owned by the actor's main loop, borrowed from the enclosing future.
struct TunnelActor<'a> {
    adapter: &'a mut AdapterHandle,
    handshake: &'a mut RsdHandshake,
    stream: StreamClient,
    audio_udp: UdpSocketHandle,
    video_udp: UdpSocketHandle,
    our_ssrc: u32,
    want_frames: bool,
    frames_tx: mpsc::Sender<TunnelFrame>,
    last_frame_ms: Arc<AtomicU64>,
    stream_epoch: Instant,
    /// Created from the first RTP packet's payload type (100 = HEVC,
    /// 123 = H.264 — the offer sends only the AVC bank, but stay adaptive).
    depack: Option<VideoDepack>,
    /// Depacketizer output accumulated until the next AUD NAL marks the end
    /// of a picture — `take_output()` drains whatever bytes are ready, which
    /// is only part of a frame mid-picture.
    pending: Vec<u8>,
    /// RTP timestamp of the first packet that contributed to `pending`.
    pending_ts: Option<u32>,
    device_rtp_port: Option<u16>,
    device_ssrc: Option<u32>,
    last_sr: Option<SenderReport>,
    last_rtp_at: Instant,
    last_seq: u16,
    ts_base: Option<u32>,
    /// Set once an RTP packet with the marker bit has been seen — switches
    /// AU cutting to the marker (and disables the mid-picture stale flush).
    seen_marker: bool,
    /// RTP gap seen: discard everything up to and including the next marker.
    drop_until_marker: bool,
    /// RTP gap on a stream without markers: the picture the gap sat in is
    /// corrupt; the next AUD-split access unit (its remainder) is dropped.
    drop_next_au: bool,
    fir_seq: u8,
    /// RTP timestamps of completed pictures not yet acknowledged in an APP
    /// frame-ack packet (drained on the feedback tick).
    frame_acks: Vec<u32>,
    /// Pictures completed since the last RCTL report.
    frames_since_report: u16,
    packets: u64,
    frames: u64,
    dropped_frames: u64,
    stall_reported: bool,
    touch: Option<TouchClient>,
    indigo: Option<IndigoClient>,
    capture: Option<CaptureClient>,
    /// Lazily connected pasteboardservice client (non-ASCII text input).
    paste: Option<PasteClient>,
}

impl TunnelActor<'_> {
    fn on_video_datagram(&mut self, data: Vec<u8>, source_port: u16) {
        if is_rtcp(&data) {
            if let Some(sr) = SenderReport::parse_first(&data) {
                self.last_sr = Some(sr);
            }
            return;
        }
        let Some(rtp) = RtpPacket::parse(&data) else { return };
        self.packets += 1;
        self.last_rtp_at = Instant::now();
        let prev_seq = self.last_seq;
        self.last_seq = rtp.sequence_number;
        if self.stall_reported {
            self.stall_reported = false;
        }
        if self.device_rtp_port.is_none() {
            self.device_rtp_port = Some(source_port);
            self.device_ssrc = Some(rtp.ssrc);
            let name = match rtp.payload_type {
                HEVC_PAYLOAD_TYPE => "HEVC",
                AVC_PAYLOAD_TYPE => "H.264",
                _ => "unknown",
            };
            eprintln!(
                "[tunnel] first RTP: pt={} ({name}) ssrc={:08x} from :{source_port}",
                rtp.payload_type, rtp.ssrc,
            );
        }
        if !self.want_frames {
            return;
        }
        if self.depack.is_none() {
            self.depack = match rtp.payload_type {
                HEVC_PAYLOAD_TYPE => Some(VideoDepack::Hevc(HevcDepacketizer::new())),
                AVC_PAYLOAD_TYPE => Some(VideoDepack::Avc(AvcDepacketizer::new())),
                other => {
                    if self.frames == 0 && self.packets % 200 == 1 {
                        eprintln!("[tunnel] payload type {other}: tunnel video disabled");
                    }
                    return;
                }
            };
        }
        // RTP gap: everything in flight is stitched from non-contiguous
        // payloads. VideoToolbox renders missing slices as artifacts instead
        // of throwing, and the corruption propagates through every subsequent
        // delta until the next IDR — so drop the whole picture the gap sits
        // in and resync the depacketizer past the skipped sequence numbers.
        // On a marker-bearing stream everything up to and including the next
        // marker is dropped; without markers the corrupt picture's remainder
        // is dropped at the next AUD boundary instead.
        if prev_seq != 0 && rtp.sequence_number != prev_seq.wrapping_add(1) {
            if !self.drop_until_marker && !self.drop_next_au {
                eprintln!(
                    "[tunnel] RTP gap {} -> {}: dropping picture",
                    prev_seq, rtp.sequence_number
                );
            }
            self.pending.clear();
            self.pending_ts = None;
            if self.seen_marker {
                self.drop_until_marker = true;
            } else {
                self.drop_next_au = true;
                if let Some(d) = self.depack.as_mut() {
                    d.resync(rtp.sequence_number);
                }
            }
        }
        if self.drop_until_marker {
            if rtp.marker {
                self.drop_until_marker = false;
                if let Some(d) = self.depack.as_mut() {
                    d.resync(rtp.sequence_number.wrapping_add(1));
                }
            }
            return;
        }
        let out = match self.depack.as_mut() {
            Some(d) => {
                d.push(rtp.sequence_number, rtp.timestamp, rtp.payload);
                d.take_output()
            }
            None => return,
        };
        if out.is_empty() {
            return;
        }
        if self.pending.is_empty() {
            self.pending_ts = Some(rtp.timestamp);
        }
        self.pending.extend_from_slice(&out);
        if rtp.marker {
            // The marker bit closes each picture — the authoritative
            // access-unit boundary (the same signal pymobiledevice3 frames
            // on). Emitting here means a static screen's initial IDR goes
            // out as soon as it is complete, without waiting for the next
            // frame's AUD.
            self.seen_marker = true;
            let au_ts = self.pending_ts.take().unwrap_or(rtp.timestamp);
            let au = std::mem::take(&mut self.pending);
            self.emit_access_unit(au, au_ts);
            return;
        }
        // Emit only COMPLETE pictures: the depacketizer writes an AUD NAL
        // before each new picture's NALs, so an AUD that isn't the first NAL
        // closes the picture before it. Feeding a WebCodecs decoder partial
        // pictures as separate chunks breaks decoding.
        let hevc = self.depack.as_ref().is_some_and(|d| d.is_hevc());
        while let Some(au) = split_access_unit(&mut self.pending, hevc) {
            // What remains starts the next picture, whose timestamp is the
            // boundary-triggering packet's.
            let au_ts = self.pending_ts.replace(rtp.timestamp).unwrap_or(rtp.timestamp);
            if self.drop_next_au {
                self.drop_next_au = false;
                continue;
            }
            self.emit_access_unit(au, au_ts);
        }
    }

    /// Flush a partially-assembled access unit when the RTP flow has gone
    /// quiet (static screen): the picture is complete, but no next-frame AUD
    /// will ever arrive to close it. Only used on streams that never set the
    /// RTP marker bit — a marker-bearing stream is always cut at the marker,
    /// and a mid-picture silence there is packet loss, handled by dropping
    /// until the next marker instead of emitting a truncated picture.
    fn flush_stale_pending(&mut self) {
        if self.seen_marker {
            return;
        }
        if self.pending.is_empty() || self.last_rtp_at.elapsed() < FRAME_FLUSH_AFTER {
            return;
        }
        let au = std::mem::take(&mut self.pending);
        let ts = self.pending_ts.take().unwrap_or(0);
        self.emit_access_unit(au, ts);
    }

    /// Send one complete access unit to the frame subscriber.
    fn emit_access_unit(&mut self, mut au: Vec<u8>, au_ts: u32) {
        strip_apple_trailer(&mut au);
        if au.is_empty() {
            return;
        }
        let hevc = self.depack.as_ref().is_some_and(|d| d.is_hevc());
        if self.frames == 0 {
            eprintln!(
                "[tunnel] first frame emitted: {} bytes, key={}",
                au.len(),
                annexb_contains_key(&au, hevc)
            );
        }
        let base = *self.ts_base.get_or_insert(au_ts);
        let timestamp_us = (au_ts.wrapping_sub(base) as u64) * 100_000 / 9;
        let key = annexb_contains_key(&au, hevc);
        self.frames += 1;
        self.frame_acks.push(au_ts);
        self.frames_since_report = self.frames_since_report.saturating_add(1);
        self.last_frame_ms
            .store(self.stream_epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let frame = TunnelFrame::AccessUnit { key, data: au, timestamp_us };
        if self.frames_tx.try_send(frame).is_err() {
            // Subscriber gone or too slow: drop the frame, keep the stream.
            self.dropped_frames += 1;
        }
    }

    async fn send_keepalive(&mut self) {
        if let Some(port) = self.device_rtp_port {
            let blocks = [ReportBlock {
                source_ssrc: self.device_ssrc.unwrap_or(0),
                highest_seq: self.last_seq as u32,
                lsr: self.last_sr.map(|s| s.ntp_middle).unwrap_or(0),
                ..Default::default()
            }];
            let pkt = build_liveness(self.our_ssrc, "xphoneuse@tunnel", &blocks);
            if let Err(e) = self.video_udp.send_to(port, pkt).await {
                eprintln!("[tunnel] keepalive send failed: {e}");
            }
        }
        let idle = self.last_rtp_at.elapsed();
        if idle > STALL_WARN {
            if !self.stall_reported {
                eprintln!("[tunnel] no RTP for {idle:?} — requesting keyframe");
                self.stall_reported = true;
            }
            if let Some(port) = self.device_rtp_port {
                let blocks = [ReportBlock {
                    source_ssrc: self.device_ssrc.unwrap_or(0),
                    highest_seq: self.last_seq as u32,
                    lsr: self.last_sr.map(|s| s.ntp_middle).unwrap_or(0),
                    ..Default::default()
                }];
                let pkt = build_keyframe_request(
                    self.our_ssrc,
                    "xphoneuse@tunnel",
                    self.device_ssrc.unwrap_or(0),
                    &blocks,
                    self.fir_seq,
                );
                self.fir_seq = self.fir_seq.wrapping_add(1);
                let _ = self.video_udp.send_to(port, pkt).await;
            }
        }
    }

    /// Per-frame APP acks + the periodic `RCTL` receiver-control report, at
    /// the cadence Apple's Device Hub uses (~50ms). The encoder reads these as
    /// "a receiver is still decoding"; without them a burst of new screen
    /// content can be followed by total silence even though the RTP path and
    /// the tunnel are healthy.
    async fn send_frame_feedback(&mut self) {
        // An input-only tunnel doesn't decode, so it has nothing to
        // acknowledge; leave that session's traffic exactly as it was.
        if !self.want_frames {
            return;
        }
        let Some(port) = self.device_rtp_port else { return };
        for ts in self.frame_acks.drain(..) {
            let ack = build_frame_ack(self.our_ssrc, ts);
            if self.video_udp.send_to(port, ack).await.is_err() {
                break;
            }
        }
        let clock_ms = (self.stream_epoch.elapsed().as_millis() & 0xffff) as u16;
        let frames = std::mem::take(&mut self.frames_since_report);
        let report = build_rctl(self.our_ssrc, clock_ms, frames, self.last_seq);
        let _ = self.video_udp.send_to(port, report).await;
    }

    // --- lazy service clients ---------------------------------------------

    async fn touch(&mut self) -> Result<&mut TouchClient, String> {
        if self.touch.is_none() {
            let xpc = connect_xpc_service(self.adapter, self.handshake, SVC_UNIVERSAL_HID).await?;
            self.touch = Some(TouchClient::new(xpc));
        }
        Ok(self.touch.as_mut().unwrap())
    }

    async fn indigo(&mut self) -> Result<&mut IndigoClient, String> {
        if self.indigo.is_none() {
            let xpc = connect_xpc_service(self.adapter, self.handshake, SVC_INDIGO_HID).await?;
            self.indigo = Some(IndigoClient::new(xpc));
        }
        Ok(self.indigo.as_mut().unwrap())
    }

    // --- input ------------------------------------------------------------

    async fn execute(&mut self, verb: HidVerb) -> Result<(), String> {
        match verb {
            HidVerb::Tap { x, y } => {
                let (x, y) = norm_to_u16(x, y);
                self.touch().await?.tap(x, y).await.map_err(idevice_err)
            }
            HidVerb::Touch { action, x, y } => {
                let (x, y) = norm_to_u16(x, y);
                let state = match action {
                    TouchAction::Down | TouchAction::Move => TOUCHSCREEN_STATE_CONTACT,
                    TouchAction::Up => TOUCHSCREEN_STATE_RELEASE,
                };
                self.touch().await?.send_touchscreen(state, x, y, None).await.map_err(idevice_err)
            }
            HidVerb::Swipe { x1, y1, x2, y2, duration_ms } => {
                let (x1, y1) = norm_to_u16(x1, y1);
                let (x2, y2) = norm_to_u16(x2, y2);
                // 30 steps x 20ms (~600ms) reads as a deliberate drag; scale
                // the step count with the requested duration.
                let steps = ((duration_ms.max(80)) / 20).clamp(2, 100) as u32;
                self.touch().await?.drag(x1, y1, x2, y2, steps, 20).await.map_err(idevice_err)
            }
            HidVerb::Scroll { dx, dy } => {
                // Bridge semantics: a drag by (dx, dy) logical units from
                // wherever the gesture starts; wheel scrolls start centered.
                let (x1, y1) = norm_to_u16(0.5, 0.5);
                let (x2, y2) = norm_to_u16(0.5 + dx / LOGICAL_PLANE, 0.5 + dy / LOGICAL_PLANE);
                self.touch().await?.drag(x1, y1, x2, y2, 8, 16).await.map_err(idevice_err)
            }
            HidVerb::Key { usage } => {
                let indigo = self.indigo().await?;
                type_key(indigo, usage, false).await.map_err(idevice_err)
            }
            HidVerb::Text { text } => {
                // The HID keyboard only carries printable ASCII (same as the
                // BLE bridge). Non-ASCII text instead goes through the device
                // pasteboard — set it, then press Cmd+V — which delivers
                // arbitrary UTF-8 into the focused field without AgentInput.
                if !text.is_ascii() {
                    return self.paste_text(&text).await;
                }
                let indigo = self.indigo().await?;
                for ch in text.chars() {
                    if let Some((usage, shift)) = ascii_to_usage(ch) {
                        type_key(indigo, usage, shift).await.map_err(idevice_err)?;
                    }
                    // Unsupported characters are dropped, matching the
                    // bridge's map_text behavior.
                }
                Ok(())
            }
            HidVerb::Button { button } => self.press_button(&button).await,
        }
    }

    async fn press_button(&mut self, button: &str) -> Result<(), String> {
        match button {
            // (usage_page, usage, hold_ms) — same table as tools/src/hid.rs.
            "home" => self.send_button(0x0C, 0x40, 80).await,
            "power" | "lock" | "sleep" => self.send_button(0x0C, 0x30, 200).await,
            "volume_up" => self.send_button(0x0C, 0xE9, 80).await,
            "volume_down" => self.send_button(0x0C, 0xEA, 80).await,
            "recents" => self.app_switcher_gesture().await,
            // iOS has no Back key; the system-wide back gesture is an edge
            // swipe toward the center.
            "back" => {
                let (x1, y1) = norm_to_u16(0.02, 0.5);
                let (x2, y2) = norm_to_u16(0.5, 0.5);
                self.touch().await?.drag(x1, y1, x2, y2, 10, 30).await.map_err(idevice_err)
            }
            other => Err(format!("button '{other}' not available over the tunnel")),
        }
    }

    async fn send_button(&mut self, usage_page: u64, usage: u64, hold_ms: u64) -> Result<(), String> {
        let indigo = self.indigo().await?;
        indigo.send_button(usage_page, usage, ButtonState::Down).await.map_err(idevice_err)?;
        tokio::time::sleep(Duration::from_millis(hold_ms)).await;
        indigo.send_button(usage_page, usage, ButtonState::Up).await.map_err(idevice_err)
    }

    /// The app-switcher system gesture, mirroring [`crate::hid::app_switcher_gesture`]:
    /// a slow drag up from the bottom edge that pauses mid-screen before
    /// releasing. Verified against an iPhone (iOS 26): the touch must start
    /// essentially AT the edge and travel well up the screen.
    async fn app_switcher_gesture(&mut self) -> Result<(), String> {
        // Same logical-plane coordinates as the bridge version.
        const X: f64 = 960.0;
        const Y_START: f64 = 1915.0;
        const Y_END: f64 = 800.0;
        const STEPS: u32 = 12;

        let (x, y_start) = norm_to_u16(X / LOGICAL_PLANE, Y_START / LOGICAL_PLANE);
        let (_, y_end) = norm_to_u16(X / LOGICAL_PLANE, Y_END / LOGICAL_PLANE);
        let touch = self.touch().await?;
        touch
            .send_touchscreen(TOUCHSCREEN_STATE_CONTACT, x, y_start, None)
            .await
            .map_err(idevice_err)?;
        tokio::time::sleep(Duration::from_millis(80)).await;
        for i in 1..=STEPS {
            let t = i as f64 / STEPS as f64;
            let y = (y_start as f64 + (y_end as f64 - y_start as f64) * t).round() as u16;
            touch
                .send_touchscreen(TOUCHSCREEN_STATE_CONTACT, x, y, None)
                .await
                .map_err(idevice_err)?;
            tokio::time::sleep(Duration::from_millis(70)).await;
        }
        // The pause at mid-screen is what makes iOS open the switcher
        // instead of completing the swipe to the home screen.
        tokio::time::sleep(Duration::from_millis(900)).await;
        touch
            .send_touchscreen(TOUCHSCREEN_STATE_RELEASE, x, y_end, None)
            .await
            .map_err(idevice_err)
    }

    // --- pasteboard (non-ASCII text input) ---------------------------------

    /// Write `text` to the device pasteboard, then press Cmd+V so it lands in
    /// the focused field. This is the tunnel's path for text the HID keyboard
    /// cannot type: the pasteboard carries arbitrary UTF-8, and a GUI+V
    /// keystroke pastes it exactly like a user would (no paste-permission
    /// prompt — that only fires when an app *reads* the pasteboard).
    async fn paste_text(&mut self, text: &str) -> Result<(), String> {
        if self.paste.is_none() {
            let stream = open_rsd_stream(self.adapter, self.handshake, SVC_PASTEBOARD).await?;
            // from_stream is the Send-bounded public constructor (same reason
            // as the screenshot client).
            let client = PasteClient::from_stream(stream)
                .await
                .map_err(|e| format!("pasteboardservice handshake: {e:?}"))?;
            self.paste = Some(client);
        }
        let set = { self.paste.as_mut().unwrap().set_text(text, GENERAL_PASTEBOARD).await };
        if let Err(e) = set {
            // Drop the possibly-wedged client; the next paste reconnects.
            self.paste = None;
            return Err(format!("pasteboard SET: {e:?}"));
        }
        // Give the pasteboard a beat to land before the keystroke reads it.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let indigo = self.indigo().await?;
        paste_keystroke(indigo).await.map_err(idevice_err)
    }

    // --- screenshots ------------------------------------------------------

    async fn screenshot(&mut self, format: ImageFormat) -> Result<Vec<u8>, String> {
        if self.capture.is_none() {
            let stream =
                open_rsd_stream(self.adapter, self.handshake, SVC_SCREEN_CAPTURE).await?;
            // The struct's fields are private; from_stream is the public
            // constructor (and, unlike connect_rsd, its future is
            // Send-bounded, so it is safe inside the spawned actor).
            let client = CaptureClient::from_stream(stream)
                .await
                .map_err(|e| format!("screencaptureservice handshake: {e:?}"))?;
            self.capture = Some(client);
        }
        match self.capture.as_mut().unwrap().take_screenshot(None, format).await {
            Ok(bytes) => Ok(bytes),
            Err(e) => {
                // Drop the possibly-wedged client; the next request
                // reconnects.
                self.capture = None;
                Err(format!("tunnel take_screenshot: {e:?}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HID keyboard helpers (ported from tools/src/hid.rs)
// ---------------------------------------------------------------------------

/// IdeviceError → string for the stringly-typed request replies.
fn idevice_err(e: idevice::IdeviceError) -> String {
    format!("{e:?}")
}

/// HID Keyboard/Keypad usage for Left Shift.
const KEY_LEFT_SHIFT: u64 = 0xE1;
/// HID Keyboard/Keypad usage for Left GUI (Command on iOS).
const KEY_LEFT_GUI: u64 = 0xE3;
/// HID Keyboard/Keypad usage for the V key.
const KEY_V: u64 = 0x19;

/// Press Cmd+V — the paste keystroke, bracketed like [`type_key`] brackets
/// Shift.
///
/// Spaced, and not for politeness: iOS takes one keyboard report at a time, and
/// four sent in a single burst are not four keystrokes. The modifier can land
/// after the key it was supposed to modify - the V arrives unmodified and
/// pastes nothing - or the whole burst collapses into one report. It fails
/// intermittently, which is exactly how it looked on a real iPhone: the text
/// appears in the keyboard's clipboard suggestion (so the pasteboard SET
/// worked) while the field stays empty, and the next attempt may well work.
async fn paste_keystroke(indigo: &mut IndigoClient) -> Result<(), idevice::IdeviceError> {
    const GAP: Duration = Duration::from_millis(15);
    indigo.send_keyboard(KEY_LEFT_GUI, ButtonState::Down).await?;
    tokio::time::sleep(GAP).await;
    indigo.send_keyboard(KEY_V, ButtonState::Down).await?;
    tokio::time::sleep(GAP).await;
    indigo.send_keyboard(KEY_V, ButtonState::Up).await?;
    tokio::time::sleep(GAP).await;
    indigo.send_keyboard(KEY_LEFT_GUI, ButtonState::Up).await?;
    tokio::time::sleep(GAP).await;
    Ok(())
}

/// Press a key (down then up), bracketing with Left Shift if `shift` is set.
async fn type_key(indigo: &mut IndigoClient, usage: u64, shift: bool) -> Result<(), idevice::IdeviceError> {
    // Same spacing as the paste keystroke, and for the same reason: reports
    // sent back to back are one keystroke to iOS, not several.
    const GAP: Duration = Duration::from_millis(8);
    if shift {
        indigo.send_keyboard(KEY_LEFT_SHIFT, ButtonState::Down).await?;
        tokio::time::sleep(GAP).await;
    }
    indigo.send_keyboard(usage, ButtonState::Down).await?;
    tokio::time::sleep(GAP).await;
    indigo.send_keyboard(usage, ButtonState::Up).await?;
    if shift {
        tokio::time::sleep(GAP).await;
        indigo.send_keyboard(KEY_LEFT_SHIFT, ButtonState::Up).await?;
    }
    // A small gap so the device's input stack registers discrete keystrokes.
    tokio::time::sleep(Duration::from_millis(12)).await;
    Ok(())
}

/// Map an ASCII character to its HID Keyboard/Keypad usage and whether Shift
/// is required (US layout).
fn ascii_to_usage(c: char) -> Option<(u64, bool)> {
    Some(match c {
        'a'..='z' => (0x04 + (c as u64 - 'a' as u64), false),
        'A'..='Z' => (0x04 + (c as u64 - 'A' as u64), true),
        '1'..='9' => (0x1E + (c as u64 - '1' as u64), false),
        '0' => (0x27, false),
        '\n' => (0x28, false), // Return
        '\t' => (0x2B, false), // Tab
        ' ' => (0x2C, false),  // Space
        '!' => (0x1E, true),
        '@' => (0x1F, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        '-' => (0x2D, false),
        '_' => (0x2D, true),
        '=' => (0x2E, false),
        '+' => (0x2E, true),
        '[' => (0x2F, false),
        '{' => (0x2F, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Coordinate / codec helpers
// ---------------------------------------------------------------------------

/// Normalized 0..=1 → 16-bit HID touchscreen coordinate.
fn norm_to_u16(x: f64, y: f64) -> (u16, u16) {
    let x = ((x.clamp(0.0, 1.0) * 65535.0).round() as u16).min(65535);
    let y = ((y.clamp(0.0, 1.0) * 65535.0).round() as u16).min(65535);
    (x, y)
}

/// Whether an Annex-B access unit contains a keyframe: HEVC IRAP NAL
/// (types 16..=23) or H.264 IDR (type 5).
fn annexb_contains_key(annexb: &[u8], hevc: bool) -> bool {
    let mut i = 0;
    while i + 3 < annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            let header = annexb[i + 3];
            let key = if hevc {
                (16..=23).contains(&((header >> 1) & 0x3f))
            } else {
                (header & 0x1f) == 5
            };
            if key {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// HEVC NAL unit type of the AUD (access unit delimiter).
const HEVC_NAL_AUD: u8 = 35;
/// H.264 NAL unit type of the AUD.
const AVC_NAL_AUD: u8 = 9;

/// Apple's DisplayService appends a fixed 14-byte proprietary footer after
/// the final fragment of each coded-slice NAL. It is not part of the HEVC
/// bitstream — Apple's own receiver (avconferenced) strips it before decode.
/// Matched as an exact NAL suffix, so streams that don't carry it are
/// untouched. (Reverse-engineered and confirmed on-device by pymobiledevice3.)
const APPLE_NAL_TRAILER: &[u8] = &[
    0x04, 0xf0, 0x0a, 0xc0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x04, 0xec, 0x0a, 0xb0, 0x03,
];

/// Remove [`APPLE_NAL_TRAILER`] from the tail of every NAL in an Annex-B
/// access unit (see the constant's doc for why).
fn strip_apple_trailer(au: &mut Vec<u8>) {
    let mut out: Vec<u8> = Vec::with_capacity(au.len());
    let mut trimmed = false;
    let mut cursor = 0;
    while let Some((sc, hdr)) = next_annexb_nal(au, cursor) {
        let next = next_annexb_nal(au, hdr).map_or(au.len(), |(s, _)| s);
        out.extend_from_slice(&au[sc..next]);
        if next >= hdr + 2 + APPLE_NAL_TRAILER.len()
            && au[next - APPLE_NAL_TRAILER.len()..next] == *APPLE_NAL_TRAILER
        {
            out.truncate(out.len() - APPLE_NAL_TRAILER.len());
            trimmed = true;
        }
        cursor = next;
    }
    if trimmed {
        *au = out;
    }
}

/// Find the next Annex-B start code at or after `from`. Returns the index of
/// the start code's first byte and the index of the NAL header that follows
/// it (start codes are 3 or 4 bytes).
fn next_annexb_nal(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 < buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if buf[i + 2] == 1 {
                return Some((i, i + 3));
            }
            if buf[i + 2] == 0 && i + 4 < buf.len() && buf[i + 3] == 1 {
                return Some((i, i + 4));
            }
        }
        i += 1;
    }
    None
}

/// Split one complete access unit off the front of `buf`, at the first AUD
/// NAL that is not the buffer's first NAL (an AUD opens each picture, so a
/// non-leading AUD closes the picture before it). Returns `None` while no
/// boundary has been seen — the picture at the front is still incomplete.
/// The AUD itself stays in `buf` as the start of the next picture.
fn split_access_unit(buf: &mut Vec<u8>, hevc: bool) -> Option<Vec<u8>> {
    let mut cursor = 0;
    while let Some((sc, hdr)) = next_annexb_nal(buf, cursor) {
        if hdr < buf.len() {
            let is_aud = if hevc {
                (buf[hdr] >> 1) & 0x3f == HEVC_NAL_AUD
            } else {
                buf[hdr] & 0x1f == AVC_NAL_AUD
            };
            if is_aud && sc > 0 {
                let au = buf[..sc].to_vec();
                buf.drain(..sc);
                return Some(au);
            }
        }
        cursor = hdr;
    }
    None
}

/// The active video depacketizer, chosen from the stream's RTP payload type.
enum VideoDepack {
    Hevc(HevcDepacketizer),
    Avc(AvcDepacketizer),
}

impl VideoDepack {
    fn is_hevc(&self) -> bool {
        matches!(self, VideoDepack::Hevc(_))
    }

    fn push(&mut self, seq: u16, timestamp: u32, payload: &[u8]) {
        match self {
            VideoDepack::Hevc(d) => d.push(seq, timestamp, payload),
            VideoDepack::Avc(d) => d.push(seq, timestamp, payload),
        }
    }

    fn take_output(&mut self) -> Vec<u8> {
        match self {
            VideoDepack::Hevc(d) => d.take_output(),
            VideoDepack::Avc(d) => d.take_output(),
        }
    }

    fn resync(&mut self, seq: u16) {
        match self {
            VideoDepack::Hevc(d) => d.resync(seq),
            VideoDepack::Avc(d) => d.resync(seq),
        }
    }
}

// ---------------------------------------------------------------------------
// Bridge command parsing (the inverse of crate::hid's map_* functions)
// ---------------------------------------------------------------------------

/// Parse a bridge command line (as produced by [`crate::hid`]'s `map_*`
/// functions) into a [`HidVerb`] for the tunnel transport. The roundtrip is
/// pinned by unit tests so the two transports stay interchangeable at the
/// `ios_hid_send` dispatch points.
pub fn parse_bridge_command(cmd: &str) -> Result<HidVerb, String> {
    let cmd = cmd.trim();
    let (head, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
    match head {
        "TAP" => {
            let (x, y) = parse_pair(rest)?;
            Ok(HidVerb::Tap { x: x / LOGICAL_PLANE, y: y / LOGICAL_PLANE })
        }
        "MDOWN" => {
            let (x, y) = parse_pair(rest)?;
            Ok(HidVerb::Touch { action: TouchAction::Down, x: x / LOGICAL_PLANE, y: y / LOGICAL_PLANE })
        }
        "M" => {
            let (x, y) = parse_pair(rest)?;
            Ok(HidVerb::Touch { action: TouchAction::Move, x: x / LOGICAL_PLANE, y: y / LOGICAL_PLANE })
        }
        "MUP" => {
            let (x, y) = parse_pair(rest)?;
            Ok(HidVerb::Touch { action: TouchAction::Up, x: x / LOGICAL_PLANE, y: y / LOGICAL_PLANE })
        }
        "K" => {
            let usage = match rest {
                "ENTER" => 0x28,
                "BKSP" => 0x2A,
                "DEL" => 0x4C,
                "UP" => 0x52,
                "DOWN" => 0x51,
                "LEFT" => 0x50,
                "RIGHT" => 0x4F,
                "ESC" => 0x29,
                "TAB" => 0x2B,
                other => return Err(format!("unknown key name: {other:?}")),
            };
            Ok(HidVerb::Key { usage })
        }
        "TEXT" => Ok(HidVerb::Text { text: rest.to_string() }),
        "KEY" => {
            let button = match rest {
                "HOME" => "home",
                "BACK" => "back",
                "APP_SWITCH" => "recents",
                "POWER" => "power",
                "VOL+" => "volume_up",
                "VOL-" => "volume_down",
                other => return Err(format!("unknown button: {other:?}")),
            };
            Ok(HidVerb::Button { button: button.to_string() })
        }
        "SCROLL" => {
            let (dx, dy) = parse_signed_pair(rest)?;
            Ok(HidVerb::Scroll { dx, dy })
        }
        "SWIPE" => {
            let parts: Vec<&str> = rest.split(',').collect();
            if parts.len() != 5 {
                return Err(format!("bad SWIPE: {cmd:?}"));
            }
            let nums: Result<Vec<f64>, _> = parts.iter().map(|p| p.parse::<f64>().map_err(|_| "bad number".to_string())).collect();
            let nums = nums?;
            Ok(HidVerb::Swipe {
                x1: nums[0] / LOGICAL_PLANE,
                y1: nums[1] / LOGICAL_PLANE,
                x2: nums[2] / LOGICAL_PLANE,
                y2: nums[3] / LOGICAL_PLANE,
                duration_ms: nums[4].max(0.0) as u64,
            })
        }
        other => Err(format!("unknown bridge command: {other:?}")),
    }
}

fn parse_pair(rest: &str) -> Result<(f64, f64), String> {
    let (x, y) = rest
        .split_once(',')
        .ok_or_else(|| format!("expected \"x,y\", got {rest:?}"))?;
    let x: f64 = x.trim().parse().map_err(|_| format!("bad x: {x:?}"))?;
    let y: f64 = y.trim().parse().map_err(|_| format!("bad y: {y:?}"))?;
    Ok((x, y))
}

fn parse_signed_pair(rest: &str) -> Result<(f64, f64), String> {
    parse_pair(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The developer-image case has to be distinguishable from every other
    /// reason a tunnel fails: the app mounts an image for that one and falls
    /// back to AirPlay for the rest.
    #[test]
    fn only_the_missing_service_error_is_recognised() {
        let missing = format!(
            "{MISSING_SERVICE_MARK}com.apple.coredevice.displayservice — RSD listed 64 services"
        );
        assert!(is_missing_service_error(&missing));
        assert!(is_missing_service_error(&format!(
            "tunnel build failed: {missing}"
        )));

        for other in [
            "usbmuxd connect failed: no device",
            "CoreDeviceProxy: timeout waiting for the tunnel",
            "device is locked",
            "",
        ] {
            assert!(!is_missing_service_error(other), "false positive on {other:?}");
        }
    }

    /// Two logical-plane units of slop: map_* truncates, parse divides by
    /// LOGICAL_PLANE; both stay on the same 1920-plane by construction.
    const EPS: f64 = 2.0 / LOGICAL_PLANE;

    fn assert_norm_eq(got: f64, want: f64) {
        assert!(
            (got - want).abs() < EPS,
            "normalized mismatch: got {got}, want {want}"
        );
    }

    #[test]
    fn tap_roundtrip() {
        let cmd = crate::hid::map_tap(0.73, 0.62);
        let HidVerb::Tap { x, y } = parse_bridge_command(&cmd).unwrap() else {
            panic!("expected Tap for {cmd:?}");
        };
        assert_norm_eq(x, 0.73);
        assert_norm_eq(y, 0.62);
    }

    #[test]
    fn touch_roundtrip_all_actions() {
        for (action, cmd) in [
            (TouchAction::Down, crate::hid::map_touch("down", 0.1, 0.9).unwrap()),
            (TouchAction::Move, crate::hid::map_touch("move", 0.5, 0.5).unwrap()),
            (TouchAction::Up, crate::hid::map_touch("up", 0.99, 0.01).unwrap()),
        ] {
            let HidVerb::Touch { action: got_action, x, y } =
                parse_bridge_command(&cmd).unwrap()
            else {
                panic!("expected Touch for {cmd:?}");
            };
            assert_eq!(got_action, action);
            let (wx, wy) = match action {
                TouchAction::Down => (0.1, 0.9),
                TouchAction::Move => (0.5, 0.5),
                TouchAction::Up => (0.99, 0.01),
            };
            assert_norm_eq(x, wx);
            assert_norm_eq(y, wy);
        }
    }

    #[test]
    fn key_roundtrip() {
        // Every keycode map_key can express.
        for (keycode, usage) in [
            (66u32, 0x28u64), // ENTER
            (67, 0x2A),       // BKSP
            (112, 0x4C),      // DEL
            (19, 0x52),       // UP
            (20, 0x51),       // DOWN
            (21, 0x50),       // LEFT
            (22, 0x4F),       // RIGHT
            (111, 0x29),      // ESC
            (61, 0x2B),       // TAB
        ] {
            let cmd = crate::hid::map_key(keycode).unwrap();
            let HidVerb::Key { usage: got } = parse_bridge_command(&cmd).unwrap() else {
                panic!("expected Key for {cmd:?}");
            };
            assert_eq!(got, usage, "keycode {keycode}");
        }
    }

    #[test]
    fn text_roundtrip() {
        for line in crate::hid::map_text("Hello, World! 123") {
            let HidVerb::Text { text } = parse_bridge_command(&line).unwrap() else {
                panic!("expected Text for {line:?}");
            };
            assert!(!text.is_empty());
        }
        // Long text splits into 58-char chunks; every chunk parses.
        let long = "a".repeat(100);
        let lines = crate::hid::map_text(&long);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            parse_bridge_command(line).unwrap();
        }
    }

    #[test]
    fn text_verb_preserves_non_ascii() {
        // The tunnel's Unicode path (pasteboard + Cmd+V) relies on
        // send_text / macro replay passing the raw text through a single
        // TEXT verb — unlike map_text, which strips non-ASCII for the
        // firmware bridge.
        let HidVerb::Text { text } = parse_bridge_command("TEXT 你好，world!").unwrap() else {
            panic!("expected Text verb");
        };
        assert_eq!(text, "你好，world!");
    }

    #[test]
    fn png_size_from_ihdr() {
        // Minimal PNG: signature + IHDR with big-endian 1179x2556 (the
        // iPhone 15 Pro's true screen, which the encoder pads to 1184x2576).
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        png.extend_from_slice(&[0, 0, 0, 13]); // chunk length
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1179u32.to_be_bytes());
        png.extend_from_slice(&2556u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, ...
        assert_eq!(parse_png_size(&png), Some((1179, 2556)));
        // Too short / wrong chunk → no size (crop disabled, not a panic).
        assert_eq!(parse_png_size(&png[..20]), None);
        assert_eq!(parse_png_size(b"not a png at all...."), None);
    }

    #[test]
    fn button_roundtrip() {
        for (button, cmd) in [
            ("home", crate::hid::map_button("home").unwrap()),
            ("back", crate::hid::map_button("back").unwrap()),
            ("recents", crate::hid::map_button("recents").unwrap()),
            ("power", crate::hid::map_button("power").unwrap()),
            ("volume_up", crate::hid::map_button("volume_up").unwrap()),
            ("volume_down", crate::hid::map_button("volume_down").unwrap()),
        ] {
            let HidVerb::Button { button: got } = parse_bridge_command(&cmd).unwrap() else {
                panic!("expected Button for {cmd:?}");
            };
            assert_eq!(got, button);
        }
    }

    #[test]
    fn scroll_roundtrip_keeps_logical_units() {
        let cmd = crate::hid::map_scroll(0.5, -0.5);
        let HidVerb::Scroll { dx, dy } = parse_bridge_command(&cmd).unwrap() else {
            panic!("expected Scroll for {cmd:?}");
        };
        assert_eq!(dx, 250.0);
        assert_eq!(dy, -250.0);
    }

    #[test]
    fn swipe_roundtrip() {
        let cmd = crate::hid::map_swipe(0.1, 0.2, 0.8, 0.9, 600);
        let HidVerb::Swipe { x1, y1, x2, y2, duration_ms } =
            parse_bridge_command(&cmd).unwrap()
        else {
            panic!("expected Swipe for {cmd:?}");
        };
        assert_norm_eq(x1, 0.1);
        assert_norm_eq(y1, 0.2);
        assert_norm_eq(x2, 0.8);
        assert_norm_eq(y2, 0.9);
        assert_eq!(duration_ms, 600);
    }

    #[test]
    fn app_switcher_steps_all_parse() {
        for (cmd, _) in crate::hid::app_switcher_gesture() {
            let verb = parse_bridge_command(&cmd)
                .unwrap_or_else(|e| panic!("app-switcher step {cmd:?}: {e}"));
            match verb {
                HidVerb::Touch { action, .. } => {
                    assert!(matches!(action, TouchAction::Down | TouchAction::Move | TouchAction::Up))
                }
                other => panic!("app-switcher step {cmd:?} parsed to {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_bridge_command("").is_err());
        assert!(parse_bridge_command("TAP").is_err());
        assert!(parse_bridge_command("TAP abc,def").is_err());
        assert!(parse_bridge_command("BOGUS 1,2").is_err());
        assert!(parse_bridge_command("K NOPE").is_err());
    }

    #[test]
    fn coordinates_clamp_into_u16() {
        assert_eq!(norm_to_u16(-0.5, 2.0), (0, 65535));
        assert_eq!(norm_to_u16(0.5, 0.5), (32768, 32768));
    }

    #[test]
    fn keyframe_detection_scans_annexb() {
        // VPS/SPS/PPS + IDR-slice (HEVC IRAP type 19 → header byte 0x26).
        let annexb = [
            0, 0, 0, 1, 0x40, 0x01, // VPS (type 32)
            0, 0, 0, 1, 0x42, 0x01, // SPS (type 33)
            0, 0, 0, 1, 0x26, 0x01, // IDR (type 19)
        ];
        assert!(annexb_contains_key(&annexb, true));
        // P-slice (type 1 → header byte 0x02) is not a keyframe.
        let non_key = [0, 0, 0, 1, 0x02, 0x01];
        assert!(!annexb_contains_key(&non_key, true));
        // 3-byte start codes count too.
        let short_sc = [0, 0, 1, 0x26, 0x01];
        assert!(annexb_contains_key(&short_sc, true));
    }

    #[test]
    fn apple_trailer_is_stripped() {
        // Slice NAL carrying Apple's 14-byte footer, followed by an AUD: only
        // the footer must go; everything else is byte-identical.
        let mut au: Vec<u8> = Vec::new();
        au.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01]);
        au.extend_from_slice(&[0xDD; 8]);
        au.extend_from_slice(APPLE_NAL_TRAILER);
        au.extend_from_slice(&[0, 0, 0, 1, 0x46, 0x01, 0x50]);
        let expect = au.len() - APPLE_NAL_TRAILER.len();
        strip_apple_trailer(&mut au);
        assert_eq!(au.len(), expect);
        assert!(!au.windows(APPLE_NAL_TRAILER.len()).any(|w| w == APPLE_NAL_TRAILER));

        // A trailer-less AU must pass through untouched.
        let plain: Vec<u8> = vec![0, 0, 0, 1, 0x02, 0x01, 0xAA, 0xBB];
        let mut au2 = plain.clone();
        strip_apple_trailer(&mut au2);
        assert_eq!(au2, plain);
    }

    #[test]
    fn leading_aud_is_not_a_boundary() {
        // sc + AUD + sc + P-slice: the AUD opens the (only) picture, so no
        // complete picture can be split off yet.
        let mut buf: Vec<u8> = Vec::new();
        for nal in [[0x46u8, 0x01, 0x50], [0x02, 0x01, 0xAA]] {
            buf.extend_from_slice(&[0, 0, 0, 1]);
            buf.extend_from_slice(&nal);
        }
        assert!(split_access_unit(&mut buf, true).is_none());
        assert_eq!(buf.len(), 14);
    }

    #[test]
    fn depacketized_frames_split_at_aud() {
        use idevice::core_device::HevcDepacketizer;

        let mut depack = HevcDepacketizer::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();

        // Frame 1 (ts 100): VPS + SPS + PPS + IDR slice, one NAL per packet.
        for (seq, payload) in [
            (1u16, &[0x40u8, 0x01, 0xAA][..]), // VPS (type 32)
            (2, &[0x42, 0x01, 0xBB]),          // SPS (type 33)
            (3, &[0x44, 0x01, 0xCC]),          // PPS (type 34)
            (4, &[0x26, 0x01, 0xDD]),          // IDR slice (type 19)
        ] {
            depack.push(seq, 100, payload);
            pending.extend(depack.take_output());
            while let Some(au) = split_access_unit(&mut pending, true) {
                frames.push(au);
            }
        }
        assert!(frames.is_empty(), "picture 1 incomplete until the next one starts");

        // Frame 2 (ts 200): the depacketizer writes an AUD first — that
        // closes picture 1.
        depack.push(5, 200, &[0x02, 0x01, 0xEE]); // P slice (type 1)
        pending.extend(depack.take_output());
        while let Some(au) = split_access_unit(&mut pending, true) {
            frames.push(au);
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 4 * 7, "VPS/SPS/PPS/IDR, 4 NALs");
        assert!(annexb_contains_key(&frames[0], true));

        // Frame 3 (ts 300): closes picture 2 (AUD + one P slice, not a key).
        depack.push(6, 300, &[0x02, 0x01, 0xFF]);
        pending.extend(depack.take_output());
        while let Some(au) = split_access_unit(&mut pending, true) {
            frames.push(au);
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].len(), 2 * 7, "AUD + P slice");
        assert!(!annexb_contains_key(&frames[1], true));
    }
}
