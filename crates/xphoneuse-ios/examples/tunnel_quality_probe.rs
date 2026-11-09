//! Throwaway diagnostic: measure how faithful the CoreDevice display stream is
//! to the real screen.
//!
//! One tunnel, one media session: it subscribes to the depacketized H.264
//! access units (written to `<prefix>.h264` as Annex-B) and, mid-capture, takes
//! one lossless screencaptureservice PNG (`<prefix>-truth.png`). Decode the
//! Annex-B with ffmpeg and compare the frames to the PNG - the PSNR of the
//! matching frame is what the mirror actually shows.
//!
//! The screen must be static for the comparison to mean anything.
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_quality_probe -- [prefix] [seconds] [udid]

use xphoneuse_ios::tunnel::{TunnelFrame, TunnelHandle};
use idevice::core_device::ImageFormat;
use std::io::Write;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let prefix = args.next().unwrap_or_else(|| "quality".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let udid = args
        .next()
        .unwrap_or_else(|| "00008140-0000000000000000".into());

    let tunnel = TunnelHandle::start(&udid, true).await.expect("tunnel start");
    let mut frames = tunnel.subscribe_frames().await.expect("subscribe");

    let au_path = format!("{prefix}.h264");
    let png_path = format!("{prefix}-truth.png");

    // Frames first (the stream needs a moment to produce a keyframe), then the
    // lossless capture, then keep counting - the PNG has to sit between the
    // frames it is compared against.
    let capture_after = Duration::from_secs(3);
    let started = Instant::now();
    let mut capture_done = false;
    let mut n = 0u64;
    let mut keys = 0u64;
    let mut bytes = 0u64;
    let mut out = std::fs::File::create(&au_path).expect("create h264 dump");

    while started.elapsed() < Duration::from_secs(seconds) {
        let next = tokio::time::timeout(Duration::from_secs(2), frames.recv()).await;
        let frame = match next {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => continue,
        };
        match frame {
            TunnelFrame::AccessUnit { key, data, .. } => {
                out.write_all(&data).expect("write au");
                n += 1;
                bytes += data.len() as u64;
                if key {
                    keys += 1;
                }
                if n % 60 == 0 {
                    eprintln!(
                        "[quality] {n} AUs ({keys} key), {bytes} bytes at {:?}",
                        started.elapsed()
                    );
                }
                if !capture_done && started.elapsed() >= capture_after {
                    capture_done = true;
                    let t = Instant::now();
                    match tunnel.screenshot(ImageFormat::Png).await {
                        Ok(png) => {
                            std::fs::write(&png_path, &png).expect("write png");
                            eprintln!(
                                "[quality] truth PNG {} bytes in {:?} after {n} AUs",
                                png.len(),
                                t.elapsed()
                            );
                        }
                        Err(e) => eprintln!("[quality] screenshot failed: {e}"),
                    }
                }
            }
            _ => {}
        }
    }

    let _ = out.flush();
    eprintln!(
        "[quality] {n} access units ({keys} keyframes), {bytes} bytes in {:?} -> {au_path}, {png_path}",
        started.elapsed()
    );
    tunnel.stop();
}
