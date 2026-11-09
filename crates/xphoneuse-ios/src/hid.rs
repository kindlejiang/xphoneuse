//! CDC-ACM transport for the AC636N BLE-HID bridge (fw-AC63_BT_SDK).
//!
//! The bridge presents itself as a USB CDC-ACM virtual serial port and
//! controls the iPhone over Bluetooth HID (keyboard + absolute touch).
//! Every command line is wrapped in an encrypted frame:
//!
//! ```text
//! $ BASE64( AES128-CBC( PKCS7( "JLHD" ‖ DIR(1B) ‖ NONCE(4B) ‖ text ) ) ) CRLF
//! ```
//!
//! DIR = 0x01 for host→device commands, 0x02 for replies. The device
//! publishes a fresh one-time NONCE with every reply; the host must embed
//! it in the next command (used once, replays rejected). The session opens
//! with an `AUTH` command carrying NONCE=0 (the only exempt frame); the
//! reply (`OK DEV MAC=… MODEL=… VER=…`) both identifies the device and
//! carries the first usable NONCE.
//!
//! See `docs/uart-hid-command-protocol.md` and
//! `docs/uart-hid-security-design.md` in the firmware SDK.

use aes::Aes128;
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use cbc::{Decryptor, Encryptor};
use serialport::{SerialPort, SerialPortInfo};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

/// Reference model string the bridge reports via AUTH. Not enforced:
/// production firmware customizes it (e.g. "AiH" + Bluetooth address
/// suffix), and the encrypted AUTH exchange is the real identity proof.
pub const HID_MODEL: &str = "AC636N-KBMS";
/// Firmware version this client was written against.
pub const HID_FW_VER: &str = "1.4.0";

const MAGIC: &[u8; 4] = b"JLHD";
const DIR_CMD: u8 = 0x01;
const DIR_REPLY: u8 = 0x02;
const HEAD_LEN: usize = 9; // MAGIC(4) + DIR(1) + NONCE(4)
const READ_TIMEOUT: Duration = Duration::from_millis(1500);
const AUTH_TIMEOUT: Duration = Duration::from_millis(800);

type Aes128CbcEnc = Encryptor<Aes128>;
type Aes128CbcDec = Decryptor<Aes128>;

/// Development placeholder key from the firmware's `uart_hid_key.h`
/// (00 01 02 … 0F). Override with `UART_HID_KEY` (32 hex chars) — the
/// production key must never be committed to the repository.
const DEV_KEY: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
];
/// Matching development IV (10 11 12 … 1F).
const DEV_IV: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
];

fn load_key() -> ([u8; 16], [u8; 16]) {
    if let Ok(hex) = std::env::var("UART_HID_KEY") {
        if let Ok(key) = hex_to_bytes(hex.trim()) {
            return (key, DEV_IV);
        }
        eprintln!("[hid] UART_HID_KEY is not 32 hex chars; using dev key");
    }
    (DEV_KEY, DEV_IV)
}

fn hex_to_bytes(hex: &str) -> Result<[u8; 16]> {
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected 32 hex characters");
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

// --- framing ---------------------------------------------------------------

fn pkcs7_pad(buf: &mut Vec<u8>) {
    let pad = 16 - buf.len() % 16;
    buf.resize(buf.len() + pad, pad as u8);
}

/// Pack a plaintext line into an encrypted frame (with `$` prefix, no CRLF).
fn pack_frame(dir: u8, nonce: [u8; 4], text: &str, key: &[u8; 16], iv: &[u8; 16]) -> String {
    let mut plain: Vec<u8> = Vec::with_capacity(HEAD_LEN + text.len() + 16);
    plain.extend_from_slice(MAGIC);
    plain.push(dir);
    plain.extend_from_slice(&nonce);
    plain.extend_from_slice(text.as_bytes());
    pkcs7_pad(&mut plain);

    let cipher = Aes128CbcEnc::new_from_slices(key, iv).expect("valid key/iv sizes");
    // encrypt in place: the PKCS7-padded plaintext is the buffer
    let mut crypt = plain;
    let crypt_len = crypt.len();
    cipher.encrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut crypt, crypt_len)
        .expect("pkcs7-padded input");
    format!("${}", base64::engine::general_purpose::STANDARD.encode(&crypt))
}

