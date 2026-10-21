//! Remote video as RTP H.264 tracks (one per display), for WebRTC viewers. Congestion is
//! handled the way browsers do for video calls: Google Congestion Control over
//! transport-cc picks the rate, lost packets are resent (NACK/RTX), and a keyframe is sent
//! only when the browser asks (PLI/FIR). The SCTP data channels this replaces back off like
//! TCP and stall on a long link.
//!
//! Frames come straight from the shared encoders (`Shared::frames`); the bridge's loopback
//! WebTransport session still carries sign-in, control, input, files and audio.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use rtc::interceptor::{
    Attribute, BandwidthEstimator, EstimatorStats, Gcc, Interceptor, NackResponderBuilder,
    PacerBuilder, Packet, PacketReport, Slot, StreamInfo, TaggedPacket,
};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RTCRtpRtxParameters, RtpCodecKind,
};
use rtc::sansio::Protocol;
use tokio::sync::broadcast;
use tracing::{debug, info};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::peer_connection::{
    CongestionFeedback, MediaEngine, PeerConnection, Registry, configure_congestion_control,
    register_default_interceptors,
};
use webrtc::rtp_transceiver::RtpSender;

/// GCC grows only ~8%/s and doesn't probe, so it starts where a typical internet link
/// already gives a readable picture.
const INITIAL_BPS: f64 = 5_000_000.0;
const MIN_BPS: f64 = 1_000_000.0;
const MAX_BPS: f64 = 40_000_000.0;
/// The pacer releases at exactly the target it's given; pace above what the encoder is
/// asked for so a keyframe drains in a few frame times, not a second.
const PACING_FACTOR: f64 = 2.0;
/// Headroom left for retransmissions when the estimate sets the encoder's bitrate.
const ENCODER_SHARE: f64 = 0.85;
/// A browser's keyframe requests are honoured at most this often per display.
const KEYFRAME_SPACING: Duration = Duration::from_secs(1);
/// Displays carried at most (offers ask for this many video m-lines).
pub const MAX_TRACKS: usize = glass_proto::MAX_STREAMS;

const H264: &str = "video/H264";

fn h264(pt: u8, profile_level_id: &str) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: format!(
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile_level_id}"
            ),
            // nack, nack pli and transport-cc are added by the interceptor helpers (they
            // only touch codecs already registered, so register codecs first).
            rtcp_feedback: vec![RTCPFeedback {
                typ: "ccm".to_owned(),
                parameter: "fir".to_owned(),
            }],
        },
        payload_type: pt,
    }
}

fn rtx(pt: u8, apt: u8) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: "video/rtx".to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: format!("apt={apt}"),
            rtcp_feedback: vec![],
        },
        payload_type: pt,
    }
}

/// H.264 in the profiles browsers offer. VideoToolbox emits High; Chrome offers 64001f,
/// Safari 640c1f, everyone 42e01f (matching compares profile bytes only, not the level).
pub fn media_engine() -> Result<MediaEngine> {
    let mut m = MediaEngine::default();
    for (pt, plid, rtx_pt) in [
        (102u8, "640033", 103u8),
        (104, "640c33", 105),
        (106, "4d0033", 107),
        (108, "42e033", 109),
    ] {
        m.register_codec(h264(pt, plid), RtpCodecKind::Video)?;
        m.register_codec(rtx(rtx_pt, pt), RtpCodecKind::Video)?;
    }
    Ok(m)
}

/// Publishes GCC's target for the encoder, and paces above it.
struct ReportingEstimator {
    inner: Gcc,
    target: Arc<AtomicU64>,
}

impl ReportingEstimator {
    fn publish(&self) {
        self.target
            .store(self.inner.target_bitrate().to_bits(), Ordering::Relaxed);
    }
}

impl BandwidthEstimator for ReportingEstimator {
    fn on_reports(&mut self, now: Instant, reports: &[PacketReport]) {
        self.inner.on_reports(now, reports);
        self.publish();
    }
    /// Only the pacer reads this (so never take the encoder target from stats).
    fn target_bitrate(&self) -> f64 {
        self.inner.target_bitrate() * PACING_FACTOR
    }
    fn handle_timeout(&mut self, now: Instant) {
        self.inner.handle_timeout(now);
        self.publish();
    }
    fn poll_timeout(&self) -> Option<Instant> {
        self.inner.poll_timeout()
    }
    fn stats(&self) -> EstimatorStats {
        self.inner.stats()
    }
}

