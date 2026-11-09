use anyhow::{Result, anyhow};
use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::dvt::screenshot::ScreenshotClient;
use idevice::lockdown::LockdownClient;
use idevice::provider::IdeviceProvider;
use idevice::rsd::RsdHandshake;
use idevice::screenshotr::ScreenshotService;
use idevice::tcp::handle::AdapterHandle;
use idevice::ReadWrite;

use crate::device::provider_for;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex, Notify};

/// Upper bound for one classic (iOS <= 16) screenshot attempt. Without this,
/// a tunnel whose device vanished mid-exchange can hang for minutes on
/// socket timeouts.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether a failure is the device saying it does not have the service this
/// needs - which is what a missing (or stale) developer image looks like, and
/// the one failure worth preparing an image over.
///
/// Matched on what the *device* answers, not on any prefix this crate adds:
/// the same check runs on a raw error (the preflight's probe) and on a wrapped
/// one (a capture), and a wrapper string would only be there in one of them.
///
/// The classic service answers in two different ways depending on whether
/// Developer Mode is on, and both are the same thing missing - the image
/// (verified on 16.7.12): with the switch off, lockdown refuses to start it
/// (`InvalidService`); with it on, lockdown agrees and the connection then dies
/// (`device socket io failed`), because the service binary an image provides is
/// not on the device. A device that is merely unpaired or unplugged says
/// something else, and is not this.
///
/// The modern route is blunter: the DVT channel's `dtservicehub` is simply
/// absent from the RSD service list.
pub(crate) fn is_service_missing(error: &str) -> bool {
    error.contains("InvalidService")
        || error.contains("device socket io failed")
        || error.contains("Remote Server service not found")
}

/// Whether the classic `screenshotr` service answers for this device, which is
/// the whole of what iOS 16 needs to be asked before it can be captured.
///
/// `Ok(false)` is the missing-image case (see [`is_service_missing`]) and is
/// what a preflight prepares for; every other failure comes back as `Err`, so
/// a device that is merely unplugged is not mistaken for one that needs work.
pub async fn classic_service_ready(udid: &str) -> Result<bool> {
    let provider = provider_for(udid).await?;
    match ScreenshotService::connect(provider.as_ref()).await {
        Ok(_) => Ok(true),
        Err(e) if is_service_missing(&format!("{e:?}")) => Ok(false),
        Err(e) => Err(anyhow!("screenshotr connect failed: {e} (is the device paired?)")),
    }
}

/// How long a caller waits for one persistent-channel capture. Strictly
/// above every actor deadline below, so the caller essentially always gets a
/// definitive answer (success / wedged / gone) instead of a bare timeout.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);
/// Hard per-capture deadline inside the session actor for a channel's FIRST
/// capture (a cold encode of fresh rich content is genuinely slow on-device:
/// measured 6.9MB in 5.8s). Reaching it means the channel is stuck - either
/// a wedged encoder or a trickling one - and dtservicehub dispatches
/// serially per connection, so one stuck takeScreenshot blocks every later
/// request. Tearing the connection down and reopening is the observed
/// recovery (field log: stuck 30s -> teardown -> 5s gap -> fresh channel
/// served the same content in 5.8s). Killing a stuck request is safe.
const ACTOR_CAPTURE_DEADLINE: Duration = Duration::from_secs(12);
/// Budget for captures on a channel that has ALREADY served one. Cache hits
/// take ~130-800ms, but a screen-change re-encode costs the same as a cold
/// one (~6s worst observed for a 6.9MB screen), so the same budget as a
/// first capture applies - a tighter one misjudges legit re-encodes as
/// wedged (observed: an 8s budget killed a re-encode that needed ~6-8s).
const SERVED_CAPTURE_DEADLINE: Duration = Duration::from_secs(12);
/// Upper bound for building the persistent DVT channel (tunnel + RSD + service
/// handshake); ~120ms warm, slower on a cold device.
const BUILD_TIMEOUT: Duration = Duration::from_secs(15);
/// After a dtservicehub channel that served captures closes, the device needs
/// quiet time before the next one opens: reopening within ~250ms hangs the new
/// channel indefinitely (observed on iOS 26 with 1s polling). Teardowns of
/// channels that never completed a capture do not impose this gap.
const CHANNEL_RECYCLE_GAP: Duration = Duration::from_secs(5);
/// How long without ANY device-side traffic on the connection counts as a
/// wedged encoder during the PRE-STREAM phase (before the first screenshot
/// byte arrives): the on-device encode can legitimately stay silent for
/// seconds (~5.8s observed), so this must exceed the slowest observed encode;
/// 10s = ~1.7x margin. Once bytes ARE flowing, [`STREAM_STALL`] applies
/// instead. A wedged encoder sends nothing at all, forever.
const WEDGE_SILENCE: Duration = Duration::from_secs(10);
/// Once screenshot bytes ARE flowing, how long without a new byte means the
/// transfer stalled. A healthy encode streams continuously (6.6MB over
/// ~5.8s, no observed mid-stream pauses), so mid-stream quiet is a stall,
/// not slowness. This catches the observed "transfer stops partway" pattern
/// (3 kills at ~4.0MB of a ~7MB screenshot) within seconds instead of
/// waiting out the full absolute deadline.
const STREAM_STALL: Duration = Duration::from_secs(4);
/// After a capture is killed mid-stream (stall), the device needs longer
/// than the normal recycle gap to clean up before the next channel: fresh
/// channels opened only ~5s after a stall kill stalled identically
/// (observed 3/3), while ones opened ~8s after served the same content in
/// ~4.8s.
const STALL_RECYCLE_GAP: Duration = Duration::from_secs(10);
/// After a wedge verdict, fail fast instead of rebuilding channels: the same
/// screen content re-wedges every fresh channel (observed on iOS 26), so
/// retries just burn 30s+ each and hammer the device. Recovery comes from the
/// screen content changing, not from us retrying.
const WEDGE_COOLDOWN: Duration = Duration::from_secs(10);