/// Unpack an encrypted line: returns (dir, nonce, plaintext). Verifies the
/// `$` prefix, Base64, MAGIC and PKCS7 padding.
fn unpack_frame(line: &str, key: &[u8; 16], iv: &[u8; 16]) -> Result<(u8, [u8; 4], String)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let b64 = line
        .strip_prefix('$')
        .ok_or_else(|| anyhow!("frame does not start with '$'"))?;
    let crypt = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow!("bad base64: {e}"))?;
    if crypt.len() < HEAD_LEN || crypt.len() % 16 != 0 {
        bail!("bad ciphertext length {}", crypt.len());
    }

    let cipher = Aes128CbcDec::new_from_slices(key, iv).expect("valid key/iv sizes");
    let mut plain = crypt;
    cipher.decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut plain)
        .map_err(|e| anyhow!("decrypt failed: {e}"))?;

    if plain.len() < HEAD_LEN || &plain[..4] != MAGIC {
        bail!("MAGIC mismatch (wrong key or not our device)");
    }
    let dir = plain[4];
    let mut nonce = [0u8; 4];
    nonce.copy_from_slice(&plain[5..9]);

    let pad = *plain.last().expect("non-empty") as usize;
    if pad == 0 || pad > 16 || pad > plain.len() - HEAD_LEN {
        bail!("bad PKCS7 padding");
    }
    for &b in &plain[plain.len() - pad..] {
        if b as usize != pad {
            bail!("bad PKCS7 padding");
        }
    }
    let text = std::str::from_utf8(&plain[HEAD_LEN..plain.len() - pad])
        .map_err(|e| anyhow!("reply is not utf-8: {e}"))?
        .to_string();
    Ok((dir, nonce, text))
}

// --- session ----------------------------------------------------------------

/// A connected, AUTH-verified bridge session. Commands are strictly
/// one-question-one-answer: `send()` waits for the reply (which rotates the
/// NONCE for the next command).
pub struct HidSession {
    port_name: String,
    port: Box<dyn SerialPort>,
    reader: BufReader<Box<dyn SerialPort>>,
    nonce: [u8; 4],
    key: [u8; 16],
    iv: [u8; 16],
    pub mac: String,
    pub model: String,
    pub fw_ver: String,
}

impl HidSession {
    /// Open `port_name`, assert DTR/RTS, and run the AUTH handshake.
    /// Returns an error unless the device proves itself via the encrypted
    /// AUTH exchange (MAGIC + direction check, MAC/MODEL/VER fields present).
    /// The MODEL value itself is not validated: production firmware
    /// customizes it (e.g. "AiH" + Bluetooth address suffix).
    pub fn connect(port_name: &str) -> Result<Self> {
        let mut port = serialport::new(port_name, 115200)
            .timeout(AUTH_TIMEOUT)
            .open()
            .map_err(|e| anyhow!("open {}: {e}", port_name))?;

        // The USB channel only sends data while the host holds DTR/RTS.
        port.write_data_terminal_ready(true).ok();
        port.write_request_to_send(true).ok();

        let (key, iv) = load_key();
        let mut reader = BufReader::new(
            port.try_clone()
                .map_err(|e| anyhow!("clone port: {e}"))?,
        );
        let mut nonce = [0u8; 4];
        let mut info: Option<(String, String, String)> = None;

        // AUTH is repeat-safe: every reply rotates the NONCE.
        for _ in 0..4 {
            write_frame(&mut *port, &pack_frame(DIR_CMD, nonce, "AUTH", &key, &iv))?;
            match read_frame(&mut reader, AUTH_TIMEOUT) {
                Ok((dir, n, text)) => {
                    if dir != DIR_REPLY {
                        continue;
                    }
                    nonce = n;
                    if let Some(parsed) = parse_auth_reply(&text) {
                        info = Some(parsed);
                        break;
                    }
                }
                Err(_) => continue, // timeout / bad frame: retry AUTH
            }
        }

        let (mac, model, fw_ver) =
            info.ok_or_else(|| anyhow!("AUTH failed on {}: not our device or wrong key", port_name))?;

        port.set_timeout(READ_TIMEOUT)
            .map_err(|e| anyhow!("set timeout: {e}"))?;

        Ok(Self {
            port_name: port_name.to_string(),
            port,
            reader,
            nonce,
            key,
            iv,
            mac,
            model,
            fw_ver,
        })
    }

