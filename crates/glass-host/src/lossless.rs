//! Lossless refresh, the 4:4:4 half of sharp text. Video is 4:2:0 (4:2:2 at best on
//! macOS), so coloured text and thin lines are never quite exact. Once a screen has been
//! still for a moment, a viewer with sharp text on also gets the picture itself, as a PNG:
//! full colour, every pixel exact. After the first whole picture only the part that changed
//! since the last one is sent. The viewer keeps it on top until the screen really changes
//! (video frames that only refine an unchanged picture carry `FLAG_REFRESH`).
//!
//! On macOS capture switches to BGRA while sharp text is on, so the last captured image is
//! the exact picture. Linux sends the BGRx frames PipeWire captured, unconverted.
//! WebRTC viewers get it over the bridged "uni" data channel; RTP (<video>) viewers don't.
//!
//! Stream layout (host -> client, unidirectional): `STREAM_LOSSLESS`, u32 LE header length,
//! JSON [`Header`], then the PNG.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use crate::control::STREAM_LOSSLESS;

/// A captured picture, 4 bytes per pixel (B, G, R, X), rows packed.
pub struct RawFrame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
    /// Host clock when it was captured (as video `capture_us`).
    pub capture_us: u64,
}

/// x, y, width, height in pixels.
pub type Rect = [usize; 4];

/// How long a screen must stay unchanged before it's sent losslessly.
pub const STILL: Duration = Duration::from_millis(300);
/// At most this often per stream.
pub const MIN_GAP: Duration = Duration::from_millis(500);
/// Pictures bigger than this (compressed) aren't sent: video is good enough then.
pub const MAX_BYTES: usize = 8 << 20;
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Header {
    pub stream: u8,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// The whole picture's size (the part goes at x, y of it).
    pub fw: usize,
    pub fh: usize,
    pub capture_us: u64,
}

/// The smallest rectangle covering every pixel that differs, `None` when they're the same.
/// A different size (or nothing before) means the whole picture.
pub fn dirty_rect(prev: Option<&RawFrame>, cur: &RawFrame) -> Option<Rect> {
    let whole = Some([0, 0, cur.width, cur.height]);
    let Some(prev) = prev else { return whole };
    if prev.width != cur.width || prev.height != cur.height {
        return whole;
    }
    let row = cur.width * 4;
    let rows_differ = |y: usize| prev.bgra[y * row..][..row] != cur.bgra[y * row..][..row];
    let top = (0..cur.height).find(|&y| rows_differ(y))?;
    let bottom = (top..cur.height).rev().find(|&y| rows_differ(y)).unwrap_or(top);
    let (mut left, mut right) = (cur.width, 0);
    for y in top..=bottom {
        let (a, b) = (&prev.bgra[y * row..][..row], &cur.bgra[y * row..][..row]);
        if a == b {
            continue;
        }
        let differs = |x: usize| a[x * 4..x * 4 + 3] != b[x * 4..x * 4 + 3];
        if let Some(l) = (0..left.min(cur.width)).find(|&x| differs(x)) {
            left = l;
        }
        if let Some(r) = (right..cur.width).rev().find(|&x| differs(x)) {
            right = right.max(r);
        }
    }
    if left > right {
        // Only the unused fourth byte changed.
        return None;
    }
    Some([left, top, right - left + 1, bottom - top + 1])
}

/// Same picture (colour bytes only)?
pub fn same(a: &RawFrame, b: &RawFrame) -> bool {
    a.width == b.width && a.height == b.height && dirty_rect(Some(a), b).is_none()
}

/// A PNG (RGB, 8-bit) of `rect` of the frame. Fast compression: this runs per refresh.
pub fn encode_png(frame: &RawFrame, [x, y, w, h]: Rect) -> Result<Vec<u8>> {
    anyhow::ensure!(x + w <= frame.width && y + h <= frame.height && w > 0 && h > 0, "bad rect");
    let mut rgb = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        let line = &frame.bgra[(row * frame.width + x) * 4..][..w * 4];
        for px in line.chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        enc.set_filter(png::FilterType::Sub);
        enc.write_header()?.write_image_data(&rgb)?;
    }
    Ok(out)
}

/// The PNG with a colour profile (iCCP chunk, right after IHDR), so a browser shows a
/// wide-colour (Display P3) picture as captured instead of reading it as sRGB.
pub fn with_icc(png: &[u8], icc: &[u8]) -> Vec<u8> {
    // Signature (8) + IHDR chunk (4 length + 4 type + 13 data + 4 CRC).
    const AFTER_IHDR: usize = 8 + 25;
    if png.len() < AFTER_IHDR || &png[12..16] != b"IHDR" {
        return png.to_vec();
    }
    let mut data = b"Display P3\0\0".to_vec(); // profile name, then compression method 0
    data.extend_from_slice(&zlib_stored(icc));
    let mut chunk = Vec::with_capacity(data.len() + 12);
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let typed_start = chunk.len();
    chunk.extend_from_slice(b"iCCP");
    chunk.extend_from_slice(&data);
    let crc = crc32(&chunk[typed_start..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..AFTER_IHDR]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[AFTER_IHDR..]);
    out
}

