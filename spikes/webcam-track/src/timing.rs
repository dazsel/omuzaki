//! Latency statistics and process CPU time, for the benchmark numbers in the report.

use std::time::Instant;

/// Summary of a set of durations, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Summary {
    /// How many samples.
    pub n: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// Slowest sample.
    pub max: f64,
}

impl Summary {
    /// Summarises `seconds` (converted to milliseconds); `None` for an empty set.
    pub fn of_seconds(seconds: &[f64]) -> Option<Self> {
        if seconds.is_empty() {
            return None;
        }
        let mut ms: Vec<f64> = seconds.iter().map(|s| s * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        #[allow(clippy::cast_precision_loss)] // sample counts are far below f64's 53 bits
        let mean = ms.iter().sum::<f64>() / ms.len() as f64;
        Some(Self { n: ms.len(), mean, p50: percentile(&ms, 0.50), p95: percentile(&ms, 0.95), max: ms[ms.len() - 1] })
    }
}

/// Nearest-rank percentile of an ascending, non-empty slice.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // rank is within the slice
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Measures the CPU time this process (all threads) uses while the meter runs.
pub struct CpuMeter {
    wall: Instant,
    cpu_at_start: f64,
}

/// Linux reports process CPU time in clock ticks of 1/100 s (`USER_HZ`, fixed at 100 on every Linux ABI we run on).
const TICKS_PER_SECOND: f64 = 100.0;

/// Total user + system CPU seconds of this process, from `/proc/self/stat`, or `None` where that is unavailable.
fn process_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The command name (field 2) may contain spaces; everything after its closing parenthesis is plain fields,
    // starting at field 3, so utime and stime (fields 14 and 15) are at offsets 11 and 12.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let utime: f64 = fields.nth(11)?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    Some((utime + stime) / TICKS_PER_SECOND)
}

impl CpuMeter {
    /// Starts measuring.
    pub fn start() -> Self {
        Self { wall: Instant::now(), cpu_at_start: process_cpu_seconds().unwrap_or(0.0) }
    }

    /// CPU use since [`CpuMeter::start`] as a percentage of one core (200 means two cores fully busy), and the
    /// wall-clock seconds measured. `None` where `/proc` is unavailable.
    pub fn finish(&self) -> Option<(f64, f64)> {
        let wall = self.wall.elapsed().as_secs_f64();
        let cpu = process_cpu_seconds()? - self.cpu_at_start;
        Some((100.0 * cpu / wall, wall))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_uses_nearest_rank_percentiles_in_milliseconds() {
        let seconds: Vec<f64> = (1..=100).map(|i| f64::from(i) / 1000.0).collect();
        let s = Summary::of_seconds(&seconds).unwrap();
        assert_eq!((s.n, s.p50, s.p95, s.max), (100, 50.0, 95.0, 100.0));
        assert!((s.mean - 50.5).abs() < 1e-9);
        assert!(Summary::of_seconds(&[]).is_none());
        assert_eq!(Summary::of_seconds(&[0.002]).unwrap().p95, 2.0);
    }

    #[test]
    fn cpu_meter_sees_busy_work() {
        let meter = CpuMeter::start();
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed().as_millis() < 120 {
            x = x.wrapping_add(std::hint::black_box(1));
        }
        std::hint::black_box(x);
        if let Some((percent, wall)) = meter.finish() {
            assert!(wall >= 0.12 && percent > 50.0, "{percent}% over {wall}s");
        }
    }
}