    /// Send one command line and wait for its reply (plaintext, without the
    /// trailing "OK " prefix handling — the raw reply line is returned).
    pub fn send(&mut self, cmd: &str) -> Result<String> {
        let frame = pack_frame(DIR_CMD, self.nonce, cmd, &self.key, &self.iv);
        write_frame(&mut *self.port, &frame)?;

        // One question, one answer. On failure, re-sync with AUTH (its
        // reply carries a fresh NONCE; repeat-safe) so the next command
        // starts from a known state.
        match read_frame(&mut self.reader, READ_TIMEOUT) {
            Ok((dir, nonce, text)) => {
                if dir != DIR_REPLY {
                    bail!("unexpected frame direction {:#x} on {}", dir, self.port_name);
                }
                self.nonce = nonce;
                Ok(text)
            }
            Err(e) => {
                let _ = self.resync();
                Err(e)
            }
        }
    }

    /// Re-run the AUTH handshake to obtain a fresh NONCE (used after a
    /// timeout or a dropped reply). Nonce carried by `send` may be stale
    /// after this; callers should just retry their command.
    pub fn resync(&mut self) -> Result<()> {
        for _ in 0..3 {
            write_frame(&mut *self.port, &pack_frame(DIR_CMD, [0; 4], "AUTH", &self.key, &self.iv))?;
            if let Ok((DIR_REPLY, n, text)) = read_frame(&mut self.reader, AUTH_TIMEOUT) {
                if let Some((mac, _, _)) = parse_auth_reply(&text) {
                    if mac == self.mac {
                        self.nonce = n;
                        return Ok(());
                    }
                }
            }
        }
        bail!("resync AUTH failed on {}", self.port_name)
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

fn write_frame(port: &mut dyn SerialPort, frame: &str) -> Result<()> {
    port.write_all(frame.as_bytes())
        .and_then(|_| port.write_all(b"\r\n"))
        .and_then(|_| port.flush())
        .map_err(|e| anyhow!("serial write failed: {e}"))
}

fn read_frame(reader: &mut BufReader<Box<dyn SerialPort>>, timeout: Duration) -> Result<(u8, [u8; 4], String)> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => {}
            Ok(_) => {
                let s = String::from_utf8_lossy(&line).into_owned();
                if s.trim().is_empty() {
                    continue;
                }
                let (key, iv) = load_key();
                return unpack_frame(s.trim_end_matches(['\r', '\n']), &key, &iv);
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(anyhow!("serial read failed: {e}")),
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("reply timeout"));
        }
    }
}

