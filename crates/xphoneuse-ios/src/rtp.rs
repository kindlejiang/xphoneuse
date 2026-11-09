//! Minimal RTP H.264 depacketizer for the uxplay `-vrtp` stream.
//!
//! uxplay is spawned with `-vrtp "config-interval=1 ! udpsink host=127.0.0.1
//! port=N"`, which repackets the AirPlay H.264 elementary stream via
//! `rtph264pay` onto a local UDP socket. This module turns those RTP packets
//! back into Annex-B access units.

/// A reassembled H.264 access unit in Annex-B byte-stream format.
pub struct AccessUnit {
    pub data: Vec<u8>,
    /// RTP timestamp converted to microseconds (relative to the first unit).
    pub timestamp_us: u64,
    /// True when the access unit contains an IDR (NAL type 5).
    pub key: bool,
}

pub struct RtpDepacketizer {
    buffer: Vec<u8>,
    timestamp: u32,
    first_timestamp: Option<u32>,
    wrap_offset: u64,
    payload_type: u8,
}

impl RtpDepacketizer {
    pub fn new() -> Self {
        Self::with_payload_type(96)
    }

    /// Like `new`, but accepting a different dynamic payload type (e.g. 123
    /// for the CoreDevice tunnel's H.264 screen stream).
    pub fn with_payload_type(payload_type: u8) -> Self {
        Self {
            buffer: Vec::new(),
            timestamp: 0,
            first_timestamp: None,
            wrap_offset: 0,
            payload_type,
        }
    }

    /// Feed one RTP packet (header included). Returns a completed access unit
    /// when the packet's marker bit closes one.
    pub fn push(&mut self, packet: &[u8]) -> Option<AccessUnit> {
        if packet.len() < 12 {
            return None;
        }

        let marker = packet[1] & 0x80 != 0;
        let payload_type = packet[1] & 0x7f;
        if payload_type != self.payload_type {
            // Not the H.264 dynamic payload type we were built for
            return None;
        }

        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let rtp_ts = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let csrc_count = (packet[0] & 0x0f) as usize;
        let mut header_len = 12 + csrc_count * 4;

        // Extension header (X bit): 16-bit profile, 16-bit length in 32-bit words
        if packet[0] & 0x10 != 0 && packet.len() > header_len + 4 {
            let ext_len = u16::from_be_bytes([packet[header_len + 2], packet[header_len + 3]]) as usize;
            header_len += 4 + ext_len * 4;
        }

        if packet.len() <= header_len {
            return None;
        }
        let payload = &packet[header_len..];
        let _ = sequence;

        let timestamp_us = self.abs_timestamp(rtp_ts);
        self.timestamp = rtp_ts;

        if payload.is_empty() {
            return None;
        }

        let nal_type = payload[0] & 0x1f;
        match nal_type {
            1..=23 => {
                // Single NAL unit packet
                push_nal(&mut self.buffer, payload);
            }
            24 => {
                // STAP-A: several NALs in one packet
                let mut off = 1;
                while off + 2 <= payload.len() {
                    let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
                    off += 2;
                    if off + size > payload.len() {
                        break;
                    }
                    push_nal(&mut self.buffer, &payload[off..off + size]);
                    off += size;
                }
            }
            28 => {
                // FU-A fragment
                if payload.len() < 2 {
                    return None;
                }
                let fu_header = payload[1];
                let start = fu_header & 0x80 != 0;
                let end = fu_header & 0x40 != 0;
                let reconstructed_type = (payload[0] & 0xe0) | (fu_header & 0x1f);
                let fragment = &payload[2..];

                if start {
                    // Start a new NAL (drop any half-received one)
                    let mut nal = Vec::with_capacity(fragment.len() + 5);
                    nal.push(reconstructed_type);
                    nal.extend_from_slice(fragment);
                    finish_nal(&mut self.buffer, nal);
                } else if !self.buffer.is_empty() {
                    // Continuation: append raw fragment bytes
                    self.buffer.extend_from_slice(fragment);
                }
                let _ = end;
            }
            _ => {
                // Other aggregation/packetization modes are not produced by
                // rtph264pay by default; ignore.
            }
        }

        if marker {
            let data = std::mem::take(&mut self.buffer);
            if data.is_empty() {
                return None;
            }
            let key = contains_idr(&data);
            Some(AccessUnit {
                data,
                timestamp_us,
                key,
            })
        } else {
            None
        }
    }

