//! Probe serial ports for the AC636N BLE-HID bridge and verify it with the
//! encrypted AUTH handshake. Usage:
//!
//!   cargo run -p xphoneuse-ios --example hid_probe              # probe all ports
//!   cargo run -p xphoneuse-ios --example hid_probe -- COM7       # probe one port
//!   cargo run -p xphoneuse-ios --example hid_probe -- COM7 STATUS
//!                                                              # + send a command
//!
//! Key override: UART_HID_KEY=<32 hex chars> (defaults to the dev key).

use xphoneuse_ios::hid::{HidSession, HID_FW_VER, HID_MODEL};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let port_filter = args.get(1).filter(|a| *a != "--");
    let command = args.get(2).cloned();

    println!("== serial ports ==");
    let ports = serialport::available_ports()?;
    if ports.is_empty() {
        println!("(none)");
        return Ok(());
    }
    for p in &ports {
        let kind = match &p.port_type {
            serialport::SerialPortType::UsbPort(info) => {
                format!("usb {:04x}:{:04x}", info.vid, info.pid)
            }
            _ => "other".to_string(),
        };
        println!("  {}  ({})", p.port_name, kind);
    }

    println!("\n== AUTH probe (reference MODEL={}, VER={}) ==", HID_MODEL, HID_FW_VER);
    let candidates: Vec<&str> = match port_filter {
        Some(name) => vec![name.as_str()],
        None => ports.iter().map(|p| p.port_name.as_str()).collect(),
    };

    let mut found: Option<String> = None;
    for name in candidates {
        print!("  {} ... ", name);
        match HidSession::connect(name) {
            Ok(session) => {
                println!("OK  MAC={} MODEL={} VER={}", session.mac, session.model, session.fw_ver);
                found = Some(session.port_name().to_string());
            }
            Err(e) => println!("fail: {e}"),
        }
    }

    match found {
        None => {
            println!("\nno bridge device found; check USB connection and DTR/RTS");
            Ok(())
        }
        Some(name) => {
            if let Some(cmd) = command {
                let mut session = HidSession::connect(&name)?;
                println!("\n>> {}", cmd);
                match session.send(&cmd) {
                    Ok(reply) => println!("<< {}", reply),
                    Err(e) => println!("send failed: {e}"),
                }
            }
            Ok(())
        }
    }
}
