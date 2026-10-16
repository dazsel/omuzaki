//! The percentile of the timing table, in a file of its own so `tests/track_percentile.rs` can test it (an example's
//! own `#[test]`s are not run).

/// The nearest-rank percentile `pct` (1 to 100) of the ascending `values`: the value at 1-based rank
/// `ceil(pct * n / 100)`, computed in integers so no float rounding can move it. 0.0 for no values.
pub fn nearest_rank(values: &[f64], pct: usize) -> f64 {
    let rank = (pct * values.len()).div_ceil(100).clamp(1, values.len().max(1));
    values.get(rank - 1).copied().unwrap_or(0.0)
}
