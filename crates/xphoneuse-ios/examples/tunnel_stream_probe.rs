//! Probe: CoreDevice display stream (live screen over the tunnel).
//!
//! Starts the displayservice media stream (audio first, then video, same
//! clientSessionID - the DeviceHub pattern) and soaks it for N seconds:
//! - prints the negotiated answer structure and the first RTP packet
//!   (payload type 100 = HEVC vs 123 = H.264, ssrc, source ports)
//! - sends RTCP liveness (receiver report + SDES) every 5s to the observed
//!   RTP source port, to find out whether that is enough to keep the
//!   encoder alive past the documented ~20-25s RTCP timeout
//! - reassembles HEVC with HevcDepacketizer and dumps Annex-B output to
//!   tunnel-stream-out.h265 (verify with `ffplay tunnel-stream-out.h265`)
//!
//! Usage: cargo run -p xphoneuse-ios --release --example tunnel_stream_probe [seconds]
//! (default 120)

use idevice::{IdeviceService, RsdService};
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::core_device::{
    build_liveness, build_screen_audio_offer, build_screen_video_offer_with_codec,
    build_start_audio_parameters, build_start_video_parameters, is_rtcp, AvcDepacketizer,
    CallInfoBlob, DisplayServiceClient, HevcDepacketizer, RtpPacket, ScreenVideoCodec,
    SenderReport,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::{AdapterHandle, UdpSocketHandle};
use std::io::Write;
use std::time::{Duration, Instant};

/// What we (the controller) support, NOT the device's mask. Device Hub sends
/// 140 for screen sharing; the device's larger mask produces an invalid config.
const CLIENT_SUPPORTED_FEATURES: u64 = 140;

async fn start_screen_media_stream(
    adapter: &mut AdapterHandle,
    handshake: &mut RsdHandshake,
) -> Result<(DisplayServiceClient<Box<dyn idevice::ReadWrite>>, UdpSocketHandle, UdpSocketHandle), String> {
    let mut client = DisplayServiceClient::connect_rsd(adapter, handshake)
        .await
        .map_err(|e| format!("no display service: {e:?}"))?;

    let audio_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(audio) failed: {e:?}"))?;
    let video_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(video) failed: {e:?}"))?;
    let receiver_ip = adapter.host_ip().to_string();
    let audio_receiver_port = audio_udp.local_port();
    let receiver_port = video_udp.local_port();
    let sender_ip = adapter.peer_ip().to_string();
    eprintln!("[stream] video receiver = {receiver_ip}:{receiver_port}, sender = {sender_ip}");

    // VCCallInfoBlob describing this (host) endpoint - values mirror a
    // captured Device Hub offer the device accepts.
    let call_info = CallInfoBlob {
        call_id: 0,
        client_version: 1,
        device_type: "Mac17,7".into(),
        framework_version: "2205.3.1".into(),
        os_version: "25F71".into(),
        device_name: None,
        audio_device_uid: None,
    };

    let client_session_id = uuid::Uuid::new_v4();

    // Audio first to establish the screen-sharing session.
    let audio_call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let audio_offer = build_screen_audio_offer(&audio_call_id, &call_info)
        .map_err(|e| format!("audio offer build failed: {e:?}"))?;
    let audio_params = build_start_audio_parameters(
        &receiver_ip,
        audio_receiver_port,
        &sender_ip,
        50000,
        audio_offer,
        CLIENT_SUPPORTED_FEATURES,
        client_session_id,
    );
    client
        .start_media_stream(audio_params)
        .await
        .map_err(|e| format!("audio startMediaStream failed: {e:?}"))?;
    eprintln!("[stream] audio stream started (session {client_session_id})");

    // Video on the same session. TUNNEL_CODEC=h264 offers only the AVC bank
    // so the device cannot pick HEVC (whose decode path clips tall pictures
    // on some Windows hosts).
    let codec = match std::env::var("TUNNEL_CODEC").as_deref() {
        Ok("h264") => ScreenVideoCodec::H264,
        _ => ScreenVideoCodec::HevcPreferred,
    };
    let call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let our_ssrc = uuid::Uuid::new_v4().as_u128() as u32;
    let offer = build_screen_video_offer_with_codec(&call_id, &call_info, our_ssrc, codec)
        .map_err(|e| format!("video offer build failed: {e:?}"))?;
    let params = build_start_video_parameters(
        &receiver_ip,
        receiver_port,
        &sender_ip,
        50001,
        offer,
        CLIENT_SUPPORTED_FEATURES,
        1,
        client_session_id,
    );
    let answer = client
        .start_media_stream(params)
        .await
        .map_err(|e| format!("video startMediaStream failed: {e:?}"))?;
    eprintln!("[stream] video stream started (session {client_session_id})");
    eprintln!("[stream] answer plist: {answer:#?}");

    Ok((client, audio_udp, video_udp))
}

#[tokio::main]
async fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    let devices = xphoneuse_ios::device::list_devices().await.expect("list devices");
    let Some(target) = devices.first() else {
        eprintln!("no iOS devices visible to usbmuxd");
        std::process::exit(1);
    };
    eprintln!("target: {} ios={}", target.udid, target.ios_version);

    let provider = xphoneuse_ios::device::provider_for(&target.udid)
        .await
        .expect("provider");

    let t0 = Instant::now();
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .expect("CoreDeviceProxy");
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .expect("software tunnel")
        .to_async_handle();
    let stream = adapter.connect(rsd_port).await.expect("rsd port connect");
    let mut handshake = RsdHandshake::new(stream).await.expect("RSD handshake");
    eprintln!("[stream] tunnel+RSD up in {:?}", t0.elapsed());

    let (mut client, audio_udp, video_udp) = start_screen_media_stream(&mut adapter, &mut handshake)
        .await
        .expect("start stream");
    eprintln!("[stream] stream up in {:?} (total)", t0.elapsed());

    // Keep the audio socket drained (we do not decode audio in this probe).
    let audio_drain = tokio::spawn(async move {
        while let Ok(_dg) = audio_udp.recv().await {}
    });

    let our_ssrc = uuid::Uuid::new_v4().as_u128() as u32;
    let cname = "probe@xphoneuse";
    let h264 = std::env::var("TUNNEL_CODEC").as_deref() == Ok("h264");
    let mut depack = HevcDepacketizer::new();
    let mut avc_depack = AvcDepacketizer::new();
    let dump_path = if h264 { "tunnel-stream-out.h264" } else { "tunnel-stream-out.h265" };
    let mut dump = std::fs::File::create(dump_path).expect("create dump");

    let mut device_rtp_port: Option<u16> = None;
    let mut device_ssrc: Option<u32> = None;
    let mut last_sr: Option<SenderReport> = None;
    let mut last_rtp_at = Instant::now();
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut access_units = 0u64;
    let start = Instant::now();
    let mut keepalive = tokio::time::interval(Duration::from_secs(5));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut reported_first = false;
    let mut marker_count = 0u64;
    loop {
        if start.elapsed().as_secs() >= seconds {
            eprintln!("[stream] soak window over");
            break;
        }
        tokio::select! {
            dg = video_udp.recv() => {
                let dg = match dg {
                    Ok(dg) => dg,
                    Err(e) => {
                        eprintln!("[stream] video recv failed after {:?}: {e}", start.elapsed());
                        break;
                    }
                };
                if !reported_first {
                    eprintln!(
                        "[stream] first datagram: {} bytes, sport={} dport={}",
                        dg.data.len(),
                        dg.source_port,
                        dg.destination_port
                    );
                }
                if is_rtcp(&dg.data) {
                    if let Some(sr) = SenderReport::parse_first(&dg.data) {
                        eprintln!(
                            "[stream] RTCP SR from port {}: ssrc={:08x} ntp_middle={:08x}",
                            dg.source_port, sr.ssrc, sr.ntp_middle
                        );
                        last_sr = Some(sr);
                    }
                    continue;
                }
                let Some(rtp) = RtpPacket::parse(&dg.data) else {
                    eprintln!("[stream] unparseable datagram ({} bytes)", dg.data.len());
                    continue;
                };
                if rtp.marker {
                    marker_count += 1;
                }
                if !reported_first {
                    eprintln!(
                        "[stream] first RTP: pt={} marker={} seq={} ts={} ssrc={:08x}",
                        rtp.payload_type, rtp.marker, rtp.sequence_number, rtp.timestamp, rtp.ssrc
                    );
                    device_rtp_port = Some(dg.source_port);
                    device_ssrc = Some(rtp.ssrc);
                    reported_first = true;
                }
                packets += 1;
                bytes += dg.data.len() as u64;
                last_rtp_at = Instant::now();
                if h264 && packets <= 200 {
                    // Raw payload capture for packetization analysis.
                    let mut rec = (rtp.payload.len() as u32).to_be_bytes().to_vec();
                    rec.extend_from_slice(rtp.payload);
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("tunnel-raw-avc.bin")
                        .expect("raw dump")
                        .write_all(&rec)
                        .expect("raw write");
                }
                let out: Vec<u8> = if h264 {
                    avc_depack.push(rtp.sequence_number, rtp.timestamp, rtp.payload);
                    avc_depack.take_output()
                } else {
                    depack.push(rtp.sequence_number, rtp.timestamp, rtp.payload);
                    depack.take_output()
                };
                if !out.is_empty() {
                    access_units += 1;
                    dump.write_all(&out).expect("write dump");
                    if access_units % 60 == 0 {
                        eprintln!(
                            "[stream] {:#?}: {} pkts, {} bytes, {} AUs, param_sets={}",
                            start.elapsed(),
                            packets,
                            bytes,
                            access_units,
                            depack.has_parameter_sets()
                        );
                    }
                }
            }
            _ = keepalive.tick() => {
                let Some(port) = device_rtp_port else {
                    eprintln!("[stream] keepalive: no RTP yet ({:#?})", start.elapsed());
                    continue;
                };
                let blocks = [idevice::core_device::ReportBlock {
                    source_ssrc: device_ssrc.unwrap_or(0),
                    highest_seq: packets as u32,
                    lsr: last_sr.map(|s| s.ntp_middle).unwrap_or(0),
                    ..Default::default()
                }];
                let pkt = build_liveness(our_ssrc, cname, &blocks);
                match video_udp.send_to(port, pkt).await {
                    Ok(()) => eprintln!("[stream] keepalive sent to :{port} at {:#?}", start.elapsed()),
                    Err(e) => eprintln!("[stream] keepalive send failed: {e}"),
                }
                // Stall detection: no RTP for 10s while the socket is alive.
                if last_rtp_at.elapsed() > Duration::from_secs(10) {
                    eprintln!(
                        "[stream] WARNING: no RTP for {:?} at {:#?}",
                        last_rtp_at.elapsed(),
                        start.elapsed()
                    );
                }
            }
        }
    }

    let _ = audio_drain.abort();
    eprintln!(
        "[stream] totals: {packets} packets, {bytes} bytes, {access_units} access units in {:?}",
        start.elapsed()
    );
    eprintln!("[stream] marker-set packets: {marker_count}/{packets}");
    eprintln!("[stream] dump written to {dump_path}");

    match client.stop_media_stream().await {
        Ok(_) => eprintln!("[stream] stopped cleanly"),
        Err(e) => eprintln!("[stream] stop returned {e:?} (channel closing on stop is normal)"),
    }
}
