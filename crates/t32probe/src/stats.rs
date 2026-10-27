// SPDX-License-Identifier: Apache-2.0
//! Percentiles and rates. Pure.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
}

impl Stats {
    /// Nearest rank: `p(q)` is the sample at rank `ceil(q * n)` of the sorted samples.
    pub fn from_samples(samples: &mut [Duration]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        let rank = |q: f64| samples[((q * n as f64).ceil() as usize).clamp(1, n) - 1];
        Some(Self { n, p50: rank(0.50), p95: rank(0.95), max: samples[n - 1] })
    }

    /// `0.31/0.52/1.90ms`: p50/p95/max in one token.
    pub fn compact(&self) -> String {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        format!("{:.2}/{:.2}/{:.2}ms", ms(self.p50), ms(self.p95), ms(self.max))
    }
}

/// 10^6 bytes per second; `None` below the clock's useful resolution.
pub fn mb_per_sec(bytes: u64, elapsed: Duration) -> Option<f64> {
    (elapsed >= Duration::from_micros(100)).then(|| bytes as f64 / 1e6 / elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nearest_rank_percentiles() {
        let mut v: Vec<Duration> = (1..=100).map(ms).collect();
        let s = Stats::from_samples(&mut v).unwrap();
        assert_eq!((s.p50, s.p95, s.max), (ms(50), ms(95), ms(100)));
        assert_eq!(s.compact(), "50.00/95.00/100.00ms");
        let s = Stats::from_samples(&mut [ms(7)]).unwrap();
        assert_eq!((s.p50, s.p95, s.max), (ms(7), ms(7), ms(7)));
        assert!(Stats::from_samples(&mut []).is_none());
    }

    #[test]
    fn rate_needs_a_measurable_interval() {
        assert!(mb_per_sec(1_000_000, Duration::from_micros(10)).is_none());
        assert_eq!(mb_per_sec(1_000_000, Duration::from_secs(1)), Some(1.0));
    }
}
