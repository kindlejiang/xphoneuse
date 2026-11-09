//! Smoke test for the uxplay.dll FFI session: start the server, print the
//! events for a few seconds, stop it.
//!
//! Run from the workspace root:
//!   cargo run -p xphoneuse-ios --example uxplay_smoke
//!
//! Expect "ready as ..." log events. Point an iPhone at Screen Mirroring
//! → "XPhoneUse-smoke" to see Connected / VideoData events live.

use xphoneuse_ios::uxplay_ffi::{UxplayEvent, UxplaySession};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // uxplay_dll_path() appends "resources" to the resource dir; point it
    // at src-tauri so it resolves to src-tauri/resources (where the built
    // uxplay.dll and its MinGW dependency DLLs were copied).
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let src_tauri = std::path::Path::new(&manifest).join("../../src-tauri");
    xphoneuse_ios::uxplay::set_resource_dir(src_tauri);

    println!("starting uxplay FFI session (5 seconds)...");
    let mut session = match UxplaySession::start("XPhoneUse-smoke", 60) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("start error debug: {:#?}", e);
            return Err(e.into());
        }
    };
    let receiver = session
        .take_receiver()
        .ok_or("event receiver unavailable")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frames = 0u32;
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(UxplayEvent::VideoData { data, timestamp_ns, .. }) => {
                frames += 1;
                if frames <= 2 {
                    println!("VideoData: {} bytes, ts={} ns", data.len(), timestamp_ns);
                }
            }
            Ok(other) => println!("event: {:?}", other),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    println!("frames received: {}", frames);

    session.stop();
    println!("stopped OK");
    Ok(())
}
