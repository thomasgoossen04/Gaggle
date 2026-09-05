//! Rolling download / upload throughput history — the data behind the GUI's
//! Stats tab. Kept always-on (not gated on any tab being open) so it is
//! independently testable headless, per this repo's "logic in app-state,
//! pixels in gui" split.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

/// One throughput reading, taken on [`Manager`](crate::App)'s 2 s tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedSample {
    /// Wall-clock time the reading was taken.
    pub at: SystemTime,
    /// Aggregate download rate across every active transfer, bytes/sec. Always
    /// `0` for a remote accelerator's row — it only ever serves outward.
    pub down_bps: u64,
    /// Aggregate rate this node (or a remote accelerator) is serving to
    /// others, bytes/sec.
    pub up_bps: u64,
}

/// A capped ring of [`SpeedSample`]s. The default capacity holds a little over
/// one hour at the manager's 2 s sampling cadence — the widest window the
/// Stats tab offers.
#[derive(Debug, Clone)]
pub struct SpeedHistory {
    samples: VecDeque<SpeedSample>,
    cap: usize,
}

impl Default for SpeedHistory {
    fn default() -> Self {
        // 1 h at one sample / 2 s, plus a minute of slack.
        Self::with_capacity(60 * 60 / 2 + 30)
    }
}

impl SpeedHistory {
    pub fn with_capacity(cap: usize) -> Self {
        Self { samples: VecDeque::new(), cap: cap.max(1) }
    }

    /// Append a sample, evicting the oldest once at capacity.
    pub fn push(&mut self, sample: SpeedSample) {
        if self.samples.len() >= self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Every retained sample, oldest first.
    pub fn snapshot(&self) -> Vec<SpeedSample> {
        self.samples.iter().copied().collect()
    }

    /// The samples taken within `window` of the newest one.
    pub fn within(&self, window: Duration) -> Vec<SpeedSample> {
        let Some(newest) = self.samples.back().map(|s| s.at) else {
            return Vec::new();
        };
        let cutoff = newest.checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);
        self.samples.iter().filter(|s| s.at >= cutoff).copied().collect()
    }
}

/// Turn two readings of a monotonically-growing cumulative byte counter into a
/// bytes/sec rate over the elapsed wall-clock gap. `None` when no time has
/// passed or the counter went backwards (a daemon restart) — the caller keeps
/// its previous value rather than plotting a spike or a negative rate.
pub fn rate_from_cumulative(
    prev: (Instant, u64),
    now: Instant,
    total: u64,
) -> Option<u64> {
    let dt = now.checked_duration_since(prev.0)?.as_secs_f64();
    if dt <= 0.0 || total < prev.1 {
        return None;
    }
    Some(((total - prev.1) as f64 / dt) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_caps_and_keeps_the_newest() {
        let mut h = SpeedHistory::with_capacity(3);
        for i in 0..5 {
            h.push(SpeedSample {
                at: SystemTime::UNIX_EPOCH + Duration::from_secs(i),
                down_bps: i * 10,
                up_bps: 0,
            });
        }
        let s = h.snapshot();
        assert_eq!(s.len(), 3);
        assert_eq!(s.first().unwrap().down_bps, 20); // 0 and 10 evicted
        assert_eq!(s.last().unwrap().down_bps, 40);
    }

    #[test]
    fn within_slices_relative_to_the_newest_sample() {
        let mut h = SpeedHistory::with_capacity(100);
        for i in 0..10 {
            h.push(SpeedSample {
                at: SystemTime::UNIX_EPOCH + Duration::from_secs(i * 10),
                down_bps: i,
                up_bps: 0,
            });
        }
        // Newest is at t=90s; a 25 s window keeps t=70, 80, 90.
        let got = h.within(Duration::from_secs(25));
        assert_eq!(got.iter().map(|s| s.down_bps).collect::<Vec<_>>(), vec![7, 8, 9]);
    }

    #[test]
    fn rate_from_cumulative_handles_time_and_counter_resets() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(rate_from_cumulative((t0, 1_000), t1, 5_000), Some(2_000));
        // Counter went backwards (restart) → no rate.
        assert_eq!(rate_from_cumulative((t0, 5_000), t1, 1_000), None);
        // No time elapsed → no rate.
        assert_eq!(rate_from_cumulative((t0, 1_000), t0, 5_000), None);
    }
}
