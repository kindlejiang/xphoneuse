//! Probe: while the device is in the degraded state (screenshot encoder
//! stuck: fresh encodes hang, cached frames return fast), measure whether a
//! full teardown + gap + rebuild reliably yields one capture per cycle.
//!
//! Each iteration builds a complete fresh stack (tunnel -> RSD ->
//! dtservicehub -> screenshot channel), takes ONE capture with a generous
//! timeout, drops everything, and waits out a gap. If every iteration
//! succeeds quickly, cycling connections is a viable degraded-mode cadence.
//!
//! Usage: cargo run -p xphoneuse-ios --example screenshot_recycle_probe [cycles] [gap_ms]

use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::dvt::screenshot::ScreenshotClient;
use idevice::rsd::RsdHandshake;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let cycles: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let gap_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ({})", target.name, target.udid);

    for i in 0..cycles {
        let provider = xphoneuse_ios::device::provider_for(&target.udid)
            .await
            .expect("provider");
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
            .expect("dtservicehub service");
        let stream = adapter.connect(service_port).await.expect("service connect");
        let mut server = RemoteServerClient::new(stream);
        server.read_message(0).await.expect("initial message");
        let mut client = ScreenshotClient::new(&mut server)
            .await
            .expect("screenshot client");
        let built = t.elapsed();

        let t = Instant::now();
        let shot = tokio::time::timeout(Duration::from_secs(30), client.take_screenshot()).await;
        match shot {
            Ok(Ok(png)) => eprintln!(
                "cycle {:>2}: build {:>7.1?}  capture {:>8} bytes in {:>7.1?}",
                i + 1,
                built,
                png.len(),
                t.elapsed()
            ),
            Ok(Err(e)) => {
                eprintln!("cycle {:>2}: capture ERROR in {:?}: {e:?}", i + 1, t.elapsed())
            }
            Err(_) => {
                eprintln!("cycle {:>2}: capture HUNG for 30s", i + 1)
            }
        }
        // Drop the whole stack (adapter, server, client), then wait the gap.
        drop(client);
        drop(server);
        drop(adapter);
        if i + 1 < cycles {
            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
        }
    }
}
