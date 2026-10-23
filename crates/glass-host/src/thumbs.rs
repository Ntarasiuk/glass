//! Display previews: small pictures of every display (shown or not), so the viewer can show
//! a strip of them to switch between. The viewer asks while its strip is showing; each
//! answer carries only the pictures that changed since the last one it got.

use std::collections::HashMap;

use anyhow::Result;
use base64::Engine;
use serde::Serialize;

use crate::lossless::RawFrame;

/// Biggest preview, in pixels (aspect kept).
pub const MAX: [u32; 2] = [320, 200];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Thumb {
    pub id: u32,
    /// A PNG, base64.
    pub png: String,
}

/// The size `w`×`h` scaled down (never up) to fit `max`, at least 1×1.
pub fn fit(w: usize, h: usize, [mw, mh]: [u32; 2]) -> (usize, usize) {
    let scale = (mw as f64 / w as f64).min(mh as f64 / h as f64).min(1.0);
    (((w as f64 * scale).round() as usize).max(1), ((h as f64 * scale).round() as usize).max(1))
}

/// Scales a 4-byte-per-pixel picture (BGRx, or RGBx when `rgb`) down to fit `max`, averaging
/// each block of source pixels. Returns BGRA.
pub fn downscale(data: &[u8], w: usize, h: usize, stride: usize, rgb: bool, max: [u32; 2]) -> Option<RawFrame> {
    if w == 0 || h == 0 || stride < w * 4 || data.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let (tw, th) = fit(w, h, max);
    let mut bgra = Vec::with_capacity(tw * th * 4);
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                let row = &data[y * stride..];
                for x in x0..x1 {
                    let p = &row[x * 4..x * 4 + 3];
                    for c in 0..3 {
                        sum[c] += p[c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let [a, b, c] = sum.map(|s| (s / n) as u8);
            bgra.extend_from_slice(&if rgb { [c, b, a, 255] } else { [a, b, c, 255] });
        }
    }
    Some(RawFrame { width: tw, height: th, bgra, capture_us: 0 })
}

/// A preview as PNG (scaled down first if it's bigger than `MAX`).
pub fn png(frame: &RawFrame) -> Result<Vec<u8>> {
    if fit(frame.width, frame.height, MAX) != (frame.width, frame.height) {
        let small = downscale(&frame.bgra, frame.width, frame.height, frame.width * 4, false, MAX)
            .ok_or_else(|| anyhow::anyhow!("bad frame"))?;
        return crate::lossless::encode_png(&small, [0, 0, small.width, small.height]);
    }
    crate::lossless::encode_png(frame, [0, 0, frame.width, frame.height])
}

/// What one viewer was last sent, so unchanged previews aren't sent again.
#[derive(Default)]
pub struct Sent(HashMap<u32, u64>);

impl Sent {
    /// The previews among `pictures` that differ from what this viewer has.
    pub fn changed(&mut self, pictures: Vec<(u32, Vec<u8>)>) -> Vec<Thumb> {
        use std::hash::{Hash, Hasher};
        let mut out = Vec::new();
        for (id, png) in pictures {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            png.hash(&mut h);
            let hash = h.finish();
            if self.0.insert(id, hash) != Some(hash) {
                out.push(Thumb { id, png: base64::engine::general_purpose::STANDARD.encode(&png) });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_without_upscaling() {
        assert_eq!(fit(2560, 1600, MAX), (320, 200));
        assert_eq!(fit(1920, 1080, MAX), (320, 180));
        assert_eq!(fit(100, 50, MAX), (100, 50));
    }

    #[test]
    fn averages_blocks_and_swaps_rgb() {
        // 2×1 RGBx: red and blue, averaged into one purple pixel (as BGRA).
        let data = [255, 0, 0, 0, 0, 0, 255, 0];
        let small = downscale(&data, 2, 1, 8, true, [1, 1]).unwrap();
        assert_eq!((small.width, small.height), (1, 1));
        assert_eq!(small.bgra, vec![127, 0, 127, 255]);
    }

    #[test]
    fn sends_only_what_changed() {
        let mut sent = Sent::default();
        assert_eq!(sent.changed(vec![(1, vec![1]), (2, vec![2])]).len(), 2);
        let again = sent.changed(vec![(1, vec![1]), (2, vec![3])]);
        assert_eq!(again.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2]);
    }
}
