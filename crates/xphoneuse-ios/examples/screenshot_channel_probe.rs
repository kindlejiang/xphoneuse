//! Probe: after a screenshot capture wedges on one channel, determine the
//! blast radius - is it the SCREENSHOT CHANNEL (recoverable by opening a new
//! channel on the same connection) or the whole CONNECTION/tunnel?
//!
//! Sequence: capture #1 (expect OK on wedge-prone rich content), capture #2
//! (expect hang), then a brand-new ScreenshotClient on the SAME server
//! (capture #3). If #3 works, cycling just the screenshot channel is a cheap
//! recovery. Run with RUST_LOG=idevice=debug to see reader activity.
//!
//! Usage: RUST_LOG=idevice=info cargo run -p xphoneuse-ios --example screenshot_channel_probe

use idevice::IdeviceService;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::dvt::screenshot::ScreenshotClient;
use idevice::rsd::RsdHandshake;
use std::time::{Duration, Instant};

async fn try_capture<R: idevice::ReadWrite>(
    client: &mut ScreenshotClient<'_, R>,
    label: &str,
) -> bool {
    let t = Instant::now();
    let shot = tokio::time::timeout(Duration::from_secs(20), client.take_screenshot()).await;
    match shot {
        Ok(Ok(png)) => {
            eprintln!("{label}: {:>8} bytes in {:>7.1?}  OK", png.len(), t.elapsed());
            true
        }
        Ok(Err(e)) => {
            eprintln!("{label}: ERROR in {:?}: {e:?}", t.elapsed());
            false
        }
        Err(_) => {
            eprintln!("{label}: HUNG for 20s", );
            false
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "idevice=info".into()),
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
    eprintln!("stack up in {:?} (one connection, one channel)", t.elapsed());

    try_capture(&mut client, "capture #1 (same channel)").await;
    try_capture(&mut client, "capture #2 (same channel)").await;

    // New screenshot channel on the SAME server/connection.
    let t = Instant::now();
    match ScreenshotClient::new(&mut server).await {
        Ok(mut client2) => {
            eprintln!("new screenshot channel on same connection in {:?}", t.elapsed());
            try_capture(&mut client2, "capture #3 (NEW channel, same connection)").await;
            try_capture(&mut client2, "capture #4 (NEW channel, 2nd shot)").await;
        }
        Err(e) => {
            eprintln!("new ScreenshotClient failed: {e:?} (connection-level damage)");
        }
    }
}
