//! System audio: captures the default output's monitor through PipeWire, as 48 kHz
//! stereo f32, and packs it into the same 5 ms s16 packets the macOS host sends.

use std::sync::Arc;

use glass_proto::{AUDIO_FRAMES_PER_PACKET, AUDIO_SAMPLE_RATE, audio_packet};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

use super::clock::now_us;
pub use crate::media::AudioPackets;

pub const SUPPORTED: bool = true;
const CHANNELS: usize = 2;

struct State {
    packets: AudioPackets,
    pending: Vec<i16>,
    seq: u32,
}

pub fn start(packets: AudioPackets) {
    let spawned = std::thread::Builder::new().name("glass-audio".into()).spawn(move || {
        if let Err(e) = run(packets) {
            tracing::warn!("audio capture stopped: {e:#}");
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("audio thread: {e}");
    }
}

fn run(packets: AudioPackets) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };
    // Record what's playing (the output's monitor), not the microphone.
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    let stream = pw::stream::StreamBox::new(&core, "glass-audio", props)?;

    let state = State { packets, pending: Vec::new(), seq: 0 };
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let bytes = &bytes[..size.min(bytes.len())];
            st.pending.extend(bytes.chunks_exact(4).map(|b| {
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            }));
            let per_packet = AUDIO_FRAMES_PER_PACKET * CHANNELS;
            let mut offset = 0;
            while st.pending.len() - offset >= per_packet {
                let packet = audio_packet(st.seq, now_us(), CHANNELS as u8, &st.pending[offset..offset + per_packet]);
                // No receivers (nobody listening) is fine.
                let _ = st.packets.send(Arc::new(packet));
                st.seq = st.seq.wrapping_add(1);
                offset += per_packet;
            }
            st.pending.drain(..offset);
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(AUDIO_SAMPLE_RATE);
    info.set_channels(CHANNELS as u32);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let values = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or_else(|| anyhow::anyhow!("audio format pod"))?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    mainloop.run();
    Ok(())
}
