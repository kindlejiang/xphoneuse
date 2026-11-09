// H.264 RTP depacketization (RFC 6184) for the CoreDevice display stream.
//
// The device sends H.264 over plaintext RTP, dynamic payload type 123, when
// the negotiator offer only carries the AVC codec bank. The packetization is
// standard RFC 6184 (single NAL / STAP-A / FU-A), with two Apple quirks:
//
// - The first RTP payload is not a NAL at all but a proprietary stream
//   header (forbidden_zero_bit set) containing an "avc1" record with an
//   embedded avcC — the SPS/PPS live there and are NOT repeated in-band.
// - Like the HEVC stream, pictures are change-driven per frame and each RTP
//   timestamp change starts a new access unit.

const AVC_NAL_HEADER_LEN: usize = 1;

const NAL_TYPE_STAP_A: u8 = 24;
const NAL_TYPE_FU_A: u8 = 28;

// Parameter-set / keyframe NAL unit types.
const NAL_TYPE_IDR: u8 = 5;
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

/// H.264 access-unit delimiter NAL (type 9, primary_pic_type any).
const AUD_NAL: [u8; 2] = [0x09, 0xf0];
const ANNEXB_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];
const MAX_REORDER_BUFFER: usize = 128;

#[inline]
fn nal_type(nal_header_byte0: u8) -> u8 {
    nal_header_byte0 & 0x1f
}

/// Reassembles H.264 NAL units from RTP payloads and emits an Annex-B stream.
#[derive(Debug, Default)]
pub struct AvcDepacketizer {
    reorder: std::collections::BTreeMap<u16, (u32, Vec<u8>)>,
    next_seq: Option<u16>,
    last_ts: Option<u32>,

    fu_buffer: Vec<u8>,
    fu_active: bool,

    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    params_emitted_since_idr: bool,

    out: Vec<u8>,
}

impl AvcDepacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, seq: u16, timestamp: u32, payload: &[u8]) {
        if self.next_seq.is_none() {
            self.next_seq = Some(seq);
        }

        // Ignore packets we've already moved past (stale duplicates / late
        // arrivals behind the cursor).
        if let Some(next) = self.next_seq
            && seq_less_than(seq, next)
        {
            return;
        }

        self.reorder.insert(seq, (timestamp, payload.to_vec()));
        self.drain_in_order();