    /// 90 kHz RTP clock to microseconds, handling u32 wraparound.
    fn abs_timestamp(&mut self, rtp_ts: u32) -> u64 {
        if self.first_timestamp.is_none() {
            self.first_timestamp = Some(rtp_ts);
        }
        if rtp_ts < self.timestamp && self.timestamp - rtp_ts > u32::MAX / 2 {
            // wrapped around
            self.wrap_offset += u32::MAX as u64 + 1;
        }
        let first = self.first_timestamp.unwrap();
        let ticks = self.wrap_offset + (rtp_ts as u64).wrapping_sub(first as u64);
        ticks * 1_000_000 / 90_000
    }
}

impl Default for RtpDepacketizer {
    fn default() -> Self {
        Self::new()
    }
}

fn push_nal(buffer: &mut Vec<u8>, nal: &[u8]) {
    buffer.extend_from_slice(&[0, 0, 0, 1]);
    buffer.extend_from_slice(nal);
}

fn finish_nal(buffer: &mut Vec<u8>, mut nal: Vec<u8>) {
    buffer.extend_from_slice(&[0, 0, 0, 1]);
    buffer.append(&mut nal);
}

/// Detect an IDR slice NAL (type 5) in an Annex-B byte stream.
fn contains_idr(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let nal_start = i + 3;
            if nal_start < data.len() && data[nal_start] & 0x1f == 5 {
                return true;
            }
            i = nal_start + 1;
        } else if i + 5 <= data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            let nal_start = i + 4;
            if nal_start < data.len() && data[nal_start] & 0x1f == 5 {
                return true;
            }
            i = nal_start + 1;
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp_packet(payload: &[u8], marker: bool, ts: u32) -> Vec<u8> {
        let mut p = vec![0x80, if marker { 0x80 | 96 } else { 96 }];
        p.extend_from_slice(&[0, 1]);
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 0]); // SSRC
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn single_nal_frame() {
        let mut d = RtpDepacketizer::new();
        let sps = vec![0x67u8, 0x42, 0x00, 0x1f];
        let au = d.push(&rtp_packet(&sps, true, 0)).unwrap();
        assert!(au.data.starts_with(&[0, 0, 0, 1, 0x67]));
        assert!(!au.key);
    }

    #[test]
    fn fu_a_reassembly() {
        let mut d = RtpDepacketizer::new();
        // IDR NAL (type 5, NRI 3) split into two FU-A fragments
        let nal: Vec<u8> = vec![0x65, 0x88, 0x84, 0x21, 0xa0];
        let fu_indicator = 0x7c; // F=0, NRI=3, type=28 (FU-A)
        // start fragment: FU header 0x80|5 = S=1, type 5
        let f1 = vec![fu_indicator, 0x80 | 5, 0x88, 0x84];
        let f2 = vec![fu_indicator, 0x40 | 5, 0x21, 0xa0];
        assert!(d.push(&rtp_packet(&f1, false, 90)).is_none());
        let au = d.push(&rtp_packet(&f2, true, 90)).unwrap();
        assert!(au.key);
        assert_eq!(&au.data[..5], &[0, 0, 0, 1, 0x65]);
        assert_eq!(&au.data[5..], &nal[1..]);
    }

    #[test]
    fn timestamp_is_relative_to_first_packet() {
        let mut d = RtpDepacketizer::new();
        let au = d.push(&rtp_packet(&[0x67, 0x01], true, 90000)).unwrap();
        assert_eq!(au.timestamp_us, 0);
        // +90 RTP ticks per frame, 90 kHz clock -> 1 ms
        let au = d.push(&rtp_packet(&[0x41, 0x01], true, 90090)).unwrap();
        assert_eq!(au.timestamp_us, 1_000);
    }
}
