use xphoneuse_ios::rtp::RtpDepacketizer;
use xphoneuse_ios::uxplay_ffi::UxplayEvent;
use anyhow::Result;
use base64::Engine;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::net::UdpSocket;
use tokio::process::Child;
use tokio::sync::Notify;

use crate::video::{FrameEvent, nals_to_avcc, parse_h264_config, parse_h265_config, split_nals};

const RTP_RECV_BUFFER: usize = 64 * 1024;

/// Forward the uxplay FFI (uxplay.dll) NAL stream to the frontend as the
/// same `FrameEvent`s the Android (scrcpy) and exe fallback paths produce.
/// The uxplay callbacks feed a std mpsc channel; a bridge thread moves the
/// events onto a tokio channel so this loop can stay async.
pub async fn stream_ios_video_ffi(
    receiver: std::sync::mpsc::Receiver<UxplayEvent>,
    channel: Channel<FrameEvent>,
    shutdown: Arc<Notify>,
    agent_input: Arc<std::sync::Mutex<Option<xphoneuse_ios::agent_input::AgentInputClient>>>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UxplayEvent>();
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });

    let mut config_sent = false;
    let mut seen_keyframe = false;
    let mut base_ts_ns: Option<u64> = None;

    loop {
        let event = tokio::select! {
            r = rx.recv() => match r {
                Some(e) => e,
                None => break, // session dropped
            },
            _ = shutdown.notified() => break,
        };

        match event {
            UxplayEvent::VideoData {
                data,
                timestamp_ns,
                ..
            } => {
                let nals = split_nals(&data);

                // Emit the decoder config as soon as SPS+PPS are seen
                if !config_sent {
                    let has_sps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 7);
                    let has_pps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 8);
                    if has_sps && has_pps {
                        let (codec, desc) = parse_h264_config(&nals);
                        if !desc.is_empty() {
                            let _ = channel.send(FrameEvent::Config {
                                codec,
                                description: base64::engine::general_purpose::STANDARD
                                    .encode(&desc),
                                width: None,
                                height: None,
                            });
                            config_sent = true;
                        }
                    }
                }

                let is_idr = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 5);

                // WebCodecs requires the first chunk to be a keyframe
                if !seen_keyframe {
                    if !is_idr {
                        continue;
                    }
                    seen_keyframe = true;
                }

                let avcc = nals_to_avcc(&data);
                if avcc.is_empty() {
                    continue;
                }

                let base = *base_ts_ns.get_or_insert(timestamp_ns);
                let timestamp_us = timestamp_ns.saturating_sub(base) / 1000;

                let _ = channel.send(FrameEvent::Packet {
                    key: is_idr,
                    data: base64::engine::general_purpose::STANDARD.encode(&avcc),
                    timestamp: timestamp_us,
                });
            }
            UxplayEvent::VideoFlush => {
                // New client / stream reset: require a fresh keyframe before
                // feeding the decoder again.
                seen_keyframe = false;
                base_ts_ns = None;
            }
            UxplayEvent::VideoCodec { codec } => {
                if codec != 0 {
                    eprintln!("[ios] unexpected mirror codec {}", codec);
                }
            }
            UxplayEvent::Connected { client_name, client_ip, .. } => {
                println!("[ios] mirroring client connected: {} (raw client_ip: {:?})", client_name, client_ip);
                // The mirroring phone's LAN address is exactly where the
                // AgentInput keyboard listens - auto-configure it when the
                // user has not set one up manually (PIN-less default).
                let host = client_ip
                    .trim_start_matches('[')
                    .trim_start_matches("::ffff:")
                    .split('%')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(']')
                    .to_string();
                println!("[ios] agent_input auto-detect: parsed host {:?} from client_ip", host);
                if host.is_empty() {
                    println!("[ios] agent_input auto-detect: no client IP reported, skipping");
                } else if !host.contains('.') {
                    // IPv6 (e.g. AWDL peer-to-peer link-local) - AgentInput is
                    // not reachable over it from this machine.
                    println!("[ios] agent_input auto-detect: non-IPv4 address, skipping");
                } else {
                    let mut rb = agent_input.lock().unwrap();
                    if rb.is_none() {
                        *rb = Some(xphoneuse_ios::agent_input::AgentInputClient::new(
                            host.clone(),
                            xphoneuse_ios::agent_input::DEFAULT_PORT,
                            None,
                        ));
                        println!("[ios] agent_input auto-configured for {host}:{}", xphoneuse_ios::agent_input::DEFAULT_PORT);
                        let _ = channel.send(FrameEvent::AgentInput { host });
                    } else {
                        println!("[ios] agent_input already configured, keeping existing config");
                    }
                }
            }
            UxplayEvent::Disconnected => {
                println!("[ios] mirroring client disconnected");
                seen_keyframe = false;
                base_ts_ns = None;
            }
            UxplayEvent::Log { message, .. } => {
                println!("[uxplay] {}", message);
            }
            UxplayEvent::VideoSize { .. } => {}
        }
    }

    let _ = channel.send(FrameEvent::Disconnected {
        reason: "disconnected".to_string(),
    });
}

