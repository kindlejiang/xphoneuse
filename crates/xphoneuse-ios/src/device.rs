use anyhow::{anyhow, Result};
use idevice::{
    IdeviceService,
    lockdown::LockdownClient,
    provider::IdeviceProvider,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdDevice},
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct IosDevice {
    pub udid: String,
    pub name: String,
    pub model: String,
    pub product_type: String,
    pub ios_version: String,
    /// "usb" | "network" | "unknown"
    pub connection_type: String,
    /// Remote address when seen over Wi-Fi (usbmuxd Network entry)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_ip: Option<String>,
    pub paired: bool,
    /// True when lockdown queries succeeded (device is trusted and available)
    pub available: bool,
}

struct CacheEntry {
    device: IosDevice,
    fetched_at: Instant,
}

const CACHE_TTL: Duration = Duration::from_secs(2);

/// Per-UDID cache: the UI polls the device list every few seconds and a
/// lockdown round-trip per device is not free.
static DEVICE_CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    DEVICE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn list_devices() -> Result<Vec<IosDevice>> {
    let mut devices = vec![];

    let found = match usbmuxd_devices().await {
        Ok(d) => d,
        // usbmuxd not running (no iTunes / Apple Devices app) - just no iOS devices
        Err(_) => return Ok(devices),
    };

    // usbmuxd lists one entry per connection, so a phone with Wi-Fi sync on
    // appears twice (USB + Network). Collapse to one entry per UDID,
    // preferring USB (the tunnel and lockdown paths are more reliable there);
    // the Wi-Fi entry only shows up once the cable is out.
    let mut deduped: Vec<UsbmuxdDevice> = Vec::with_capacity(found.len());
    for usb in found {
        if let Some(i) = deduped.iter().position(|d| d.udid == usb.udid) {
            if connection_rank(&usb.connection_type) < connection_rank(&deduped[i].connection_type) {
                deduped[i] = usb;
            }
        } else {
            deduped.push(usb);
        }
    }

    for usb in deduped {
        let cached = {
            let c = cache().lock().await;
            c.get(&usb.udid)
                .filter(|e| e.fetched_at.elapsed() < CACHE_TTL)
                .map(|e| e.device.clone())
        };
        if let Some(mut d) = cached {
            // Connection state is live info, never serve it from the cache
            // (a cable may have gone in or out since the probe).
            (d.connection_type, d.network_ip) = connection_info(&usb.connection_type);
            devices.push(d);
            continue;
        }

        let device = probe_device(&usb).await;
        cache().lock().await.insert(
            usb.udid.clone(),
            CacheEntry {
                device: device.clone(),
                fetched_at: Instant::now(),
            },
        );
        devices.push(device);
    }

    Ok(devices)
}

/// USB beats Network beats Unknown when the same UDID is listed twice.
fn connection_rank(c: &Connection) -> u8 {
    match c {
        Connection::Usb => 0,
        Connection::Network(_) => 1,
        Connection::Unknown(_) => 2,
    }
}

/// Normalized `(connection_type, network_ip)` pair for the UI.
fn connection_info(c: &Connection) -> (String, Option<String>) {
    match c {
        Connection::Usb => ("usb".to_string(), None),
        Connection::Network(ip) => ("network".to_string(), Some(ip.to_string())),
        Connection::Unknown(_) => ("unknown".to_string(), None),
    }
}

async fn usbmuxd_devices() -> Result<Vec<UsbmuxdDevice>> {
    let mut conn = UsbmuxdAddr::default()
        .connect(0)
        .await
        .map_err(|e| anyhow!("usbmuxd connect failed: {e}"))?;
    conn.get_devices()
        .await
        .map_err(|e| anyhow!("usbmuxd list failed: {e}"))
}

/// Build a provider for the given UDID (the device must currently be visible
/// to usbmuxd).
pub async fn provider_for(udid: &str) -> Result<UsbmuxdProviderBox> {
    let mut conn = UsbmuxdAddr::default()
        .connect(0)
        .await
        .map_err(|e| anyhow!("usbmuxd connect failed: {e}"))?;
    let usb = conn
        .get_device(udid)
        .await
        .map_err(|e| anyhow!("device {udid} not found: {e}"))?;
    Ok(Box::new(usb.to_provider(UsbmuxdAddr::default(), "xphoneuse")))
}

pub type UsbmuxdProviderBox = Box<dyn IdeviceProvider>;

async fn probe_device(usb: &UsbmuxdDevice) -> IosDevice {
    let udid = usb.udid.clone();
    let (connection_type, network_ip) = connection_info(&usb.connection_type);

    let provider: UsbmuxdProviderBox =
        Box::new(usb.to_provider(UsbmuxdAddr::default(), "xphoneuse"));

    let paired = provider.get_pairing_file().await.is_ok();

    let (name, model, product_type, ios_version, available) =
        match LockdownClient::connect(provider.as_ref()).await {
            Ok(mut lockdown) => {
                let name = lockdown
                    .get_value(Some("DeviceName"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                    .unwrap_or_else(|| "iPhone".to_string());
                let product_type = lockdown
                    .get_value(Some("ProductType"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                    .unwrap_or_default();
                let ios_version = lockdown
                    .get_value(Some("ProductVersion"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                    .unwrap_or_default();
                let model = lockdown
                    .get_value(Some("ModelNumber"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                    .unwrap_or_else(|| product_type.clone());
                (name, model, product_type, ios_version, true)
            }
            Err(_) => ("iPhone".to_string(), String::new(), String::new(), String::new(), false),
        };

    IosDevice {
        udid,
        name,
        model,
        product_type,
        ios_version,
        connection_type,
        network_ip,
        paired,
        available,
    }
}
