//! The scrcpy connect path without the GUI.
//!
//! "Clicking connect just spins" is ambiguous from the outside: the frontend
//! awaits `connect_device`, so a hang in `start_server` and a hang in the
//! frontend look the same. This runs `start_server` on its own and prints how
//! far it got, so the two can be told apart.
//!
//! Usage: cargo run -p xphoneuse-core --example scrcpy_connect_probe -- \
//!            <serial> <scrcpy-server-path>

use std::time::{Duration, Instant};

use xphoneuse_core::scrcpy::{start_server, StreamSettings};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let serial = args.next().unwrap_or_else(|| "AKRSUT1611001468".to_string());
    let server = args
        .next()
        .unwrap_or_else(|| "src-tauri/target/release/resources/scrcpy-server-v2.7".to_string());

    if let Ok(dir) = std::env::var("XPHONEUSE_RESOURCE_DIR") {
        xphoneuse_core::adb::set_resource_dir(std::path::PathBuf::from(dir));
    }

    let settings = StreamSettings::default();
    println!("serial  = {serial}");
    println!("server  = {server}");
    println!("settings= {settings:?}");

    let started = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(45),
        start_server(&serial, &server, 27183, &settings),
    )
    .await
    {
        Ok(Ok((_streams, _child))) => {
            println!("\nRESULT: connected in {:?}", started.elapsed());
        }
        Ok(Err(e)) => {
            println!("\nRESULT: failed after {:?}\n  {e:#}", started.elapsed());
        }
        Err(_) => {
            println!(
                "\nRESULT: TIMED OUT after 45s - hung inside start_server \
                 (the [scrcpy-server] lines above say how far the device got)"
            );
        }
    }
}
