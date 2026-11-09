//! Probe: classic screenshotr over the RemoteXPC tunnel (iOS 17+).
//!
//! lockdown-over-usbmuxd answers InvalidService for com.apple.mobile.screenshotr
//! on iOS 17+, but the RSD tunnel exposes it as com.apple.screenshotr.shim.remote
//! (this is what pymobiledevice3 uses for `developer screenshot`). Checks whether
//! the service is listed, whether it serves screenshots on iOS 26, how fast, and
//! what image format it returns (screenshotr usually replies with TIFF).
//!
//! Usage: cargo run -p xphoneuse-ios --example screenshotr_tunnel_probe [n] [interval_ms]

use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::rsd::RsdHandshake;
use idevice::screenshotr::ScreenshotService;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let interval_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ios={}", target.udid, target.ios_version);

    let provider = xphoneuse_ios::device::provider_for(&target.udid)
        .await
        .expect("provider");

    let t0 = Instant::now();
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .expect("CoreDeviceProxy");
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .expect("software tunnel")
        .to_async_handle();
    let stream = adapter.connect(rsd_port).await.expect("rsd port connect");
    let mut handshake = RsdHandshake::new(stream).await.expect("RSD handshake");
    eprintln!("tunnel+RSD up in {:?}", t0.elapsed());

    let mut has_screenshotr = false;
    for (name, s) in &handshake.services {
        if name.contains("screenshot") {
            has_screenshotr = true;
        }
        if name.contains("screenshot") || name.contains("instruments") {
            eprintln!("service: {} -> port {}", name, s.port);
        }
    }
    if !has_screenshotr {
        eprintln!("no screenshotr service in RSD list - cannot probe");
        std::process::exit(2);
    }

    let mut service: ScreenshotService = handshake
        .connect(&mut adapter)
        .await
        .expect("screenshotr connect");
    eprintln!("screenshotr connected in {:?} (total)", t0.elapsed());

    for i in 0..n {
        let t = Instant::now();
        match service.take_screenshot().await {
            Ok(data) => {
                let magic = &data[..data.len().min(8)];
                eprintln!(
                    "[{i}] {} bytes in {:?} magic {:02X?}",
                    data.len(),
                    t.elapsed(),
                    magic
                );
                if i == 0 {
                    std::fs::write("screenshotr-tunnel-out.bin", &data).expect("write");
                    eprintln!("saved screenshotr-tunnel-out.bin");
                }
            }
            Err(e) => eprintln!("[{i}] FAILED in {:?}: {e}", t.elapsed()),
        }
        if i + 1 < n {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }
}
