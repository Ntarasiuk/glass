//! Adaptive bitrate, AIMD like TCP: back off hard when any client sees loss or a latency
//! climb, then creep back up while the network is clean.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Fraction kept on congestion.
const DECREASE: f64 = 0.7;
/// Increase per clean report, as a fraction of the maximum.
const INCREASE: f64 = 0.1;
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
}

pub struct RateControl {
    max: u32,
    target: u32,
    last_decrease: Option<Instant>,
    latencies: VecDeque<f64>,
}

impl RateControl {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            target: max,
            last_decrease: None,
            latencies: VecDeque::new(),
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

        let min = (self.max as f64 * MIN_FRACTION) as u32;
        let next = if congested {
            self.last_decrease = Some(now);
            ((self.target as f64 * DECREASE) as u32).max(min)
        } else if self
            .last_decrease
            .is_none_or(|t| now.duration_since(t) >= HOLD_AFTER_DECREASE)
            && r.frames > 0
        {
            (self.target + (self.max as f64 * INCREASE) as u32).min(self.max)
        } else {
            self.target
        };
        // Back off on any congestion; ignore increases under 5% so encoders aren't poked
        // every second near the cap.
        let changed =
            next < self.target || next as f64 - self.target as f64 >= self.target as f64 * 0.05;
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
        }
    }

    #[test]
    fn backs_off_on_loss_and_recovers() {
        let mut rc = RateControl::new(MAX);
        let t0 = Instant::now();
        assert_eq!(rc.report(clean(), t0), None, "already at max");
        assert_eq!(
            rc.report(
                Report {
                    dropped: 3,
                    ..clean()
                },
                t0
            ),
            Some(21_000_000)
        );
        // Held right after a decrease.
        assert_eq!(rc.report(clean(), t0 + Duration::from_secs(1)), None);
        assert_eq!(
            rc.report(clean(), t0 + Duration::from_secs(2)),
            Some(24_000_000)
        );
        for i in 3..10 {
            rc.report(clean(), t0 + Duration::from_secs(i));
        }
        assert_eq!(rc.target(), MAX);
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
            rc.report(
                Report {
                    dropped: 1,
                    ..clean()
                },
                t0,
            );
        }
        assert_eq!(rc.target(), (MAX as f64 * MIN_FRACTION) as u32);
    }
}