/// Parse "OK DEV MAC=<12hex> MODEL=<model> VER=<ver>".
fn parse_auth_reply(text: &str) -> Option<(String, String, String)> {
    let rest = text.strip_prefix("OK DEV ")?;
    let mut mac = None;
    let mut model = None;
    let mut ver = None;
    for part in rest.split_whitespace() {
        if let Some(v) = part.strip_prefix("MAC=") {
            mac = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("MODEL=") {
            model = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("VER=") {
            ver = Some(v.to_string());
        }
    }
    Some((mac?, model?, ver?))
}

/// List ports that pass the AUTH handshake. Each probe opens the port and
/// runs a full encrypted AUTH exchange, so non-bridge ports answer with a
/// timeout (slow: ~0.8 s per port).
pub fn detect() -> Vec<(String, String, String, String)> {
    let mut found = Vec::new();
    let ports = match serialport::available_ports() {
        Ok(p) => cdc_acm_ports(p),
        Err(_) => return found,
    };
    for p in ports {
        if let Ok(session) = HidSession::connect(&p.port_name) {
            found.push((p.port_name, session.mac, session.model, session.fw_ver));
        }
    }
    found
}

/// Keep only the ports the bridge can actually be on: USB CDC-ACM.
///
/// Probing the others costs far more than it looks. A Bluetooth "Standard
/// Serial over Bluetooth link" port blocks inside the driver for ~13 s before
/// `open` fails (ERROR_SEM_TIMEOUT), and nothing in [`SerialPortInfo`] gives
/// it away - on Windows every one of them reports `SerialPortType::Unknown`,
/// so the old type-based filter let them all through.
///
/// * Windows: the inbox CDC-ACM driver is `usbser`, and
///   `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM` maps every COM port to its device
///   path (`\Device\USBSER000` versus `\Device\BthModem0`). One key read
///   answers the question for every port at once.
/// * macOS: a CDC-ACM device is `tty.usbmodem*`; FTDI/CP210x/CH34x are
///   `tty.usbserial*` or `tty.SLAB*`.
/// * Linux: `ttyACM*` is CDC-ACM, `ttyUSB*` is everything else.
///
/// If the platform mapping cannot be read the list is returned unchanged: a
/// bridge that is slow to find beats one that is never found.
fn cdc_acm_ports(ports: Vec<SerialPortInfo>) -> Vec<SerialPortInfo> {
    #[cfg(windows)]
    let names = windows_cdc_acm_ports();
    #[cfg(windows)]
    if names.is_none() {
        eprintln!("[hid-detect] cannot read the COM port map; probing every port");
        return ports;
    }

    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for p in ports {
        #[cfg(windows)]
        let is_cdc_acm = names
            .as_ref()
            .is_some_and(|n| n.iter().any(|c| c.eq_ignore_ascii_case(&p.port_name)));
        #[cfg(target_os = "macos")]
        let is_cdc_acm = p.port_name.contains("usbmodem");
        #[cfg(all(unix, not(target_os = "macos")))]
        let is_cdc_acm = p.port_name.starts_with("/dev/ttyACM");

        if is_cdc_acm {
            kept.push(p);
        } else {
            skipped.push(p.port_name);
        }
    }
    if kept.is_empty() && !skipped.is_empty() {
        eprintln!(
            "[hid-detect] no CDC-ACM port found; skipped [{}]",
            skipped.join(", ")
        );
    } else if !skipped.is_empty() {
        eprintln!(
            "[hid-detect] skipping non-CDC-ACM port(s): [{}]",
            skipped.join(", ")
        );
    }
    kept
}

/// COM ports whose Windows driver is the inbox CDC-ACM one (`usbser`), read
/// from the device map. `None` when the key cannot be read at all.
#[cfg(windows)]
fn windows_cdc_acm_ports() -> Option<Vec<String>> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"HARDWARE\DEVICEMAP\SERIALCOMM", KEY_READ)
        .ok()?;
    let mut ports = Vec::new();
    for entry in key.enum_values() {
        let (device_path, _) = entry.ok()?;
        // The value *name* is the device path; the data is the COM name.
        if !device_path.to_ascii_uppercase().contains("USBSER") {
            continue;
        }
        if let Ok(name) = key.get_value::<String, _>(&device_path) {
            ports.push(name);
        }
    }
    Some(ports)
}

