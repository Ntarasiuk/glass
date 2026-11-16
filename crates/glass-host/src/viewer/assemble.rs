//! One display stream's video, from datagrams to whole frames in decode order: fragment
//! reassembly, loss repair (NACKs), keyframe requests and acks for frames that won't be
//! drawn. The same rules as `web/video-stream.js`, so the host sees the native viewer
//! behave like the browser one.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use glass_proto::{FLAG_HEVC, FLAG_KEYFRAME, FLAG_REFRESH, VIDEO_HEADER_LEN, VideoHeader, fletcher16};

const KEYFRAME_RETRY: Duration = Duration::from_millis(250);
/// Doubles until a keyframe arrives: each request adds another large frame to a full queue.
const KEYFRAME_RETRY_MAX: Duration = Duration::from_millis(1000);
/// A gap this old is loss, not reordering (cellular reorders).
const NACK_REORDER: Duration = Duration::from_millis(60);
const NACK_RETRY: Duration = Duration::from_millis(250);
/// How long frames wait for a missing one, at least.
const HOLD_DELTA_MIN: Duration = Duration::from_millis(250);
/// And for a keyframe that's still on its way.
const HOLD_KEY: Duration = Duration::from_millis(4000);
const HOLD_FRAMES: usize = 300;
/// How long a new connection waits before asking for a keyframe.
const FIRST_KEYFRAME: Duration = Duration::from_millis(1000);
/// Fits a datagram.
const NACK_MAX_FRAGMENTS: usize = 500;

/// A whole, checked frame for the decoder.
#[derive(Debug)]
pub struct Frame {
    pub id: u32,
    /// Annex B.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub hevc: bool,
    /// Only refines a picture that hasn't changed (a lossless refresh may stay on top).
    pub refresh: bool,
    pub capture_us: u64,
}

/// What the stream needs done.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Decode this frame (boxed in [`Assembler::take_frames`]).
    Decode,
    /// The frame arrived but won't be drawn: say so, or the host reads the wait as a queue.
    Ack(u32),
    /// Ask the host for a keyframe.
    Keyframe,
    /// Ask for these fragments of a frame again (none: all of it).
    Nack(u32, Vec<u16>),
}

struct Partial {
    parts: Vec<Option<Vec<u8>>>,
    received: usize,
    keyframe: bool,
    hevc: bool,
    refresh: bool,
    capture_us: u64,
    checksum: u16,
    first_at: Instant,
    last_at: Instant,
    max_idx: usize,
    nacked_at: Option<Instant>,
}

pub struct Assembler {
    pending: BTreeMap<u32, Partial>,
    /// Complete frames waiting for an earlier one (with repair).
    ready: BTreeMap<u32, Partial>,
    newest: Option<u32>,
    /// Frames up to this one were given up on.
    floor: Option<u32>,
    last_decoded: Option<u32>,
    gap_since: Option<Instant>,
    waiting_for_keyframe: bool,
    waiting_since: Instant,
    keyframe_retry: Duration,
    last_keyframe_request: Option<Instant>,
    whole_asked: HashMap<u32, Instant>,
    /// The codec of the chain being decoded.
    hevc: Option<bool>,
    /// The host sends lost fragments again when asked (welcome `features.nack`).
    pub repair: bool,
    actions: Vec<Action>,
    frames: Vec<Frame>,
    pub dropped: u64,
}

impl Assembler {
    pub fn new(now: Instant) -> Self {
        Self {
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            newest: None,
            floor: None,
            last_decoded: None,
            gap_since: None,
            waiting_for_keyframe: true,
            waiting_since: now,
            keyframe_retry: KEYFRAME_RETRY,
            last_keyframe_request: None,
            whole_asked: HashMap::new(),
            hevc: None,
            repair: false,
            actions: Vec::new(),
            frames: Vec::new(),
            dropped: 0,
        }
    }

    /// New session: frame ids restart.
    pub fn reset(&mut self, now: Instant) {
        let repair = self.repair;
        *self = Self::new(now);
        self.repair = repair;
    }

