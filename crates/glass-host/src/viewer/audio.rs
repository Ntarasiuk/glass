//! The other computer's sound, out of this Mac's speakers: Opus (or raw PCM) packets from
//! the session into a short buffer that the output device drains. The buffer is kept
//! around 40 ms: enough to ride out jitter, short enough to stay with the picture.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const RATE: f64 = 48_000.0;

/// Frames that went out to the speakers with sound in them (for `glass viewer-selftest`).
pub static PLAYED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Start playing once this much is buffered…
const PRIME_SECS: f64 = 0.03;
/// …and drop the oldest past this, so a stall never turns into lasting delay.
const MAX_SECS: f64 = 0.12;

struct Ring {
    /// Interleaved stereo at 48 kHz.
    samples: VecDeque<f32>,
    primed: bool,
}

pub struct Player {
    ring: Arc<Mutex<Ring>>,
    opus: opus::Decoder,
    pcm: Vec<i16>,
    _stop: std::sync::mpsc::Sender<()>,
}

impl Player {
    pub fn new(volume: Arc<AtomicU32>) -> Result<Self> {
        let ring = Arc::new(Mutex::new(Ring { samples: VecDeque::new(), primed: false }));
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready) = std::sync::mpsc::channel::<Result<()>>();
        let for_device = ring.clone();
        // The output stream belongs to the thread that made it.
        std::thread::Builder::new().name("glass-viewer-audio".into()).spawn(move || {
            let started = (|| -> Result<cpal::Stream> {
                let device = cpal::default_host().default_output_device().context("no sound output")?;
                let def = device.default_output_config()?;
                let channels = def.channels().max(1) as usize;
                let out_rate = def.sample_rate() as f64;
                let config = cpal::StreamConfig { channels: def.channels(), sample_rate: def.sample_rate(), buffer_size: cpal::BufferSize::Default };
                // Resampled on the way out when the device isn't at 48 kHz.
                let step = RATE / out_rate;
                let mut pos = 0.0f64;
                // Self-tests play everything through, silently.
                let mute = std::env::var_os("GLASS_VIEWER_MUTE").is_some();
                let stream = device.build_output_stream::<f32, _, _>(
                    config,
                    move |out, _| {
                        let gain = f32::from_bits(volume.load(Ordering::Relaxed)).clamp(0.0, 1.0);
                        let mut ring = for_device.lock().unwrap();
                        if !ring.primed && ring.samples.len() as f64 >= PRIME_SECS * RATE * 2.0 {
                            ring.primed = true;
                        }
                        for frame in out.chunks_mut(channels) {
                            let (l, r) = if ring.primed && ring.samples.len() >= 4 {
                                let i = pos as usize * 2;
                                let s = |k: usize| ring.samples.get(k).copied().unwrap_or(0.0);
                                let t = pos.fract() as f32;
                                let l = s(i) * (1.0 - t) + s(i + 2) * t;
                                let r = s(i + 1) * (1.0 - t) + s(i + 3) * t;
                                pos += step;
                                let whole = (pos as usize).min(ring.samples.len() / 2);
                                ring.samples.drain(..whole * 2);
                                pos -= whole as f64;
                                (l, r)
                            } else {
                                // Ran dry: wait to build up again rather than crackle.
                                ring.primed = false;
                                (0.0, 0.0)
                            };
                            for (c, x) in frame.iter_mut().enumerate() {
                                *x = if mute { 0.0 } else if c % 2 == 0 { l * gain } else { r * gain };
                            }
                            if ring.primed {
                                PLAYED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    },
                    |e| tracing::warn!("viewer audio: {e}"),
                    None,
                )?;
                stream.play()?;
                Ok(stream)
            })();
            match started {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stopped.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
        ready.recv().context("sound thread ended")??;
        Ok(Self { ring, opus: opus::Decoder::new(48_000, opus::Channels::Stereo)?, pcm: vec![0; 5760 * 2], _stop: stop })
    }

    /// One AUDIO_OPUS packet's payload.
    pub fn opus(&mut self, packet: &[u8]) {
        match self.opus.decode(packet, &mut self.pcm, false) {
            Ok(frames) => {
                let pcm = std::mem::take(&mut self.pcm);
                self.push(&pcm[..frames * 2], 2);
                self.pcm = pcm;
            }
            Err(e) => tracing::debug!("viewer audio: opus: {e}"),
        }
    }

    /// Interleaved s16 samples (one or two channels).
    pub fn push(&mut self, pcm: &[i16], channels: usize) {
        let mut ring = self.ring.lock().unwrap();
        for frame in pcm.chunks_exact(channels.max(1)) {
            let l = frame[0] as f32 / 32768.0;
            let r = frame.get(1).map_or(l, |&r| r as f32 / 32768.0);
            ring.samples.push_back(l);
            ring.samples.push_back(r);
        }
        let max = (MAX_SECS * RATE) as usize * 2;
        if ring.samples.len() > max {
            let target = (PRIME_SECS * RATE) as usize * 2;
            let excess = ring.samples.len() - target;
            ring.samples.drain(..excess);
        }
    }
}

/// This Mac's microphone, to the other computer: 5 ms mono s16 packets at 48 kHz
/// (KIND_MIC datagrams, as the web viewer sends).
pub struct Mic {
    _stop: std::sync::mpsc::Sender<()>,
}

impl Mic {
    pub fn start(send: impl Fn(Vec<u8>) + Send + 'static) -> Result<Self> {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::Builder::new().name("glass-viewer-mic".into()).spawn(move || {
            let started = (|| -> Result<cpal::Stream> {
                let device = cpal::default_host().default_input_device().context("no microphone")?;
                let def = device.default_input_config()?;
                let channels = def.channels().max(1) as usize;
                let step = def.sample_rate() as f64 / RATE;
                let config = cpal::StreamConfig { channels: def.channels(), sample_rate: def.sample_rate(), buffer_size: cpal::BufferSize::Default };
                let (mut pos, mut prev, mut chunk, mut seq) = (0.0f64, 0.0f32, Vec::<i16>::with_capacity(glass_proto::AUDIO_FRAMES_PER_PACKET), 0u32);
                let clock = std::time::Instant::now();
                tracing::info!("viewer mic: {} at {} Hz, {channels} channel(s), {:?}", device.description().map(|d| d.name().to_string()).unwrap_or_default(), def.sample_rate(), def.sample_format());
                let mut first = true;
                let stream = device.build_input_stream::<f32, _, _>(
                    config,
                    move |data, _| {
                        if std::mem::take(&mut first) {
                            tracing::info!("viewer mic: first {} samples", data.len());
                        }
                        for frame in data.chunks(channels) {
                            let x = frame.iter().sum::<f32>() / frame.len() as f32;
                            // Resampled to 48 kHz by linear interpolation.
                            while pos < 1.0 {
                                let s = prev + (x - prev) * pos as f32;
                                chunk.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                                pos += step;
                                if chunk.len() == glass_proto::AUDIO_FRAMES_PER_PACKET {
                                    let mut p = vec![glass_proto::KIND_MIC, 1, 0, 0];
                                    p.extend_from_slice(&seq.to_le_bytes());
                                    p.extend_from_slice(&(clock.elapsed().as_micros() as u64).to_le_bytes());
                                    for s in chunk.drain(..) {
                                        p.extend_from_slice(&s.to_le_bytes());
                                    }
                                    seq = seq.wrapping_add(1);
                                    send(p);
                                }
                            }
                            pos -= 1.0;
                            prev = x;
                        }
                    },
                    |e| tracing::warn!("viewer mic: {e}"),
                    None,
                )?;
                stream.play()?;
                Ok(stream)
            })();
            match started {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stopped.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
        ready.recv().context("microphone thread ended")??;
        Ok(Self { _stop: stop })
    }
}