        // If we're stalled waiting on a lost packet, skip the gap.
        if self.reorder.len() > MAX_REORDER_BUFFER
            && let Some((&lowest, _)) = self.reorder.iter().next()
        {
            // A lost packet breaks any in-flight fragment.
            self.reset_fu();
            self.next_seq = Some(lowest);
            self.drain_in_order();
        }
    }

    /// Jump the sequence cursor to `seq` and drop in-flight reassembly state.
    /// Called by consumers that discarded a run of packets (e.g. after an RTP
    /// gap) so the reorder buffer doesn't stall waiting for skipped sequence
    /// numbers. Buffered packets before `seq` are dropped.
    pub fn resync(&mut self, seq: u16) {
        self.reorder.retain(|&s, _| !seq_less_than(s, seq));
        self.next_seq = Some(seq);
        self.reset_fu();
    }

    /// Process buffered packets while they are contiguous from the cursor.
    fn drain_in_order(&mut self) {
        while let Some(next) = self.next_seq {
            let Some((ts, payload)) = self.reorder.remove(&next) else {
                break;
            };
            // A timestamp change marks a new access unit (frame). Close the
            // previous picture by emitting an AUD before the new one's NALs,
            // so the consumer can split complete pictures at the boundary.
            if let Some(prev) = self.last_ts
                && prev != ts
            {
                self.write_annexb(&AUD_NAL);
            }
            self.last_ts = Some(ts);
            self.process_payload(&payload);
            self.next_seq = Some(next.wrapping_add(1));
        }
    }

    /// Handle a single RTP payload according to its NAL structure.
    fn process_payload(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        if payload[0] & 0x80 != 0 {
            // forbidden_zero_bit set — not a NAL. Apple prefixes the stream
            // with a proprietary header record carrying the avcC (SPS/PPS).
            self.process_stream_header(payload);
            return;
        }
        match nal_type(payload[0]) {
            NAL_TYPE_STAP_A => self.process_stap_a(payload),
            NAL_TYPE_FU_A => self.process_fu_a(payload),
            1..=23 => {
                // A new single NAL implies any in-flight fragment was lost.
                self.reset_fu();
                self.emit_nal(payload);
            }
            _ => {} // 0, 25..=31: reserved — drop
        }
    }

    /// Apple's first-payload stream header: a record containing "avc1" and an
    /// embedded avcC with the SPS/PPS (never sent in-band on this stream).
    /// We only care about the avcC; everything else is ignored.
    fn process_stream_header(&mut self, payload: &[u8]) {
        let Some(k) = find_subslice(payload, b"avcC") else {
            return;
        };
        // avcC layout: [avcC](4) ver(1) profile(1) compat(1) level(1)
        //              ff(1) numSPS(1) [u16 len SPS]... numPPS(1) [u16 len PPS]...
        let a = &payload[k + 4..];
        if a.len() < 8 {
            return;
        }
        let num_sps = a[5] & 0x1f;
        let mut off = 6;
        for _ in 0..num_sps {
            let Some(len) = read_u16(a, off) else { return };
            off += 2;
            let Some(nal) = a.get(off..off + len as usize) else { return };
            if nal_type(nal[0]) == NAL_TYPE_SPS {
                self.sps = Some(nal.to_vec());
            }
            off += len as usize;
        }
        let Some(num_pps) = a.get(off).copied() else { return };
        off += 1;
        for _ in 0..num_pps {
            let Some(len) = read_u16(a, off) else { return };
            off += 2;
            let Some(nal) = a.get(off..off + len as usize) else { return };
            if nal_type(nal[0]) == NAL_TYPE_PPS {
                self.pps = Some(nal.to_vec());
            }
            off += len as usize;
        }
        // The next IDR needs them injected before it.
        self.params_emitted_since_idr = false;
    }

    /// STAP-A (type 24): `[NAL hdr][ (16-bit size)(NAL unit) ]+`.
    fn process_stap_a(&mut self, payload: &[u8]) {
        self.reset_fu();
        let mut off = AVC_NAL_HEADER_LEN;
        while off + 2 <= payload.len() {
            let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
            off += 2;
            let Some(nal) = payload.get(off..off + size) else {
                break; // truncated / malformed
            };
            self.emit_nal(nal);
            off += size;
        }
    }

    /// FU-A (type 28): a 1-byte FU-A NAL header, then a 1-byte FU header
    /// `[S|E|FuType(5)]`, then a fragment of the original NAL's payload. The
    /// original NAL header is the FU-A header with the type replaced by the
    /// FU type.
    fn process_fu_a(&mut self, payload: &[u8]) {
        if payload.len() < AVC_NAL_HEADER_LEN + 1 {
            return;
        }
        let fu_header = payload[1];
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let fu_type = fu_header & 0x1f;
        let fragment = &payload[2..];

        if start {
            let b0 = (payload[0] & 0xe0) | fu_type;
            self.fu_buffer.clear();
            self.fu_buffer.push(b0);
            self.fu_buffer.extend_from_slice(fragment);
            self.fu_active = true;
        } else if self.fu_active {
            self.fu_buffer.extend_from_slice(fragment);
        } else {
            // Middle/end fragment with no start — the start packet was lost.
            return;
        }

        if end && self.fu_active {
            let nal = std::mem::take(&mut self.fu_buffer);
            self.fu_active = false;
            self.emit_nal(&nal);
        }
    }

    /// Append one complete NAL unit to the output, caching parameter sets and
    /// re-injecting them before key frames so a decoder can join mid-stream
    /// (the AVC stream never repeats SPS/PPS in-band — see
    /// `process_stream_header`).
    fn emit_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        let t = nal_type(nal[0]);

        match t {
            NAL_TYPE_SPS => {
                self.sps = Some(nal.to_vec());
                self.params_emitted_since_idr = true;
            }
            NAL_TYPE_PPS => {
                self.pps = Some(nal.to_vec());
                self.params_emitted_since_idr = true;
            }
            _ => {}
        }

        // Before an IDR (key) frame, make sure parameter sets precede it —
        // but only ONCE per key frame (see HevcDepacketizer::emit_nal for the
        // multi-slice rationale).
        if t == NAL_TYPE_IDR && !self.params_emitted_since_idr {
            let sets: Vec<Vec<u8>> = [self.sps.clone(), self.pps.clone()]
                .into_iter()
                .flatten()
                .collect();
            for set in sets {
                self.write_annexb(&set);
            }
            self.params_emitted_since_idr = true;
        }

        // Re-arm injection when a non-IDR VCL NAL (a P/B slice) goes by.
        if t == 1 {
            self.params_emitted_since_idr = false;
        }

        self.write_annexb(nal);
    }

    fn write_annexb(&mut self, nal: &[u8]) {
        self.out.extend_from_slice(&ANNEXB_START_CODE);
        self.out.extend_from_slice(nal);
    }

    fn reset_fu(&mut self) {
        self.fu_buffer.clear();
        self.fu_active = false;
    }

    /// Drain and return all Annex-B output accumulated so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// True once both parameter sets (SPS/PPS) have been observed.
    pub fn has_parameter_sets(&self) -> bool {
        self.sps.is_some() && self.pps.is_some()
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn read_u16(buf: &[u8], off: usize) -> Option<u16> {
    let b = buf.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

/// RTP sequence-number comparison with 16-bit wraparound (RFC 1982). Returns
/// true if `a` is "before" `b` in sequence order.
#[inline]
fn seq_less_than(a: u16, b: u16) -> bool {
    let diff = b.wrapping_sub(a);
    diff != 0 && diff < 0x8000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real device's stream header (captured off a live AVC tunnel
    /// stream): forbidden-bit first byte, an "avc1" record, and an embedded
    /// avcC carrying a 17-byte SPS and 4-byte PPS.
    const STREAM_HEADER: &[u8] = &[
        0x92, 0xe6, 0xc0, 0xa3, 0x00, 0x00, 0x00, 0x86, 0x61, 0x76, 0x63, 0x31, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x04, 0xa0, 0x0a, 0x10, 0x00, 0x48,
        0x00, 0x00, 0x00, 0x48, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x05, 0x48, 0x2e, 0x32,
        0x36, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x18, 0xff, 0xff, 0x00, 0x00, 0x00, 0x2c, 0x61, 0x76, 0x63, 0x43, 0x01,
        0x64, 0x00, 0x33, 0xff, 0xe1, 0x00, 0x11, 0x27, 0x64, 0x00, 0x33, 0x4b, 0x04, 0xc5,
        0x14, 0x04, 0xa0, 0x14, 0x3a, 0x6e, 0x04, 0x04, 0x04, 0x04, 0x01, 0x00, 0x04, 0x28,
        0x4a, 0xe3, 0xcb, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    const SPS: &[u8] = &[
        0x27, 0x64, 0x00, 0x33, 0x4b, 0x04, 0xc5, 0x14, 0x04, 0xa0, 0x14, 0x3a, 0x6e, 0x04,
        0x04, 0x04, 0x04,
    ];
    const PPS: &[u8] = &[0x28, 0x4a, 0xe3, 0xcb];

    #[test]
    fn stream_header_supplies_parameter_sets_for_idr() {
        let mut d = AvcDepacketizer::new();
        assert!(!d.has_parameter_sets());
        // First payload: Apple's proprietary header (forbidden bit set).
        d.push(1, 0, STREAM_HEADER);
        assert!(d.has_parameter_sets());
        assert!(d.take_output().is_empty());

        // An IDR fragmented as FU-A (observed on-device: header 0x3c,
        // FU header 0x85 = start/type 5, then 0x05 continuation, 0x45 end).
        let idr_payload: Vec<u8> = [0xaa, 0xbb, 0xcc, 0xdd].to_vec();
        let frag1 = [0x3c, 0x85].iter().chain(idr_payload.iter()).cloned().collect::<Vec<_>>();
        d.push(2, 100, &frag1);
        let frag2 = [0x3c, 0x05, 0xee, 0xff].to_vec();
        d.push(3, 100, &frag2);
        let frag3 = [0x3c, 0x45, 0x11].to_vec();
        d.push(4, 100, &frag3);
        let out = d.take_output();

        // Annex-B: the timestamp change (0 -> 100) opens the picture with an
        // AUD, then the depacketizer injects the stream-header SPS/PPS before
        // the reconstructed IDR (0x25 header — FU-A header 0x3c's nri bits
        // with type 5 — plus fragments in order).
        let mut expect = Vec::new();
        expect.extend_from_slice(&ANNEXB_START_CODE);
        expect.extend_from_slice(&AUD_NAL);
        expect.extend_from_slice(&ANNEXB_START_CODE);
        expect.extend_from_slice(SPS);
        expect.extend_from_slice(&ANNEXB_START_CODE);
        expect.extend_from_slice(PPS);
        expect.extend_from_slice(&ANNEXB_START_CODE);
        expect.extend_from_slice(&[0x25, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11]);
        assert_eq!(out, expect);
    }

    #[test]
    fn timestamp_change_emits_aud_boundary() {
        let mut d = AvcDepacketizer::new();
        d.push(1, 0, STREAM_HEADER);
        // P-slice single NAL (type 1) in picture A, then picture B.
        d.push(2, 100, &[0x41, 0x00]);
        d.push(3, 200, &[0x41, 0x01]);
        let out = d.take_output();
        // Each timestamp change writes an AUD that OPENS the new picture, so
        // the stream is AUD_A, P_A, AUD_B, P_B — a consumer splits pictures
        // at the non-leading AUD.
        let pos_aud_a = find_subslice(&out, &AUD_NAL).expect("AUD opening picture A");
        let pos_a = find_subslice(&out, &[0x41, 0x00]).expect("picture A NAL");
        let pos_aud_b = find_subslice(&out[pos_a..], &AUD_NAL)
            .map(|p| p + pos_a)
            .expect("AUD opening picture B");
        let pos_b = find_subslice(&out, &[0x41, 0x01]).expect("picture B NAL");
        assert!(pos_aud_a < pos_a && pos_a < pos_aud_b && pos_aud_b < pos_b);
    }

    #[test]
    fn resync_skips_dropped_sequence_range() {
        let mut d = AvcDepacketizer::new();
        d.push(1, 0, &[0x41, 0x00]);
        d.resync(5);
        // Packets before the cursor are ignored; seq 5 drains immediately.
        d.push(2, 0, &[0x41, 0x01]);
        d.push(5, 10, &[0x41, 0x02]);
        let out = d.take_output();
        assert!(find_subslice(&out, &[0x41, 0x01]).is_none());
        assert!(find_subslice(&out, &[0x41, 0x02]).is_some());
    }
}
