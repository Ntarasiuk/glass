//! Adds `bitstream_restriction` (max_num_reorder_frames = 0) to an H.264 SPS that lacks
//! it. Without it a decoder can't know that frames come out in decoding order, and
//! Chromium's hardware decoder (VA-API on Linux) then holds back a full DPB of frames
//! (about 10 at 3440x1440, level 5.2: ~200 ms) before showing any. NVENC through
//! GStreamer writes none; OpenH264 and VA-API do.

/// Reads bits MSB first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(b as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |acc, _| Some((acc << 1) | self.bit()?))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }

    fn se(&mut self) -> Option<i32> {
        let v = self.ue()?;
        Some(if v % 2 == 1 { v.div_ceil(2) as i32 } else { -((v / 2) as i32) })
    }
}

/// Writes bits MSB first.
#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    bits: usize,
}

impl Writer {
    fn bit(&mut self, b: u32) {
        if self.bits % 8 == 0 {
            self.out.push(0);
        }
        if b != 0 {
            *self.out.last_mut().unwrap() |= 1 << (7 - self.bits % 8);
        }
        self.bits += 1;
    }

    fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1);
        }
    }

    fn ue(&mut self, v: u32) {
        let v1 = v + 1;
        let len = 32 - v1.leading_zeros();
        self.bits(0, len - 1);
        self.bits(v1, len);
    }
}

/// NAL payload (after the header byte) without emulation prevention bytes.
fn to_rbsp(ebsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut zeros = 0;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn to_ebsp(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + 8);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn skip_scaling_list(r: &mut Bits, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Some(())
}

fn skip_hrd(r: &mut Bits) -> Option<()> {
    let count = r.ue()? + 1;
    if count > 32 {
        return None;
    }
    r.bits(8)?; // bit_rate_scale, cpb_size_scale
    for _ in 0..count {
        r.ue()?;
        r.ue()?;
        r.bit()?;
    }
    r.bits(20)?; // four 5-bit lengths
    Some(())
}

enum Patch {
    /// It has a `bitstream_restriction` already.
    Has,
    /// The payload with one added.
    Added(Vec<u8>),
}

/// The SPS NAL payload (header byte excluded) with `bitstream_restriction` added, or None
/// if it can't be parsed (then it's left alone).
fn patch_sps(payload: &[u8]) -> Option<Patch> {
    patch(payload, true)
}

fn patch(payload: &[u8], verify: bool) -> Option<Patch> {
    let rbsp = to_rbsp(payload);
    let mut r = Bits { data: &rbsp, pos: 0 };
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags, level
    r.ue()?; // seq_parameter_set_id
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let cycle = r.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                r.se()?;
            }
        }
        _ => {}
    }
    let max_num_ref_frames = r.ue()?;
    if max_num_ref_frames > 16 {
        return None;
    }
    r.bit()?;
    r.ue()?;
    r.ue()?;
    if r.bit()? == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?;
    if r.bit()? == 1 {
        for _ in 0..4 {
            r.ue()?;
        }
    }
    let vui_flag_pos = r.pos;
    let has_vui = r.bit()? == 1;
    if has_vui {
        if r.bit()? == 1 && r.bits(8)? == 255 {
            r.bits(32)?;
        }
        if r.bit()? == 1 {
            r.bit()?;
        }
        if r.bit()? == 1 {
            r.bits(4)?;
            if r.bit()? == 1 {
                r.bits(24)?;
            }
        }
        if r.bit()? == 1 {
            r.ue()?;
            r.ue()?;
        }
        if r.bit()? == 1 {
            r.bits(32)?;
            r.bits(32)?;
            r.bit()?;
        }
        let nal_hrd = r.bit()? == 1;
        if nal_hrd {
            skip_hrd(&mut r)?;
        }
        let vcl_hrd = r.bit()? == 1;
        if vcl_hrd {
            skip_hrd(&mut r)?;
        }
        if nal_hrd || vcl_hrd {
            r.bit()?;
        }
        r.bit()?; // pic_struct_present_flag
        if r.bit()? == 1 {
            return Some(Patch::Has);
        }
    }
    // Everything before the restriction (or before the VUI flag, to add a VUI).
    let keep = if has_vui { r.pos - 1 } else { vui_flag_pos };
    let mut w = Writer::default();
    let mut copy = Bits { data: &rbsp, pos: 0 };
    for _ in 0..keep {
        w.bit(copy.bit()?);
    }
    if !has_vui {
        w.bit(1); // vui_parameters_present_flag
        w.bits(0, 8); // aspect, overscan, signal type, chroma loc, timing, hrd x2, pic_struct
    }
    w.bit(1); // bitstream_restriction_flag
    w.bit(1); // motion_vectors_over_pic_boundaries_flag
    w.ue(2); // max_bytes_per_pic_denom (the default)
    w.ue(1); // max_bits_per_mb_denom (the default)
    w.ue(16); // log2_max_mv_length_horizontal
    w.ue(16); // log2_max_mv_length_vertical
    w.ue(0); // max_num_reorder_frames: output order is decoding order
    w.ue(max_num_ref_frames.max(1)); // max_dec_frame_buffering
    w.bit(1); // rbsp_stop_one_bit
    while w.bits % 8 != 0 {
        w.bit(0);
    }
    let patched = to_ebsp(&w.out);
    // What was written must read back, restriction and all; otherwise keep the original.
    if verify && !matches!(patch(&patched, false)?, Patch::Has) {
        return None;
    }
    Some(Patch::Added(patched))
}

