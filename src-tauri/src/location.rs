//! Where the phone thinks it is.
//!
//! Two very different mechanisms behind one pair of commands:
//!
//! * **iOS** - an Instruments channel in the developer image, held open by a
//!   session in [`xphoneuse_ios::location`]. Nothing persists on the device:
//!   the position is a message on that channel, and the phone goes back to its
//!   real GPS the moment it closes.
//! * **Android** - `cmd location`'s test provider (Android 12+). This one
//!   *does* persist on the device until it is removed, so "stop" has to
//!   actually take it away rather than just stop talking to it.
//!
//! Coordinates are WGS-84 throughout - what a GPS reports, what OpenStreetMap
//! draws, and what both devices expect. Chinese maps show GCJ-02 instead, and
//! the conversion happens only where such a map is involved (see
//! [`open_map_link`] and `xphoneuse_core::geo`).

use tauri::{AppHandle, State};
use xphoneuse_core::adb;

use crate::state::AppState;

/// Android 12 (API 31) is where `cmd location` grew test providers. Older
/// versions have no adb-only way in (a mock-location app plus an appops grant
/// would be needed), and saying so beats failing obscurely.
const ANDROID_MIN_SDK: u32 = 31;

/// What the UI needs to draw the section: which device it would act on,
/// whether that device can do it at all, and whether it is doing it now.
#[derive(serde::Serialize)]
pub struct LocationStatus {
    /// "android", "ios", or "none" when no device is connected.
    pub platform: String,
    /// Whether this platform/device can simulate a position at all.
    pub supported: bool,
    /// Whether a position is being simulated right now.
    pub active: bool,
    /// Why it is unavailable, in the device's or the platform's own words.
    pub reason: Option<String>,
}

impl LocationStatus {
    fn none(reason: &str) -> Self {
        Self {
            platform: "none".into(),
            supported: false,
            active: false,
            reason: Some(reason.into()),
        }
    }
}

/// The Android device's API level, from the device itself.
async fn android_sdk(serial: &str) -> Result<u32, String> {
    let raw = adb::shell_output(serial, "getprop ro.build.version.sdk")
        .await
        .map_err(|e| e.to_string())?;
    raw.trim()
        .parse()
        .map_err(|_| format!("the device did not report its API level ({:?})", raw.trim()))
}

/// The shell user's uid. `AID_SHELL`, and the uid the appop below is granted
/// to.
const SHELL_UID: u32 = 2000;

/// Whether a GPS test provider is installed and feeding the system.
///
/// There is no `is-provider-enabled` subcommand (tried it: the device answers
/// "Unknown command"), so this reads the provider list instead, where a test
/// provider is marked as one: `gps provider [mock]`.
async fn android_active(serial: &str) -> bool {
    adb::shell_output(serial, "dumpsys location")
        .await
        .map(|out| out.lines().any(|line| line.trim_start().starts_with("gps provider [mock]")))
        .unwrap_or(false)
}

/// Let the shell user mock a location.
///
/// Android refuses `add-test-provider` without the `mock_location` appop, and
/// the device says so itself - its own `cmd location` help reads: "Requires
/// MOCK_LOCATION permissions which can be enabled by running `adb shell appops
/// set <uid> android:mock_location allow`". That is a change on the device
/// rather than in this app (it is what a mock-location app would be granted),
/// it is idempotent, and it is what makes the feature work at all on a phone
/// that is not rooted - so it is done as part of setting a position, not
/// hidden behind another switch.
async fn android_allow_mock(serial: &str) -> Result<(), String> {
    adb::shell_output(
        serial,
        &format!("appops set --uid {SHELL_UID} android:mock_location allow"),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("could not grant the mock-location permission: {e}"))
}

async fn android_set(serial: &str, lat: f64, lon: f64) -> Result<(), String> {
    android_allow_mock(serial).await?;
    // Adding a provider that already exists fails, and that is the normal case
    // from the second call onwards - the provider is left in place between
    // positions on purpose, so its error is not an error here.
    let _ = adb::shell_output(serial, "cmd location providers add-test-provider gps").await;
    adb::shell_output(serial, "cmd location providers set-test-provider-enabled gps true")
        .await
        .map_err(|e| format!("could not enable the test provider: {e}"))?;
    adb::shell_output(
        serial,
        &format!("cmd location providers set-test-provider-location gps --location {lat:.6},{lon:.6}"),
    )
    .await
    .map_err(|e| format!("could not set the position: {e}"))?;
    Ok(())
}

async fn android_clear(serial: &str) -> Result<(), String> {
    // Each of these fails in a way that is not worth reporting on its own - the
    // provider may not exist, or the appop may not be granted because this
    // session never set a position - so the exit codes are ignored and the
    // outcome is checked instead, below.
    //
    // Disabled *and* removed: an enabled provider keeps feeding the system the
    // last position it was given, so switching it off is what actually hands
    // the device back to its real GPS.
    let _ = android_allow_mock(serial).await;
    let _ = adb::shell_output(serial, "cmd location providers set-test-provider-enabled gps false").await;
    let _ = adb::shell_output(serial, "cmd location providers remove-test-provider gps").await;
    if android_active(serial).await {
        return Err(
            "the phone still reports a simulated position - remove the test provider by hand \
             (adb shell cmd location providers remove-test-provider gps)"
                .to_string(),
        );
    }
    Ok(())
}

