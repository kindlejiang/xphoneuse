//! Probe: try the classic screenshotr (lockdown) service on any iOS version.
use idevice::IdeviceService;

use idevice::screenshotr::ScreenshotService;

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    if devices.is_empty() {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    }
    let target = &devices[0];
    eprintln!("target: {} ios={} conn={}", target.udid, target.ios_version, target.connection_type);

    let provider = xphoneuse_ios::device::provider_for(&target.udid).await.expect("provider");
    let start = std::time::Instant::now();
    match ScreenshotService::connect(provider.as_ref()).await {
        Ok(mut service) => {
            eprintln!("screenshotr connected in {:?}", start.elapsed());
            match service.take_screenshot().await {
                Ok(data) => {
                    eprintln!("got {} bytes in {:?}", data.len(), start.elapsed());
                    let magic = &data[..data.len().min(8)];
                    eprintln!("magic bytes: {:02X?}", magic);
                    std::fs::write("screenshotr-out.bin", &data).expect("write");
                    eprintln!("saved screenshotr-out.bin");
                }
                Err(e) => eprintln!("take_screenshot FAILED: {e}"),
            }
        }
        Err(e) => eprintln!("screenshotr connect FAILED: {e}"),
    }
}