/// Budget for a one-shot capture over a running CoreDevice tunnel before
/// falling back to DVT. Abandoning the request is safe (the tunnel actor
/// survives; no device-side channel is torn down mid-flight).
const TUNNEL_CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-UDID cached iOS version (read once; it never changes while the process
/// runs, and the screenshot preview polls every second).
static VERSION_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn version_cache() -> &'static Mutex<HashMap<String, String>> {
    VERSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// --- persistent DVT screenshot channel --------------------------------------
//
// One long-lived tunnel + instruments server + screenshot channel per device,
// serving captures on demand. This sidesteps the dtservicehub hang entirely
// (no channel churn) and is ~5x faster than open/close per shot (~130ms vs
// ~700ms measured on an iPhone with iOS 26).

enum DvtRequest {
    Capture(tokio::sync::oneshot::Sender<Result<Vec<u8>, CaptureFail>>),
}

/// Why a capture failed on the actor side.
#[derive(Clone)]
enum CaptureFail {
    /// The device-side encoder is wedged on the current screen content.
    Wedged(String),
    /// The capture was killed mid-stream: bytes were flowing but the
    /// transfer stalled or outran its budget. Fresh channels opened too
    /// soon after a stall stall too - the rebuild must respect
    /// [`STALL_RECYCLE_GAP`].
    Stalled(String),
    /// Any other failure; the channel is dead, rebuild it.
    Other(String),
}

/// Wraps a stream and counts every byte arriving from the device. The actor
/// samples the counter while a capture is in flight: a wedged encoder stops
/// sending traffic entirely, while a slow encode streams screenshot fragments
/// continuously, so "no new bytes for WEDGE_SILENCE" is a wedge verdict.
#[derive(Debug)]
struct CountingStream<S> {
    inner: S,
    rx_bytes: Arc<AtomicU64>,
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let got = buf.filled().len().saturating_sub(before);
                if got > 0 {
                    self.rx_bytes.fetch_add(got as u64, Ordering::Relaxed);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct DvtSession {
    req: tokio::sync::mpsc::UnboundedSender<DvtRequest>,
    /// Force-teardown for a wedged channel (cancels an in-flight capture).
    kill: Arc<Notify>,
}

fn dvt_sessions() -> &'static std::sync::Mutex<HashMap<String, Arc<DvtSession>>> {
    static M: OnceLock<std::sync::Mutex<HashMap<String, Arc<DvtSession>>>> = OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Earliest time a new DVT channel may open for a UDID (last close plus its
/// gap). The std mutex is never held across an await.
fn dvt_close_at() -> &'static std::sync::Mutex<HashMap<String, Instant>> {
    static M: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn current_dvt_session(udid: &str) -> Option<Arc<DvtSession>> {
    dvt_sessions().lock().unwrap().get(udid).cloned()
}

/// Remove `s` from the map if it is still the registered session for `udid`.
fn drop_dvt_session(udid: &str, s: &Arc<DvtSession>) {
    let mut m = dvt_sessions().lock().unwrap();
    if m.get(udid).map(|cur| Arc::ptr_eq(cur, s)).unwrap_or(false) {
        m.remove(udid);
    }
}

/// Record a channel teardown so the next channel respects `gap`. Never
/// shortens an existing deadline: a stall's longer gap must survive the
/// actor's exit-time recording (which only knows the normal gap). Recording
/// from the caller too (not just the actor) closes the race where the actor
/// has not exited yet when a replacement is already spawning.
fn record_channel_close(udid: &str, gap: Duration) {
    let not_before = Instant::now() + gap;
    let mut m = dvt_close_at().lock().unwrap();
    let entry = m.entry(udid.to_string()).or_insert(not_before);
    if *entry < not_before {
        *entry = not_before;
    }
}

/// Until when a device is in wedge cooldown (fail fast instead of retrying).
fn wedged_until() -> &'static std::sync::Mutex<HashMap<String, Instant>> {
    static M: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn record_wedge(udid: &str) {
    wedged_until()
        .lock()
        .unwrap()
        .insert(udid.to_string(), Instant::now() + WEDGE_COOLDOWN);
}

/// Remaining cooldown for a wedged device, if any.
fn wedge_cooldown_left(udid: &str) -> Option<Duration> {
    wedged_until()
        .lock()
        .unwrap()
        .get(udid)
        .and_then(|until| until.checked_duration_since(Instant::now()))
}

/// Sleep until the recycle gap since the last channel close has passed.
async fn wait_channel_gap(udid: &str) {
    let until = dvt_close_at().lock().unwrap().get(udid).copied();
    if let Some(until) = until {
        let now = Instant::now();
        if until > now {
            eprintln!(
                "[ios-screenshot] waiting {:?} before opening a new dvt channel (recycle gap)",
                until - now
            );
            tokio::time::sleep(until - now).await;
        }
    }
}

/// Take a screenshot of a USB-connected iOS device, returned as PNG bytes.
///
/// The method is picked from the iOS version (no trial and error):
/// - iOS <= 16: the classic `screenshotr` lockdown service. iOS 17 removed it
///   (lockdown answers `InvalidService`), and iOS 16 answers the same until
///   Developer Mode is on AND a classic `DeveloperDiskImage.dmg` is mounted at
///   `/Developer` (tested on 16.7.12) - which this prepares and retries rather
///   than reporting, when the image is what is missing.
/// - iOS >= 17: DVT screenshot through a PERSISTENT CoreDeviceProxy tunnel
///   (requires Developer Mode on the device). Cycling a channel per shot
///   trips a device-side dtservicehub hang when channels churn faster than
///   ~1/s, so the channel stays open and serves repeated captures.
///
/// Concurrent calls share ONE in-flight attempt instead of queueing behind
/// it (see the single-flight below).
pub async fn take_screenshot(udid: &str) -> Result<Vec<u8>> {
    // Shared result of an in-flight attempt, visible to concurrent callers.
    type SharedResult = Option<Result<Vec<u8>, std::sync::Arc<anyhow::Error>>>;

    struct InFlight {
        /// Receiver holding the attempt's eventual result. The sender is
        /// owned only by the leader, so a dropped leader closes the channel.
        rx: tokio::sync::watch::Receiver<SharedResult>,
    }

    static IN_FLIGHT: tokio::sync::Mutex<Option<std::sync::Arc<InFlight>>> =
        tokio::sync::Mutex::const_new(None);

    /// Ensures a result is published even if the leader future is dropped
    /// mid-attempt, so followers never wait forever.
    struct LeaderGuard {
        tx: tokio::sync::watch::Sender<SharedResult>,
        sent: bool,
    }

    impl Drop for LeaderGuard {
        fn drop(&mut self) {
            if !self.sent {
                let _ = self
                    .tx
                    .send(Some(Err(anyhow!("screenshot attempt aborted").into())));
            }
        }
    }

    enum Role {
        Leader {
            tx: tokio::sync::watch::Sender<SharedResult>,
        },
        Follower(std::sync::Arc<InFlight>),
    }

    loop {
        let role = {
            let mut slot = IN_FLIGHT.lock().await;
            let start_fresh = match slot.as_ref() {
                None => true,
                // Finished (has a result) -> replace with a new attempt.
                Some(a) => a.rx.borrow().is_some(),
            };
            if start_fresh {
                let (tx, rx) = tokio::sync::watch::channel(None);
                *slot = Some(std::sync::Arc::new(InFlight { rx }));
                Role::Leader { tx }
            } else {
                Role::Follower(slot.as_ref().unwrap().clone())
            }
        };

        match role {
            Role::Leader { tx } => {
                let mut guard = LeaderGuard { tx, sent: false };
                let result = run_screenshot(udid).await.map_err(std::sync::Arc::new);
                let _ = guard.tx.send(Some(result.clone()));
                guard.sent = true;
                return result.map_err(|e| anyhow!("{e:#}"));
            }
            Role::Follower(attempt) => {
                eprintln!("[ios-screenshot] joining in-flight attempt (sharing result)");
                let mut rx = attempt.rx.clone();
                let res = rx.borrow_and_update().clone();
                if let Some(res) = res {
                    return res.map_err(|e| anyhow!("{e:#}"));
                }
                // Leader aborted without a result (its guard published an
                // error on drop, or the channel closed): take over fresh.
                if rx.changed().await.is_err() {
                    continue;
                }
                let res = rx.borrow_and_update().clone();
                if let Some(res) = res {
                    return res.map_err(|e| anyhow!("{e:#}"));
                }
            }
        }
    }
}

async fn run_screenshot(udid: &str) -> Result<Vec<u8>> {
    let t0 = Instant::now();
    let provider = provider_for(udid).await?;
    log_stage("provider_for", t0);

    let t1 = Instant::now();
    let version = match device_version(provider.as_ref(), udid).await {
        Ok(v) => v,
        Err(e) => {
            // Version unknown: fall back to trying both (classic first).
            eprintln!("[ios-screenshot] could not read iOS version: {e:#}");
            return take_screenshot_any(udid, provider).await;
        }
    };
    let major = crate::developer_image::major_of(&version).unwrap_or(0);
    log_stage(&format!("version(v{major})"), t1);

    let t2 = Instant::now();
    let png = if major >= 17 {
        match capture_modern(udid).await {
            Ok(png) => png,
            // The DVT channel's service comes from the personalized image; a
            // device that does not offer it has none mounted (or a stale one).
            Err(e) if is_service_missing(&format!("{e:#}")) => {
                prepare_for_capture(udid, crate::developer_image::Route::Personalized, e).await?;
                capture_modern(udid).await?
            }
            Err(e) => return Err(e),
        }
    } else {
        match classic_capture(provider.as_ref(), udid).await {
            Ok(png) => png,
            // iOS 16 answers `InvalidService` for `screenshotr` unless both
            // Developer Mode is on and the classic image is mounted, so this is
            // the missing-image case rather than anything wrong with the call.
            Err(e) if is_service_missing(&format!("{e:#}")) => {
                let route = crate::developer_image::route_for(&version);
                prepare_for_capture(udid, route, e).await?;
                classic_capture(provider.as_ref(), udid).await?
            }
            Err(e) => return Err(e),
        }
    };
    eprintln!(
        "[ios-screenshot] capture: {:?}, {} bytes (total {:?})",
        t2.elapsed(),
        png.len(),
        t0.elapsed()
    );
    Ok(png)
}

/// Prepare the developer image a capture is missing, and say so clearly when
/// that does not work: the failure that got us here is less useful than the
/// reason the fix failed, but losing it entirely would make a broken image
/// indistinguishable from a broken screenshot.
async fn prepare_for_capture(
    udid: &str,
    route: crate::developer_image::Route,
    cause: anyhow::Error,
) -> Result<()> {
    eprintln!("[ios-screenshot] {udid}: capture came up short ({cause:#}); preparing the developer image");
    crate::developer_image::ensure(udid, route)
        .await
        .map_err(|e| {
            anyhow!("iOS screenshot failed ({cause:#}), and preparing the developer image failed: {e}")
        })?;
    Ok(())
}

/// The modern capture chain: the tunnel's one-shot service when a tunnel is
/// running, then the persistent DVT channel.
async fn capture_modern(udid: &str) -> Result<Vec<u8>> {
    // The CoreDevice one-shot capture service (Xcode's Devices-window path)
    // over an already-running tunnel — much faster and more stable than DVT. A
    // timeout only abandons the REQUEST: the tunnel actor keeps serving other
    // channels, and no device-side service is torn down mid-flight (the
    // mistake that poisons the DVT channel).
    let t_tun = Instant::now();
    let attempt = tokio::time::timeout(
        TUNNEL_CAPTURE_TIMEOUT,
        crate::tunnel::tunnel_screenshot(udid, idevice::core_device::ImageFormat::Png),
    )
    .await;
    match attempt {
        Ok(Some(Ok(bytes))) => {
            eprintln!(
                "[ios-screenshot] tunnel capture: {:?}, {} bytes",
                t_tun.elapsed(),
                bytes.len()
            );
            Ok(bytes)
        }
        Ok(Some(Err(e))) => {
            eprintln!("[ios-screenshot] tunnel capture failed ({e}), falling back to DVT");
            screenshot_dvt_persistent(udid).await
        }
        // No tunnel running (AirPlay/bridge modes) — straight to DVT.
        Ok(None) => screenshot_dvt_persistent(udid).await,
        Err(_) => {
            eprintln!(
                "[ios-screenshot] tunnel capture timed out after {TUNNEL_CAPTURE_TIMEOUT:?}, falling back to DVT"
            );
            screenshot_dvt_persistent(udid).await
        }
    }
}

/// The classic `screenshotr` capture, bounded by [`ATTEMPT_TIMEOUT`]: a device
/// that vanished mid-exchange otherwise hangs on socket timeouts for minutes.
async fn classic_capture(provider: &dyn IdeviceProvider, udid: &str) -> Result<Vec<u8>> {
    match tokio::time::timeout(ATTEMPT_TIMEOUT, screenshot_classic(provider)).await {
        Ok(r) => r,
        Err(_) => Err(anyhow!("iOS screenshot timed out ({udid})")),
    }
}

/// Why a capture request failed.
enum CaptureErr {
    /// The caller's wait budget ran out. The capture may still be running
    /// (or queued) on the channel - keep the session, do NOT tear it down.
    /// Killing a mid-flight capture poisons the device-side service.
    TimedOut,
    /// The device-side encoder is wedged on the current screen content.
    /// Retrying (even on a fresh channel) re-wedges; enter cooldown instead.
    Wedged(String),
    /// The capture was killed mid-stream: bytes flowed but the transfer
    /// stalled. The channel is torn down and rebuilt after the longer
    /// [`STALL_RECYCLE_GAP`] - a fresh channel opened after only the normal
    /// gap stalled identically (observed 3/3).
    Stalled(String),
    /// The session is dead or reported a failure; rebuild it.
    Gone(String),
}

/// Capture via the per-UDID persistent DVT channel, rebuilding it when dead.
/// A slow capture returns `TimedOut` after one attempt (retrying on the same
/// channel just queues behind the slow capture); only real failures rebuild.
async fn screenshot_dvt_persistent(udid: &str) -> Result<Vec<u8>> {
    let mut last_fail = String::new();
    for attempt in 1..=2u32 {
        // Wedge cooldown: the current screen content wedges the device-side
        // encoder; fresh channels wedge on it too. Fail fast (and cheaply)
        // until it expires - the screen changing is what recovers.
        if let Some(left) = wedge_cooldown_left(udid) {
            return Err(anyhow!(
                "iOS screenshot unavailable for another {left:?}: the device screenshot \
                 encoder is wedged on the current screen content. Wait for the screen to \
                 change, or start AirPlay mirroring (live frames do not use the encoder)"
            ));
        }
        let session = match current_dvt_session(udid) {
            Some(s) => s,
            None => {
                let s = match spawn_dvt_session(udid).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[ios-screenshot] dvt session build failed: {e}");
                        return Err(anyhow!("iOS screenshot failed: {e}"));
                    }
                };
                dvt_sessions().lock().unwrap().insert(udid.to_string(), s.clone());
                s
            }
        };
        match capture_via(&session).await {
            Ok(png) => return Ok(png),
            Err(CaptureErr::TimedOut) => {
                eprintln!(
                    "[ios-screenshot] dvt capture still in flight after {CAPTURE_TIMEOUT:?} \
                     (slow encode); keeping the channel"
                );
                return Err(anyhow!(
                    "iOS screenshot timed out (capture still in flight); start AirPlay \
                     mirroring for live frames"
                ));
            }
            Err(CaptureErr::Wedged(msg)) => {
                eprintln!("[ios-screenshot] dvt capture wedged: {msg}");
                record_wedge(udid);
                record_channel_close(udid, CHANNEL_RECYCLE_GAP);
                drop_dvt_session(udid, &session);
                return Err(anyhow!(
                    "iOS screenshot failed: the device screenshot encoder wedged on the \
                     current screen content ({msg}); wait for the screen to change, or start \
                     AirPlay mirroring (live frames do not use the encoder)"
                ));
            }
            Err(CaptureErr::Stalled(msg)) => {
                eprintln!("[ios-screenshot] dvt capture attempt {attempt} stalled: {msg}");
                // The device needs the longer gap to clean up the killed
                // mid-flight capture; the retry below waits it out inside
                // spawn_dvt_session (wait_channel_gap), so a stall costs one
                // gap + one fresh capture instead of failing the command.
                record_channel_close(udid, STALL_RECYCLE_GAP);
                drop_dvt_session(udid, &session);
                last_fail = msg;
            }
            Err(CaptureErr::Gone(e)) => {
                // Treat any teardown of a served channel as a used-channel
                // close: the rebuild must respect the recycle gap.
                record_channel_close(udid, CHANNEL_RECYCLE_GAP);
                drop_dvt_session(udid, &session);
                eprintln!("[ios-screenshot] dvt capture attempt {attempt} failed: {e}");
                last_fail = e;
            }
        }
    }
    Err(anyhow!(
        "iOS screenshot failed (dvt channel unusable): {last_fail}. Wait a few seconds \
         and retry, or start AirPlay mirroring (live frames do not use the encoder)"
    ))
}

/// Send one capture request to the session actor and wait for its reply.
async fn capture_via(s: &DvtSession) -> Result<Vec<u8>, CaptureErr> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    s.req
        .send(DvtRequest::Capture(tx))
        .map_err(|_| CaptureErr::Gone("dvt session is gone".to_string()))?;
    match tokio::time::timeout(CAPTURE_TIMEOUT, rx).await {
        Ok(Ok(res)) => res.map_err(|e| match e {
            CaptureFail::Wedged(msg) => CaptureErr::Wedged(msg),
            CaptureFail::Stalled(msg) => CaptureErr::Stalled(msg),
            CaptureFail::Other(msg) => CaptureErr::Gone(msg),
        }),
        // Actor dropped the reply without answering (it died mid-request).
        Ok(Err(_)) => Err(CaptureErr::Gone(
            "dvt session dropped the request".to_string(),
        )),
        // Slow capture: give up on the reply but leave the channel alone.
        // Never kill here - even a fresh channel's first capture can be a
        // legitimately slow encode, and killing it mid-flight poisons the
        // device-side service. The actor watchdog is the only killer.
        Err(_) => Err(CaptureErr::TimedOut),
    }
}

/// Build a fresh persistent channel and spawn its actor. Waits out the
/// recycle gap first; the caller registers the session in the map.
async fn spawn_dvt_session(udid: &str) -> Result<Arc<DvtSession>, String> {
    wait_channel_gap(udid).await;
    let provider = provider_for(udid)
        .await
        .map_err(|e| format!("device unavailable: {e:#}"))?;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let kill = Arc::new(Notify::new());
    let session = Arc::new(DvtSession {
        req: tx,
        kill: kill.clone(),
    });
    let (built_tx, built_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(run_dvt_actor(
        udid.to_string(),
        provider,
        kill,
        rx,
        built_tx,
    ));
    match tokio::time::timeout(BUILD_TIMEOUT, built_rx).await {
        Ok(Ok(Ok(()))) => Ok(session),
        Ok(Ok(Err(e))) => Err(e),
        // Actor exited without reporting (task panicked, etc.).
        Ok(Err(_)) => Err("dvt actor exited without reporting".to_string()),
        Err(_) => Err("dvt session build timed out".to_string()),
    }
}

/// Owns the tunnel, instruments server and screenshot client for one device.
/// Exits (releasing everything) when the channel dies, is killed, or all
/// senders are gone.
async fn run_dvt_actor(
    udid: String,
    provider: Box<dyn IdeviceProvider>,
    kill: Arc<Notify>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<DvtRequest>,
    built: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let t = Instant::now();
    let (_adapter, mut server, rx_bytes) = match build_dvt_server(provider.as_ref()).await {
        Ok(x) => x,
        Err(e) => {
            let _ = built.send(Err(e));
            return;
        }
    };
    let mut client = match ScreenshotClient::new(&mut server).await {
        Ok(c) => c,
        Err(e) => {
            let _ = built.send(Err(format!("screenshot client init failed: {e}")));
            return;
        }
    };
    let _ = built.send(Ok(()));
    eprintln!(
        "[ios-screenshot] dvt session up in {:?} (persistent channel)",
        t.elapsed()
    );

    let mut used = false;
    'sessions: loop {
        // Wait for the first request of this capture round.
        let req = tokio::select! {
            _ = kill.notified() => break 'sessions,
            req = rx.recv() => match req {
                None => break 'sessions,
                Some(r) => r,
            },
        };
        let DvtRequest::Capture(reply) = req;

        // Run the capture. Requests arriving while it is in flight share
        // this capture's result instead of queueing a fresh device-side
        // encode: with 1s polling and ~6s encodes, queuing would make the
        // device encode back-to-back frames that are stale on arrival.
        // No short timeout on the FIRST capture: a cold encode of fresh rich
        // content is legitimately slow on-device. Later captures on a served
        // channel get the tighter budget - they are cache hits or screen-
        // change re-encodes, anything past that is a wedge.
        let budget = if used {
            SERVED_CAPTURE_DEADLINE
        } else {
            ACTOR_CAPTURE_DEADLINE
        };
        let mut waiters = vec![reply];
        let mut fut = Box::pin(client.take_screenshot());
        let deadline = tokio::time::Instant::now() + budget;
        // Progress watchdog, sampled twice a second. Two phases with
        // different tolerances: before the first screenshot byte arrives the
        // device is encoding, which can legitimately stay silent for seconds
        // (WEDGE_SILENCE); once bytes ARE flowing a healthy encode streams
        // continuously, so mid-stream quiet for STREAM_STALL is a stall
        // verdict - the observed failure mode (transfers stopping partway,
        // 3x at ~4.0MB of ~7MB) is caught within seconds instead of at the
        // absolute deadline. The deadline stays as the backstop for a
        // capture that keeps trickling without ever finishing.
        let started = Instant::now();
        let capture_start_rx = rx_bytes.load(Ordering::Relaxed);
        let mut sampled_rx = capture_start_rx;
        let mut last_progress: Option<Instant> = None;
        let mut progress_tick = tokio::time::interval(Duration::from_millis(500));
        progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let shot = loop {
            tokio::select! {
                _ = kill.notified() => break 'sessions,
                _ = tokio::time::sleep_until(deadline) => {
                    let got = rx_bytes.load(Ordering::Relaxed) - capture_start_rx;
                    let fail = if got > 0 {
                        CaptureFail::Stalled(format!(
                            "capture stalled after {budget:?}{}",
                            progress_detail(got, last_progress)
                        ))
                    } else {
                        CaptureFail::Wedged(format!("capture wedged for {budget:?}"))
                    };
                    for w in waiters.drain(..) {
                        let _ = w.send(Err(fail.clone()));
                    }
                    break 'sessions;
                }
                _ = progress_tick.tick() => {
                    let now_rx = rx_bytes.load(Ordering::Relaxed);
                    if now_rx > sampled_rx {
                        sampled_rx = now_rx;
                        last_progress = Some(Instant::now());
                        continue;
                    }
                    let quiet_for = last_progress
                        .map(|t| t.elapsed())
                        .unwrap_or_else(|| started.elapsed());
                    let tolerance = if last_progress.is_some() { STREAM_STALL } else { WEDGE_SILENCE };
                    if quiet_for < tolerance {
                        continue;
                    }
                    let got = now_rx - capture_start_rx;
                    let fail = if last_progress.is_some() {
                        CaptureFail::Stalled(format!(
                            "screenshot stream went quiet for {quiet_for:?}{}",
                            progress_detail(got, last_progress)
                        ))
                    } else {
                        CaptureFail::Wedged(format!("no device traffic for {quiet_for:?}"))
                    };
                    for w in waiters.drain(..) {
                        let _ = w.send(Err(fail.clone()));
                    }
                    break 'sessions;
                }
                r = &mut fut => break r,
                req = rx.recv() => match req {
                    None => break 'sessions,
                    Some(DvtRequest::Capture(tx)) => waiters.push(tx),
                },
            }
        };
        match shot {
            Ok(png) => {
                used = true;
                for w in waiters.drain(..) {
                    let _ = w.send(Ok(png.clone()));
                }
            }
            Err(e) => {
                let msg = format!("screenshot failed: {e}");
                for w in waiters.drain(..) {
                    let _ = w.send(Err(CaptureFail::Other(msg.clone())));
                }
                break 'sessions;
            }
        }
    }

    // A channel that served captures must not be followed by a new one too
    // quickly - record the close for the recycle gap (the caller records it
    // too; the max of the two deadlines wins, so a stall's longer gap is
    // never shortened by this).
    if used {
        record_channel_close(&udid, CHANNEL_RECYCLE_GAP);
        eprintln!("[ios-screenshot] dvt session closed after use");
    }
}

/// Human-readable progress context for a kill message: how many bytes the
/// capture received and, when known, how long ago the last byte arrived
/// (separates "still trickling" from "stopped partway").
fn progress_detail(got: u64, last_progress: Option<Instant>) -> String {
    match last_progress {
        Some(t) => format!(" ({got} bytes received, last byte {:?} ago)", t.elapsed()),
        None => format!(" ({got} bytes received)"),
    }
}

/// Build the CoreDeviceProxy tunnel, RSD handshake and instruments server
/// (everything except the screenshot channel itself). The adapter is returned
/// alongside because the streams ride on it - dropping it tears down the
/// tunnel, so the caller must keep it alive. The returned counter tallies
/// every byte the device sends on the instruments connection, for the actor's
/// wedge watchdog.
pub(crate) async fn build_dvt_server(
    provider: &dyn IdeviceProvider,
) -> Result<
    (
        AdapterHandle,
        RemoteServerClient<Box<dyn ReadWrite>>,
        Arc<AtomicU64>,
    ),
    String,
> {
    let proxy = CoreDeviceProxy::connect(provider)
        .await
        .map_err(|e| format!("CoreDeviceProxy connect failed: {e}"))?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|e| format!("failed to create software tunnel: {e}"))?
        .to_async_handle();

    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|e| format!("failed to connect to RSD port: {e}"))?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|e| format!("RSD handshake failed: {e}"))?;

    // Resolve the Instruments service port from the handshake. Going through
    // the `RsdService` trait fails the Send check ("implementation ... is not
    // general enough"), because its `connect_rsd` RPITIT is not Send-bounded.
    const DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";
    let service_port = handshake
        .services
        .get(DTSERVICEHUB)
        .map(|s| s.port)
        .ok_or_else(|| {
            "Remote Server service not found; enable Developer Mode on the device \
             (Settings > Privacy & Security > Developer Mode)"
                .to_string()
        })?;

    let stream = adapter
        .connect(service_port)
        .await
        .map_err(|e| format!("failed to connect to Instruments service: {e}"))?;
    let rx_bytes = Arc::new(AtomicU64::new(0));
    let counted = CountingStream {
        inner: stream,
        rx_bytes: rx_bytes.clone(),
    };
    let mut server = RemoteServerClient::new(Box::new(counted) as Box<dyn ReadWrite>);
    server
        .read_message(0)
        .await
        .map_err(|e| format!("failed to read initial message: {e}"))?;
    Ok((adapter, server, rx_bytes))
}

