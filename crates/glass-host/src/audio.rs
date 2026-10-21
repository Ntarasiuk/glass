//! System audio from ScreenCaptureKit → fixed 5 ms s16 PCM packets.

use std::ptr::NonNull;
use std::sync::Arc;

use glass_proto::{AUDIO_FRAMES_PER_PACKET, AUDIO_SAMPLE_RATE, audio_packet};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBuffer};
use tokio::sync::broadcast;

use crate::clock::cmtime_us;

pub type AudioPackets = broadcast::Sender<Arc<Vec<u8>>>;

pub struct Packetizer {
    packets: AudioPackets,
    channels: usize,
    /// Interleaved samples not yet sent, and the capture time of the first one.
    pending: Vec<i16>,
    pending_us: u64,
    seq: u32,
}

impl Packetizer {
    pub fn new(packets: AudioPackets) -> Self {
        Self { packets, channels: 2, pending: Vec::new(), pending_us: 0, seq: 0 }
    }

    pub fn push(&mut self, sample: &CMSampleBuffer) {
        let Some(pcm) = (unsafe { to_interleaved_s16(sample) }) else {
            return;
        };
        let (samples, channels) = pcm;
        if self.seq == 0 && self.pending.is_empty() {
            tracing::info!("audio: first buffer ({} frames, {channels} ch)", samples.len() / channels.max(1));
        }
        if channels != self.channels {
            self.pending.clear();
            self.channels = channels;
        }
        if self.pending.is_empty() {
            self.pending_us = cmtime_us(unsafe { sample.presentation_time_stamp() });
        }
        self.pending.extend_from_slice(&samples);

        let per_packet = AUDIO_FRAMES_PER_PACKET * channels;
        let packet_us = AUDIO_FRAMES_PER_PACKET as u64 * 1_000_000 / AUDIO_SAMPLE_RATE as u64;
        let mut offset = 0;
        while self.pending.len() - offset >= per_packet {
            let chunk = &self.pending[offset..offset + per_packet];
            let packet = audio_packet(self.seq, self.pending_us, channels as u8, chunk);
            // No receivers (nobody listening) is fine.
            let _ = self.packets.send(Arc::new(packet));
            self.seq = self.seq.wrapping_add(1);
            self.pending_us += packet_us;
            offset += per_packet;
        }
        self.pending.drain(..offset);
    }
}

/// Returns interleaved s16 samples and the channel count.
unsafe fn to_interleaved_s16(sample: &CMSampleBuffer) -> Option<(Vec<i16>, usize)> {
    let format = unsafe { sample.format_description() }?;
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&format).as_ref() }?;
    let frames = unsafe { sample.num_samples() } as usize;
    let channels = asbd.mChannelsPerFrame as usize;
    if asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0 || asbd.mBitsPerChannel != 32 || channels == 0 {
        tracing::warn!("unsupported audio format: flags {:#x}, {} bits", asbd.mFormatFlags, asbd.mBitsPerChannel);
        return None;
    }

    let block = unsafe { sample.data_buffer() }?;
    let len = unsafe { block.data_length() };
    let mut bytes = vec![0u8; len];
    let status =
        unsafe { block.copy_data_bytes(0, len, NonNull::new_unchecked(bytes.as_mut_ptr().cast())) };
    if status != 0 || len < frames * channels * 4 {
        return None;
    }
    let float = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    let to_s16 = |v: f32| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;

    let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    let mut out = Vec::with_capacity(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            // Planar buffers store each channel's samples back to back.
            let idx = if planar { c * frames + f } else { f * channels + c };
            out.push(to_s16(float(idx)));
        }
    }
    Some((out, channels))
}
