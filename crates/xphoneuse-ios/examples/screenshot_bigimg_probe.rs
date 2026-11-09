//! Probe: reproduce the "second large screenshot wedges the channel" hang
//! with full vendored-crate debug logging.
//!
//! Symptom (app log, rich screen ~6.3MB PNG): one channel serves exactly one
//! large capture; the next capture on it hangs into the timeout, forcing a
//! kill+rebuild every shot. Small captures (~500KB) repeat fine.
//!
//! While this runs, view a photo or other rich content on the iPhone so the
//! screenshots come out large (multi-MB), then watch where the second capture
//! stalls (reader stuck? message dropped on an unknown channel? fragment
//! misparse?).
//!
//! Usage: RUST_LOG=idevice=debug cargo run -p xphoneuse-ios --example screenshot_bigimg_probe

use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::dvt::screenshot::ScreenshotClient;
use idevice::rsd::RsdHandshake;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "idevice=debug".into()),
        )
        .with_target(true)
        .init();

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ({})", target.name, target.udid);
    let provider = xphoneuse_ios::device::provider_for(&target.udid)
        .await
        .expect("provider");

    // Same channel construction as the app's persistent session.
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

    // No interval between captures at first: the goal is to see the SECOND
    // large capture on the same channel hang immediately.
    let mut big_seen = false;
    for i in 0..usize::MAX {
        let t = Instant::now();
        let shot = tokio::time::timeout(Duration::from_secs(15), client.take_screenshot()).await;
        match shot {
            Ok(Ok(png)) => {
                eprintln!(
                    "CAP #{:>2}: {:>8} bytes in {:>8.1?}{}",
                    i + 1,
                    png.len(),
                    t.elapsed(),
                    if png.len() > 2_000_000 { "  << LARGE" } else { "" }
                );
                if png.len() > 2_000_000 {
                    big_seen = true;
                }
            }
            Ok(Err(e)) => {
                eprintln!("CAP #{:>2} ERROR in {:?}: {e:?}", i + 1, t.elapsed());
                break;
            }
            Err(_) => {
                eprintln!(
                    "CAP #{:>2} HUNG for 15s (channel wedged after big_seen={big_seen})",
                    i + 1
                );
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}