    /// What to do since last asked, and the frames to decode (in order).
    pub fn take(&mut self) -> (Vec<Action>, Vec<Frame>) {
        (std::mem::take(&mut self.actions), std::mem::take(&mut self.frames))
    }

    pub fn on_fragment(&mut self, h: &VideoHeader, datagram: &[u8], now: Instant) {
        let id = h.frame_id;
        if self.repair && (self.ready.contains_key(&id) || self.floor.is_some_and(|f| id <= f) || self.last_decoded.is_some_and(|l| id <= l)) {
            return;
        }
        let count = (h.frag_count as usize).max(1);
        let frame = self.pending.entry(id).or_insert_with(|| Partial {
            parts: vec![None; count],
            received: 0,
            keyframe: h.flags & FLAG_KEYFRAME != 0,
            hevc: h.flags & FLAG_HEVC != 0,
            refresh: h.flags & FLAG_REFRESH != 0,
            capture_us: h.capture_us,
            checksum: h.checksum,
            first_at: now,
            last_at: now,
            max_idx: 0,
            nacked_at: None,
        });
        if self.newest.is_none_or(|n| id > n) {
            self.newest = Some(id);
        }
        let idx = h.frag_idx as usize;
        frame.last_at = now;
        frame.max_idx = frame.max_idx.max(idx);
        let Some(slot) = frame.parts.get_mut(idx) else { return };
        if slot.is_some() {
            return;
        }
        *slot = Some(datagram.get(VIDEO_HEADER_LEN..).unwrap_or_default().to_vec());
        frame.received += 1;
        if frame.received != frame.parts.len() {
            return;
        }
        let frame = self.pending.remove(&id).unwrap();
        if !self.repair {
            self.on_frame(id, frame, now);
            return;
        }
        self.ready.insert(id, frame);
        self.advance(now);
    }

    /// Decodes what's complete, in order. A complete keyframe restarts the chain.
    fn advance(&mut self, now: Instant) {
        loop {
            let next = self.last_decoded.map(|l| l.wrapping_add(1));
            let id = match next {
                Some(n) if !self.waiting_for_keyframe && self.ready.contains_key(&n) => Some(n),
                _ => self.ready.iter().find(|(_, f)| f.keyframe).map(|(k, _)| *k),
            };
            let Some(id) = id else { return };
            let frame = self.ready.remove(&id).unwrap();
            if frame.keyframe {
                self.forget_before(id);
            }
            self.gap_since = None;
            self.decode(id, frame, now);
        }
    }

    fn forget_before(&mut self, id: u32) {
        self.pending.retain(|k, _| *k >= id);
        self.ready.retain(|k, _| *k >= id);
        if let Some(prev) = id.checked_sub(1) {
            self.floor = Some(self.floor.map_or(prev, |f| f.max(prev)));
        }
    }

    /// Gives up on what's missing: only a keyframe gets the picture going again.
    fn give_up(&mut self, now: Instant) {
        if let Some(n) = self.newest {
            self.forget_before(n.wrapping_add(1));
        }
        self.gap_since = None;
        self.dropped += 1;
        self.request_keyframe(now);
    }

    /// The host no longer has a frame this viewer asked for.
    pub fn frame_gone(&mut self, id: u32, now: Instant) {
        if self.waiting_for_keyframe {
            if self.pending.get(&id).is_some_and(|f| f.keyframe) {
                self.pending.remove(&id);
                self.request_keyframe(now);
            }
            return;
        }
        if self.last_decoded.map(|l| l.wrapping_add(1)) == Some(id) {
            self.give_up(now);
        }
    }

