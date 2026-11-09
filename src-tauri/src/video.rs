use anyhow::Result;
use base64::Engine;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::Notify;

const FLAG_CONFIG: u64 = 1 << 63;
const FLAG_KEY_FRAME: u64 = 1 << 62;

#[derive(Clone, Copy, PartialEq)]
pub enum VideoCodec {
    H264,
    H265,
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "event", content = "data")]
pub enum FrameEvent {
    #[serde(rename = "config")]
    Config {
        codec: String,
        description: String,
        /// Visible (croppable) frame size — the iOS tunnel stream pads the
        /// coded picture to whole macroblocks with black borders; when set,
        /// the renderer crops the decoder output to this size. None for the
        /// paths whose coded size already equals the visible size.
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    #[serde(rename = "packet")]
    Packet { key: bool, data: String, timestamp: u64 },
    #[serde(rename = "disconnected")]
    Disconnected { reason: String },
    /// iOS only: the mirroring client connected from this IP, and AgentInput
    /// (non-ASCII text input) was auto-configured for it.
    #[serde(rename = "agent_input")]
    AgentInput { host: String },
}

pub async fn stream_video(
    mut video_socket: TcpStream,
    channel: Channel<FrameEvent>,
    shutdown: Arc<Notify>,
    codec: VideoCodec,
) {
    let result = tokio::select! {
        r = forward_loop(&mut video_socket, &channel, codec) => r,
        _ = shutdown.notified() => Ok(()),
    };

    if let Err(e) = result {
        let _ = channel.send(FrameEvent::Disconnected {
            reason: e.to_string(),
        });
    }
}

async fn forward_loop(
    socket: &mut TcpStream,
    channel: &Channel<FrameEvent>,
    codec: VideoCodec,
) -> Result<()> {
    loop {
        let mut header = [0u8; 12];
        socket.read_exact(&mut header).await?;

        let pts_flags = u64::from_be_bytes(header[0..8].try_into()?);
        let is_config = pts_flags & FLAG_CONFIG != 0;
        let is_key = pts_flags & FLAG_KEY_FRAME != 0;
        let pts = pts_flags & !(FLAG_CONFIG | FLAG_KEY_FRAME);

        let size = u32::from_be_bytes(header[8..12].try_into()?) as usize;
        if size == 0 {
            continue;
        }

        let mut data = vec![0u8; size];
        socket.read_exact(&mut data).await?;

        if is_config {
            let nals = split_nals(&data);
            let (codec_str, desc) = match codec {
                VideoCodec::H264 => parse_h264_config(&nals),
                VideoCodec::H265 => parse_h265_config(&nals),
            };
            let _ = channel.send(FrameEvent::Config {
                codec: codec_str,
                description: base64::engine::general_purpose::STANDARD.encode(&desc),
                width: None,
                height: None,
            });
        } else {
            let avcc = nals_to_avcc(&data);
            let _ = channel.send(FrameEvent::Packet {
                key: is_key,
                data: base64::engine::general_purpose::STANDARD.encode(&avcc),
                timestamp: pts,
            });
        }
    }
}

pub(crate) fn parse_h264_config(nals: &[&[u8]]) -> (String, Vec<u8>) {
    let mut sps_list: Vec<&[u8]> = Vec::new();
    let mut pps_list: Vec<&[u8]> = Vec::new();
    let mut codec = String::from("avc1.42001e");

    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        if nal_type == 7 && nal.len() >= 4 {
            codec = format!("avc1.{:02x}{:02x}{:02x}", nal[1], nal[2], nal[3]);
            sps_list.push(nal);
        } else if nal_type == 8 {
            pps_list.push(nal);
        }
    }

    (codec, build_avcc(&sps_list, &pps_list))
}

pub(crate) fn parse_h265_config(nals: &[&[u8]]) -> (String, Vec<u8>) {
    let mut vps_list: Vec<&[u8]> = Vec::new();
    let mut sps_list: Vec<&[u8]> = Vec::new();
    let mut pps_list: Vec<&[u8]> = Vec::new();
    let mut codec = String::from("hev1.1.6.L93.B0");

    for nal in nals {
        if nal.len() < 2 {
            continue;
        }
        let nal_type = (nal[0] >> 1) & 0x3F;
        match nal_type {
            32 => vps_list.push(nal),
            33 => {
                if let Some(s) = build_hevc_codec_string(nal) {
                    codec = s;
                }
                sps_list.push(nal);
            }
            34 => pps_list.push(nal),
            _ => {}
        }
    }

    (codec, build_hvcc(&vps_list, &sps_list, &pps_list))
}