/// Forward the CoreDevice tunnel display stream (Annex-B access units from
/// the tunnel actor) to the frontend as the same `FrameEvent`s the other
/// paths produce. The tunnel normally negotiates HEVC, but the codec is
/// detected from the NAL headers so an H.264 answer would also decode.
/// Apple sends the parameter sets in-band with each keyframe, so the decoder
/// config is (re)emitted from the first access unit that carries a complete
/// parameter-set set.
pub async fn stream_ios_video_tunnel(
    mut frames: tokio::sync::mpsc::Receiver<xphoneuse_ios::tunnel::TunnelFrame>,
    channel: Channel<FrameEvent>,
    shutdown: Arc<Notify>,
) {
    let mut config_sent = false;
    let mut seen_keyframe = false;
    // True visible screen size from the actor's screencapture probe — the
    // encoder pads the coded picture to whole macroblocks with black borders.
    let mut visible: Option<(u32, u32)> = None;

    loop {
        let frame = tokio::select! {
            f = frames.recv() => match f {
                Some(f) => f,
                None => break, // tunnel actor stopped
            },
            _ = shutdown.notified() => break,
        };
        let (key, data, timestamp_us) = match frame {
            xphoneuse_ios::tunnel::TunnelFrame::AccessUnit { key, data, timestamp_us } => {
                (key, data, timestamp_us)
            }
            xphoneuse_ios::tunnel::TunnelFrame::VisibleSize { width, height } => {
                // Arrives before any access units (the actor sends it before
                // its main loop); if it somehow comes late, a later config
                // would be needed — not worth the reconfigure complexity.
                if !config_sent {
                    visible = Some((width, height));
                }
                continue;
            }
        };

        let nals = split_nals(&data);
        if nals.is_empty() {
            continue;
        }

        if !config_sent {
            let has_h264_sps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 7);
            let has_h264_pps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 8);
            let has_hevc_sps = nals.iter().any(|n| n.len() > 1 && (n[0] >> 1) & 0x3f == 33);
            let has_hevc_pps = nals.iter().any(|n| n.len() > 1 && (n[0] >> 1) & 0x3f == 34);
            // build_hvcc tolerates a missing VPS; build_avcc needs SPS+PPS.
            let cfg = if has_hevc_sps && has_hevc_pps {
                Some(parse_h265_config(&nals))
            } else if has_h264_sps && has_h264_pps {
                Some(parse_h264_config(&nals))
            } else {
                None
            };
            if let Some((codec, desc)) = cfg {
                if !desc.is_empty() {
                    eprintln!(
                        "[tunnel] video config: {} (hvcC/avcC {} bytes)",
                        codec,
                        desc.len()
                    );
                    let _ = channel.send(FrameEvent::Config {
                        codec,
                        description: base64::engine::general_purpose::STANDARD.encode(&desc),
                        width: visible.map(|(w, _)| w),
                        height: visible.map(|(_, h)| h),
                    });
                    config_sent = true;
                }
            }
        }

        // WebCodecs requires the first chunk to be a keyframe
        if !seen_keyframe {
            if !key {
                continue;
            }
            seen_keyframe = true;
        }

        let avcc = nals_to_avcc(&data);
        if avcc.is_empty() {
            continue;
        }

        let _ = channel.send(FrameEvent::Packet {
            key,
            data: base64::engine::general_purpose::STANDARD.encode(&avcc),
            timestamp: timestamp_us,
        });
    }

    let _ = channel.send(FrameEvent::Disconnected {
        reason: "disconnected".to_string(),
    });
}

/// Forward the uxplay RTP H.264 stream to the frontend as the same
/// `FrameEvent`s the Android (scrcpy) path produces, so the WebCodecs
/// decoder and canvas renderer work unchanged.
pub async fn stream_ios_video(
    mut socket: UdpSocket,
    mut child: Child,
    channel: Channel<FrameEvent>,
    shutdown: Arc<Notify>,
) {
    let result = tokio::select! {
        r = forward_loop(&mut socket, &channel, shutdown.clone()) => r,
        _ = child.wait() => Err(anyhow::anyhow!("uxplay exited")),
        _ = shutdown.notified() => Ok(()),
    };

    let _ = child.kill().await;

    let reason = match result {
        Ok(()) => "disconnected".to_string(),
        Err(e) => e.to_string(),
    };
    let _ = channel.send(FrameEvent::Disconnected { reason });
}

async fn forward_loop(
    socket: &mut UdpSocket,
    channel: &Channel<FrameEvent>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut depacketizer = RtpDepacketizer::new();
    let mut buf = vec![0u8; RTP_RECV_BUFFER];
    let mut config_sent = false;
    let mut seen_keyframe = false;

    loop {
        let (len, _) = tokio::select! {
            r = socket.recv_from(&mut buf) => r?,
            _ = shutdown.notified() => return Ok(()),
        };

        let Some(au) = depacketizer.push(&buf[..len]) else {
            continue;
        };

        let nals = split_nals(&au.data);

        // Emit the decoder config as soon as SPS+PPS are seen (they may arrive
        // in an access unit of their own, before the first keyframe)
        if !config_sent {
            let has_sps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 7);
            let has_pps = nals.iter().any(|n| !n.is_empty() && n[0] & 0x1f == 8);
            if has_sps && has_pps {
                let (codec, desc) = parse_h264_config(&nals);
                if !desc.is_empty() {
                    let _ = channel.send(FrameEvent::Config {
                        codec,
                        description: base64::engine::general_purpose::STANDARD.encode(&desc),
                        width: None,
                        height: None,
                    });
                    config_sent = true;
                }
            }
        }

        // WebCodecs requires the first chunk to be a keyframe
        if !seen_keyframe {
            if !au.key {
                continue;
            }
            seen_keyframe = true;
        }

        let avcc = nals_to_avcc(&au.data);
        if avcc.is_empty() {
            continue;
        }

        let _ = channel.send(FrameEvent::Packet {
            key: au.key,
            data: base64::engine::general_purpose::STANDARD.encode(&avcc),
            timestamp: au.timestamp_us,
        });
    }
}
