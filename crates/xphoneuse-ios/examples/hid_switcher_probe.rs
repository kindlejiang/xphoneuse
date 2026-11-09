//! Probe: find a touch sequence over the BLE-HID bridge that opens the iOS
//! app switcher. iOS has no consumer key for it, so candidates are gesture
//! variants (slow drag up from the bottom edge, pause, release). After each
//! variant the screen is captured via DVT so the result can be inspected.
//!
//! All variants run from the HOME screen (swipe-up-and-hold opens the
//! switcher from there too), reset with KEY HOME between attempts.
//!
//! Usage: cargo run -p xphoneuse-ios --example hid_switcher_probe

use std::time::Duration;

fn send(hid: &mut xphoneuse_ios::hid::HidSession, cmd: &str) {
    match hid.send(cmd) {
        Ok(reply) => eprintln!("  -> {cmd:<28} => {reply}"),
        Err(e) => eprintln!("  -> {cmd:<28} => ERR {e}"),
    }
}

async fn shot(udid: &str, name: &str) {
    match xphoneuse_ios::screenshot::take_screenshot(udid).await {
        Ok(png) => {
            let path = std::env::temp_dir().join(name);
            let path = path.display().to_string();
            std::fs::write(&path, &png).ok();
            eprintln!("  saved {path} ({} bytes)", png.len());
        }
        Err(e) => eprintln!("  screenshot failed: {e:#}"),
    }
}

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ({})", target.name, target.udid);

    let mut hid = tokio::task::spawn_blocking(xphoneuse_ios::hid::detect_first)
        .await
        .unwrap()
        .expect("HID bridge not found");

    // Start from the home screen.
    send(&mut hid, "KEY HOME");
    tokio::time::sleep(Duration::from_secs(1)).await;
    shot(&target.udid, "sw0_home.png").await;

    // Variant A: production gesture (10 steps x 60ms, hold 700ms).
    eprintln!("[A] production gesture");
    for (cmd, delay) in xphoneuse_ios::hid::app_switcher_gesture() {
        send(&mut hid, &cmd);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    shot(&target.udid, "swA_prod.png").await;

    // Reset to home.
    send(&mut hid, "KEY HOME");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Variant B: firmware SWIPE, slow (800ms to 60% height).
    eprintln!("[B] SWIPE 960,1870,960,1150,800");
    send(&mut hid, "SWIPE 960,1870,960,1150,800");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    shot(&target.udid, "swB_swipe800.png").await;

    send(&mut hid, "KEY HOME");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Variant C: slower manual drag, longer hold (15 steps x 100ms, 1200ms).
    eprintln!("[C] slow manual drag, hold 1200ms");
    send(&mut hid, "MDOWN 960,1900");
    tokio::time::sleep(Duration::from_millis(100)).await;
    for i in 1..=15 {
        let y = 1900 - (1900 - 1100) * i / 15;
        send(&mut hid, &format!("M 960,{y}"));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    send(&mut hid, "MUP 960,1100");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    shot(&target.udid, "swC_slow.png").await;

    send(&mut hid, "KEY HOME");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Variant D: start even closer to the edge, drag to ~40% height.
    eprintln!("[D] edge start 1915, end 800, hold 900ms");
    send(&mut hid, "MDOWN 960,1915");
    tokio::time::sleep(Duration::from_millis(80)).await;
    for i in 1..=12 {
        let y = 1915 - (1915 - 800) * i / 12;
        send(&mut hid, &format!("M 960,{y}"));
        tokio::time::sleep(Duration::from_millis(70)).await;
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    send(&mut hid, "MUP 960,800");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    shot(&target.udid, "swD_edge.png").await;

    send(&mut hid, "KEY HOME");
    eprintln!("done");
}