    /// Asks again for what's missing; called every few ms.
    pub fn repair_tick(&mut self, rtt: Duration, now: Instant) {
        if self.waiting_for_keyframe && self.pending.is_empty() && self.ready.is_empty() {
            // Nothing at all yet: the encoder may have dropped it, and nothing else would ask.
            if now - self.waiting_since > FIRST_KEYFRAME {
                self.request_keyframe(now);
            }
            return;
        }
        if !self.repair {
            return;
        }
        let newest = self.newest;
        let ask = |id: u32, frame: &mut Partial, actions: &mut Vec<Action>| {
            if frame.nacked_at.is_some_and(|t| now - t < NACK_RETRY) {
                return;
            }
            let newer = newest.is_some_and(|n| n > id);
            let quiet = now - frame.last_at;
            let missing: Vec<u16> = (0..frame.parts.len())
                .filter(|&i| frame.parts[i].is_none())
                .filter(|&i| if i < frame.max_idx { quiet > NACK_REORDER } else { newer || quiet > NACK_RETRY })
                .take(NACK_MAX_FRAGMENTS)
                .map(|i| i as u16)
                .collect();
            if missing.is_empty() {
                return;
            }
            frame.nacked_at = Some(now);
            actions.push(Action::Nack(id, missing));
        };

        if self.waiting_for_keyframe {
            let key = self.pending.iter().find(|(_, f)| f.keyframe).map(|(k, _)| *k);
            match key {
                Some(key) => {
                    let frame = self.pending.get_mut(&key).unwrap();
                    if now - frame.first_at > HOLD_KEY {
                        self.pending.remove(&key);
                        self.request_keyframe(now);
                    } else {
                        ask(key, frame, &mut self.actions);
                    }
                }
                // Frames but no keyframe in sight (the first ones may have been lost altogether).
                None => self.request_keyframe(now),
            }
            return;
        }

        let Some(next) = self.last_decoded.map(|l| l.wrapping_add(1)) else { return };
        if self.ready.contains_key(&next) || self.newest.is_none_or(|n| n < next) {
            self.gap_since = None;
            return;
        }
        let gap_since = *self.gap_since.get_or_insert(now);
        if now - gap_since > HOLD_DELTA_MIN.max(rtt * 3) || self.ready.len() > HOLD_FRAMES {
            self.give_up(now);
            return;
        }
        for (id, frame) in self.pending.iter_mut() {
            ask(*id, frame, &mut self.actions);
        }
        // Frames with nothing at all here yet: ask for all of each.
        let newest = self.newest.unwrap_or(next);
        let mut id = next;
        while id < newest && id < next.saturating_add(32) {
            if !self.pending.contains_key(&id) && !self.ready.contains_key(&id) && self.whole_asked.get(&id).is_none_or(|t| now - *t >= NACK_RETRY) {
                self.whole_asked.insert(id, now);
                self.actions.push(Action::Nack(id, Vec::new()));
            }
            id += 1;
        }
        if self.whole_asked.len() > 64 {
            self.whole_asked.clear();
        }
    }

    fn on_frame(&mut self, id: u32, frame: Partial, now: Instant) {
        // Anything older and still incomplete is lost.
        self.pending.retain(|k, _| *k > id);
        let contiguous = self.last_decoded.map(|l| l.wrapping_add(1)) == Some(id);
        // A delta in the other codec (the host just switched) can't follow this decoder.
        let same_codec = self.hevc == Some(frame.hevc);
        if !frame.keyframe && (self.waiting_for_keyframe || !contiguous || !same_codec) {
            self.dropped += 1;
            self.request_keyframe(now);
            self.actions.push(Action::Ack(id));
            return;
        }
        self.decode(id, frame, now);
    }

    fn decode(&mut self, id: u32, frame: Partial, now: Instant) {
        let data: Vec<u8> = frame.parts.into_iter().flatten().flatten().collect();
        if fletcher16(&data) != frame.checksum {
            // Corrupted in transit or reassembly: never feed it to the decoder.
            tracing::warn!("viewer: frame {id} checksum mismatch ({} bytes)", data.len());
            self.dropped += 1;
            self.request_keyframe(now);
            self.actions.push(Action::Ack(id));
            return;
        }
        self.waiting_for_keyframe = false;
        if frame.keyframe {
            self.keyframe_retry = KEYFRAME_RETRY;
            self.hevc = Some(frame.hevc);
        }
        self.last_decoded = Some(id);
        self.actions.push(Action::Decode);
        self.frames.push(Frame { id, data, keyframe: frame.keyframe, hevc: frame.hevc, refresh: frame.refresh, capture_us: frame.capture_us });
    }