fn log_stage(stage: &str, start: Instant) {
    eprintln!("[ios-screenshot] {stage}: {:?}", start.elapsed());
}

/// Version unknown: try the classic service first, then the DVT path. Only the
/// modern route can be prepared afterwards - picking a classic image needs the
/// `major.minor` this device just failed to tell us.
async fn take_screenshot_any(udid: &str, provider: Box<dyn IdeviceProvider>) -> Result<Vec<u8>> {
    match classic_capture(provider.as_ref(), udid).await {
        Ok(png) => return Ok(png),
        Err(e) if is_service_missing(&format!("{e:#}")) => {
            eprintln!("[ios-screenshot] screenshotr has no service; trying the modern path first");
        }
        Err(e) => eprintln!("[ios-screenshot] screenshotr failed: {e:#}"),
    }
    match capture_modern(udid).await {
        Ok(png) => Ok(png),
        Err(e) if is_service_missing(&format!("{e:#}")) => {
            prepare_for_capture(udid, crate::developer_image::Route::Personalized, e).await?;
            capture_modern(udid).await
        }
        Err(e) => Err(e),
    }
}

/// Cached iOS version for the device, as the device reports it (`16.7.12`).
/// Both the screenshot route and the developer image it may need are chosen
/// from this, so the raw string is what gets cached.
async fn device_version(provider: &dyn IdeviceProvider, udid: &str) -> Result<String> {
    if let Some(v) = version_cache().lock().await.get(udid) {
        return Ok(v.clone());
    }
    let mut lockdown = LockdownClient::connect(provider)
        .await
        .map_err(|e| anyhow!("lockdown connect failed: {e} (is the device paired?)"))?;
    let version = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(|e| anyhow!("failed to read ProductVersion: {e}"))?;
    let raw = version
        .as_string()
        .ok_or_else(|| anyhow!("unexpected ProductVersion format"))?
        .to_string();
    if crate::developer_image::major_of(&raw).is_none() {
        return Err(anyhow!("unexpected ProductVersion: {raw}"));
    }
    version_cache().lock().await.insert(udid.to_string(), raw.clone());
    Ok(raw)
}