/// Strip the 2-byte NAL header and emulation-prevention bytes (00 00 03 →
/// 00 00) to expose the raw RBSP. Required before parsing SPS fields — the
/// real device's SPS carries prevention sequences right inside the
/// profile_tier_level (e.g. compat "60 00 00 03 00" instead of "60 00 00 00"),
/// which corrupts every field parsed off the raw bytes.
fn hevc_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut i = 2;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.extend_from_slice(&nal[i..i + 2]);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

fn build_hevc_codec_string(sps: &[u8]) -> Option<String> {
    let rbsp = hevc_rbsp(sps);
    if rbsp.len() < 13 {
        return None;
    }
    let profile_byte = rbsp[1];
    let profile_idc = profile_byte & 0x1F;
    let tier_flag = (profile_byte >> 5) & 0x01;
    let compat = u32::from_be_bytes([rbsp[2], rbsp[3], rbsp[4], rbsp[5]]);
    // The codec string expresses the compatibility flags in REVERSE bit order
    // (ISO/IEC 14496-15 §A.3.3): Apple's Main-profile 0x60000000 is "6".
    let mut rev = 0u32;
    for i in 0..32 {
        rev |= ((compat >> i) & 1) << (31 - i);
    }
    let level_idc = rbsp[12];
    let tier = if tier_flag == 1 { "H" } else { "L" };
    let constraints = &rbsp[6..12];
    let mut hex_parts: Vec<String> = constraints.iter().map(|b| format!("{:02X}", b)).collect();
    while hex_parts.last().map_or(false, |s| s == "00") && hex_parts.len() > 1 {
        hex_parts.pop();
    }
    Some(format!("hev1.{}.{:X}.{}{}.{}", profile_idc, rev, tier, level_idc, hex_parts.join("")))
}

fn build_hvcc(vps_list: &[&[u8]], sps_list: &[&[u8]], pps_list: &[&[u8]]) -> Vec<u8> {
    if sps_list.is_empty() {
        return Vec::new();
    }
    let sps = sps_list[0];
    if sps.len() < 15 {
        return Vec::new();
    }

    // Parse the 12-byte profile_tier_level from the RBSP (emulation
    // prevention stripped) — the hvcC header wants the logical values,
    // while the NAL arrays below keep the original (prevention-intact) NALs.
    let rbsp = hevc_rbsp(sps);
    if rbsp.len() < 13 {
        return Vec::new();
    }
    let profile_byte = rbsp[1];
    let compat = &rbsp[2..6];
    let constraints = &rbsp[6..12];
    let level_idc = rbsp[12];

    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push(profile_byte); // profile_space + tier_flag + profile_idc
    out.extend_from_slice(compat); // general_profile_compatibility_flags
    out.extend_from_slice(constraints); // general_constraint_indicator_flags
    out.push(level_idc); // general_level_idc
    out.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc (reserved + 0)
    out.push(0xFC); // parallelismType (reserved + 0)
    out.push(0xFC | 1); // chromaFormatIdc (reserved + 1 = 4:2:0)
    out.push(0xF8); // bitDepthLumaMinus8 (reserved + 0 = 8-bit)
    out.push(0xF8); // bitDepthChromaMinus8 (reserved + 0 = 8-bit)
    out.extend_from_slice(&[0x00, 0x00]); // avgFrameRate = 0
    out.push(0x03); // constantFrameRate(0) + numTemporalLayers(0) + temporalIdNested(0) + lengthSizeMinusOne(3)

    let arrays: &[(&[&[u8]], u8)] = &[
        (vps_list, 32),
        (sps_list, 33),
        (pps_list, 34),
    ];
    let num_arrays = arrays.iter().filter(|(list, _)| !list.is_empty()).count();
    out.push(num_arrays as u8);

    for (list, nal_type) in arrays {
        if list.is_empty() {
            continue;
        }
        out.push(0x80 | nal_type); // array_completeness=1 + reserved=0 + NAL_unit_type
        out.push((list.len() >> 8) as u8);
        out.push(list.len() as u8);
        for nalu in *list {
            out.push((nalu.len() >> 8) as u8);
            out.push(nalu.len() as u8);
            out.extend_from_slice(nalu);
        }
    }

    out
}