/// Delivers keyframe requests (PLI/FIR) to the application; other RTCP stops here, except
/// that messages carrying attributes still pass on (they carry target-bitrate updates).
#[derive(Default)]
struct KeyframeRequestForwarder {
    read: VecDeque<TaggedPacket>,
    write: VecDeque<TaggedPacket>,
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequestForwarder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = rtc::shared::error::Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message.packet {
            let keep: Vec<Box<dyn rtc::rtcp::Packet>> = packets
                .iter()
                .filter(|p| {
                    let a = p.as_any();
                    a.is::<PictureLossIndication>() || a.is::<FullIntraRequest>()
                })
                .cloned()
                .collect();
            if keep.is_empty() {
                if msg.message.attributes.is_empty() {
                    return Ok(());
                }
                msg.message.packet = Packet::Rtcp(vec![]);
            } else {
                msg.message.packet = Packet::Rtcp(keep);
                msg.message.add(Attribute::DeliverToApplication);
            }
        }
        self.read.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.read.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.write.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.write.pop_front()
    }
}

impl Interceptor for KeyframeRequestForwarder {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}

/// Interceptors: congestion control (GCC over transport-cc), pacer, NACK responder with a
/// history long enough for a 1440p keyframe, and the keyframe-request forwarder. Returns
/// the estimate the encoder should follow.
pub fn registry(media: &mut MediaEngine) -> Result<(Registry, Arc<AtomicU64>)> {
    let target = Arc::new(AtomicU64::new(INITIAL_BPS.to_bits()));
    let estimator = ReportingEstimator {
        inner: Gcc::new(INITIAL_BPS, MIN_BPS, MAX_BPS),
        target: target.clone(),
    };
    let r =
        configure_congestion_control(Registry::new(), estimator, CongestionFeedback::Twcc, media)?;
    let r = register_default_interceptors(r, media)?;
    let r = r
        .with(
            Slot::Pacer,
            PacerBuilder::new()
                .with_target_bitrate(INITIAL_BPS * PACING_FACTOR)
                .build(),
        )
        .with(
            Slot::NackResponder,
            NackResponderBuilder::new().with_size(4096).build(),
        )
        .with(Slot::from(14_000), KeyframeRequestForwarder::default());
    Ok((r, target))
}

/// How many video m-lines the browser offered (the tracks it can take), up to MAX_TRACKS.
pub fn offered_video(sdp: &str) -> usize {
    sdp.lines()
        .filter(|l| l.starts_with("m=video"))
        .count()
        .min(MAX_TRACKS)
}

pub struct VideoOut {
    index: usize,
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    ssrc: u32,
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u32::from_be_bytes(b)
}

/// Binds one track per offered video m-line, in order: track i carries display stream i.
/// Call after the remote offer is set and before creating the answer.
pub async fn add_tracks(pc: &Arc<dyn PeerConnection>, count: usize) -> Result<Vec<VideoOut>> {
    let mut outs = Vec::new();
    for index in 0..count {
        let ssrc = random_u32();
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                // a=msid:<stream id> <track id>; the browser reads streams[0].id.
                format!("glass-display-{index}"),
                format!("glass-video-{index}"),
                format!("display {index}"),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        rtx: Some(RTCRtpRtxParameters { ssrc: random_u32() }),
                        ..Default::default()
                    },
                    codec: h264(0, "640033").rtp_codec,
                    ..Default::default()
                }],
            ),
        )?);
        let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        outs.push(VideoOut {
            index,
            track,
            sender,
            ssrc,
        });
    }
    Ok(outs)
}

/// The negotiated payload type that matches what we send (High, else the closest).
async fn negotiated_pt(sender: &Arc<dyn RtpSender>) -> Result<u8> {
    let p = sender.get_parameters().await?;
    let h264s: Vec<_> = p
        .rtp_parameters
        .codecs
        .iter()
        .filter(|c| c.rtp_codec.mime_type.eq_ignore_ascii_case(H264))
        .filter(|c| c.rtp_codec.sdp_fmtp_line.contains("packetization-mode=1"))
        .collect();
    let plid = |c: &RTCRtpCodecParameters| {
        c.rtp_codec
            .sdp_fmtp_line
            .split(';')
            .find_map(|kv| kv.trim().strip_prefix("profile-level-id="))
            .map(str::to_ascii_lowercase)
            .unwrap_or_default()
    };
    for prefix in ["6400", "640c", "4d00", "42e0", "4200"] {
        if let Some(c) = h264s.iter().find(|c| plid(c).starts_with(prefix)) {
            return Ok(c.payload_type);
        }
    }
    h264s
        .first()
        .map(|c| c.payload_type)
        .context("no H.264 negotiated")
}

