//! The developer disk image, and why the app has to care about it.
//!
//! On iOS 17+ the services the tunnel is built on - `displayservice`,
//! `screencaptureservice`, the HID pair and `pasteboardservice` - are not
//! published by the system. They come from the developer disk image the
//! device has mounted, so an iOS update invalidates the one that was there
//! (it was personalized for the previous build) and the tunnel then fails
//! with "device does not offer com.apple.coredevice.displayservice".
//!
//! Developer Mode cuts both ways, and the difference matters: a mounted,
//! current image keeps the services published with it off (verified on iOS 27,
//! all five, switch off), but **installing** one is refused without it - the
//! device answers `ImageMountFailed` (verified the same day, switch off and no
//! image). So the status is read only to explain a failed mount, never to
//! refuse the attempt: a device whose image is already fine should not be told
//! to go flip a switch it does not need. When it *is* off and the image is
//! stale, the app turns it on itself (the same AMFI request a person flipping
//! the switch in Settings makes) after a short countdown, because on iOS 18+
//! that restarts the device.
//!
//! Mounting is a lockdown service (`com.apple.mobile.mobile_image_mounter`),
//! so it needs no tunnel of its own: the image is downloaded, personalized by
//! Apple's TSS (the `tss` feature of the vendored idevice crate does that),
//! uploaded and mounted. The personalized image itself is Xcode's own DDI
//! bundle, mirrored on GitHub - the same source pymobiledevice3 uses, since
//! Apple publishes it through Xcode rather than a stable URL.
//!
//! There are two of these images, and which one a device wants is its iOS
//! version ([`Route`]): pre-17 wants the classic `DeveloperDiskImage.dmg` at
//! `/Developer`, which is what `screenshotr` needs - and it needs it *as well*
//! as Developer Mode, not instead of it (verified on 16.7.12). 17 and up want
//! the personalized bundle at `/System/Developer`, which is where the tunnel's
//! services and the DVT screenshot channel come from. Both come from the same
//! repository, so they share the download bases and the cache directory.
//!
//! This module owns the whole of it: check, download, mount, report.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use idevice::lockdown::LockdownClient;
use idevice::services::amfi::AmfiClient;
use idevice::services::mobile_image_mounter::ImageMounter;
use idevice::{IdeviceError, IdeviceService};

use crate::device::provider_for;

/// Which image a device needs, and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// iOS 16 and earlier: the classic image, for this `major.minor` version
    /// (`16.7`), mounted at `/Developer`.
    Classic { version: String },
    /// iOS 17 and later: the personalized bundle at `/System/Developer`.
    Personalized,
}

/// The image a device of this iOS version wants. An unreadable version gets
/// the classic one, matching how screenshots fall back when they cannot read
/// it either.
pub fn route_for(ios_version: &str) -> Route {
    match major_of(ios_version) {
        Some(major) if major >= 17 => Route::Personalized,
        _ => Route::Classic {
            version: major_minor_of(ios_version),
        },
    }
}

impl Route {
    pub fn mount_path(&self) -> &'static str {
        match self {
            Route::Classic { .. } => CLASSIC_MOUNT_PATH,
            Route::Personalized => PERSONALIZED_MOUNT_PATH,
        }
    }

    /// The image type the mounter protocol expects: the classic API calls it
    /// `Developer`, the personalized one `Personalized`.
    fn image_type(&self) -> &'static str {
        match self {
            Route::Classic { .. } => "Developer",
            Route::Personalized => "Personalized",
        }
    }
}

/// `16.7.12` -> `16`, `27` -> `27`.
pub fn major_of(ios_version: &str) -> Option<u32> {
    ios_version.trim().split('.').next()?.parse().ok()
}

/// `16.7.12` -> `16.7`; the classic images are published per minor version.
fn major_minor_of(ios_version: &str) -> String {
    let mut parts = ios_version.trim().split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        (Some(major), None) => format!("{major}.0"),
        _ => ios_version.trim().to_string(),
    }
}

/// Where the images are mounted.
const CLASSIC_MOUNT_PATH: &str = "/Developer";
const PERSONALIZED_MOUNT_PATH: &str = "/System/Developer";

/// The three payloads of the personalized bundle: remote name, local name.
/// The trust cache keeps the repository's `Image.dmg.trustcache` spelling
/// locally shortened, which is also what pymobiledevice3 caches.
const PAYLOADS: &[(&str, &str)] = &[
    ("BuildManifest.plist", "BuildManifest.plist"),
    ("Image.dmg", "Image.dmg"),
    ("Image.dmg.trustcache", "Image.trustcache"),
];

/// The classic bundle: the image and Apple's signature over it.
const CLASSIC_PAYLOADS: &[(&str, &str)] = &[
    ("DeveloperDiskImage.dmg", "DeveloperDiskImage.dmg"),
    ("DeveloperDiskImage.dmg.signature", "DeveloperDiskImage.signature"),
];

/// The bundle inside the repository, and the default place to get it from.
const DDI_DIR: &str = "PersonalizedImages/Xcode_iOS_DDI_Personalized";
const CLASSIC_DIR: &str = "DeveloperDiskImages";
const DDI_BASE: &str = "https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main";
/// Mirrors of the same repository, tried in order when the one before it
/// does not answer: raw.githubusercontent.com is unreachable from some
/// networks (it is from mainland China often enough to matter), and these
/// two are plain reverse proxies of it.
const DDI_BASE_FALLBACKS: &[&str] = &[
    "https://gh-proxy.com/https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main",
    "https://ghfast.top/https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main",
];