/// Which device the next call would act on, and what it can do.
#[tauri::command]
pub async fn location_status(state: State<'_, AppState>) -> Result<LocationStatus, String> {
    let android = {
        let session = state.session.lock().await;
        session.as_ref().map(|s| s.device_serial.clone())
    };
    if let Some(serial) = android {
        let sdk = android_sdk(&serial).await?;
        if sdk < ANDROID_MIN_SDK {
            return Ok(LocationStatus {
                platform: "android".into(),
                supported: false,
                active: false,
                reason: Some(format!(
                    "Android 12 (API 31) or newer is needed; this device reports API {sdk}"
                )),
            });
        }
        return Ok(LocationStatus {
            platform: "android".into(),
            supported: true,
            active: android_active(&serial).await,
            reason: None,
        });
    }

    let udid = {
        let session = state.ios_session.lock().await;
        session.as_ref().map(|s| s.udid.clone())
    };
    let Some(udid) = udid else {
        return Ok(LocationStatus::none("No device connected"));
    };
    let active = {
        let slot = state.location.lock().await;
        matches!(slot.as_ref(), Some((id, _)) if id == &udid)
    };
    Ok(LocationStatus {
        platform: "ios".into(),
        supported: true,
        active,
        reason: None,
    })
}

/// Put the phone at `lat`,`lon` (WGS-84).
#[tauri::command]
pub async fn location_set(
    lat: f64,
    lon: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(format!("{lat}, {lon} is not a position"));
    }

    // Android first, as everywhere else: a session on the phone in front of
    // the user wins over a view-only iOS one.
    let android = {
        let session = state.session.lock().await;
        session.as_ref().map(|s| s.device_serial.clone())
    };
    if let Some(serial) = android {
        let sdk = android_sdk(&serial).await?;
        if sdk < ANDROID_MIN_SDK {
            return Err(format!(
                "Android 12 (API 31) or newer is needed to set a position over adb; this device reports API {sdk}"
            ));
        }
        return android_set(&serial, lat, lon).await;
    }

    let udid = {
        let session = state.ios_session.lock().await;
        session.as_ref().map(|s| s.udid.clone())
    };
    let Some(udid) = udid else {
        return Err("No device connected".into());
    };

    let mut slot = state.location.lock().await;
    // A session belongs to one device: a different phone needs its own
    // channel, and the old one goes away with its handle.
    let stale = !matches!(slot.as_ref(), Some((id, _)) if id == &udid);
    if stale {
        *slot = Some((udid.clone(), xphoneuse_ios::location::start_ready(&udid).await?));
    }
    let (_, session) = slot.as_ref().expect("just filled");
    session.set(lat, lon).await
}

/// Hand the phone back to its real position.
#[tauri::command]
pub async fn location_clear(state: State<'_, AppState>) -> Result<(), String> {
    let android = {
        let session = state.session.lock().await;
        session.as_ref().map(|s| s.device_serial.clone())
    };
    if let Some(serial) = android {
        return android_clear(&serial).await;
    }

    // iOS: keep the session open - the next position should not have to pay
    // for a new channel - but tell the device to stop simulating.
    let slot = state.location.lock().await;
    match slot.as_ref() {
        Some((_, session)) => session.clear().await,
        None => Ok(()), // nothing was being simulated
    }
}

/// Open the picked position in a map the user can actually look at.
///
/// The URL is built here rather than passed in from the window, the same rule
/// the other link commands follow: the frontend picks a provider by name and
/// this decides what that means.
///
/// The two providers do not use the same coordinates. Tencent Maps is a
/// Chinese map and wants GCJ-02, so the WGS-84 position the user picked is
/// converted on the way in (`xphoneuse_core::geo`); Google Maps takes WGS-84
/// as it is.
#[tauri::command]
pub async fn open_map_link(
    lat: f64,
    lon: f64,
    provider: String,
    app: AppHandle,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(format!("{lat}, {lon} is not a position"));
    }
    let url = match provider.as_str() {
        "tencent" => {
            let (g_lat, g_lon) = xphoneuse_core::geo::wgs84_to_gcj02(lat, lon);
            format!("https://apis.map.qq.com/uri/v1/marker?marker=coord:{g_lat:.6},{g_lon:.6};title:here")
        }
        "google" => format!("https://www.google.com/maps/search/?api=1&query={lat:.6},{lon:.6}"),
        other => return Err(format!("unknown map provider {other:?}")),
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Android gate is a version comparison, and it is the difference
    /// between a working button and a confusing failure on an older phone.
    #[test]
    fn android_test_providers_start_at_api_31() {
        assert!(!(30 >= ANDROID_MIN_SDK));
        assert!(31 >= ANDROID_MIN_SDK);
    }
}
