//! Which adb the app would use, and whether it runs.
//!
//! The app tries an Android SDK install first, then whatever `adb` means on
//! PATH, then the copy it ships with - because two adb versions fight over the
//! same server port, and the machine's own adb is the one its other tools are
//! talking to. This prints the winner, which is the first thing to look at
//! when Android device listing misbehaves.
//!
//! Usage: cargo run -p xphoneuse-core --example adb_resolve [resource-dir]
//! (the resource dir is where the bundled `resources/adb[.exe]` lives, e.g.
//! `src-tauri/target/debug` for a debug build)

#[tokio::main]
async fn main() {
    if let Some(dir) = std::env::args().nth(1) {
        xphoneuse_core::adb::set_resource_dir(std::path::PathBuf::from(dir));
    } else {
        eprintln!(
            "note: no resource dir given, so the bundled adb is not a candidate\n\
             pass one to include it: e.g. src-tauri/target/debug"
        );
    }

    match xphoneuse_core::adb::adb_available().await {
        Ok(()) => println!("adb: {} (runs)", xphoneuse_core::adb::adb_location().await),
        Err(e) => println!("adb: not usable - {e}"),
    }

    match xphoneuse_core::adb::list_devices().await {
        Ok(devices) => {
            println!("{} device(s):", devices.len());
            for d in devices {
                println!("  {} {} ({})", d.serial, d.model, d.state);
            }
        }
        Err(e) => println!("listing failed: {e:#}"),
    }
}