/// How long after preparing an image that device is left alone.
///
/// Callers retry on timers - the app's AirPlay preview asks for a screenshot
/// every second - and a preparation that did not fix things would otherwise be
/// repeated every second: a download and an upload each time, and, when the
/// switch is off, a request that restarts the phone. Once a minute is fast
/// enough to feel automatic and slow enough that nothing can thrash.
const PREPARE_COOLDOWN: Duration = Duration::from_secs(60);

/// The failure the UI words itself: the switch is off, and only a person may
/// turn it on. The wording is the contract - the marker travels through
/// `EnsureError`'s Display and anyhow's context, and the front end finds it in
/// the error text and answers with its "turn on Developer Mode?" dialog.
pub const DEVELOPER_MODE_OFF_MARK: &str = "developer mode is off on the device";

/// Why the request was refused when the device has a passcode set: iOS 27
/// answers a remote enable with `Device has a passcode set` and does nothing
/// (iOS 16.7.12 accepts the same request, so this is version-dependent). The
/// switch still works *by hand* - Settings asks for the passcode there - so
/// the answer is a different sentence for the user, not a retry.
pub const PASSCODE_MARK: &str = "the device has a passcode set";

/// Progress, for a line in the UI while the user waits.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageProgress {
    Checking,
    Downloading { got: u64, total: Option<u64> },
    /// The upload runs in 64 KiB chunks, so this is that counter, not bytes.
    Mounting { sent: u64, total: u64 },
    Mounted { id: String, downloaded: bool },
}

/// Why preparing the image did not work. The variants are separate because
/// each one wants a different sentence in front of the user.
#[derive(Debug)]
pub enum EnsureError {
    /// Developer Mode is off, and installing an image needs it. The app never
    /// flips the switch on its own: a front end with a user in front of it
    /// offers (see [`enable_developer_mode`]), and everyone else reports this.
    DeveloperModeOff,
    /// This device's image was prepared moments ago and whatever is still
    /// broken is not something another download and upload will fix. Only
    /// timed callers see this.
    TooSoon { waited: Duration },
    Download(String),
    Mount(String),
}