/// Rewrites every SPS in an Annex B access unit that lacks `bitstream_restriction`.
/// `cache` keeps the last rewrite (encoders repeat the same SPS before each IDR).
pub fn add_reorder_limit(au: Vec<u8>, cache: &mut Option<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    // NAL boundaries: (start code length, payload start, payload end).
    let mut nals = Vec::new();
    let mut i = 0;
    let starts: Vec<(usize, usize)> = {
        let mut v = Vec::new();
        while i + 3 <= au.len() {
            if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
                let begin = if i > 0 && au[i - 1] == 0 { i - 1 } else { i };
                v.push((begin, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        v
    };
    for (k, &(begin, start)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(au.len(), |&(b, _)| b);
        nals.push((begin, start, end));
    }
    if !nals.iter().any(|&(_, s, e)| s < e && au[s] & 0x1f == 7) {
        return au;
    }
    let mut out = Vec::with_capacity(au.len() + 16);
    out.extend_from_slice(&au[..nals.first().map_or(au.len(), |n| n.0)]);
    for &(begin, start, end) in &nals {
        let nal = &au[start..end];
        if !nal.is_empty() && nal[0] & 0x1f == 7 {
            let patched = match cache {
                Some((from, to)) if from.as_slice() == nal => Some(to.clone()),
                _ => match patch_sps(&nal[1..]) {
                    Some(Patch::Added(body)) => {
                        let mut to = vec![nal[0]];
                        to.extend(body);
                        *cache = Some((nal.to_vec(), to.clone()));
                        Some(to)
                    }
                    _ => None,
                },
            };
            if let Some(p) = patched {
                out.extend_from_slice(&au[begin..start]);
                out.extend_from_slice(&p);
                continue;
            }
        }
        out.extend_from_slice(&au[begin..end]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NVENC's SPS (constrained baseline, level 5.2, VUI with timing, no restriction), as
    /// nvcudah264enc wrote it at 3440x1440.
    const NVENC_SPS: &[u8] = &[
        0x67, 0x42, 0xc0, 0x34, 0x95, 0x90, 0x03, 0x5c, 0x0b, 0x5b, 0x01, 0x6a, 0x02, 0x02,
        0x02, 0x80, 0x00, 0x01, 0xf4, 0x00, 0x00, 0xea, 0x60, 0x42,
    ];
    const NVENC_PPS: &[u8] = &[0x68, 0xcb, 0x8f, 0x20];

    #[test]
    fn adds_the_restriction_once() {
        let au = [&[0, 0, 0, 1][..], NVENC_SPS, &[0, 0, 0, 1], NVENC_PPS].concat();
        let mut cache = None;
        let out = add_reorder_limit(au.clone(), &mut cache);
        assert_ne!(out, au);
        // The PPS is untouched, after the new SPS.
        assert!(out.ends_with(&[&[0, 0, 0, 1][..], NVENC_PPS].concat()));
        // Patching again changes nothing: it's there now.
        let sps_end = out.len() - 4 - NVENC_PPS.len();
        assert!(matches!(patch_sps(&out[5..sps_end]), Some(Patch::Has)));
        // The cache gives the same answer.
        assert_eq!(add_reorder_limit(au, &mut cache), out);
    }

    /// Patches a whole Annex B file, to check with
    /// `ffmpeg -i OUT -c copy -bsf:v trace_headers -f null -` and by decoding it:
    /// `GLASS_H264_IN=in.h264 GLASS_H264_OUT=out.h264 cargo test -p glass-host patch_file -- --ignored`
    #[test]
    #[ignore]
    fn patch_file() {
        let (Ok(input), Ok(output)) = (std::env::var("GLASS_H264_IN"), std::env::var("GLASS_H264_OUT")) else {
            return;
        };
        let out = add_reorder_limit(std::fs::read(input).unwrap(), &mut None);
        std::fs::write(output, out).unwrap();
    }

    /// Whatever comes in, it doesn't panic, and an SPS it can't read goes out as it came.
    #[test]
    fn leaves_what_it_cant_read() {
        let check = |au: Vec<u8>| {
            let out = add_reorder_limit(au.clone(), &mut None);
            // Anything it did change now reads back with the restriction.
            assert!(out == au || matches!(patch_sps(&out[5..]), Some(Patch::Has)), "{au:x?}");
        };
        for n in 2..NVENC_SPS.len() {
            check([&[0, 0, 0, 1][..], &NVENC_SPS[..n]].concat());
        }
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..20_000 {
            let len = (seed % 40) as usize + 1;
            let mut au = vec![0, 0, 0, 1, 0x67];
            for _ in 0..len {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                // No zero bytes: one NAL, no start codes inside.
                au.push((seed as u8).max(1));
            }
            check(au);
        }
    }

    #[test]
    fn exp_golomb_round_trip() {
        let mut w = Writer::default();
        for v in [0, 1, 2, 16, 255, 70000] {
            w.ue(v);
        }
        w.bits(1, 1);
        let mut r = Bits { data: &w.out, pos: 0 };
        for v in [0, 1, 2, 16, 255, 70000] {
            assert_eq!(r.ue(), Some(v));
        }
    }

    #[test]
    fn emulation_prevention_round_trip() {
        let raw = [0u8, 0, 1, 0, 0, 0, 0, 3, 5, 0, 0];
        assert_eq!(to_rbsp(&to_ebsp(&raw)), raw);
    }
}
