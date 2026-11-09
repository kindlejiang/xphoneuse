//! Print the OS build of every connected iPhone, next to the build the
//! developer-image mirror publishes. A personalized DDI only mounts on the
//! device build it was made for (`BadBuildManifest` otherwise), so these two
//! numbers are the first thing to compare when a mount fails.

use idevice::lockdown::LockdownClient;
use idevice::IdeviceService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let devices = xphoneuse_ios::device::list_devices().await?;
    if devices.is_empty() {
        println!("no iOS devices");
    }
    for d in devices {
        println!(
            "{} {} {} ({})",
            d.udid, d.product_type, d.ios_version, d.connection_type
        );
        let provider = xphoneuse_ios::device::provider_for(&d.udid).await?;
        let mut lockdown = LockdownClient::connect(provider.as_ref()).await?;
        for key in ["ProductVersion", "BuildVersion", "ProductType", "HardwareModel"] {
            let value = lockdown
                .get_value(Some(key), None)
                .await
                .ok()
                .and_then(|v| v.as_string().map(|s| s.to_string()));
            println!("  {key} = {value:?}");
        }
        for key in ["BoardId", "ChipID", "UniqueChipID"] {
            let value = lockdown.get_value(Some(key), None).await.ok();
            println!("  {key} = {value:?}");
        }
    }
    Ok(())
}
