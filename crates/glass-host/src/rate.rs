//! Adaptive bitrate, AIMD like TCP: back off hard when any client sees loss or a latency
//! climb, then creep back up while the network is clean — but only while the stream
//! actually uses what it has (a still screen proves nothing about the link), and only
//! cautiously near the rate where the link last choked.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Fraction kept on congestion.
const DECREASE: f64 = 0.7;
/// Increase per clean report, as a fraction of the maximum.
const INCREASE: f64 = 0.1;
/// Near (and past) the rate where the link last choked, probe in steps this size instead.
const PROBE: f64 = 0.02;
/// "Near" the last choke point: this fraction of it and above.
const NEAR_CEILING: f64 = 0.85;
/// Raise only when the stream used at least this share of its allowance last second.
const USED_FOR_INCREASE: f64 = 0.4;
/// The remembered choke point is forgotten after this long without congestion.
const CEILING_MEMORY: Duration = Duration::from_secs(60);
/// Don't increase this soon after a decrease (let queues drain).
const HOLD_AFTER_DECREASE: Duration = Duration::from_secs(2);
/// Latency this far above the recent best counts as congestion (queues building up).
const LATENCY_SLACK_MS: f64 = 40.0;
/// Recent latency samples used to estimate the uncongested baseline.
const BASELINE_WINDOW: usize = 30;
/// Below this the picture becomes unusable; hold here instead.
const MIN_FRACTION: f64 = 0.08;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Report {
    /// Frames the client couldn't decode in the last second (lost or incomplete).
    pub dropped: u32,
    /// Frames decoded in the last second.
    pub frames: u32,
    /// Average capture-to-display latency in the last second, if measured.
    pub latency_ms: Option<f64>,
    /// Video bits received in the last second (older clients don't send it).
    #[serde(default)]
    pub received_bps: Option<f64>,
}

pub struct RateControl {
    max: u32,
    target: u32,
    last_decrease: Option<Instant>,
    latencies: VecDeque<f64>,
    /// The target when congestion was last seen: where the link chokes.
    ceiling: Option<u32>,
}

impl RateControl {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            target: max,
            last_decrease: None,
            latencies: VecDeque::new(),
            ceiling: None,
        }
    }

    pub fn target(&self) -> u32 {
        self.target
    }

    /// Feeds one client report; returns the new target if it changed meaningfully.
    pub fn report(&mut self, r: Report, now: Instant) -> Option<u32> {
        let baseline = self.latencies.iter().copied().fold(f64::INFINITY, f64::min);
        if let Some(l) = r.latency_ms.filter(|l| l.is_finite() && *l > 0.0) {
            self.latencies.push_back(l);
            if self.latencies.len() > BASELINE_WINDOW {
                self.latencies.pop_front();
            }
        }
        let latency_climbing = r
            .latency_ms
            .is_some_and(|l| baseline.is_finite() && l > baseline + LATENCY_SLACK_MS);
        let congested = r.dropped > 0 || latency_climbing;
        if self
            .last_decrease
            .is_some_and(|t| now.duration_since(t) > CEILING_MEMORY)
        {
            self.ceiling = None;
        }
        // Clients that don't report usage are taken to use it all (the old behaviour).
        let used = r
            .received_bps
            .is_none_or(|bps| bps >= self.target as f64 * USED_FOR_INCREASE);

        let min = (self.max as f64 * MIN_FRACTION) as u32;
        let next = if congested {
            self.last_decrease = Some(now);
            self.ceiling = Some(self.target);
            ((self.target as f64 * DECREASE) as u32).max(min)
        } else if self
            .last_decrease
            .is_none_or(|t| now.duration_since(t) >= HOLD_AFTER_DECREASE)
            && r.frames > 0
            && used
        {
            let near = self.ceiling.map(|c| (c as f64 * NEAR_CEILING) as u32);
            match near {
                // Probe gently once close to where it choked.
                Some(near) if self.target >= near => self.target + (self.max as f64 * PROBE) as u32,
                // Recover quickly, but stop short of the choke point.
                Some(near) => (self.target + (self.max as f64 * INCREASE) as u32).min(near),
                None => self.target + (self.max as f64 * INCREASE) as u32,
            }
            .min(self.max)
        } else {
            self.target
        };
        // Back off on any congestion; ignore tiny increases so encoders aren't poked
        // every second near the cap.
        let changed = next < self.target
            || next - self.target >= (self.max as f64 * PROBE) as u32
            || (next == self.max && next != self.target);
        if changed {
            self.target = next;
            Some(next)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = 30_000_000;
    fn clean() -> Report {
        Report {
            dropped: 0,
            frames: 60,
            latency_ms: Some(20.0),
            received_bps: None,
        }
    }
    fn lossy() -> Report {
        Report {
            dropped: 3,
            ..clean()
        }
    }

    #[test]
    fn backs_off_on_loss_and_recovers() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        assert_eq!(rc.report(clean(), t0), None, "already at max");
        assert_eq!(rc.report(lossy(), t0), Some(21_000_000));
        // Held right after a decrease.
        assert_eq!(rc.report(clean(), t0 + Duration::from_secs(1)), None);
        assert_eq!(
            rc.report(clean(), t0 + Duration::from_secs(2)),
            Some(24_000_000)
        );
        for i in 3..40 {
            rc.report(clean(), t0 + Duration::from_secs(i));
        }
        assert_eq!(rc.target(), MAX);
    }

    #[test]
    fn approaches_the_last_choke_point_cautiously() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        rc.report(lossy(), t0); // chokes at 30 → 21 Mbps
        let targets: Vec<u32> = (2..6)
            .map(|i| {
                rc.report(clean(), t0 + Duration::from_secs(i));
                rc.target()
            })
            .collect();
        // Quickly to 85% of 30 (25.5), then +0.6 Mbps steps.
        assert_eq!(targets, [24_000_000, 25_500_000, 26_100_000, 26_700_000]);
    }

    #[test]
    fn a_still_screen_does_not_raise_the_rate() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        for i in 0..6 {
            rc.report(lossy(), t0 + Duration::from_secs(i));
        }
        let low = rc.target();
        // A calm screen sends little: no loss, but no evidence the link can take more.
        let idle = Report {
            received_bps: Some(100_000.0),
            ..clean()
        };
        for i in 10..40 {
            rc.report(idle, t0 + Duration::from_secs(i));
        }
        assert_eq!(rc.target(), low);
        // Using it is what earns more.
        let busy = Report {
            received_bps: Some(low as f64 * 0.9),
            ..clean()
        };
        rc.report(busy, t0 + Duration::from_secs(41));
        assert!(rc.target() > low);
    }

    #[test]
    fn rising_latency_counts_as_congestion() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        for i in 0..5 {
            rc.report(clean(), t0 + Duration::from_secs(i));
        }
        let slow = Report {
            latency_ms: Some(90.0),
            ..clean()
        };
        assert_eq!(
            rc.report(slow, t0 + Duration::from_secs(6)),
            Some(21_000_000)
        );
    }

    #[test]
    fn never_drops_below_the_floor() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        for _ in 0..50 {
            rc.report(lossy(), t0);
        }
        assert_eq!(rc.target(), (MAX as f64 * MIN_FRACTION) as u32);
    }
}
