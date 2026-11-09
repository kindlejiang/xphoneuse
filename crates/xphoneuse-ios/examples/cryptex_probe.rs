//! Probe: install the Cryptex1 developer image.
//!
//! This is the path newer iPhones need - the personalized DDI has no TSS
//! identity for hardware released after the mirrored Xcode build, and the
//! device answers `BadBuildManifest`. The app picks this route on its own
//! when the personalized manifest has no identity for the phone; run this to
//! watch the steps (asset download, nonce, ticket, transfers) on their own.
//!
//! Usage: cargo run -p xphoneuse-ios --example cryptex_probe [udid] [cache-dir]

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    let args: Vec<String> = std::env::args().collect();
    let udid = args.get(1).cloned().unwrap_or_else(|| target.udid.clone());
    let cache = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("xphoneuse-ddi-cache"));
    eprintln!(
        "target: {udid} ios={} cache={}",
        target.ios_version,
        cache.display()
    );

    let assets = match xphoneuse_ios::cryptex::fetch_assets(&cache).await {
        Ok(assets) => assets,
        Err(e) => {
            eprintln!("fetch failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "assets: build {} (downloaded={})",
        assets.build, assets.downloaded
    );
    if args.iter().any(|a| a == "--fetch-only") {
        println!("ok: assets fetched (download only)");
        return;
    }

    let provider = xphoneuse_ios::device::provider_for(&udid)
        .await
        .expect("provider");
    let started = std::time::Instant::now();
    match xphoneuse_ios::cryptex::install(&provider, &assets).await {
        Ok(()) => println!("ok: cryptex installed in {:?}", started.elapsed()),
        Err(e) => {
            eprintln!("failed: {e}");
            std::process::exit(1);
        }
    }
}
