//! Probe: CoreDevice one-shot screenshots over the tunnel (via TunnelHandle),
//! with DVT coexistence verification.
//!
//! Legs:
//! 1. tunnel PNG x5 and JPEG x5, with timings and sizes (the DVT replacement
//!    candidate — Xcode's capture path).
//! 2. while the tunnel's display stream keeps running, take 3 DVT
//!    screenshots via the existing persistent-actor path, verifying the two
//!    transports do not wedge each other.
//! 3. a few more tunnel screenshots afterwards, to confirm the stream
//!    survived the DVT traffic.
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_screenshot_probe [udid]

use xphoneuse_ios::tunnel::TunnelHandle;
use idevice::core_device::ImageFormat;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let udid = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "00008140-0000000000000000".into());
    eprintln!("[cap-probe] starting tunnel for {udid}");

    // want_frames=true: closest to production use (video subscriber task
    // draining the frame channel).
    let t0 = Instant::now();
    let tunnel = TunnelHandle::start(&udid, true).await.expect("tunnel start");
    eprintln!("[cap-probe] tunnel up in {:?}", t0.elapsed());
    let mut frames = tunnel.subscribe_frames().await.expect("subscribe");

    let frame_task = tokio::spawn(async move {
        let mut n = 0u64;
        let mut keyframes = 0u64;
        let mut bytes = 0u64;
        let start = Instant::now();
        while let Some(frame) = frames.recv().await {
            let (key, data) = match frame {
                xphoneuse_ios::tunnel::TunnelFrame::AccessUnit { key, data, .. } => (key, data),
                // VisibleSize is metadata, not video — nothing to count
                _ => continue,
            };
            n += 1;
            bytes += data.len() as u64;
            if key {
                keyframes += 1;
            }
            if n % 120 == 0 {
                eprintln!(
                    "[cap-probe] frames: {n} ({keyframes} key), {bytes} bytes, {:.1}fps avg",
                    n as f64 / start.elapsed().as_secs_f64()
                );
            }
        }
    });
    tokio::time::sleep(Duration::from_secs(3)).await;

    for (format, label) in [(ImageFormat::Png, "png"), (ImageFormat::Jpeg, "jpeg")] {
        let mut total = Duration::ZERO;
        let mut sizes = Vec::new();
        // The service only accepts "png" on-device (jpeg/jpg/JPEG/heic/tiff
        // are rejected as invalid ScreenshotImageFormat) — the jpeg rounds
        // document that failure instead of erroring 5 times.
        let rounds = if label == "png" { 5 } else { 1 };
        for round in 0..rounds {
            let t = Instant::now();
            match tunnel.screenshot(format).await {
                Ok(bytes) => {
                    total += t.elapsed();
                    sizes.push(bytes.len());
                    eprintln!(
                        "[cap-probe] {label} #{round}: {:?}, {} bytes",
                        t.elapsed(),
                        bytes.len()
                    );
                }
                Err(e) => eprintln!("[cap-probe] {label} #{round} FAILED after {:?}: {e}", t.elapsed()),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !sizes.is_empty() {
            eprintln!(
                "[cap-probe] {label} avg {:?} (min {}, max {} bytes)",
                total / sizes.len() as u32,
                sizes.iter().min().unwrap(),
                sizes.iter().max().unwrap()
            );
        }
    }

    eprintln!("[cap-probe] --- DVT coexistence leg (stream stays up) ---");
    for round in 0..3 {
        let t = Instant::now();
        match xphoneuse_ios::screenshot::take_screenshot(&udid).await {
            Ok(bytes) => eprintln!(
                "[cap-probe] dvt #{round}: {:?}, {} bytes",
                t.elapsed(),
                bytes.len()
            ),
            Err(e) => eprintln!("[cap-probe] dvt #{round} FAILED after {:?}: {e:#}", t.elapsed()),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    eprintln!("[cap-probe] --- tunnel capture after DVT traffic ---");
    for round in 0..3 {
        let t = Instant::now();
        match tunnel.screenshot(ImageFormat::Png).await {
            Ok(bytes) => eprintln!(
                "[cap-probe] png-post #{round}: {:?}, {} bytes",
                t.elapsed(),
                bytes.len()
            ),
            Err(e) => eprintln!("[cap-probe] png-post #{round} FAILED after {:?}: {e}", t.elapsed()),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    tunnel.stop();
    let _ = frame_task.await;
    eprintln!("[cap-probe] done");
}
