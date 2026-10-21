//! Software H.264 (OpenH264, screen-content real-time mode), used by the Linux host and
//! by the test pattern on every platform. Output is Annex B with
//! SPS/PPS in every IDR, which the browser decodes without an out-of-band description.
// Off Linux only the test pattern uses this, so the capture-facing parts go unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use anyhow::{Result, anyhow};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder as H264, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::YUVSlices;

/// Above this many pixels, frames are halved before encoding to keep software encode
/// within the frame budget (4K → 1080p, 5K → 1440p).
const MAX_PIXELS: usize = 2560 * 1600;
/// Matches the macOS encoder: a periodic IDR before frame_num wraps.
const MAX_GOP_FRAMES: u32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Bytes B, G, R, X (BGRx / BGRA).
    Bgrx,
    /// Bytes R, G, B, X (RGBx / RGBA).
    Rgbx,
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

pub struct Encoder {
    h264: H264,
    src: (usize, usize),
    div: usize,
    out: (usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Encoder {
    /// `scale` (from --scale) of 0.5 or less also halves the frame.
    pub fn new(width: usize, height: usize, fps: u32, bitrate: u32, scale: f64) -> Result<Self> {
        let div = if width * height > MAX_PIXELS || scale <= 0.5 { 2 } else { 1 };
        let out = ((width / div) & !1, (height / div) & !1);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(MAX_GOP_FRAMES))
            .skip_frames(false)
            .num_threads(4);
        let h264 = H264::with_api_config(OpenH264API::from_source(), config).map_err(|e| anyhow!("openh264: {e}"))?;
        let (w, h) = out;
        Ok(Self {
            h264,
            src: (width, height),
            div,
            out,
            y: vec![0; w * h],
            u: vec![0; (w / 2) * (h / 2)],
            v: vec![0; (w / 2) * (h / 2)],
        })
    }

    /// Changes the target bitrate on the fly (no new keyframe).
    pub fn set_bitrate(&mut self, bps: u32) {
        let mut info = openh264_sys2::TagBitrateInfo { iLayer: openh264_sys2::SPATIAL_LAYER_ALL, iBitrate: bps as i32 };
        // SAFETY: SetOption(ENCODER_OPTION_BITRATE) takes a pointer to SBitrateInfo, which
        // lives for the duration of the call.
        unsafe {
            self.h264.raw_api().set_option(openh264_sys2::ENCODER_OPTION_BITRATE, (&raw mut info).cast());
        }
    }

    /// Size the encoder was built for; a different frame size needs a new encoder.
    pub fn source_size(&self) -> (usize, usize) {
        self.src
    }

    /// Returns Annex B data and whether it's a keyframe, or None if nothing was produced.
    pub fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        to_i420(frame, self.div, self.out, &mut self.y, &mut self.u, &mut self.v);
        if force_keyframe {
            self.h264.force_intra_frame();
        }
        let (w, h) = self.out;
        let yuv = YUVSlices::new((&self.y, &self.u, &self.v), (w, h), (w, w / 2, w / 2));
        let bits = self.h264.encode(&yuv).map_err(|e| anyhow!("encode: {e}"))?;
        let keyframe = matches!(bits.frame_type(), FrameType::IDR | FrameType::I);
        let data = bits.to_vec();
        Ok((!data.is_empty()).then_some((data, keyframe)))
    }
}

/// BT.601 limited range, 2x2-averaged chroma; `div` box-filters the source down.
fn to_i420(f: &Frame, div: usize, (w, h): (usize, usize), y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    let (ri, gi, bi) = match f.format {
        PixelFormat::Bgrx => (2, 1, 0),
        PixelFormat::Rgbx => (0, 1, 2),
    };
    // Average a div×div block of source pixels.
    let sample = |sx: usize, sy: usize| -> (i32, i32, i32) {
        let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
        for dy in 0..div {
            let row = &f.data[(sy * div + dy) * f.stride..];
            for dx in 0..div {
                let p = &row[(sx * div + dx) * 4..];
                r += p[ri] as i32;
                g += p[gi] as i32;
                b += p[bi] as i32;
            }
        }
        let n = (div * div) as i32;
        (r / n, g / n, b / n)
    };
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let (mut rs, mut gs, mut bs) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (px, py) = (cx * 2 + dx, cy * 2 + dy);
                let (r, g, b) = sample(px, py);
                y[py * w + px] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
                rs += r;
                gs += g;
                bs += b;
            }
            let (r, g, b) = (rs / 4, gs / 4, bs / 4);
            u[cy * (w / 2) + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v[cy * (w / 2) + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_and_black_to_i420() {
        let frame = |px: [u8; 4]| Frame { width: 2, height: 2, stride: 8, format: PixelFormat::Bgrx, data: px.repeat(4) };
        let (mut y, mut u, mut v) = (vec![0; 4], vec![0; 1], vec![0; 1]);
        to_i420(&frame([255, 255, 255, 0]), 1, (2, 2), &mut y, &mut u, &mut v);
        assert_eq!((y[0], u[0], v[0]), (235, 128, 128));
        to_i420(&frame([0, 0, 0, 0]), 1, (2, 2), &mut y, &mut u, &mut v);
        assert_eq!((y[0], u[0], v[0]), (16, 128, 128));
    }

    #[test]
    fn encodes_a_decodable_keyframe() {
        let frame = Frame {
            width: 64,
            height: 64,
            stride: 64 * 4,
            format: PixelFormat::Bgrx,
            data: (0..64 * 64).flat_map(|i| [(i % 251) as u8, (i % 7 * 30) as u8, 200, 0]).collect(),
        };
        let mut enc = Encoder::new(64, 64, 30, 1_000_000, 1.0).unwrap();
        let (data, keyframe) = enc.encode(&frame, true).unwrap().expect("output");
        assert!(keyframe);
        assert_eq!(&data[..4], &[0, 0, 0, 1], "Annex B");
        let nal_types: Vec<u8> = data.windows(5).filter(|w| w[..4] == [0, 0, 0, 1]).map(|w| w[4] & 0x1f).collect();
        assert!(nal_types.contains(&7) && nal_types.contains(&8) && nal_types.contains(&5), "{nal_types:?}");
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        assert!(dec.decode(&data).unwrap().is_some(), "decodes");
    }

    #[test]
    fn red_differs_by_channel_order() {
        let bgrx = Frame { width: 2, height: 2, stride: 8, format: PixelFormat::Bgrx, data: [0, 0, 255, 0].repeat(4) };
        let rgbx = Frame { width: 2, height: 2, stride: 8, format: PixelFormat::Rgbx, data: [255, 0, 0, 0].repeat(4) };
        let (mut y1, mut u1, mut v1) = (vec![0; 4], vec![0; 1], vec![0; 1]);
        let (mut y2, mut u2, mut v2) = (vec![0; 4], vec![0; 1], vec![0; 1]);
        to_i420(&bgrx, 1, (2, 2), &mut y1, &mut u1, &mut v1);
        to_i420(&rgbx, 1, (2, 2), &mut y2, &mut u2, &mut v2);
        assert_eq!((y1, u1, v1.clone()), (y2, u2, v2));
        assert!(v1[0] > 200, "red has high V");
    }
}
