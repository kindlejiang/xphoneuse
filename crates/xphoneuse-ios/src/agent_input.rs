//! AgentInput client: sends text (including non-ASCII, e.g. Chinese) to an
//! iPhone running the AgentInput keyboard (https://github.com/everettjf/Remoboard).
//!
//! AgentInput serves a WebSocket endpoint at ws://<phone-ip>:8787/ws from
//! inside the iOS keyboard extension - the server only exists while the
//! AgentInput keyboard is the active keyboard on screen, so connections are
//! established on demand and re-dialed after the keyboard closes and reopens.
//!
//! Protocol (v1, JSON text frames):
//!   connect: HTTP/1.1 upgrade, then optionally {"v":1,"t":"hello","pin":"..."}
//!            -> {"v":1,"t":"paired"} or {"v":1,"t":"deny","reason":"pin"}
//!   input:   {"v":1,"t":"input","text":"...","seq":N}
//!   delete:  {"v":1,"t":"delete","seq":N}
//!   move:    {"v":1,"t":"move","dir":"left|right|up|down","seq":N}
//! The phone echoes {"t":"context",...} after each accepted insertion.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::Engine;

pub const DEFAULT_PORT: u16 = 8787;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A configured (and lazily connected) AgentInput client.
pub struct AgentInputClient {
    host: String,
    port: u16,
    pin: Option<String>,
    stream: Option<TcpStream>,
    /// Buffered inbound bytes (leftover from the handshake read or frames
    /// not yet consumed).
    rbuf: Vec<u8>,
    seq: u64,
}

