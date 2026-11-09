//! Probe: put the iPhone at a position and hold it there.
//!
//! The point of this probe is the *holding*: a simulated position is a message
//! on a channel, not a setting, so the device snaps back to its real GPS the
//! moment the channel closes. Watch the phone's Maps app while this runs, then
//! watch it jump back.
//!
//! Usage: cargo run -p xphoneuse-ios --example location_probe -- [lat] [lon] [udid]

use std::time::Duration;

use xphoneuse_ios::location::start_ready;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let lat: f64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(39.90733);
    let lon: f64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(116.39117);
    let udid = args
        .next()
        .unwrap_or_else(|| "00008140-0000000000000000".into());

    eprintln!("[location-probe] opening a session for {udid}");
    let started = std::time::Instant::now();
    let session = match start_ready(&udid).await {
        Ok(session) => session,
        Err(e) => {
            eprintln!("[location-probe] failed after {:?}: {e}", started.elapsed());
            std::process::exit(1);
        }
    };
    eprintln!("[location-probe] session up in {:?}", started.elapsed());

    if let Err(e) = session.set(lat, lon).await {
        eprintln!("[location-probe] set failed: {e}");
        std::process::exit(1);
    }
    eprintln!("[location-probe] set to {lat}, {lon} - the phone should be there now");

    // Moving the position is the same call again; a route would just be a loop
    // of these. Here it is one small step, so the change is visible.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let (lat2, lon2) = (lat + 0.01, lon + 0.01);
    if let Err(e) = session.set(lat2, lon2).await {
        eprintln!("[location-probe] second set failed: {e}");
    } else {
        eprintln!("[location-probe] moved to {lat2}, {lon2}");
    }

    eprintln!("[location-probe] holding for 30s (Ctrl-C to stop early)");
    tokio::time::sleep(Duration::from_secs(30)).await;

    match session.clear().await {
        Ok(()) => eprintln!("[location-probe] cleared - the phone is back on its real position"),
        Err(e) => eprintln!("[location-probe] clear failed: {e}"),
    }
}
