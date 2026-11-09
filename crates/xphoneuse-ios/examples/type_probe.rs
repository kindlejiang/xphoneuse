//! Probe: type one string over the tunnel, and nothing else.
//!
//! The other input probe taps, swipes, scrolls and presses buttons on its way
//! to the keyboard section - far more than a question about one character
//! needs, and impossible to watch politely. This one starts the tunnel, waits
//! for input readiness, types the string it was given and exits, so a delivery
//! question ("does this character reach the field?") can be answered with:
//! focus a text field on the device, run this, screenshot, read the field.
//!
//! Usage:
//!   cargo run -p xphoneuse-ios --example type_probe -- "text to type" [udid]
//!
//! The text is typed through `HidVerb::Text`, the same verb the app's input box
//! and the copilot's fill button use, so what it does here is what the app does.

use xphoneuse_ios::tunnel::{HidVerb, TunnelHandle};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let text = args.next().unwrap_or_else(|| "test test".into());
    let udid = args
        .next()
        .unwrap_or_else(|| "00008140-0000000000000000".into());

    eprintln!("[type-probe] starting tunnel for {udid}");
    let tunnel = TunnelHandle::start(&udid, false).await.expect("tunnel start");
    while !tunnel.input_ready() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The auth gate takes a moment to settle after the stream is up; the other
    // probe waits here too.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = std::time::Instant::now();
    tunnel
        .input(HidVerb::Text { text: text.clone() })
        .await
        .expect("type");
    eprintln!(
        "[type-probe] typed {:?} ({} chars) in {:?}",
        text,
        text.chars().count(),
        started.elapsed()
    );

    // Hold the stream briefly so the last keystrokes land before the tunnel
    // goes away, then let the caller screenshot.
    tokio::time::sleep(Duration::from_millis(800)).await;
    tunnel.stop();
}