impl std::fmt::Display for EnsureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnsureError::DeveloperModeOff => write!(f, "{DEVELOPER_MODE_OFF_MARK}"),
            EnsureError::TooSoon { waited } => write!(
                f,
                "this device's developer image was prepared {}s ago; waiting {}s before trying again",
                waited.as_secs(),
                PREPARE_COOLDOWN.saturating_sub(*waited).as_secs()
            ),
            EnsureError::Download(e) => write!(f, "{e}"),
            EnsureError::Mount(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EnsureError {}

pub struct EnsureResult {
    /// The bundle's name: an Apple build id for personalized images, the
    /// `major.minor` version for classic ones.
    pub id: String,
    pub downloaded: bool,
}

/// Where preparation progress is reported, when anyone is listening. The app
/// sets one at startup so the same line appears whichever path asked for an
/// image - a tunnel start, a screenshot that came up short; a probe sets one to
/// print. Unset means the work simply happens quietly, which is what the
/// standalone MCP server and the CLIs want.
static SINK: OnceLock<Box<dyn Fn(ImageProgress) + Send + Sync>> = OnceLock::new();

pub fn set_progress_sink(sink: impl Fn(ImageProgress) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

pub(crate) fn report(progress: ImageProgress) {
    if let Some(sink) = SINK.get() {
        sink(progress);
    }
}

/// When each device's image was last prepared, so timed callers cannot make a
/// failed preparation repeat every second.
static PREPARED_AT: OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>> =
    OnceLock::new();

fn prepared_at() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>> {
    PREPARED_AT.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// How long ago this device's image was prepared, when that was recent enough
/// to still count.
fn prepared_recently(udid: &str) -> Option<Duration> {
    let at = *prepared_at().lock().unwrap().get(udid)?;
    let waited = at.elapsed();
    (waited < PREPARE_COOLDOWN).then_some(waited)
}

fn mark_prepared(udid: &str) {
    prepared_at()
        .lock()
        .unwrap()
        .insert(udid.to_string(), std::time::Instant::now());
}

/// Devices this run has already turned Developer Mode on for.
///
/// The device restarts when that happens, and while it is coming back up the
/// switch can read as off again - which, asked again, restarts it again, and
/// again: a phone that never finishes booting. So it is once per device per
/// run, ever, and a second attempt is the user's to make by hand.
static ENABLED: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn enabled_already(udid: &str) -> bool {
    ENABLED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap()
        .contains(udid)
}

fn mark_enabled(udid: &str) {
    ENABLED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap()
        .insert(udid.to_string());
}

/// Where downloaded images are kept. The app points this at its own data
/// directory; anything else lands under the temp directory, which costs a
/// re-download and nothing else.
static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn use_cache_dir(dir: impl Into<PathBuf>) {
    let _ = CACHE_DIR.set(dir.into());
}

pub(crate) fn cache_dir() -> PathBuf {
    CACHE_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("xphoneuse-developer-images"))
}

/// Make sure the device has the developer image `route` asks for.
///
/// Called when something came up short: a tunnel that cannot offer the
/// services the image provides, a screenshot whose service is not there.
/// Nothing here costs anything on a device whose image is already mounted and
/// current - the failure is what triggers the work, and a cached bundle is
/// only ever re-uploaded, never re-downloaded.
///
/// **This never turns Developer Mode on** and never restarts the phone, no
/// matter who calls it or how often: the switch is off, that is all it says,
/// and a front end with a user in front of it may offer to flip it (see
/// [`enable_developer_mode`]). That is on purpose - this runs from timers
/// (the app previews screenshots once a second), and anything that could
/// restart the phone from a timer turns a booting device into a device that
/// never finishes booting.
pub async fn ensure(udid: &str, route: Route) -> Result<EnsureResult, EnsureError> {
    // Timed callers must not turn a broken device into a download loop, so
    // the work happens at most once a minute per device.
    if let Some(waited) = prepared_recently(udid) {
        return Err(EnsureError::TooSoon { waited });
    }

    report(ImageProgress::Checking);

    let provider = provider_for(udid)
        .await
        .map_err(|e| EnsureError::Mount(format!("device {udid}: {e}")))?;

    // Developer Mode is read but NOT required *for a device whose image is
    // fine*: a current image keeps publishing every service we need with the
    // switch off (verified on iOS 27 with it off). Reaching this function at
    // all, though, means the device is not offering them - its image is stale
    // or missing - and installing one is exactly what needs the switch, for
    // the classic image too (verified on 16.7.12). Off means the mount would
    // be refused anyway (`ImageMountFailed`), so nothing is downloaded.
    let developer_mode = read_developer_mode(&provider).await;
    if developer_mode == Some(false) {
        return Err(EnsureError::DeveloperModeOff);
    }

    // From here the work is real, so the device goes on the cooldown: what
    // follows is a download and an upload that a second attempt would repeat.
    mark_prepared(udid);

    let bundle = fetch_bundle(&route, &cache_dir())
        .await
        .map_err(EnsureError::Download)?;

    // iOS 17+: the personalized image's TSS identities are keyed by the
    // device's `ApChipID`/`ApBoardID` pair, and the mirrored Xcode only
    // carries the hardware that existed when Apple built it. A newer device
    // has no identity there at all (`BadBuildManifest`), and only the Cryptex1
    // variant - what Xcode installs for those devices - can be personalized.
    if let Bundle::Personalized { manifest, .. } = &bundle {
        let (board, chip) = board_chip(&provider).await.map_err(EnsureError::Mount)?;
        if !manifest_has_identity(manifest, board, chip) {
            eprintln!(
                "[ddi] {udid}: no personalized identity for board {board:#X} chip {chip:#X} \
                 - installing the Cryptex DDI instead"
            );
            let assets = crate::cryptex::fetch_assets(&cache_dir())
                .await
                .map_err(EnsureError::Download)?;
            crate::cryptex::install(&provider, &assets)
                .await
                .map_err(EnsureError::Mount)?;
            return Ok(EnsureResult {
                id: format!("cryptex-{}", assets.build),
                downloaded: assets.downloaded,
            });
        }
    }

    // No Developer Mode translation here: an off switch never gets this far.
    mount(&provider, &bundle).await.map_err(EnsureError::Mount)?;

    Ok(EnsureResult {
        id: bundle.id().to_string(),
        downloaded: bundle.downloaded(),
    })
}

/// Make the Developer Mode row appear in the device's Settings.
///
/// A device that has never been used for development does not show it at all
/// (iOS creates the row from a flag AMFI sets - `AMFIShowOverridePath`), which
/// makes the manual route the app hands over to impossible to follow: the user
/// goes looking for a switch that is not on the screen. Xcode does this by
/// being connected; the reveal action does it over the wire.
pub async fn reveal_developer_mode(udid: &str) -> Result<(), String> {
    let provider = provider_for(udid)
        .await
        .map_err(|e| format!("device {udid}: {e}"))?;
    let mut amfi = AmfiClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("amfi: {e:?}"))?;
    amfi.reveal_developer_mode_option_in_ui()
        .await
        .map_err(|e| format!("reveal: {e:?}"))
}

/// Whether Developer Mode is on, when the device will say. `None` means the
/// question could not be asked, which is not the same as "off".
pub async fn developer_mode_status(udid: &str) -> Option<bool> {
    let provider = provider_for(udid).await.ok()?;
    read_developer_mode(&provider).await
}

/// Turn Developer Mode on: the same request a person flipping the switch in
/// Settings makes, and on iOS 18+ it restarts the device on the spot.
///
/// Deliberately not called from [`ensure`] or from any other automatic path -
/// whoever calls this owns the warning that has to come first. The app asks in
/// a dialog with a countdown the user can cancel; a phone restarting itself
/// because a timer asked is not a thing this codebase does any more.
///
/// Once per device per run: the device is down for a minute after this, and
/// while it is coming back the switch can read as off again, so a second
/// request would restart it a second time.
pub async fn enable_developer_mode(udid: &str) -> Result<(), String> {
    if enabled_already(udid) {
        return Err(format!(
            "Developer Mode was already turned on for {udid} in this run; \
             if it still reads as off, it has to be flipped by hand"
        ));
    }

    // 1. Ask. The device answers, then goes down to apply it.
    let provider = provider_for(udid)
        .await
        .map_err(|e| format!("device {udid}: {e}"))?;
    enable_developer_mode_with(udid, &provider).await?;
    eprintln!("[ddi] {udid}: Developer Mode enable accepted; waiting out the restart");

    // 2. Wait out the restart - and it has to restart: an enable that is going
    //    to take effect takes the device down within seconds.
    wait_for_restart(udid).await?;

    // 3. Answer the one-time confirmation the restart ends with. This is the
    //    step that makes the switch stick: without it the device comes back
    //    reporting Developer Mode as off, which is exactly how it looked the
    //    first time this was tried (and why asking again just restarted the
    //    phone again).
    //    Same exchange as `pymobiledevice3 amfi enable-developer-mode`, which
    //    also waits for the reconnect and then confirms.
    let provider = provider_for(udid)
        .await
        .map_err(|e| format!("device {udid} after its restart: {e}"))?;
    let mut amfi = AmfiClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("amfi after the restart: {e:?}"))?;
    amfi.accept_developer_mode()
        .await
        .map_err(|e| format!("confirming Developer Mode after the restart: {e:?}"))?;
    eprintln!("[ddi] {udid}: restart confirmed");

    // 4. Read it back. A switch that still says off is reported rather than
    //    left for the user to discover, and the caller's answer to that is the
    //    manual route.
    for _ in 0..6 {
        let Ok(provider) = provider_for(udid).await else {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        if read_developer_mode(&provider).await == Some(true) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err("Developer Mode was confirmed but the device still reports it as off".to_string())
}

/// Wait for the device to go away and come back, which is what the enable
/// request sets off. Two consecutive misses count as "gone", so a mux hiccup
/// is not mistaken for the restart.
async fn wait_for_restart(udid: &str) -> Result<(), String> {
    let gone_by = Instant::now() + Duration::from_secs(60);
    let mut misses = 0;
    loop {
        if Instant::now() > gone_by {
            return Err("the device never restarted; Developer Mode was not applied".to_string());
        }
        if provider_for(udid).await.is_err() {
            misses += 1;
            if misses >= 2 {
                break;
            }
        } else {
            misses = 0;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let back_by = Instant::now() + Duration::from_secs(180);
    while Instant::now() < back_by {
        if provider_for(udid).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err("the device did not come back after its restart".to_string())
}

/// Whether Developer Mode is on, when the device will say. `None` means the
/// question could not be asked, which is not the same as "off", and is treated
/// as "let the mount decide".
async fn read_developer_mode(provider: &crate::device::UsbmuxdProviderBox) -> Option<bool> {
    match AmfiClient::connect(provider.as_ref()).await {
        Ok(mut amfi) => match amfi.get_developer_mode_status().await {
            Ok(on) => Some(on),
            Err(e) => {
                eprintln!("[ddi] developer mode status unavailable: {e:?}");
                None
            }
        },
        Err(e) => {
            eprintln!("[ddi] amfi unavailable: {e:?}");
            None
        }
    }
}

/// The request itself. If the device refuses it, the option is revealed in
/// Settings as a fallback - on a device that never saw Xcode the switch is not
/// there to find.
async fn enable_developer_mode_with(
    udid: &str,
    provider: &crate::device::UsbmuxdProviderBox,
) -> Result<(), String> {
    let mut amfi = AmfiClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("amfi: {e:?}"))?;

    match amfi.enable_developer_mode().await {
        Ok(()) => {
            // Marked before anything else can ask again: the device is about
            // to go down, and it must not be asked to do this twice.
            mark_enabled(udid);
            Ok(())
        }
        Err(e) => {
            // The option is revealed in Settings no matter why this failed: a
            // device that never saw Xcode does not even show the switch, and
            // the manual route is what an unreachable one has left.
            let _ = amfi.reveal_developer_mode_option_in_ui().await;
            Err(match e {
                // The one refusal worth naming: it is not a failure to retry,
                // it is the device saying the switch has to be flipped by
                // hand. Everything else is reported with its protocol shape.
                idevice::IdeviceError::UnknownErrorType(message)
                    if message.to_ascii_lowercase().contains("passcode") =>
                {
                    format!("{PASSCODE_MARK}, so it refuses the request from the host")
                }
                other => format!("{other:?}"),
            })
        }
    }
}

/// Upload `bundle` and mount it, replacing whatever is mounted now.
///
/// The unmount first is deliberate: this runs because the services are
/// missing, and a mounted image that is not providing them is exactly the
/// stale one an iOS update leaves behind. Its absence is fine too (nothing
/// mounted), so its failure is only logged.
async fn mount(provider: &crate::device::UsbmuxdProviderBox, bundle: &Bundle) -> Result<(), String> {
    let mut mounter = ImageMounter::connect(provider.as_ref())
        .await
        .map_err(|e| format!("image mounter: {e:?}"))?;

    let mount_path = bundle.route().mount_path();
    if let Err(e) = mounter.unmount_image(mount_path).await {
        // "Nothing mounted" is the ordinary answer here, and the device gives
        // it as an error - after which it stops answering on that connection
        // altogether (observed on 16.7.12: the ReceiveBytes that follows dies
        // with `early eof`, or aborts part-way through the upload). So a
        // refused unmount gets a fresh connection rather than a half-broken
        // one, and the upload below starts from a service that is listening.
        eprintln!("[ddi] unmount {mount_path}: {e:?}; reconnecting the mounter");
        mounter = ImageMounter::connect(provider.as_ref())
            .await
            .map_err(|e| format!("image mounter after a refused unmount: {e:?}"))?;
    }

    report(ImageProgress::Mounting { sent: 0, total: 0 });
    let chunk: u64 = 64 * 1024;
    let progress = |((i, n), _): ((usize, usize), ())| {
        report(ImageProgress::Mounting {
            sent: (i as u64 + 1) * chunk,
            total: (n as u64 + 1) * chunk,
        });
        async {}
    };

    match bundle {
        // The classic image is plain: bytes plus Apple's signature over them,
        // no TSS ticket and no trust cache.
        Bundle::Classic { image, signature, .. } => {
            mounter
                .upload_image_with_progress(
                    bundle.route().image_type(),
                    image,
                    signature.clone(),
                    progress,
                    (),
                )
                .await
                .map_err(|e| format!("upload {mount_path}: {e:?}"))?;
            mounter
                .mount_image(bundle.route().image_type(), signature.clone(), None, None)
                .await
                .map_err(|e| format!("mount {mount_path}: {e:?}"))?;
        }
        // The personalized one is signed for this device: the TSS request
        // needs its chip id.
        Bundle::Personalized {
            image,
            trustcache,
            manifest,
            ..
        } => {
            let chip_id = unique_chip_id(provider).await?;
            let mounted = mounter
                .mount_personalized_with_callback(
                    provider.as_ref(),
                    image.clone(),
                    trustcache.clone(),
                    manifest,
                    None,
                    chip_id,
                    progress,
                    (),
                )
                .await;
            if let Err(e) = mounted {
                // The TSS identity is keyed by the device's chip and board
                // ids, and the mirror's personalized DDI only carries the
                // hardware that existed when Apple built it. Name the device
                // that went unmatched - "BadBuildManifest" alone reads like a
                // corrupt download.
                if matches!(e, IdeviceError::BadBuildManifest) {
                    return Err(format!(
                        "mount {mount_path}: the developer image has no build identity for this \
                         device ({}), so it predates this model - a newer DDI is needed \
                         (XPHONEUSE_DDI_BASE can point at one)",
                        identity_of(provider).await
                    ));
                }
                return Err(format!("mount {mount_path}: {e:?}"));
            }
        }
    }

    report(ImageProgress::Mounted {
        id: bundle.id().to_string(),
        downloaded: bundle.downloaded(),
    });
    Ok(())
}

/// The device's `ApBoardID`/`ApChipID` pair, which keys every personalized
/// DDI build identity.
async fn board_chip(provider: &crate::device::UsbmuxdProviderBox) -> Result<(u64, u64), String> {
    let mut lockdown = LockdownClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("lockdown: {e:?}"))?;
    let board = lockdown
        .get_value(Some("BoardId"), None)
        .await
        .map_err(|e| format!("BoardId: {e:?}"))?;
    let chip = lockdown
        .get_value(Some("ChipID"), None)
        .await
        .map_err(|e| format!("ChipID: {e:?}"))?;
    let number = |value: &plist::Value, key: &str| -> Result<u64, String> {
        match value {
            plist::Value::Integer(n) => n
                .as_unsigned()
                .ok_or_else(|| format!("{key} is not an unsigned integer")),
            _ => Err(format!("{key} is not an integer")),
        }
    };
    Ok((number(&board, "BoardId")?, number(&chip, "ChipID")?))
}

/// Whether the personalized manifest carries an identity for this hardware.
/// An unreadable manifest counts as "yes": that keeps the old behaviour (and
/// its error) rather than sending every device down the cryptex path.
fn manifest_has_identity(manifest: &[u8], board: u64, chip: u64) -> bool {
    fn number(value: Option<&plist::Value>) -> Option<u64> {
        match value? {
            plist::Value::Integer(n) => n.as_unsigned(),
            plist::Value::String(s) => {
                let trimmed = s.trim_start_matches("0x");
                let radix = if trimmed.len() == s.len() { 10 } else { 16 };
                u64::from_str_radix(trimmed, radix).ok()
            }
            _ => None,
        }
    }

    let Ok(value) = plist::from_bytes::<plist::Value>(manifest) else {
        return true;
    };
    let Some(identities) = value
        .as_dictionary()
        .and_then(|d| d.get("BuildIdentities"))
        .and_then(|v| v.as_array())
    else {
        return true;
    };
    identities.iter().any(|identity| {
        identity.as_dictionary().is_some_and(|identity| {
            number(identity.get("ApBoardID")) == Some(board)
                && number(identity.get("ApChipID")) == Some(chip)
        })
    })
}

/// "<ProductType>, board 0x08, chip 0x8140" - the three values that decide
/// whether the personalized DDI can be signed for this device.
async fn identity_of(provider: &crate::device::UsbmuxdProviderBox) -> String {
    let mut lockdown = match LockdownClient::connect(provider.as_ref()).await {
        Ok(l) => l,
        Err(_) => return "device ids unavailable".to_string(),
    };
    let mut parts = Vec::new();
    if let Ok(value) = lockdown.get_value(Some("ProductType"), None).await {
        if let Some(name) = value.as_string() {
            parts.push(name.to_string());
        }
    }
    for (label, key) in [("board", "BoardId"), ("chip", "ChipID")] {
        if let Ok(value) = lockdown.get_value(Some(key), None).await {
            let number = match &value {
                plist::Value::Integer(n) => n.as_unsigned(),
                _ => None,
            };
            if let Some(number) = number {
                parts.push(format!("{label} 0x{number:04X}"));
            }
        }
    }
    if parts.is_empty() {
        "device ids unavailable".to_string()
    } else {
        parts.join(", ")
    }
}

/// The device's chip id, which the TSS ticket is personalized to. The first
/// lockdown query can fail before a session is started, which is the whole
/// reason this is not a one-liner.
async fn unique_chip_id(provider: &crate::device::UsbmuxdProviderBox) -> Result<u64, String> {
    let mut lockdown = LockdownClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("lockdown: {e:?}"))?;

    let value = match lockdown.get_value(Some("UniqueChipID"), None).await {
        Ok(v) => v,
        Err(_) => {
            let pairing = provider
                .get_pairing_file()
                .await
                .map_err(|e| format!("pairing file: {e:?}"))?;
            lockdown
                .start_session(&pairing)
                .await
                .map_err(|e| format!("lockdown session: {e:?}"))?;
            lockdown
                .get_value(Some("UniqueChipID"), None)
                .await
                .map_err(|e| format!("UniqueChipID: {e:?}"))?
        }
    };

    value
        .as_unsigned_integer()
        .ok_or_else(|| "UniqueChipID is not a number".to_string())
}

/// What actually gets uploaded. The two routes carry different payloads, so
/// they are separate shapes rather than one struct with fields that are
/// sometimes empty: the mount code then cannot read the wrong one.
enum Bundle {
    Classic {
        /// `16.7`, which is also the cache directory name.
        version: String,
        image: Vec<u8>,
        signature: Vec<u8>,
        downloaded: bool,
    },
    Personalized {
        /// The bundle's Apple build id (`27A5228h`), and its cache directory.
        build: String,
        image: Vec<u8>,
        trustcache: Vec<u8>,
        manifest: Vec<u8>,
        downloaded: bool,
    },
}

impl Bundle {
    fn route(&self) -> Route {
        match self {
            Bundle::Classic { version, .. } => Route::Classic {
                version: version.clone(),
            },
            Bundle::Personalized { .. } => Route::Personalized,
        }
    }

    /// The name a [`EnsureResult`] reports.
    fn id(&self) -> &str {
        match self {
            Bundle::Classic { version, .. } => version,
            Bundle::Personalized { build, .. } => build,
        }
    }

    fn downloaded(&self) -> bool {
        match self {
            Bundle::Classic { downloaded, .. } | Bundle::Personalized { downloaded, .. } => {
                *downloaded
            }
        }
    }
}

/// The bundle for `route` on disk, downloading it when it is not there yet.
async fn fetch_bundle(route: &Route, cache_dir: &Path) -> Result<Bundle, String> {
    if let Some(bundle) = read_cached(route, cache_dir) {
        return Ok(bundle);
    }
    match route {
        Route::Classic { version } => fetch_classic(version, cache_dir).await,
        Route::Personalized => fetch_personalized(cache_dir).await,
    }
}

/// The classic bundle: two small files, published per `major.minor` version.
async fn fetch_classic(version: &str, cache_dir: &Path) -> Result<Bundle, String> {
    let dir = cache_dir.join(classic_dir_name(version));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let mut downloader = Downloader::new();
    for (remote, local) in CLASSIC_PAYLOADS {
        let path = format!("{CLASSIC_DIR}/{version}/{remote}");
        downloader.to_file(&path, &dir.join(local)).await?;
    }

    let read = |name: &str| -> Result<Vec<u8>, String> {
        std::fs::read(dir.join(name)).map_err(|e| format!("read {name}: {e}"))
    };
    Ok(Bundle::Classic {
        version: version.to_string(),
        image: read("DeveloperDiskImage.dmg")?,
        signature: read("DeveloperDiskImage.signature")?,
        downloaded: true,
    })
}

/// The personalized bundle. The manifest names the build, which is also the
/// cache directory, so it is downloaded first.
async fn fetch_personalized(cache_dir: &Path) -> Result<Bundle, String> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("create {}: {e}", cache_dir.display()))?;

    let mut downloader = Downloader::new();
    let manifest_tmp = cache_dir.join("BuildManifest.plist.part");
    downloader
        .to_file(
            &format!("{DDI_DIR}/BuildManifest.plist"),
            &manifest_tmp,
        )
        .await?;

    let manifest = std::fs::read(&manifest_tmp).map_err(|e| format!("read manifest: {e}"))?;
    let build = build_from_manifest(&manifest)?;
    let dir = cache_dir.join(&build);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::rename(&manifest_tmp, dir.join("BuildManifest.plist"))
        .map_err(|e| format!("store manifest: {e}"))?;

    for (remote, local) in PAYLOADS.iter().filter(|(r, _)| *r != "BuildManifest.plist") {
        downloader
            .to_file(&format!("{DDI_DIR}/{remote}"), &dir.join(local))
            .await?;
    }

    let read = |name: &str| -> Result<Vec<u8>, String> {
        std::fs::read(dir.join(name)).map_err(|e| format!("read {name}: {e}"))
    };
    Ok(Bundle::Personalized {
        build,
        image: read("Image.dmg")?,
        trustcache: read("Image.trustcache")?,
        manifest: read("BuildManifest.plist")?,
        downloaded: true,
    })
}

