//! Probe: what this device publishes over the CoreDevice tunnel's Remote
//! Service Discovery.
//!
//! Everything the app can do to a device is whatever appears in this list, so
//! when a service it needs goes missing the first question is what the device
//! offers instead. The `com.apple.coredevice.*` family comes from the developer
//! image the device has mounted, so an iOS update (which invalidates that
//! image) or an image that was never mounted is the usual answer.
//!
//! Usage: cargo run -p xphoneuse-ios --example rsd_services
//! (the app's own tunnel is per-udid; this one is a second, read-only tunnel)

use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::rsd::RsdHandshake;
use idevice::IdeviceService;

/// The services tunnel.rs needs, in the order it needs them.
const NEEDED: &[&str] = &[
    "com.apple.coredevice.displayservice",
    "com.apple.coredevice.screencaptureservice",
    "com.apple.coredevice.hid.universalhidservice",
    "com.apple.coredevice.hid.indigo",
    "com.apple.coredevice.pasteboardservice",
];

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    let udid = std::env::args().nth(1).unwrap_or_else(|| target.udid.clone());
    eprintln!("target: {udid} ios={}", target.ios_version);

    let provider = xphoneuse_ios::device::provider_for(&udid)
        .await
        .expect("provider");
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .expect("CoreDeviceProxy");
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .expect("software tunnel")
        .to_async_handle();
    let stream = adapter.connect(rsd_port).await.expect("rsd port connect");
    let handshake = RsdHandshake::new(stream).await.expect("RSD handshake");

    for key in ["ProductVersion", "ProductType", "DeviceClass", "UniqueDeviceID"] {
        if let Some(v) = handshake.properties.get(key) {
            println!("{key}: {v:?}");
        }
    }

    let mut names: Vec<&str> = handshake.services.keys().map(String::as_str).collect();
    names.sort_unstable();

    let coredevice: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| n.starts_with("com.apple.coredevice."))
        .collect();

    println!("\n{} services, {} of them com.apple.coredevice.*:", names.len(), coredevice.len());
    for name in &coredevice {
        println!("  {name}");
    }

    println!("\nthe app needs:");
    for needed in NEEDED {
        let mark = if handshake.services.contains_key(*needed) { "ok     " } else { "MISSING" };
        println!("  {mark} {needed}");
    }

    // Reported, not judged: the screen/HID/pasteboard services come from the
    // mounted developer image, and a current one keeps them published with
    // Developer Mode off (verified on iOS 27). Still worth knowing - a mount
    // the device refuses is usually this.
    match idevice::services::amfi::AmfiClient::connect(provider.as_ref()).await {
        Ok(mut amfi) => match amfi.get_developer_mode_status().await {
            Ok(true) => println!("\nDeveloper Mode: ON"),
            Ok(false) => println!("\nDeveloper Mode: OFF (the mounted image is what matters)"),
            Err(e) => println!("\nDeveloper Mode: unknown ({e:?})"),
        },
        Err(e) => println!("\nDeveloper Mode: unknown, amfi unreachable ({e:?})"),
    }

    println!("\nall services:");
    for name in &names {
        println!("  {name}");
    }
}