async fn screenshot_classic(provider: &dyn IdeviceProvider) -> Result<Vec<u8>> {
    let mut service = ScreenshotService::connect(provider)
        .await
        .map_err(|e| anyhow!("screenshotr connect failed: {e} (is the device paired?)"))?;
    service
        .take_screenshot()
        .await
        .map_err(|e| anyhow!("screenshot failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two answers a device without its image gives, and the one thing
    /// that must not be mistaken for them: a device that is simply not paired.
    /// This predicate decides whether to prepare an image, so a false positive
    /// downloads 16 MB over somebody's phone for nothing, and a false negative
    /// walks the user into a mirror that cannot take a screenshot (both
    /// happened on 2026-09-16; the wording is the contract).
    #[test]
    fn only_a_missing_service_looks_like_a_missing_service() {
        // Verbatim from an iPhone 16.7.12, switch off, no image mounted.
        assert!(is_service_missing(
            "unknown error `InvalidService` returned from device"
        ));
        // The same device with the switch on and still no image.
        assert!(is_service_missing("device socket io failed"));
        // The modern route, from the RSD service list.
        assert!(is_service_missing(
            "Remote Server service not found; enable Developer Mode on the device"
        ));

        assert!(!is_service_missing("InvalidHostID"));
        assert!(!is_service_missing("failed to read ProductVersion: not paired"));
        assert!(!is_service_missing("device not found"));
    }
}