impl AgentInputClient {
    pub fn new(host: impl Into<String>, port: u16, pin: Option<String>) -> Self {
        Self {
            host: host.into(),
            port: if port == 0 { DEFAULT_PORT } else { port },
            pin: pin.filter(|p| !p.is_empty()),
            stream: None,
            rbuf: Vec::new(),
            seq: 0,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Retarget the port (used when a USB forward is rebuilt for a new
    /// session and the local listener moves to a different port).
    pub fn set_port(&mut self, port: u16) {
        self.port = if port == 0 { DEFAULT_PORT } else { port };
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Dial, upgrade to WebSocket, and pair. Also used to validate a saved
    /// configuration; [`send_text`] re-dials automatically on demand.
    pub fn connect(&mut self) -> Result<(), String> {
        eprintln!("[agent-input] connecting to {}:{} ...", self.host, self.port);
        self.stream = None;
        self.rbuf.clear();

        let mut stream = match self.dial() {
            Ok(s) => {
                eprintln!("[agent-input] websocket upgrade ok ({}:{})", self.host, self.port);
                s
            }
            Err(e) => {
                eprintln!("[agent-input] dial failed: {e}");
                return Err(e);
            }
        };

        // Pairing. PIN-less servers pair the client proactively right after
        // the upgrade (`paired` arrives within a few hundred ms); when a PIN
        // is required the server stays silent until we send `hello`.
        match read_msg(&mut stream, &mut self.rbuf, Some(PROBE_TIMEOUT)) {
            Ok(Some(Msg::Deny)) => return Err(self.deny_error()),
            Ok(Some(_)) => {} // proactive pairing - done
            Ok(None) => {
                // No proactive pairing: send `hello` (mirrors the reference
                // Python client, which always sends it).
                let pin = self.pin.clone().unwrap_or_default();
                let hello = format!(r#"{{"v":1,"t":"hello","pin":"{pin}"}}"#);
                eprintln!("[agent-input] no proactive pairing, sending hello");
                write_frame(&mut stream, OPCODE_TEXT, hello.as_bytes())
                    .map_err(|e| format!("send hello: {e}"))?;
                match read_msg(&mut stream, &mut self.rbuf, Some(REPLY_TIMEOUT)) {
                    Ok(Some(Msg::Deny)) => return Err(self.deny_error()),
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err("AgentInput did not respond to pairing".to_string())
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }

        eprintln!("[agent-input] paired with {}:{}", self.host, self.port);
        self.stream = Some(stream);
        Ok(())
    }

    fn deny_error(&self) -> String {
        if self.pin.is_some() {
            "AgentInput rejected the PIN".to_string()
        } else {
            "AgentInput requires a PIN (set one in Settings)".to_string()
        }
    }

    /// Insert text at the phone's cursor. Re-dials once if the connection
    /// died (e.g. the keyboard was closed and reopened).
    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.seq += 1;
        let payload =
            serde_json::json!({"v": 1, "t": "input", "text": text, "seq": self.seq}).to_string();
        self.send_payload(payload.as_bytes(), true)
    }

    /// Read frames until a `context` echo arrives (the phone's mirror of the
    /// focused text field) or the timeout hits. Returns the `before` text.
    pub fn wait_context(&mut self, timeout: Duration) -> Result<Option<String>, String> {
        let Some(mut stream) = self.stream.take() else {
            return Ok(None);
        };
        let result = loop {
            match read_msg(&mut stream, &mut self.rbuf, Some(timeout)) {
                Ok(Some(Msg::Context(before))) => break Ok(Some(before)),
                Ok(Some(_)) => continue, // paired/quickwords/info/... - keep waiting
                Ok(None) => break Ok(None),
                Err(e) => break Err(e),
            }
        };
        if result.is_ok() {
            self.stream = Some(stream);
        }
        result
    }

    /// Insert a newline (like the keyboard's return key).
    pub fn enter(&mut self) -> Result<(), String> {
        self.seq += 1;
        let payload = serde_json::json!({"v": 1, "t": "input", "text": "\n", "seq": self.seq})
            .to_string();
        self.send_payload(payload.as_bytes(), false)
    }

    /// Delete one character backwards.
    pub fn delete(&mut self) -> Result<(), String> {
        self.seq += 1;
        let payload = serde_json::json!({"v": 1, "t": "delete", "seq": self.seq}).to_string();
        self.send_payload(payload.as_bytes(), false)
    }

    /// Move the text cursor (`left`/`right`/`up`/`down`).
    pub fn move_cursor(&mut self, dir: &str) -> Result<(), String> {
        self.seq += 1;
        let payload =
            serde_json::json!({"v": 1, "t": "move", "dir": dir, "seq": self.seq}).to_string();
        self.send_payload(payload.as_bytes(), false)
    }

    fn send_payload(&mut self, payload: &[u8], check_reply: bool) -> Result<(), String> {
        if self.stream.is_none() {
            eprintln!("[agent-input] no socket yet, dialing before send");
            self.connect()?;
        }
        eprintln!("[agent-input] -> {}", String::from_utf8_lossy(payload));
        let stream = self.stream.as_mut().unwrap();
        if let Err(e) = write_frame(stream, OPCODE_TEXT, payload) {
            // Socket died (keyboard closed): retry once on a fresh one.
            eprintln!("[agent-input] send on live socket failed ({e}), re-dialing once");
            self.stream = None;
            self.connect()?;
            let stream = self.stream.as_mut().unwrap();
            write_frame(stream, OPCODE_TEXT, payload).map_err(|e| e.to_string())?;
            let _ = e;
        }

        if check_reply {
            // `deny` means we never paired; EOF means the keyboard extension
            // died. A missing `context` echo within the timeout is NOT an
            // error - some versions stay silent after a successful insert.
            let mut stream = self.stream.take().expect("stream was just set");
            let result = match read_msg(&mut stream, &mut self.rbuf, Some(REPLY_TIMEOUT)) {
                Ok(Some(Msg::Deny)) => Err("AgentInput rejected input (PIN required or changed)".into()),
                Ok(Some(_)) => Ok(()),
                Ok(None) => Ok(()), // no reply in time - assume delivered
                Err(e) => {
                    // A dead socket must not be reused.
                    eprintln!("[agent-input] reply read failed: {e}");
                    Err(format!("AgentInput reply failed: {e}"))
                }
            };
            if result.is_ok() {
                self.stream = Some(stream);
            }
            result
        } else {
            Ok(())
        }
    }

    fn dial(&mut self) -> Result<TcpStream, String> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}: {e}", self.host))?
            .next()
            .ok_or_else(|| format!("host {} resolved to no address", self.host))?;
        let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| format!("connect {addr}: {e} - is the AgentInput keyboard active on the phone?"))?;
        stream.set_nodelay(true).ok();

        // Random-ish 16-byte Sec-WebSocket-Key (trusted LAN peer; the value
        // only needs to be unlikely to repeat across our own handshakes).
        let mut seed = ws_seed();
        let mut key_bytes = [0u8; 16];
        for b in key_bytes.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (seed >> 33) as u8;
        }
        let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);

        let request = format!(
            "GET /ws HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            self.host, self.port
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("send handshake: {e}"))?;

        stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| format!("read handshake: {e}"))?;
            if n == 0 {
                return Err("connection closed during handshake".into());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = find_header_end(&buf) {
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                if !head.starts_with("HTTP/1.1 101") {
                    return Err(format!(
                        "unexpected handshake response: {}",
                        head.lines().next().unwrap_or("")
                    ));
                }
                // Keep any bytes after the header as start of the frame buffer.
                // (The proactive `paired` frame often arrives in the same read.)
                self.rbuf = buf[end + 4..].to_vec();
                return Ok(stream);
            }
            if buf.len() > 16 * 1024 {
                return Err("handshake response too large".into());
            }
        }
    }
}

fn ws_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos as u64 ^ ((std::process::id() as u64) << 32)
}

enum Msg {
    Accepted,
    Deny,
    /// `context` echo: the text before the phone's cursor.
    Context(String),
}

