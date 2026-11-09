//! What is installed on an iPhone.
//!
//! lockdown's installation proxy (`com.apple.mobile.installation_proxy`) with
//! a `Lookup` command: the same door `pymobiledevice3 apps list` uses, and the
//! iOS answer to `pm list packages`. Unlike everything else the app does with
//! an iPhone, it needs no developer image and no Developer Mode - the ordinary
//! pairing is enough.
//!
//! What comes back is more than Android's listing: the bundle id carries the
//! display name and version alongside it, so an agent can match on what the
//! user calls the app ("微信") rather than on the id nobody sees.

use idevice::IdeviceService;
use idevice::services::installation_proxy::InstallationProxyClient;

/// One installed app.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// The `launch` identifier: `com.tencent.xin`.
    pub bundle_id: String,
    /// What the home screen calls it ("微信"), empty when the device did not
    /// say (some app extensions have no name of their own).
    pub name: String,
    /// Marketing version (`CFBundleShortVersionString`), empty when absent.
    pub version: String,
}

/// Which apps to ask for: `"user"` (installed by someone), `"system"` (the
/// device's own, including the app extensions behind them), or `"any"`.
pub fn kind_arg(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "user" | "3" | "third-party" => "User",
        "system" | "sys" => "System",
        _ => "Any",
    }
}

/// List the device's apps, sorted by name (then id, so the order is stable).
pub async fn list_apps(udid: &str, kind: &str) -> Result<Vec<AppInfo>, String> {
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let mut proxy = InstallationProxyClient::connect(provider.as_ref())
        .await
        .map_err(|e| format!("installation proxy failed: {e} (is the device unlocked and paired?)"))?;
    let mut apps = proxy
        .get_apps(Some(kind_arg(kind)), None)
        .await
        .map_err(|e| format!("listing apps failed: {e}"))?;

    let mut out: Vec<AppInfo> = apps
        .drain()
        .map(|(bundle_id, info)| {
            let dict = info.as_dictionary();
            let field = |key: &str| {
                dict.and_then(|d| d.get(key))
                    .and_then(|v| v.as_string())
                    .unwrap_or_default()
                    .to_string()
            };
            // Display name first: it is what the user sees, and the extension
            // bundles that have none are exactly the ones nobody launches.
            let name = ["CFBundleDisplayName", "CFBundleName"]
                .iter()
                .map(|key| field(key))
                .find(|value| !value.is_empty())
                .unwrap_or_default();
            AppInfo {
                bundle_id,
                name,
                version: field("CFBundleShortVersionString"),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.bundle_id.cmp(&b.bundle_id))
    });
    Ok(out)
}
