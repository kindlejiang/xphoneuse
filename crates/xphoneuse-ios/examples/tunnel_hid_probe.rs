//! Probe: CoreDevice HID input over the tunnel (via TunnelHandle).
//!
//! Exercises every HidVerb against the real device while a headless display
//! stream holds the auth gate open, with pauses so a human can watch the
//! screen. Verifies: the ~300ms auth gate timing, tap coordinates/direction,
//! drag velocity, scroll direction/scale, keyboard typing and buttons.
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_hid_probe [udid]

use xphoneuse_ios::tunnel::{HidVerb, TouchAction, TunnelHandle};
use std::time::{Duration, Instant};

const PAUSE: Duration = Duration::from_secs(3);

fn show(what: &str) {
    eprintln!("\n[hid-probe] === {what} (watch the device) ===");
}

#[tokio::main]
async fn main() {
    let udid = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "00008140-0000000000000000".into());
    eprintln!("[hid-probe] starting headless tunnel for {udid}");

    let t0 = Instant::now();
    let tunnel = TunnelHandle::start(&udid, false).await.expect("tunnel start");
    eprintln!("[hid-probe] tunnel up in {:?}", t0.elapsed());

    while !tunnel.input_ready() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!("[hid-probe] input_ready after {:?}", t0.elapsed());

    // --- touch ---
    show("tap center");
    tunnel
        .input(HidVerb::Tap { x: 0.5, y: 0.5 })
        .await
        .expect("tap");
    tokio::time::sleep(PAUSE).await;

    show("tap top-left quadrant (0.25, 0.25)");
    tunnel
        .input(HidVerb::Tap { x: 0.25, y: 0.25 })
        .await
        .expect("tap");
    tokio::time::sleep(PAUSE).await;

    show("swipe up slowly (home screen), 800ms");
    tunnel
        .input(HidVerb::Swipe { x1: 0.5, y1: 0.7, x2: 0.5, y2: 0.2, duration_ms: 800 })
        .await
        .expect("swipe");
    tokio::time::sleep(PAUSE).await;

    show("scroll down (content up) by +250 logical units");
    tunnel
        .input(HidVerb::Scroll { dx: 0.0, dy: 250.0 })
        .await
        .expect("scroll");
    tokio::time::sleep(PAUSE).await;

    show("scroll up by -250 logical units");
    tunnel
        .input(HidVerb::Scroll { dx: 0.0, dy: -250.0 })
        .await
        .expect("scroll");
    tokio::time::sleep(PAUSE).await;

    show("raw touch: down center, move to 0.25,0.75, up");
    tunnel
        .input(HidVerb::Touch { action: TouchAction::Down, x: 0.5, y: 0.5 })
        .await
        .expect("down");
    tokio::time::sleep(Duration::from_millis(200)).await;
    for i in 1..=10 {
        let t = i as f64 / 10.0;
        tunnel
            .input(HidVerb::Touch {
                action: TouchAction::Move,
                x: 0.5 + (0.25 - 0.5) * t,
                y: 0.5 + (0.75 - 0.5) * t,
            })
            .await
            .expect("move");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tunnel
        .input(HidVerb::Touch { action: TouchAction::Up, x: 0.25, y: 0.75 })
        .await
        .expect("up");
    tokio::time::sleep(PAUSE).await;

    // --- keyboard (needs a focused text field to show anything) ---
    show("typing text (open a note/search field on the device first)");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let t0 = Instant::now();
    tunnel
        .input(HidVerb::Text { text: "Hello, XPhoneUse tunnel! 123".into() })
        .await
        .expect("text");
    eprintln!("[hid-probe] typed 28 chars in {:?}", t0.elapsed());
    tokio::time::sleep(PAUSE).await;

    show("key: RETURN");
    tunnel.input(HidVerb::Key { usage: 0x28 }).await.expect("key");
    tokio::time::sleep(PAUSE).await;

    // --- buttons ---
    for button in ["volume_up", "volume_down", "power", "home", "recents", "back"] {
        show(&format!("button: {button}"));
        match tunnel.input(HidVerb::Button { button: button.into() }).await {
            Ok(()) => {}
            Err(e) => eprintln!("[hid-probe] {button} error: {e}"),
        }
        tokio::time::sleep(PAUSE).await;
    }

    // --- second round to catch auth-gate flakiness ---
    show("repeat tap center (stream has been up a while now)");
    tunnel
        .input(HidVerb::Tap { x: 0.5, y: 0.5 })
        .await
        .expect("tap");
    tokio::time::sleep(PAUSE).await;

    tunnel.stop();
    eprintln!("[hid-probe] done");
}
