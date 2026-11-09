//! Probe: hammer the production `take_screenshot()` path at the app's polling
//! cadence. The app's auto-screenshot preview calls it every ~1s; this checks
//! that no attempt stalls (the old open/close-per-shot design wedged on the
//! dtservicehub channel-reopen window and burned 3-20s timeouts).
//!
//! Usage: cargo run -p xphoneuse-ios --example screenshot_loop_probe [n] [interval_ms]

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
        .unwrap_or(20);
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

    let mut failures = 0usize;
    for i in 0..n {
        let t = Instant::now();
        match xphoneuse_ios::screenshot::take_screenshot(&target.udid).await {
            Ok(png) => eprintln!(
                "#{:>2}: {:>8} bytes in {:>8.1?}  hash {:016x}",
                i + 1,
                png.len(),
                t.elapsed(),
                quick_hash(&png)
            ),
            Err(e) => {
                failures += 1;
                eprintln!("#{:>2} FAILED in {:?}: {e:#}", i + 1, t.elapsed());
            }
        }
        if i + 1 < n {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }
    eprintln!("done: {failures}/{n} failures");
    if failures > 0 {
        std::process::exit(1);
    }
}