/// Read one inbound message, parsing any frames already buffered in `rbuf`
/// before touching the socket. `Ok(None)` = timeout (nothing pending);
/// `Err("connection closed...")` on EOF / close frame.
fn read_msg(
    stream: &mut TcpStream,
    rbuf: &mut Vec<u8>,
    timeout: Option<Duration>,
) -> Result<Option<Msg>, String> {
    loop {
        if let Some((frame, consumed)) = parse_frame(rbuf)? {
            rbuf.drain(..consumed);
            match frame.opcode {
                OPCODE_CLOSE => return Err("connection closed by phone".into()),
                OPCODE_PING => {
                    let payload = frame.payload.clone();
                    write_frame(stream, OPCODE_PONG, &payload).map_err(|e| e.to_string())?;
                }
                OPCODE_TEXT => {
                    let text = String::from_utf8_lossy(&frame.payload).into_owned();
                    eprintln!("[agent-input] <- {text}");
                    let msg = match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(v) => match v.get("t").and_then(|t| t.as_str()) {
                            Some("deny") => Msg::Deny,
                            Some("context") => Msg::Context(
                                v.get("before").and_then(|b| b.as_str()).unwrap_or("").to_string(),
                            ),
                            _ => Msg::Accepted,
                        },
                        Err(_) => Msg::Accepted,
                    };
                    return Ok(Some(msg));
                }
                _ => return Ok(Some(Msg::Accepted)),
            }
        }
        // Need more bytes from the socket.
        stream.set_read_timeout(timeout).ok();
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return Err("connection closed".into()),
            Ok(n) => rbuf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

const OPCODE_TEXT: u8 = 0x1;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;

struct RawFrame {
    opcode: u8,
    #[allow(dead_code)]
    payload: Vec<u8>,
}

/// Parse one complete frame from `buf`; `Ok(None)` = need more bytes.
/// Server frames are unmasked.
fn parse_frame(buf: &[u8]) -> Result<Option<(RawFrame, usize)>, String> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let mut len = (buf[1] & 0x7F) as u64;
    let mut off = 2usize;
    if len == 126 {
        if buf.len() < off + 2 {
            return Ok(None);
        }
        len = u16::from_be_bytes([buf[off], buf[off + 1]]) as u64;
        off += 2;
    } else if len == 127 {
        if buf.len() < off + 8 {
            return Ok(None);
        }
        let mut ext = [0u8; 8];
        ext.copy_from_slice(&buf[off..off + 8]);
        len = u64::from_be_bytes(ext);
        off += 8;
    }
    let mask = if masked {
        if buf.len() < off + 4 {
            return Ok(None);
        }
        let mut m = [0u8; 4];
        m.copy_from_slice(&buf[off..off + 4]);
        off += 4;
        Some(m)
    } else {
        None
    };
    if len > (1 << 20) {
        return Err("frame too large".into());
    }
    let end = off + len as usize;
    if buf.len() < end {
        return Ok(None);
    }
    let mut payload = buf[off..end].to_vec();
    if let Some(mask) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Some((RawFrame { opcode, payload }, end)))
}

/// Write one masked client frame (clients must mask per RFC 6455).
fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x80 | opcode];
    let mask_key: [u8; 4] = {
        let seed = ws_seed();
        [seed as u8, (seed >> 16) as u8, (seed >> 32) as u8, (seed >> 48) as u8]
    };
    match payload.len() {
        n if n < 126 => frame.push(0x80 | n as u8),
        n if n <= 0xFFFF => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask_key);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask_key[i % 4]));
    stream.write_all(&frame)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_header_end_works() {
        assert_eq!(find_header_end(b"HTTP/1.1 101 X\r\nA: b\r\n\r\nleft"), Some(20));
        assert_eq!(find_header_end(b"no header end"), None);
    }

    #[test]
    fn parse_frame_handles_text_and_partial() {
        // Unmasked text frame "hi": 0x81 0x02 'h' 'i'
        let buf = [0x81, 0x02, b'h', b'i'];
        let (frame, used) = parse_frame(&buf).unwrap().unwrap();
        assert_eq!(frame.opcode, OPCODE_TEXT);
        assert_eq!(used, 4);

        // Truncated frame returns None (needs more bytes).
        assert!(parse_frame(&buf[..3]).unwrap().is_none());

        // 16-bit extended length (126 -> next 2 bytes = 200) needs 4+200 bytes.
        let mut ext = vec![0x81, 126, 0x00, 0xC8];
        ext.extend(std::iter::repeat(b'x').take(200));
        let (_, used) = parse_frame(&ext).unwrap().unwrap();
        assert_eq!(used, 204);
    }

    #[test]
    fn parse_frame_unmasks_masked_payload() {
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        let payload = b"hello";
        let masked: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
        let mut buf = vec![0x81, 0x80 | payload.len() as u8];
        buf.extend_from_slice(&mask);
        buf.extend_from_slice(&masked);
        let (frame, used) = parse_frame(&buf).unwrap().unwrap();
        assert_eq!(frame.payload, payload.to_vec());
        assert_eq!(used, buf.len());
    }
}