/// A zlib stream of `bytes` in stored (uncompressed) blocks: a profile is a few hundred
/// bytes, so compressing it isn't worth a dependency.
fn zlib_stored(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = bytes.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// The bytes of one lossless stream: tag, header length, header, PNG.
pub fn stream_bytes(header: &Header, png: &[u8]) -> Vec<u8> {
    let json = serde_json::to_vec(header).expect("header serializes");
    let mut out = Vec::with_capacity(5 + json.len() + png.len());
    out.push(STREAM_LOSSLESS);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(png);
    out
}

/// One stream's state for one viewer.
#[derive(Default)]
struct Track {
    generation: Option<u64>,
    /// The newest picture seen and when it last changed.
    latest: Option<Arc<RawFrame>>,
    changed: Option<Instant>,
    /// The picture this viewer was last sent (what its copy matches).
    sent: Option<Arc<RawFrame>>,
    sent_at: Option<Instant>,
}

/// What to send for one stream now, if anything; advances `t`.
fn step(t: &mut Track, generation: u64, read: impl FnOnce() -> Option<Arc<RawFrame>>, now: Instant) -> Option<(Arc<RawFrame>, Rect)> {
    if t.generation != Some(generation) {
        t.generation = Some(generation);
        let cur = read()?;
        // A new capture of the same picture (or only the cursor blinking off-screen) isn't a change.
        if !t.latest.as_ref().is_some_and(|l| same(l, &cur)) {
            t.changed = Some(now);
        }
        t.latest = Some(cur);
    }
    let latest = t.latest.clone()?;
    if now.duration_since(t.changed?) < STILL || t.sent_at.is_some_and(|s| now.duration_since(s) < MIN_GAP) {
        return None;
    }
    let rect = dirty_rect(t.sent.as_deref(), &latest)?;
    t.sent = Some(latest.clone());
    t.sent_at = Some(now);
    Some((latest, rect))
}

/// Sends lossless refreshes to one session while `wanted()` says so. Ends with the session.
pub async fn run(
    conn: wtransport::Connection,
    shared: Arc<crate::server::Shared>,
    wanted: impl Fn() -> bool,
    reset: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut tracks: Vec<Track> = Vec::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = conn.closed() => return,
            _ = tick.tick() => {}
        }
        if !wanted() {
            tracks.clear(); // its copy is stale once video takes over again
            continue;
        }
        if reset.swap(false, std::sync::atomic::Ordering::Relaxed) {
            tracks.iter_mut().for_each(|t| t.sent = None);
        }
        let streams = shared.video.streams().len();
        tracks.resize_with(streams, Track::default);
        for (i, t) in tracks.iter_mut().enumerate() {
            let Some(generation) = shared.video.frame_generation(i as u8) else { continue };
            let video = shared.video.clone();
            let now = Instant::now();
            let Some((frame, rect)) = step(t, generation, || video.raw_frame(i as u8), now) else { continue };
            let icc = video.icc(i as u8);
            let encoded = tokio::task::spawn_blocking(move || {
                encode_png(&frame, rect).map(|png| (frame, match icc { Some(icc) => with_icc(&png, &icc), None => png }))
            })
            .await;
            let Ok(Ok((frame, png))) = encoded else { continue };
            if png.len() > MAX_BYTES {
                t.sent = None; // try again whole next time, once it's smaller
                continue;
            }
            let header = Header {
                stream: i as u8,
                x: rect[0],
                y: rect[1],
                w: rect[2],
                h: rect[3],
                fw: frame.width,
                fh: frame.height,
                capture_us: frame.capture_us,
            };
            if let Err(e) = send(&conn, &stream_bytes(&header, &png)).await {
                tracing::debug!("lossless refresh: {e:#}");
                t.sent = None;
            }
        }
    }
}

async fn send(conn: &wtransport::Connection, bytes: &[u8]) -> Result<()> {
    let mut stream = conn.open_uni().await?.await?;
    stream.write_all(bytes).await?;
    stream.finish().await?;
    Ok(())
}

