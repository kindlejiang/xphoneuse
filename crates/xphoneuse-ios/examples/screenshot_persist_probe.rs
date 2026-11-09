//! Probe: take repeated screenshots over ONE persistent DVT channel.
//!
//! Open/close-per-shot cycling hits the dtservicehub hang when channels
//! churn faster than ~1/s; a channel that stays open sidesteps that. This
//! checks whether the device actually serves repeated `takeScreenshot` calls
//! on the same channel, how fast they are, and whether the bytes change
//! (same hash on a static screen is fine; identical hashes across a screen
//! change would mean a stale cached image).
//!
//! Usage: cargo run -p xphoneuse-ios --example screenshot_persist_probe [n] [interval_ms]

use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::dvt::screenshot::ScreenshotClient;
use idevice::rsd::RsdHandshake;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

fn quick_hash(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

#[tokio::main]
async fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let interval_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ({})", target.name, target.udid);
    let provider = xphoneuse_ios::device::provider_for(&target.udid)
        .await
        .expect("provider");

    // One tunnel, one server, one screenshot channel - then N captures.
    let t = Instant::now();
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .expect("CoreDeviceProxy");
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .expect("software tunnel")
        .to_async_handle();
    let stream = adapter.connect(rsd_port).await.expect("rsd connect");
    let handshake = RsdHandshake::new(stream).await.expect("rsd handshake");
    const DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";
    let service_port = handshake
        .services
        .get(DTSERVICEHUB)
        .map(|s| s.port)
        .expect("dtservicehub service (Developer Mode on?)");
    let stream = adapter.connect(service_port).await.expect("service connect");
    let mut server = RemoteServerClient::new(stream);
    server.read_message(0).await.expect("initial message");
    let mut client = ScreenshotClient::new(&mut server)
        .await
        .expect("screenshot client");
    eprintln!(
        "persistent channel up in {:?} (service port {service_port})",
        t.elapsed()
    );

    for i in 0..n {
        let t = Instant::now();
        let shot = tokio::time::timeout(Duration::from_secs(10), client.take_screenshot()).await;
        match shot {
            Ok(Ok(png)) => eprintln!(
                "#{:>2}: {:>8} bytes in {:>8.1?}  hash {:016x}",
                i + 1,
                png.len(),
                t.elapsed(),
                quick_hash(&png)
            ),
            Ok(Err(e)) => {
                eprintln!("#{:>2} FAILED in {:?}: {e:?}", i + 1, t.elapsed());
                break;
            }
            Err(_) => {
                eprintln!("#{:>2} TIMED OUT after {:?}", i + 1, t.elapsed());
                break;
            }
        }
        if i + 1 < n {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }
}
