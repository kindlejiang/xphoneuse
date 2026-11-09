//! Launching an app on an iPhone.
//!
//! There is no `adb shell monkey` here. The way in is DVT's process-control
//! channel - `com.apple.instruments.server.services.processcontrol`, served by
//! the developer image and reached exactly like the screenshot path reaches
//! its own channel (CoreDeviceProxy, RSD, then the instruments hub). It is
//! what Xcode's "launch app" uses, so what it starts is an ordinary
//! foreground launch: SpringBoard brings the app up the same way tapping its
//! icon does.
//!
//! It needs no active screen stream and no tunnel session - only the image.

use idevice::dvt::process_control::ProcessControlClient;

use crate::screenshot::{build_dvt_server, is_service_missing};

/// Launch `bundle_id` (e.g. `com.tencent.xin`) and return its pid.
///
/// Fails when the app is not installed, when the identifier is wrong, and when
/// the image that serves the channel is not mounted - the device's own words
/// are in the error.
pub async fn launch_app(udid: &str, bundle_id: &str) -> Result<u64, String> {
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| format!("{e:#}"))?;

    // Everything but the launch channel itself. The adapter is *held* on
    // purpose (a named binding, not `_`): the streams ride on it, and dropping
    // it would tear the tunnel down in the middle of the launch.
    let (_adapter, mut server, _rx_bytes) = build_dvt_server(provider.as_ref()).await?;

    let mut control = ProcessControlClient::new(&mut server)
        .await
        .map_err(|e| format!("process-control channel failed: {e}"))?;
    // Not suspended (there is no debugger to attach), and an instance left
    // over from a previous launch is replaced rather than reused - the user
    // asked for the app, not for whatever state it was in.
    control
        .launch_app(bundle_id, None, None, false, true)
        .await
        .map_err(|e| format!("could not launch {bundle_id}: {e}"))
}

/// Launch, giving the developer image one chance to be prepared first.
///
/// The channel lives in the image Xcode mounts, and an iOS update invalidates
/// the one the device had - the same failure the screenshot path knows by
/// heart. Preparing the image is this side's job; turning Developer Mode on is
/// not (see [`crate::developer_image::ensure`], which never does), so a device
/// whose switch is off reports exactly that instead.
pub async fn launch_app_ready(udid: &str, bundle_id: &str) -> Result<u64, String> {
    match launch_app(udid, bundle_id).await {
        Err(error) if is_service_missing(&error) => {
            crate::developer_image::ensure(udid, crate::developer_image::Route::Personalized)
                .await
                .map_err(|e| {
                    format!("{error}; preparing the developer image also failed: {e}")
                })?;
            launch_app(udid, bundle_id).await
        }
        result => result,
    }
}