/// The classic image's cache directory. Named, not bare, so it cannot collide
/// with the personalized bundles' Apple build ids (`27A5228h`).
fn classic_dir_name(version: &str) -> String {
    format!("classic-{version}")
}

/// The cached bundle for `route`, if a complete one is there. A cache with
/// several personalized builds picks the last by name - they are Apple build
/// ids, which sort usably - and one that is missing any of its files is
/// skipped, which is also what keeps the two routes off each other's
/// directories.
fn read_cached(route: &Route, cache_dir: &Path) -> Option<Bundle> {
    let is_complete = |dir: &Path, names: &[&str]| names.iter().all(|n| dir.join(n).is_file());

    match route {
        Route::Classic { version } => {
            let dir = cache_dir.join(classic_dir_name(version));
            let names: Vec<&str> = CLASSIC_PAYLOADS.iter().map(|(_, l)| *l).collect();
            if !is_complete(&dir, &names) {
                return None;
            }
            let read = |name: &str| std::fs::read(dir.join(name)).ok();
            Some(Bundle::Classic {
                version: version.clone(),
                image: read("DeveloperDiskImage.dmg")?,
                signature: read("DeveloperDiskImage.signature")?,
                downloaded: false,
            })
        }
        Route::Personalized => {
            let mut builds: Vec<PathBuf> = std::fs::read_dir(cache_dir)
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            builds.sort();

            for dir in builds.into_iter().rev() {
                if !is_complete(&dir, &["BuildManifest.plist", "Image.dmg", "Image.trustcache"]) {
                    continue;
                }
                let read = |name: &str| std::fs::read(dir.join(name)).ok();
                let (Some(manifest), Some(image), Some(trustcache)) = (
                    read("BuildManifest.plist"),
                    read("Image.dmg"),
                    read("Image.trustcache"),
                ) else {
                    continue;
                };
                let Some(build) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string)
                else {
                    continue;
                };
                return Some(Bundle::Personalized {
                    build,
                    image,
                    trustcache,
                    manifest,
                    downloaded: false,
                });
            }
            None
        }
    }
}