/// What the pump needs from the session it serves.
pub struct Viewer {
    /// Video is sent only once the viewer has signed in, and only while it's watching.
    pub authed: tokio::sync::watch::Receiver<bool>,
    pub watching: tokio::sync::watch::Receiver<bool>,
    /// Identifies this viewer's bitrate cap among all remote viewers.
    pub id: u64,
}

/// Sends encoded frames to the browser until the connection ends.
pub async fn pump(outs: Vec<VideoOut>, target: Arc<AtomicU64>, mut viewer: Viewer) -> Result<()> {
    let shared = crate::server::shared().context("server not running")?;
    viewer.authed.wait_for(|a| *a).await?;
    let mut frames = shared.frames.subscribe();

    let mut tracks = Vec::new();
    for out in outs {
        let pt = negotiated_pt(&out.sender).await?;
        info!(
            "remote video: display {} as RTP (payload type {pt})",
            out.index
        );
        // Keyframe requests (PLI/FIR) arrive on the track's own event stream.
        let track = out.track.clone();
        let stream = out.index as u8;
        let video = shared.video.clone();
        tokio::spawn(async move {
            let mut last = None::<Instant>;
            while let Some(ev) = track.poll().await {
                if matches!(ev, TrackLocalEvent::OnRtcpPacket(_))
                    && last.is_none_or(|t| t.elapsed() >= KEYFRAME_SPACING)
                {
                    last = Some(Instant::now());
                    debug!("remote video: keyframe requested for display {stream}");
                    video.request_keyframe(Some(stream));
                }
            }
        });
        tracks.push((out, pt));
    }
    // Frames already in flight began before this viewer: start each display on a keyframe.
    shared.video.request_keyframe(None);
    let mut started = [false; MAX_TRACKS];
    let mut last_capture = [None::<u64>; MAX_TRACKS];

    let mut report = tokio::time::interval(Duration::from_millis(500));
    let mut sent_bytes = 0usize;
    let mut last_log = Instant::now();
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let frame = match frame {
                    Ok(f) => f,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Skipped frames break the reference chain: restart on keyframes.
                        started = [false; MAX_TRACKS];
                        shared.video.request_keyframe(None);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                let s = frame.stream as usize;
                let Some((out, pt)) = tracks.get(s) else { continue };
                if !*viewer.watching.borrow() {
                    started[s] = false;
                    continue;
                }
                if !started[s] && !frame.keyframe {
                    continue;
                }
                started[s] = true;
                // RTP time advances by the real gap between captures (a still screen
                // sends rarely), so the browser plays each frame at the right moment.
                let gap = last_capture[s]
                    .map(|t| frame.capture_us.saturating_sub(t))
                    .unwrap_or(16_667)
                    .clamp(1_000, 1_000_000);
                last_capture[s] = Some(frame.capture_us);
                sent_bytes += frame.data.len();
                out.track
                    .sample_writer(out.ssrc, *pt)
                    .write_sample(&Sample {
                        data: Bytes::copy_from_slice(&frame.data),
                        duration: Duration::from_micros(gap),
                        ..Sample::new(Instant::now())
                    })
                    .await?;
            }
            _ = report.tick() => {
                let bps = f64::from_bits(target.load(Ordering::Relaxed));
                crate::server::set_remote_cap(viewer.id, Some((bps * ENCODER_SHARE) as u32));
                if last_log.elapsed() >= Duration::from_secs(10) {
                    let sent = sent_bytes as f64 * 8.0 / last_log.elapsed().as_secs_f64() / 1e6;
                    info!("remote video: estimate {:.1} Mb/s, sending {sent:.1} Mb/s", bps / 1e6);
                    sent_bytes = 0;
                    last_log = Instant::now();
                }
            }
            changed = viewer.watching.changed() => {
                if changed.is_err() { return Ok(()) }
                if *viewer.watching.borrow() {
                    shared.video.request_keyframe(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_offered_video_lines() {
        let sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";
        assert_eq!(offered_video(sdp), 2);
        assert_eq!(offered_video("v=0\r\nm=application 9 x\r\n"), 0);
        let many = "m=video 9 x\r\n".repeat(9);
        assert_eq!(offered_video(&many), MAX_TRACKS);
    }

    #[test]
    fn engine_registers_the_profiles_browsers_offer() {
        let _ = media_engine().unwrap();
    }
}
