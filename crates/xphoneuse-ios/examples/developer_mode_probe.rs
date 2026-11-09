//! Probe: turn the phone's Developer Mode on, the way the app does it.
//!
//! The app reaches this only when it needs the switch to install a developer
//! image (an iOS 27 phone whose image went stale). Older phones that never go
//! near the tunnel - iOS 16 needs Developer Mode for screenshots - have no
//! path to it at all, so this runs the same code by hand.
//!
//! The phone restarts when the request is accepted (iOS 18+; on older iOS the
//! switch flips and the restart is the user's to do), which is why nothing
//! automatic calls this: typing the command is the consent. The app asks the
//! same question in a dialog instead.
//!
//! Usage: cargo run -p xphoneuse-ios --example developer_mode_probe [udid] [--reveal]
//!
//! `--reveal` only makes the switch visible in Settings (no restart, nothing
//! else): the row is hidden on a device that has never been used for
//! development, and the manual route needs it to be there.

use xphoneuse_ios::developer_image;

#[tokio::main]
async fn main() {
    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(first) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    let args: Vec<String> = std::env::args().collect();
    let udid = args.get(1).cloned().unwrap_or_else(|| first.udid.clone());
    eprintln!("target: {udid} ios={}", first.ios_version);

    if args.iter().any(|a| a == "--reveal") {
        match developer_image::reveal_developer_mode(&udid).await {
            Ok(()) => println!("Developer Mode row revealed: 设置 → 隐私与安全性 里现在应该有了"),
            Err(e) => {
                println!("reveal failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Only a definite "off" is acted on. An unreadable status is what a device
    // that is rebooting looks like, and asking it to enable Developer Mode
    // again is asking it to reboot again - do that in a loop and the phone
    // never finishes booting (learned the hard way).
    match developer_image::developer_mode_status(&udid).await {
        Some(true) => {
            println!("Developer Mode: already on");
            return;
        }
        Some(false) => println!("Developer Mode: off — turning it on"),
        None => {
            eprintln!("Developer Mode: unreadable (the device may be rebooting); not touching it");
            std::process::exit(1);
        }
    }

    match developer_image::enable_developer_mode(&udid).await {
        Ok(()) => {
            println!("Developer Mode: on (the restart was waited out and confirmed).");
            println!("Unlock the phone and connect from the app.");
        }
        Err(e) => {
            println!("Developer Mode: could not be turned on from here: {e}");
            println!("The option was revealed in Settings as a fallback:");
            println!("  Settings -> Privacy & Security -> Developer Mode");
        }
    }
}
