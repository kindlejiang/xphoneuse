//! Probe: list usbmuxd devices and, for each, take an idevice screenshot.
//!
//! Usage: cargo run -p xphoneuse-ios --example screenshot_probe [output.png]

use std::time::Instant;

#[tokio::main]
async fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("screenshot-{}.png", std::process::id()));

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    if devices.is_empty() {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    }
    for d in &devices {
        eprintln!(
            "found: {} udid={} ios={} paired={} conn={}",
            d.name, d.udid, d.ios_version, d.paired, d.connection_type
        );
    }

    let target = &devices[0];
    eprintln!("taking screenshot of {} ...", target.udid);
    let start = Instant::now();
    match xphoneuse_ios::screenshot::take_screenshot(&target.udid).await {
        Ok(png) => {
            eprintln!("got {} bytes in {:?}", png.len(), start.elapsed());
            std::fs::write(&out, &png).expect("write png");
            eprintln!("saved to {out}");
        }
        Err(e) => {
            eprintln!("screenshot FAILED after {:?}: {e:#}", start.elapsed());
            std::process::exit(2);
        }
    }
}
