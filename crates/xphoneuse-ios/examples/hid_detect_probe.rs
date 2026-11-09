//! Bridge-detection timing probe: run `detect_first` and report what took
//! how long. The bridge should be found on the first USB port probed.

fn main() {
    let t = std::time::Instant::now();
    match xphoneuse_ios::hid::detect_first() {
        Some(s) => println!(
            "bridge on {} (MAC {}, model {}, fw {}) in {:?}",
            s.port_name(),
            s.mac,
            s.model,
            s.fw_ver,
            t.elapsed()
        ),
        None => println!("no bridge found in {:?}", t.elapsed()),
    }
}
