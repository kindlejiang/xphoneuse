//! Probe: inspect and forge the pasteboard's `sourceMetadata` over the
//! CoreDevice tunnel.
//!
//! iOS 16+ shows the "Allow Paste?" permission alert when an app reads
//! pasteboard content written by another process — which includes everything
//! this service SETs. The alert's attribution comes from the pasteboard's
//! recorded origin, so this probe dumps what the device records (PULL, raw
//! plist) and lets us SET with forged metadata to find a variant the device
//! treats as exempt.
//!
//! Usage:
//! ```text
//! cargo run -p xphoneuse-ios --example pasteboard_probe -- pull
//! cargo run -p xphoneuse-ios --example pasteboard_probe -- set "text" [meta...] [--paste]
//! ```
//!
//! `meta` args (may repeat): `null` (the default), `empty` (empty dict), or
//! `key=value` string entries. `--paste` sends Cmd+V over hid.indigo after
//! the SET (the app must be mirroring for the HID auth gate to be open).
//!
//! Workflow: user copies text on the phone by hand → `pull` shows what a
//! same-device copy records → forge those fields with `set k=v ...` →
//! `--paste` and see whether the alert still appears.

use idevice::core_device::hid::{ButtonState, IndigoHidClient};
use idevice::core_device::{
    PasteboardServiceClient, ScreenCaptureServiceClient, GENERAL_PASTEBOARD};
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::AdapterHandle;
use idevice::{IdeviceService, ReadWrite, RemoteXpcClient, RsdService};
use idevice::xpc::{Dictionary, XPCObject};

const KEY_LEFT_GUI: u64 = 0xE3;
const KEY_V: u64 = 0x19;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_else(|| "pull".to_string());
    let paste = args.iter().any(|a| a.as_str() == "--paste");
    let text = args
        .iter()
        .skip(1)
        .find(|a| {
            let a = a.as_str();
            !a.contains('=') && a != "--paste" && a != "null" && a != "empty"
        })
        .cloned();
    let meta_args: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| {
            let a = a.as_str();
            a.contains('=') || a == "null" || a == "empty"
        })
        .collect();

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ios={}", target.udid, target.ios_version);

    let provider = xphoneuse_ios::device::provider_for(&target.udid)
        .await
        .expect("provider");

    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .expect("CoreDeviceProxy");
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .expect("software tunnel")
        .to_async_handle();
    let stream = adapter.connect(rsd_port).await.expect("rsd port connect");
    let handshake = RsdHandshake::new(stream).await.expect("RSD handshake");

    let mut pb = open_pasteboard(&mut adapter, &handshake).await;

    match mode.as_str() {
        "pull" => {
            let reply = pb.get_raw(GENERAL_PASTEBOARD).await.expect("PULL");
            println!("{}", dump(&reply));
        }
        "capture" => {
            // Ground-truth framebuffer capture (screencaptureservice), for
            // comparing edge content against the display stream.
            let port = handshake
                .services
                .get("com.apple.coredevice.screencaptureservice")
                .expect("screencaptureservice in RSD list")
                .port;
            let stream = adapter.connect(port).await.expect("capture connect");
            let mut cap: idevice::core_device::ScreenCaptureServiceClient<Box<dyn ReadWrite>> =
                ScreenCaptureServiceClient::from_stream(Box::new(stream) as Box<dyn ReadWrite>)
                    .await
                    .expect("capture handshake");
            let png = cap
                .take_screenshot(None, idevice::core_device::ImageFormat::Png)
                .await
                .expect("capture");
            std::fs::write("capture-probe.png", &png).expect("write");
            eprintln!("saved capture-probe.png ({} bytes)", png.len());
        }
        "set" => {
            let text = text.expect("set needs a text argument");
            let source_metadata = build_metadata(&meta_args);
            eprintln!("SET {text:?} with sourceMetadata={source_metadata:?}");
            pb.set_text_ex(&text, GENERAL_PASTEBOARD, source_metadata)
                .await
                .expect("SET");
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let reply = pb.get_raw(GENERAL_PASTEBOARD).await.expect("PULL after SET");
            println!("{}", dump(&reply));
            if paste {
                let mut indigo = open_indigo(&mut adapter, &handshake).await;
                indigo
                    .send_keyboard(KEY_LEFT_GUI, ButtonState::Down)
                    .await
                    .expect("GUI down");
                indigo.send_keyboard(KEY_V, ButtonState::Down).await.expect("V down");
                indigo.send_keyboard(KEY_V, ButtonState::Up).await.expect("V up");
                indigo
                    .send_keyboard(KEY_LEFT_GUI, ButtonState::Up)
                    .await
                    .expect("GUI up");
                eprintln!("Cmd+V sent");
            }
        }
        other => {
            eprintln!("unknown mode {other:?}: use pull or set");
            std::process::exit(2);
        }
    }
}

fn build_metadata(args: &[&String]) -> XPCObject {
    if args.iter().any(|a| a.as_str() == "null") {
        return XPCObject::Null;
    }
    let mut d = Dictionary::new();
    for a in args.iter().filter(|a| a.contains('=')) {
        let (k, v) = a.split_once('=').expect("k=v");
        d.insert(k.to_string(), XPCObject::String(v.to_string()));
    }
    XPCObject::Dictionary(d)
}

fn dump(value: &plist::Value) -> String {
    let mut buf = Vec::new();
    value.to_writer_xml(&mut buf).expect("plist xml");
    String::from_utf8_lossy(&buf).into_owned()
}

async fn open_pasteboard(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> PasteboardServiceClient<Box<dyn ReadWrite>> {
    let port = handshake
        .services
        .get("com.apple.coredevice.pasteboardservice")
        .expect("pasteboardservice in RSD list")
        .port;
    let stream = adapter.connect(port).await.expect("pasteboard connect");
    PasteboardServiceClient::from_stream(Box::new(stream) as Box<dyn ReadWrite>)
        .await
        .expect("pasteboard handshake")
}

async fn open_indigo(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> IndigoHidClient<Box<dyn ReadWrite>> {
    let port = handshake
        .services
        .get("com.apple.coredevice.hid.indigo")
        .expect("hid.indigo in RSD list")
        .port;
    let stream = adapter.connect(port).await.expect("indigo connect");
    let xpc = RemoteXpcClient::new(Box::new(stream) as Box<dyn ReadWrite>)
        .await
        .expect("indigo xpc");
    IndigoHidClient::new(xpc)
}