/// The bundle's build id, which is what names its cache directory.
fn build_from_manifest(manifest: &[u8]) -> Result<String, String> {
    let value: plist::Value =
        plist::from_bytes(manifest).map_err(|e| format!("BuildManifest.plist: {e}"))?;
    value
        .as_dictionary()
        .and_then(|d| d.get("ProductBuildVersion"))
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .ok_or_else(|| "BuildManifest.plist has no ProductBuildVersion".to_string())
}

/// Where a bundle may be fetched from, in the order to try them.
fn bases() -> Vec<String> {
    // A custom base replaces the list rather than joining it: whoever sets it
    // wants that source (an internal mirror, a local server), not a race.
    if let Ok(custom) = std::env::var("XPHONEUSE_DDI_BASE") {
        let custom = custom.trim().trim_end_matches('/');
        if !custom.is_empty() {
            return vec![custom.to_string()];
        }
    }
    std::iter::once(DDI_BASE.to_string())
        .chain(DDI_BASE_FALLBACKS.iter().map(|b| b.to_string()))
        .collect()
}

/// Fetches a bundle's files, remembering which base worked so the next file
/// does not pay for the failed one again.
pub(crate) struct Downloader {
    client: reqwest::Client,
    bases: Vec<String>,
    preferred: usize,
}

