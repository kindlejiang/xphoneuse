//! Throwaway diagnostic: one lossless PNG from the CoreDevice
//! screencaptureservice over the tunnel, written to a file, so it can be
//! compared pixel-for-pixel against a live mirror frame (the H.264 decode).
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_png_dump -- out.png [udid]

use xphoneuse_ios::tunnel::TunnelHandle;
use idevice::core_device::ImageFormat;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "tunnel-truth.png".into());
    let udid = args
        .next()
        .unwrap_or_else(|| "00008140-0000000000000000".into());

    let tunnel = TunnelHandle::start(&udid, false).await.expect("tunnel start");
    let png = tunnel
        .screenshot(ImageFormat::Png)
        .await
        .expect("tunnel screenshot");
    std::fs::write(&out, &png).expect("write png");
    eprintln!("[png-dump] wrote {out} ({} bytes)", png.len());
    tunnel.stop();
}