/// A tightly packed BGRA copy of a captured BGRx or RGBx picture (rows `stride` bytes
/// apart), for lossless refresh from capture paths that hand over raw memory (Linux).
pub fn to_bgra(data: &[u8], width: usize, height: usize, stride: usize, rgb: bool) -> Option<Vec<u8>> {
    if stride < width * 4 || data.len() < stride * height.saturating_sub(1) + width * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for row in data.chunks(stride).take(height) {
        for px in row[..width * 4].chunks_exact(4) {
            let (b, r) = if rgb { (px[2], px[0]) } else { (px[0], px[2]) };
            out.extend_from_slice(&[b, px[1], r, 255]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_bgrx_and_rgbx_rows_into_bgra() {
        // 2×2, stride 12 (4 bytes of padding per row).
        let bgrx = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, 7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9];
        assert_eq!(
            to_bgra(&bgrx, 2, 2, 12, false).unwrap(),
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        assert_eq!(&to_bgra(&bgrx, 2, 2, 12, true).unwrap()[..4], &[3, 2, 1, 255]);
        assert!(to_bgra(&bgrx[..10], 2, 2, 12, false).is_none());
    }

    fn frame(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> RawFrame {
        let mut bgra = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let [b, g, r] = f(x, y);
                bgra.extend_from_slice(&[b, g, r, 255]);
            }
        }
        RawFrame { width: w, height: h, bgra, capture_us: 1 }
    }

    #[test]
    fn dirty_rect_covers_exactly_the_change() {
        let a = frame(64, 32, |_, _| [10, 20, 30]);
        let b = frame(64, 32, |x, y| if (5..9).contains(&x) && (3..20).contains(&y) { [1, 2, 3] } else { [10, 20, 30] });
        assert_eq!(dirty_rect(None, &a), Some([0, 0, 64, 32]));
        assert_eq!(dirty_rect(Some(&a), &a), None);
        assert_eq!(dirty_rect(Some(&a), &b), Some([5, 3, 4, 17]));
        assert!(same(&a, &a) && !same(&a, &b));
        let small = frame(32, 32, |_, _| [10, 20, 30]);
        assert_eq!(dirty_rect(Some(&a), &small), Some([0, 0, 32, 32]), "new size: all of it");
        let mut x_only = a.bgra.clone();
        x_only[3] = 0;
        let x_only = RawFrame { bgra: x_only, ..frame(64, 32, |_, _| [10, 20, 30]) };
        assert_eq!(dirty_rect(Some(&a), &x_only), None, "the padding byte isn't colour");
    }

    #[test]
    fn png_round_trips_every_pixel() {
        let f = frame(40, 30, |x, y| [(x * 6) as u8, (y * 8) as u8, ((x + y) * 3) as u8]);
        let png = encode_png(&f, [3, 4, 20, 10]).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (20, 10));
        for (i, px) in buf[..info.buffer_size()].chunks_exact(3).enumerate() {
            let (x, y) = (3 + i % 20, 4 + i / 20);
            let src = &f.bgra[(y * 40 + x) * 4..][..3];
            assert_eq!(px, &[src[2], src[1], src[0]], "pixel {x},{y}");
        }
        assert!(encode_png(&f, [30, 0, 20, 10]).is_err());
    }

    #[test]
    fn a_colour_profile_rides_along_and_the_png_still_reads() {
        let f = RawFrame { width: 32, height: 16, bgra: vec![200; 32 * 16 * 4], capture_us: 0 };
        let png = encode_png(&f, [0, 0, 32, 16]).unwrap();
        let icc: Vec<u8> = (0..600u32).map(|i| (i * 7) as u8).collect();
        let tagged = with_icc(&png, &icc);
        let reader = png::Decoder::new(std::io::Cursor::new(&tagged)).read_info().unwrap();
        assert_eq!(reader.info().icc_profile.as_deref(), Some(&icc[..]));
        assert_eq!(reader.info().width, 32);
    }

    #[test]
    fn waits_for_stillness_then_sends_whole_then_only_changes() {
        let a = Arc::new(frame(16, 16, |_, _| [0, 0, 0]));
        let b = Arc::new(frame(16, 16, |x, _| if x == 7 { [9, 9, 9] } else { [0, 0, 0] }));
        let t0 = Instant::now();
        let mut t = Track::default();
        assert!(step(&mut t, 1, || Some(a.clone()), t0).is_none(), "just changed");
        // Same picture captured again: still counts as still.
        assert!(step(&mut t, 2, || Some(a.clone()), t0 + Duration::from_millis(200)).is_none());
        let (_, rect) = step(&mut t, 2, || None, t0 + STILL).unwrap();
        assert_eq!(rect, [0, 0, 16, 16]);
        assert!(step(&mut t, 2, || None, t0 + Duration::from_secs(5)).is_none(), "already sent");
        let t1 = t0 + Duration::from_secs(6);
        assert!(step(&mut t, 3, || Some(b.clone()), t1).is_none());
        let (_, rect) = step(&mut t, 3, || None, t1 + STILL).unwrap();
        assert_eq!(rect, [7, 0, 1, 16]);
    }

    #[test]
    fn stream_layout() {
        let h = Header { stream: 1, x: 0, y: 0, w: 2, h: 2, fw: 2, fh: 2, capture_us: 5 };
        let bytes = stream_bytes(&h, b"PNG");
        assert_eq!(bytes[0], STREAM_LOSSLESS);
        let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let json: serde_json::Value = serde_json::from_slice(&bytes[5..5 + len]).unwrap();
        assert_eq!(json["fw"], 2);
        assert_eq!(&bytes[5 + len..], b"PNG");
    }
}
