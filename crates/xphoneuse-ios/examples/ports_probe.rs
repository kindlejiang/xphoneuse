//! List serial ports with the detail the bridge detector sees: port type and
//! the USB ids the platform reports. Run it when detection is slow.
//!
//!   cargo run -p xphoneuse-ios --example ports_probe

fn main() {
    match serialport::available_ports() {
        Ok(ports) => {
            println!("{} port(s)", ports.len());
            for p in ports {
                match &p.port_type {
                    serialport::SerialPortType::UsbPort(u) => println!(
                        "{:<10} USB     vid={:04x} pid={:04x} mfg={:?} product={:?} serial={:?}",
                        p.port_name, u.vid, u.pid, u.manufacturer, u.product, u.serial_number
                    ),
                    other => println!("{:<10} {:?}", p.port_name, other),
                }
            }
        }
        Err(e) => println!("listing failed: {e}"),
    }
}
