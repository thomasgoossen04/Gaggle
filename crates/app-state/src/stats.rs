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

/// Resample an irregular `SpeedSample` series (readings land ~every 2 s) onto
/// `n` evenly-spaced points spanning `window` back from `now`, monotone-cubic
/// interpolating between the bracketing samples. Points newer than the last
/// real sample hold that sample's value; points older than the first hold the
/// first. Returns `(seconds_before_now, value)` pairs, **oldest first**.
///
/// The Stats graphs redraw every ~200 ms but the underlying history only grows
/// every ~2 s, so drawing the raw samples makes the line freeze then jump once
/// per reading. Feeding this a fresh `now` each frame instead gives a fixed
/// point count with a curve that advances smoothly between readings — and,
/// because the count is fixed regardless of `window`, the categorical x-axis
/// can't collapse repeated labels onto one position when the window widens.
pub fn resample(
    samples: &[SpeedSample],
    now: SystemTime,
    window: Duration,
    n: usize,
    value: impl Fn(&SpeedSample) -> f64,
) -> Vec<(f64, f64)> {
    if samples.is_empty() || n == 0 {
        return Vec::new();
    }
    let t0 = samples[0].at;
    // Sample positions/values on a common f64 timeline (seconds since t0).
    let xs: Vec<f64> = samples
        .iter()
        .map(|s| s.at.duration_since(t0).unwrap_or_default().as_secs_f64())
        .collect();
    let ys: Vec<f64> = samples.iter().map(&value).collect();
    let tangents = monotone_tangents(&xs, &ys);

    let win = window.as_secs_f64().max(1.0);
    let now_x = now.duration_since(t0).unwrap_or_default().as_secs_f64();
    let last_x = *xs.last().unwrap();
    let span = (n.saturating_sub(1)).max(1) as f64;

    let mut out = Vec::with_capacity(n);
    let mut j = 0usize;
    for k in 0..n {
        let ago = win * (span - k as f64) / span; // win .. 0
        let x = now_x - ago;
        let y = if x <= xs[0] {
            ys[0]
        } else if x >= last_x {
            *ys.last().unwrap()
        } else {
            while j + 1 < xs.len() && xs[j + 1] < x {
                j += 1;
            }
            hermite(xs[j], ys[j], tangents[j], xs[j + 1], ys[j + 1], tangents[j + 1], x)
        };
        out.push((ago, y.max(0.0)));
    }
    out
}

/// Fritsch–Carlson tangents for monotone cubic Hermite interpolation — the
/// smoothed curve never overshoots into a negative or impossibly-high rate
/// between two real samples.
fn monotone_tangents(xs: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = xs.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let secants: Vec<f64> = (0..n - 1)
        .map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]).max(1e-9))
        .collect();
    let mut m = vec![0.0; n];
    m[0] = secants[0];
    m[n - 1] = secants[n - 2];
    for i in 1..n - 1 {
        m[i] = if secants[i - 1] * secants[i] <= 0.0 {
            0.0
        } else {
            (secants[i - 1] + secants[i]) / 2.0
        };
    }
    for i in 0..n - 1 {
        if secants[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / secants[i];
        let b = m[i + 1] / secants[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            m[i] = tau * a * secants[i];
            m[i + 1] = tau * b * secants[i];
        }
    }
    m
}

/// One cubic Hermite segment evaluated at `x` (with `x0 <= x <= x1`).
fn hermite(x0: f64, y0: f64, m0: f64, x1: f64, y1: f64, m1: f64, x: f64) -> f64 {
    let h = (x1 - x0).max(1e-9);
    let t = ((x - x0) / h).clamp(0.0, 1.0);
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y0
        + (t3 - 2.0 * t2 + t) * h * m0
        + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * h * m1
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

    fn series(rates: &[u64], step: Duration) -> Vec<SpeedSample> {
        rates
            .iter()
            .enumerate()
            .map(|(i, &r)| SpeedSample {
                at: SystemTime::UNIX_EPOCH + step * i as u32,
                down_bps: r,
                up_bps: r,
            })
            .collect()
    }

    #[test]
    fn resample_gives_a_fixed_count_and_stays_within_bounds() {
        let s = series(&[0, 1_000, 4_000, 2_000, 500], Duration::from_secs(2));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(8);
        let grid = resample(&s, now, Duration::from_secs(8), 90, |x| x.down_bps as f64);

        assert_eq!(grid.len(), 90);
        // Oldest first, ago descends from the window edge to 0.
        assert!((grid[0].0 - 8.0).abs() < 1e-6);
        assert!(grid.last().unwrap().0.abs() < 1e-6);
        // Monotone interpolation never overshoots the sample range.
        assert!(grid.iter().all(|&(_, v)| (0.0..=4_000.0).contains(&v)));
        // The right edge holds the last real reading.
        assert!((grid.last().unwrap().1 - 500.0).abs() < 1e-6);
    }

    #[test]
    fn resample_advances_between_frames() {
        let s = series(&[0, 1_000, 4_000, 2_000], Duration::from_secs(2));
        let w = Duration::from_secs(6);
        let a = resample(&s, SystemTime::UNIX_EPOCH + Duration::from_secs(6), w, 60, |x| {
            x.down_bps as f64
        });
        // Same samples, a fresh `now` 400 ms later: the curve must shift, so a
        // mid-grid value changes even though no new sample arrived.
        let b = resample(
            &s,
            SystemTime::UNIX_EPOCH + Duration::from_millis(6_400),
            w,
            60,
            |x| x.down_bps as f64,
        );
        assert_ne!(a[30].1, b[30].1);
    }

    #[test]
    fn resample_handles_degenerate_input() {
        assert!(resample(&[], SystemTime::UNIX_EPOCH, Duration::from_secs(60), 90, |x| {
            x.up_bps as f64
        })
        .is_empty());
        let one = series(&[1_234], Duration::from_secs(2));
        let grid = resample(&one, SystemTime::UNIX_EPOCH, Duration::from_secs(60), 10, |x| {
            x.up_bps as f64
        });
        assert_eq!(grid.len(), 10);
        assert!(grid.iter().all(|&(_, v)| (v - 1_234.0).abs() < 1e-6));
    }
}