/// rustls needs a process-level crypto provider, and reqwest is built here
/// without one of its own (`rustls-no-provider`, the feature tauri uses so
/// every TLS user in the process shares a single backend). Ring is that
/// backend - idevice compiles with it - so install it once before the first
/// request; a second install is an error worth ignoring.
fn ensure_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Downloader {
    pub(crate) fn new() -> Self {
        ensure_crypto_provider();
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            bases: bases(),
            preferred: 0,
        }
    }

    /// `remote` is relative to a base: `PersonalizedImages/.../Image.dmg`,
    /// `DeveloperDiskImages/16.7/DeveloperDiskImage.dmg`.
    pub(crate) async fn to_file(&mut self, remote: &str, dest: &Path) -> Result<(), String> {
        let mut errors = Vec::new();
        let order = (0..self.bases.len()).map(|i| (self.preferred + i) % self.bases.len());
        for index in order {
            let url = format!("{}/{remote}", self.bases[index]);
            match self.fetch(&url, dest).await {
                Ok(()) => {
                    self.preferred = index;
                    return Ok(());
                }
                Err(e) => errors.push(e),
            }
        }
        Err(format!("{remote}: {}", errors.join("; ")))
    }

    /// Streams one file to `dest`, via a `.part` next to it so an interrupted
    /// download never leaves a half file that looks cached.
    async fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{url}: http {}", response.status()));
        }
        let total = response.content_length();

        let part = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&part)
            .await
            .map_err(|e| format!("create {}: {e}", part.display()))?;
        let mut got: u64 = 0;
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| format!("{url}: {e:?}"))? {
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| format!("write {}: {e}", part.display()))?;
            got += chunk.len() as u64;
            report(ImageProgress::Downloading { got, total });
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|e| format!("flush {}: {e}", part.display()))?;
        drop(file);

        tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| format!("store {}: {e}", dest.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>ProductBuildVersion</key><string>27A5228h</string>
    <key>BuildIdentities</key><array/>
</dict></plist>"#;

    #[test]
    fn build_id_comes_from_the_manifest() {
        assert_eq!(build_from_manifest(MANIFEST.as_bytes()).unwrap(), "27A5228h");
        assert!(build_from_manifest(b"not a plist").is_err());
        assert!(build_from_manifest(
            br#"<?xml version="1.0"?><plist version="1.0"><dict/></plist>"#
        )
        .is_err());
    }

    #[test]
    fn the_bundle_is_three_payloads() {
        assert_eq!(PAYLOADS.len(), 3);
        for (remote, local) in PAYLOADS {
            assert!(!remote.is_empty() && !local.is_empty());
        }
        assert!(PAYLOADS.iter().any(|(r, _)| *r == "Image.dmg"));
        assert!(PAYLOADS.iter().any(|(_, l)| *l == "Image.trustcache"));
    }

    /// The classic bundle is the image plus Apple's signature over it - the
    /// device refuses the mount without the signature.
    #[test]
    fn the_classic_bundle_is_an_image_and_its_signature() {
        assert_eq!(CLASSIC_PAYLOADS.len(), 2);
        assert!(CLASSIC_PAYLOADS
            .iter()
            .any(|(r, _)| *r == "DeveloperDiskImage.dmg"));
        assert!(CLASSIC_PAYLOADS
            .iter()
            .any(|(r, _)| *r == "DeveloperDiskImage.dmg.signature"));
    }

    /// Which image a device wants is its iOS version, and getting it wrong
    /// means uploading a bundle the device refuses - the path is worth
    /// pinning down.
    #[test]
    fn the_route_follows_the_ios_version() {
        for version in ["16.7.12", "16.0", "15.8.3", "12.5.7"] {
            assert!(
                matches!(route_for(version), Route::Classic { .. }),
                "{version} should want the classic image"
            );
        }
        for version in ["17.0", "18.2", "26.4", "27.0"] {
            assert_eq!(route_for(version), Route::Personalized, "{version}");
        }

        // Unreadable version: the classic image, matching how a screenshot
        // falls back when it cannot read the version either.
        assert!(matches!(route_for(""), Route::Classic { .. }));
        assert!(matches!(route_for("unknown"), Route::Classic { .. }));
    }

    /// The classic images are published per `major.minor`, so the minor has to
    /// survive the trip - `16.7.12` wants the `16.7` image.
    #[test]
    fn the_classic_version_keeps_its_minor() {
        let Route::Classic { version } = route_for("16.7.12") else {
            panic!("16.7.12 is a classic device");
        };
        assert_eq!(version, "16.7");

        let Route::Classic { version } = route_for("16") else {
            panic!("16 is a classic device");
        };
        assert_eq!(version, "16.0");
    }

    /// The two routes mount in different places, and the device only accepts
    /// each image at its own path.
    #[test]
    fn each_route_mounts_where_the_device_expects_it() {
        let classic = Route::Classic {
            version: "16.7".into(),
        };
        assert_eq!(classic.mount_path(), "/Developer");
        assert_eq!(Route::Personalized.mount_path(), "/System/Developer");
    }

    /// The classic cache directory is named, so it cannot be mistaken for a
    /// personalized bundle when the personalized reader scans the cache (every
    /// one of those is an Apple build id, which starts with a digit).
    #[test]
    fn the_classic_cache_directory_is_not_a_build_id() {
        assert_eq!(classic_dir_name("16.7"), "classic-16.7");
        assert!(!classic_dir_name("16.7").starts_with(|c: char| c.is_ascii_digit()));
    }

    /// The restart marker is the contract with the caller that decides not to
    /// fall back to AirPlay, and it travels as text, so it has to survive both
    /// Display and anyhow's context formatting.
    /// A passcode refusal is the one answer that means "do it by hand": the
    /// wording is the contract with the dialog, which shows different
    /// instructions for it, so it has to survive the trip out as text.
    #[test]
    fn the_passcode_refusal_is_named() {
        let refusal = format!("{PASSCODE_MARK}, so it refuses the request from the host");
        assert!(refusal.contains(PASSCODE_MARK));
        // The vendor error is what carries the device's words; ours is on top.
        let from_device = idevice::IdeviceError::UnknownErrorType(
            "Device has a passcode set".to_string(),
        );
        assert!(format!("{from_device:?}")
            .to_ascii_lowercase()
            .contains("passcode"));
    }

    /// The front end words the "switch is off" failure itself, and it finds it
    /// in the error text - so the marker has to survive both Display and
    /// anyhow's context, all the way out to a command's `Err(String)`.
    #[test]
    fn the_developer_mode_marker_travels_with_the_error() {
        let off = EnsureError::DeveloperModeOff;
        assert!(off.to_string().contains(DEVELOPER_MODE_OFF_MARK));
        assert!(format!("tunnel start failed: {off:#}").contains(DEVELOPER_MODE_OFF_MARK));
        assert!(!EnsureError::Mount("something else".into())
            .to_string()
            .contains(DEVELOPER_MODE_OFF_MARK));
    }

    /// A device that has just had its image prepared is left alone for a
    /// while: callers retry on timers, and repeating the work every second is
    /// a download loop. Per device, and it expires.
    #[test]
    fn a_prepared_device_is_left_alone_for_a_while() {
        assert!(prepared_recently("cooldown-test-a").is_none());
        mark_prepared("cooldown-test-a");
        assert!(prepared_recently("cooldown-test-a").is_some());

        // Another device is its own business - and the cooldown is a bound,
        // not a permanent lock.
        assert!(prepared_recently("cooldown-test-b").is_none());
        assert!(PREPARE_COOLDOWN > Duration::from_secs(10));
    }

    /// Developer Mode is turned on at most once per device per run. The device
    /// restarts when it happens, and asking again while it is still booting
    /// restarts it again - the loop that leaves a phone never finishing a boot.
    #[test]
    fn developer_mode_is_only_ever_turned_on_once() {
        assert!(!enabled_already("enable-test-a"));
        mark_enabled("enable-test-a");
        assert!(enabled_already("enable-test-a"));
        assert!(!enabled_already("enable-test-b"));
    }

    /// The env override replaces the list, so a deployment with its own mirror
    /// never waits on a GitHub round trip that cannot work.
    #[test]
    fn the_base_list_follows_the_env_override() {
        let without = bases();
        assert!(without[0].starts_with("https://raw.githubusercontent.com"));

        // set_var is unsafe in 2024 edition and this crate is 2021; the test
        // is single-threaded within itself but the variable is process-wide,
        // so it is restored before the assertion finishes.
        std::env::set_var("XPHONEUSE_DDI_BASE", "http://mirror.local/ddi/");
        let with = bases();
        std::env::remove_var("XPHONEUSE_DDI_BASE");

        assert_eq!(with, vec!["http://mirror.local/ddi".to_string()]);
        assert_eq!(bases(), without);
    }
}
