//! AgentInput probe: send text to an iPhone running the AgentInput keyboard.
//!
//! Usage:
//!   cargo run -p xphoneuse-ios --example agent_input_probe -- <ip|usb> [text] [pin] [--enter]
//!
//! Examples:
//!   cargo run -p xphoneuse-ios --example agent_input_probe -- 192.168.1.20 "你好"
//!   cargo run -p xphoneuse-ios --example agent_input_probe -- usb "你好"
//!   cargo run -p xphoneuse-ios --example agent_input_probe -- usb "hi" 482103 --enter
//!
//! `usb` starts a localhost forward to the phone's AgentInput port through
//! usbmuxd (iproxy-style) - no shared WiFi/LAN needed, just the cable.

use xphoneuse_ios::agent_input::AgentInputClient;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: agent_input_probe <ip|usb> [text] [pin] [--enter]");
        std::process::exit(2);
    }
    let host_arg = args[0].clone();
    let enter = args.iter().any(|a| a == "--enter");
    let rest: Vec<String> = args[1..]
        .iter()
        .filter(|a| *a != "--enter")
        .cloned()
        .collect();
    let text = rest.first().cloned().unwrap_or_else(|| "hello".to_string());
    let pin = rest.get(1).cloned();

    // `usb`: forward the phone's AgentInput port through usbmuxd and connect
    // to the local end of the forward.
    let (host, port) = if host_arg.eq_ignore_ascii_case("usb") {
        let devices = xphoneuse_ios::device::list_devices()
            .await
            .expect("list devices");
        let Some(target) = devices.first() else {
            eprintln!("no iOS devices visible to usbmuxd");
            std::process::exit(1);
        };
        eprintln!("forwarding to {} ({})", target.name, target.udid);
        let forward =
            xphoneuse_ios::usb_forward::UsbForward::start(&target.udid, xphoneuse_ios::agent_input::DEFAULT_PORT)
                .expect("start usb forward");
        let port = forward.local_port;
        // Keep the forward alive for the whole probe.
        std::mem::forget(forward);
        ("127.0.0.1".to_string(), port)
    } else {
        (host_arg.clone(), xphoneuse_ios::agent_input::DEFAULT_PORT)
    };

    // The AgentInput client is blocking (std TcpStream) - run it on the
    // blocking pool like the app does, so it can't stall the async workers
    // the usb forward's accept loop needs (that starvation is exactly what
    // made early probe runs time out with nothing answering on the other
    // side).
    let via_usb = host_arg.eq_ignore_ascii_case("usb");
    let outcome = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut client = AgentInputClient::new(&host, port, pin);
        client.connect().map_err(|e| format!("connect failed: {e}"))?;
        println!(
            "connected ws://{}:{} ({})",
            client.host(),
            client.port(),
            if via_usb { "usb" } else { "lan" }
        );

        client.send_text(&text).map_err(|e| format!("send failed: {e}"))?;
        println!("sent: {text:?}");
        if enter {
            client.enter().map_err(|e| format!("enter failed: {e}"))?;
            println!("sent: Enter");
        }

        // Wait for the phone's `context` echo to confirm the text really
        // landed in the focused field (before = text preceding the cursor).
        match client.wait_context(std::time::Duration::from_secs(3)) {
            Ok(Some(before)) => println!("phone field now ends with: {before:?}"),
            Ok(None) => println!("no context echo (timeout) - delivery unconfirmed"),
            Err(e) => println!("context read failed: {e}"),
        }
        Ok(())
    })
    .await
    .expect("probe task panicked");

    if let Err(e) = outcome {
        eprintln!("{e}");
        std::process::exit(1);
    }
    println!("OK");
}