    /// Waits for a keyframe, asking the host (no more often than the backoff allows).
    /// Also for the decoder, when it couldn't use a frame.
    pub fn request_keyframe(&mut self, now: Instant) {
        self.waiting_for_keyframe = true;
        if self.last_keyframe_request.is_some_and(|t| now - t < self.keyframe_retry) {
            return;
        }
        self.last_keyframe_request = Some(now);
        self.keyframe_retry = (self.keyframe_retry * 2).min(KEYFRAME_RETRY_MAX);
        self.actions.push(Action::Keyframe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glass_proto::fragment_with_flags;

    fn feed(a: &mut Assembler, dgrams: &[Vec<u8>], now: Instant) {
        for d in dgrams {
            a.on_fragment(&VideoHeader::read(d).unwrap(), d, now);
        }
    }

    fn frame(id: u32, key: bool, len: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let data: Vec<u8> = (0..len).map(|i| (i as u32 ^ id) as u8).collect();
        let d = fragment_with_flags(&data, 0, id, if key { FLAG_KEYFRAME } else { 0 }, id as u64, 1200);
        (data, d)
    }

    #[test]
    fn whole_frames_come_out_in_order() {
        let now = Instant::now();
        let mut a = Assembler::new(now);
        let (k, kd) = frame(0, true, 5000);
        let (d, dd) = frame(1, false, 3000);
        feed(&mut a, &kd, now);
        feed(&mut a, &dd, now);
        let (actions, frames) = a.take();
        assert_eq!(actions, [Action::Decode, Action::Decode]);
        assert_eq!(frames.iter().map(|f| (f.id, f.keyframe)).collect::<Vec<_>>(), [(0, true), (1, false)]);
        assert_eq!((frames[0].data.clone(), frames[1].data.clone()), (k, d));
    }

    #[test]
    fn a_delta_before_any_keyframe_asks_for_one() {
        let now = Instant::now();
        let mut a = Assembler::new(now);
        feed(&mut a, &frame(5, false, 100).1, now);
        let (actions, frames) = a.take();
        assert!(frames.is_empty());
        assert_eq!(actions, [Action::Keyframe, Action::Ack(5)]);
    }

    #[test]
    fn repair_waits_for_the_missing_frame_then_nacks_it() {
        let t0 = Instant::now();
        let mut a = Assembler::new(t0);
        a.repair = true;
        feed(&mut a, &frame(0, true, 100).1, t0);
        let (_, d1) = frame(1, false, 3000);
        feed(&mut a, &d1[1..], t0); // first fragment lost
        feed(&mut a, &frame(2, false, 100).1, t0);
        assert_eq!(a.take().1.len(), 1); // only the keyframe: 2 waits for 1
        a.repair_tick(Duration::from_millis(20), t0 + Duration::from_millis(100));
        let (actions, _) = a.take();
        assert_eq!(actions, [Action::Nack(1, vec![0])]);
        feed(&mut a, &d1[..1], t0 + Duration::from_millis(120));
        let ids: Vec<u32> = a.take().1.iter().map(|f| f.id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn a_corrupt_frame_never_reaches_the_decoder() {
        let now = Instant::now();
        let mut a = Assembler::new(now);
        let (_, mut kd) = frame(0, true, 100);
        kd[0][VIDEO_HEADER_LEN] ^= 0x01;
        feed(&mut a, &kd, now);
        let (actions, frames) = a.take();
        assert!(frames.is_empty());
        assert_eq!(actions, [Action::Keyframe, Action::Ack(0)]);
    }

    #[test]
    fn keyframe_requests_back_off() {
        let t0 = Instant::now();
        let mut a = Assembler::new(t0);
        a.request_keyframe(t0);
        a.request_keyframe(t0 + Duration::from_millis(100));
        a.request_keyframe(t0 + Duration::from_millis(400));
        a.request_keyframe(t0 + Duration::from_millis(600));
        assert_eq!(a.take().0, [Action::Keyframe, Action::Keyframe]);
    }
}