/// Connect to the first bridge that passes the AUTH handshake.
///
/// Probes ports sequentially (USB ones first, see `probe_order`); each
/// non-bridge port burns up to 4 AUTH retries x AUTH_TIMEOUT (~3.2 s), so
/// this logs per-port timings - a slow connect here is almost always dead
/// COM ports ahead of the bridge.
pub fn detect_first() -> Option<HidSession> {
    let t_all = std::time::Instant::now();
    let ports = match serialport::available_ports() {
        Ok(p) => cdc_acm_ports(p),
        Err(e) => {
            eprintln!("[hid-detect] listing ports failed: {e}");
            return None;
        }
    };
    eprintln!(
        "[hid-detect] probing {} port(s): [{}]",
        ports.len(),
        ports
            .iter()
            .map(|p| p.port_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for p in ports {
        let t = std::time::Instant::now();
        match HidSession::connect(&p.port_name) {
            Ok(session) => {
                eprintln!(
                    "[hid-detect] {} AUTH ok in {:?} (MAC {})",
                    p.port_name,
                    t.elapsed(),
                    session.mac
                );
                eprintln!("[hid-detect] bridge found after {:?}", t_all.elapsed());
                return Some(session);
            }
            Err(e) => eprintln!(
                "[hid-detect] {} rejected in {:?}: {e}",
                p.port_name,
                t.elapsed()
            ),
        }
    }
    eprintln!(
        "[hid-detect] no bridge on any port after {:?}",
        t_all.elapsed()
    );
    None
}

// --- command mapping --------------------------------------------------------
//
// The bridge exposes a 1920×1920 logical touch plane; the receiving OS
// stretches it onto the physical screen, so a plain linear mapping from
// normalized coordinates is exact.

const LOGICAL_MAX: f64 = 1920.0;

fn norm_to_logical(x: f64, y: f64) -> (u32, u32) {
    let px = ((x.clamp(0.0, 1.0) * LOGICAL_MAX) as u32).min(1920);
    let py = ((y.clamp(0.0, 1.0) * LOGICAL_MAX) as u32).min(1920);
    (px, py)
}

/// Normalized touch action → bridge command line.
/// `down`/`move`/`up` map to `MDOWN`/`M`/`MUP` (hold semantics).
pub fn map_touch(action: &str, x: f64, y: f64) -> Result<String> {
    let (px, py) = norm_to_logical(x, y);
    let cmd = match action {
        "down" => format!("MDOWN {},{}", px, py),
        "move" => format!("M {},{}", px, py),
        "up" => format!("MUP {},{}", px, py),
        other => bail!("unknown touch action: {other}"),
    };
    Ok(cmd)
}

/// Normalized position → bridge `TAP` command: locate + light touch in one
/// step, handled entirely by the firmware. Prefer this over a host-side
/// MDOWN/MUP pair, where the serial round-trip in between can read as a
/// long-press.
pub fn map_tap(x: f64, y: f64) -> String {
    let (px, py) = norm_to_logical(x, y);
    format!("TAP {},{}", px, py)
}

/// Android keycode → bridge key tap. Both press and release collapse into a
/// single `K` (the bridge taps keys; hold semantics are not needed here).
pub fn map_key(keycode: u32) -> Result<String> {
    let name = match keycode {
        66 => "ENTER",
        67 => "BKSP",
        112 => "DEL",
        19 => "UP",
        20 => "DOWN",
        21 => "LEFT",
        22 => "RIGHT",
        111 => "ESC",
        61 => "TAB",
        other => bail!("no HID key mapping for Android keycode {other}"),
    };
    Ok(format!("K {}", name))
}

/// Text → `TEXT` command lines (printable ASCII only, ≤58 chars each, per
/// the firmware's 64-byte line limit). Non-ASCII characters are dropped.
pub fn map_text(text: &str) -> Vec<String> {
    let clean: String = text.chars().filter(|c| (' '..='~').contains(c)).collect();
    if clean.is_empty() {
        return Vec::new();
    }
    clean
        .as_bytes()
        .chunks(58)
        .map(|c| format!("TEXT {}", String::from_utf8_lossy(c)))
        .collect()
}

/// Device button (Android semantics) → consumer key (`KEY …`).
pub fn map_button(button: &str) -> Result<String> {
    let code = match button {
        "home" => "HOME",
        "back" => "BACK",
        "recents" => "APP_SWITCH",
        "power" => "POWER",
        "volume_up" => "VOL+",
        "volume_down" => "VOL-",
        other => bail!("unknown button: {other}"),
    };
    Ok(format!("KEY {}", code))
}

/// iOS has no consumer key for the app switcher ("recents"), so it is
/// synthesized as the system gesture: a slow drag up from the bottom edge
/// that pauses mid-screen before releasing. Returns (command, delay-after)
/// steps for the host to play back over the bridge.
///
/// Parameters verified against an iPhone (iOS 26): the touch must start
/// essentially AT the edge (y=1915 of 1920) and travel well up the screen
/// (42%); shallower drags or higher start points are ignored.
pub fn app_switcher_gesture() -> Vec<(String, std::time::Duration)> {
    const X: u32 = 960; // horizontal center of the logical plane
    const Y_START: u32 = 1915; // as close to the bottom edge as possible
    const Y_END: u32 = 800; // ~42% up the screen
    const STEPS: u32 = 12;

    let mut steps = Vec::with_capacity(STEPS as usize + 3);
    steps.push((format!("MDOWN {X},{Y_START}"), std::time::Duration::from_millis(80)));
    for i in 1..=STEPS {
        let y = Y_START - (Y_START - Y_END) * i / STEPS;
        steps.push((format!("M {X},{y}"), std::time::Duration::from_millis(70)));
    }
    // Hold: the pause at mid-screen is what makes iOS open the switcher
    // instead of completing the swipe to the home screen.
    steps.push((format!("M {X},{Y_END}"), std::time::Duration::from_millis(900)));
    steps.push((format!("MUP {X},{Y_END}"), std::time::Duration::ZERO));
    steps
}

/// Normalized scroll → `SCROLL` command (drag of up to ±500 logical units).
pub fn map_scroll(dx: f64, dy: f64) -> String {
    let sx = (dx.clamp(-1.0, 1.0) * 500.0) as i32;
    let sy = (dy.clamp(-1.0, 1.0) * 500.0) as i32;
    format!("SCROLL {},{}", sx, sy)
}

/// Normalized swipe (start → end, duration in ms) → `SWIPE` command.
pub fn map_swipe(x1: f64, y1: f64, x2: f64, y2: f64, duration_ms: u64) -> String {
    let (px1, py1) = norm_to_logical(x1, y1);
    let (px2, py2) = norm_to_logical(x2, y2);
    format!(
        "SWIPE {},{},{},{},{}",
        px1,
        py1,
        px2,
        py2,
        duration_ms.clamp(1, 10000)
    )
}

/// Convert an `ERR …` reply into a human-readable error.
pub fn map_hid_error(reply: &str) -> String {
    let err = reply.trim();
    match err {
        "ERR NOTCONN" => {
            "Bluetooth not connected - pair the bridge device in iPhone Settings > Bluetooth"
                .to_string()
        }
        "ERR BUSY" => "device busy, slow down the input rate".to_string(),
        "ERR FULL" => "keyboard buffer full (max 6 keys held)".to_string(),
        "ERR AUTH" => "session rejected by device (nonce desync)".to_string(),
        other => format!("device error: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let (key, iv) = load_key();
        let frame = pack_frame(DIR_CMD, [0xDE, 0xAD, 0xBE, 0xEF], "TAP 360,640", &key, &iv);
        assert!(frame.starts_with('$'));
        assert!(frame.len() <= 111 + 1);

        let (dir, nonce, text) = unpack_frame(&frame, &key, &iv).unwrap();
        assert_eq!(dir, DIR_CMD);
        assert_eq!(nonce, [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(text, "TAP 360,640");
    }

    #[test]
    fn frame_known_vector() {
        // Golden vector cross-checked with the firmware's sec_frame_pack:
        // key 00..0F, IV 10..1F, AUTH with zero nonce.
        let (key, iv) = load_key();
        let frame = pack_frame(DIR_CMD, [0; 4], "AUTH", &key, &iv);
        let (dir, nonce, text) = unpack_frame(&frame, &key, &iv).unwrap();
        assert_eq!((dir, nonce, text.as_str()), (DIR_CMD, [0; 4], "AUTH"));
    }

    #[test]
    fn frame_rejects_wrong_key() {
        let (key, iv) = load_key();
        let frame = pack_frame(DIR_CMD, [0; 4], "AUTH", &key, &iv);
        let mut bad = key;
        bad[0] ^= 0xFF;
        assert!(unpack_frame(&frame, &bad, &iv).is_err());
    }

    #[test]
    fn frame_rejects_plaintext() {
        let (key, iv) = load_key();
        assert!(unpack_frame("TAP 1,2", &key, &iv).is_err());
        assert!(unpack_frame("$bm90IGJhc2U2NA==", &key, &iv).is_err());
    }

    #[test]
    fn parse_auth() {
        let (mac, model, ver) =
            parse_auth_reply("OK DEV MAC=112233445566 MODEL=AC636N-KBMS VER=1.4.0").unwrap();
        assert_eq!(mac, "112233445566");
        assert_eq!(model, "AC636N-KBMS");
        assert_eq!(ver, "1.4.0");
        assert!(parse_auth_reply("OK CONN=1").is_none());
    }
}