pub(crate) fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let len = data.len();
    let mut i = 0;

    let mut sc_positions: Vec<(usize, usize)> = Vec::new();
    while i + 2 < len {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if i > 0 && data[i - 1] == 0 {
                sc_positions.push((i - 1, 4));
            } else {
                sc_positions.push((i, 3));
            }
            i += 3;
        } else {
            i += 1;
        }
    }

    for (idx, &(pos, sc_len)) in sc_positions.iter().enumerate() {
        let nal_start = pos + sc_len;
        let nal_end = if idx + 1 < sc_positions.len() {
            sc_positions[idx + 1].0
        } else {
            len
        };
        if nal_start < nal_end {
            nals.push(&data[nal_start..nal_end]);
        }
    }

    nals
}

pub(crate) fn build_avcc(sps_list: &[&[u8]], pps_list: &[&[u8]]) -> Vec<u8> {
    if sps_list.is_empty() {
        return Vec::new();
    }
    let sps = sps_list[0];
    let mut out = vec![
        1,
        sps[1],
        sps[2],
        sps[3],
        0xFF,
        0xE0 | sps_list.len() as u8,
    ];
    for s in sps_list {
        out.push((s.len() >> 8) as u8);
        out.push(s.len() as u8);
        out.extend_from_slice(s);
    }
    out.push(pps_list.len() as u8);
    for p in pps_list {
        out.push((p.len() >> 8) as u8);
        out.push(p.len() as u8);
        out.extend_from_slice(p);
    }
    out
}

pub(crate) fn nals_to_avcc(data: &[u8]) -> Vec<u8> {
    let nals = split_nals(data);
    let mut out = Vec::with_capacity(data.len());
    for nal in nals {
        let len = nal.len() as u32;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_codec_string_reverses_compat_flags() {
        // Minimal SPS: 2-byte NAL header, then profile_tier_level —
        // profile idc=1 (Main), compat flags 0x60000000, constraint 0xB0..,
        // level 120. The codec string expresses compat in reverse bit order
        // (ISO/IEC 14496-15 §A.3.3), so 0x60000000 must print as "6" — the
        // form Chrome's WebCodecs accepts (matched against pymobiledevice3's
        // on-device-verified hevc_codec_string_from_sps).
        let mut sps = vec![0x42, 0x01, 0x00];
        sps.push(0x01); // profile_space(0) + tier(0) + profile_idc(1)
        sps.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]); // compat flags
        sps.extend_from_slice(&[0xB0, 0x00, 0x00, 0x00, 0x00, 0x00]); // constraints
        sps.push(0x78); // level_idc = 120
        assert_eq!(
            build_hevc_codec_string(&sps).unwrap(),
            "hev1.1.6.L120.B0"
        );
    }

    #[test]
    fn hevc_codec_string_strips_emulation_prevention() {
        // The real device's SPS (captured off a live tunnel stream): the
        // compat field and constraints straddle 00 00 03 prevention
        // sequences. Parsed raw, compat reads 0x60000003 and level 0 —
        // parsed off the RBSP it is Main profile, level 150.
        let sps: Vec<u8> = [
            0x42, 0x01, 0x11, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xb0, 0x00,
            0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x96, 0x48, 0x00, 0x94, 0x20,
        ]
        .to_vec();
        assert_eq!(
            build_hevc_codec_string(&sps).unwrap(),
            "hev1.1.6.L150.B0"
        );
        // The hvcC header must carry the same logical PTL values.
        let hvcc = build_hvcc(&[], &[&sps], &[]);
        assert_eq!(hvcc[1], 0x01); // profile: space 0, tier 0, idc 1
        assert_eq!(&hvcc[2..6], &[0x60, 0x00, 0x00, 0x00]); // compat, EPB-free
        assert_eq!(&hvcc[6..12], &[0xb0, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(hvcc[12], 0x96); // level_idc = 150
    }
}
