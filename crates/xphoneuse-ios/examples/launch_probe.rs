//! Probe: launch an app on the iPhone over DVT process control, or list what
//! can be launched.
//!
//! The launch path needs nothing but the developer image - no tunnel session,
//! no active screen stream - so this is the whole of it: connect, launch,
//! print the pid. Watch the phone to see the app come up.
//!
//! `--list` asks the device for its installed apps instead (lockdown's
//! installation proxy, no image needed): the bundle ids to launch, and the
//! names they belong to.
//!
//! Usage:
//!   cargo run -p xphoneuse-ios --example launch_probe -- <bundle id> [udid]
//!   cargo run -p xphoneuse-ios --example launch_probe -- --list [udid]
//!   cargo run -p xphoneuse-ios --example launch_probe -- --list-system [udid]
//!   cargo run -p xphoneuse-ios --example launch_probe -- --list-any [udid]

use xphoneuse_ios::launch::launch_app_ready;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_default();
    let default_udid = || "00008140-0000000000000000".to_string();

    // --list is the apps someone chose, --list-system Apple's own, --list-any
    // both. Each takes an optional udid.
    let listing = match first.as_str() {
        "--list" => Some("User"),
        "--list-system" => Some("System"),
        "--list-any" => Some("Any"),
        _ => None,
    };
    if let Some(kind) = listing {
        let udid = args.next().unwrap_or_else(default_udid);
        list_apps(&udid, kind).await;
        return;
    }

    let bundle = if first.is_empty() { "com.apple.Preferences".to_string() } else { first };
    let udid = args.next().unwrap_or_else(default_udid);

    eprintln!("[launch-probe] launching {bundle} on {udid}");
    let started = std::time::Instant::now();
    match launch_app_ready(&udid, &bundle).await {
        Ok(pid) => eprintln!(
            "[launch-probe] launched {bundle} as pid {pid} in {:?}",
            started.elapsed()
        ),
        Err(e) => {
            eprintln!("[launch-probe] failed in {:?}: {e}", started.elapsed());
            std::process::exit(1);
        }
    }
}

async fn list_apps(udid: &str, kind: &str) {
    match xphoneuse_ios::apps::list_apps(udid, kind).await {
        Ok(apps) => {
            eprintln!("[launch-probe] {} {} apps on {udid}", apps.len(), kind.to_lowercase());
            for app in apps {
                println!("{}	{}	{}", app.bundle_id, app.name, app.version);
            }
        }
        Err(e) => {
            eprintln!("[launch-probe] {e}");
            std::process::exit(1);
        }
    }
}
