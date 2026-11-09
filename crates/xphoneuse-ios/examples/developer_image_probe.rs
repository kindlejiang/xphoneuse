//! Probe: prepare (download + personalize + mount) the developer disk image a
//! device wants for its iOS version.
//!
//! This is the whole of what the app does when something comes up short for
//! want of the services an image provides - a tunnel that cannot offer the
//! screen/HID/pasteboard services, a screenshot whose service is not there.
//! Run it on its own to see each step, and follow the tunnel route with
//! `rsd_services` to confirm the device now offers them.
//!
//! Usage: cargo run -p xphoneuse-ios --example developer_image_probe [udid] [cache-dir] [--restart]

use xphoneuse_ios::developer_image::{ensure, route_for, ImageProgress};

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
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("xphoneuse-ddi-cache"));
    let route = route_for(&target.ios_version);
    eprintln!(
        "target: {udid} ios={} cache={} route={route:?}",
        target.ios_version,
        cache.display()
    );

    xphoneuse_ios::developer_image::use_cache_dir(&cache);
    xphoneuse_ios::developer_image::set_progress_sink(|p| match p {
        ImageProgress::Checking => eprintln!("  [checking]"),
        ImageProgress::Downloading { got, total } => {
            let pct = total.map(|t| format!(" {:.0}%", got as f64 / t as f64 * 100.0));
            eprintln!("  [download] {got} bytes{}", pct.unwrap_or_default());
        }
        ImageProgress::Mounting { sent, total } => {
            eprintln!("  [mount] sent {sent}/{total} bytes");
        }
        ImageProgress::Mounted { id, downloaded } => {
            eprintln!("  [mounted] {id} (downloaded={downloaded})");
        }
    });

    // The preflight the app runs before it opens a mirror on an older iPhone:
    // "is the service that needs the image reachable?" A `not there` is what
    // leads into the preparation below; anything else is a device problem the
    // preparation would not fix.
    if matches!(route, xphoneuse_ios::developer_image::Route::Classic { .. }) {
        match xphoneuse_ios::screenshot::classic_service_ready(&udid).await {
            Ok(true) => eprintln!("classic screenshot service: ready (no image needed)"),
            Ok(false) => eprintln!("classic screenshot service: not there (needs the image)"),
            Err(e) => eprintln!("classic screenshot service: unknown ({e:#})"),
        }
    }

    let started = std::time::Instant::now();
    match ensure(&udid, route).await {
        Ok(result) => println!(
            "ok: image {} (downloaded={}) in {:?}",
            result.id,
            result.downloaded,
            started.elapsed()
        ),
        Err(e) => {
            eprintln!("failed: {e}");
            std::process::exit(1);
        }
    }
}
