//! Probe: find the valid `requestedFormat` values for the CoreDevice
//! screencapture service (the vendored client sends "jpeg", which iOS 26
//! rejects with "Cannot initialize ScreenshotImageFormat from invalid String
//! value jpeg").
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_format_probe

use idevice::IdeviceService;
use idevice::core_device::CoreDeviceServiceClient;
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::rsd::RsdHandshake;

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let target = devices.first().expect("no iOS devices visible to usbmuxd");
    eprintln!("target: {} ios={}", target.udid, target.ios_version);

    let provider = xphoneuse_ios::device::provider_for(&target.udid)
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

    let port = handshake
        .services
        .get("com.apple.coredevice.screencaptureservice")
        .map(|s| s.port)
        .expect("screencapture service port");
    let svc_stream = adapter.connect(port).await.expect("service connect");
    let mut client = CoreDeviceServiceClient::new(svc_stream)
        .await
        .expect("CoreDeviceServiceClient");

    for fmt in ["png", "jpeg", "jpg", "JPEG", "Jpeg", "heic", "HEIC", "tiff"] {
        let mut req = plist::Dictionary::new();
        req.insert("requestedFormat".into(), fmt.into());
        let t = std::time::Instant::now();
        match client
            .invoke_with_plist_action(
                "com.apple.coredevice.feature.capturescreenshot",
                req,
                "com.apple.coredevice.action.capturescreenshot",
            )
            .await
        {
            Ok(v) => {
                let len = v
                    .as_dictionary()
                    .and_then(|d| d.get("image"))
                    .and_then(|i| i.as_data().map(|d| d.len()))
                    .unwrap_or(0);
                eprintln!("[fmt] {fmt:>5}: OK {len} bytes in {:?}", t.elapsed());
            }
            Err(e) => eprintln!("[fmt] {fmt:>5}: ERR {e:?}"),
        }
    }
}
