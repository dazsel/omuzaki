//! The nearest-rank percentile of the `track_roi_timing` example: rank `ceil(p * n / 100)`, at the boundaries.

#[path = "../examples/track_roi_timing/percentile.rs"]
mod percentile;

use percentile::nearest_rank;

/// `0.0, 1.0, ... (n - 1)`, so a value is its own zero-based index.
fn ramp(n: usize) -> Vec<f64> {
    (0..n).map(|i| f64::from(u32::try_from(i).unwrap_or(u32::MAX))).collect()
}

#[test]
fn one_value_is_every_percentile() {
    assert!((nearest_rank(&[7.0], 50) - 7.0).abs() < f64::EPSILON);
    assert!((nearest_rank(&[7.0], 95) - 7.0).abs() < f64::EPSILON);
    assert!((nearest_rank(&[7.0], 100) - 7.0).abs() < f64::EPSILON);
}

#[test]
fn twenty_values_put_p95_at_the_nineteenth() {
    let values = ramp(20);
    assert!((nearest_rank(&values, 95) - 18.0).abs() < f64::EPSILON); // rank 19
    assert!((nearest_rank(&values, 50) - 9.0).abs() < f64::EPSILON); // rank 10
}

#[test]
fn a_thousand_values_put_p50_at_499_and_p95_at_949() {
    let values = ramp(1000);
    assert!((nearest_rank(&values, 50) - 499.0).abs() < f64::EPSILON);
    assert!((nearest_rank(&values, 95) - 949.0).abs() < f64::EPSILON);
}

#[test]
fn p100_is_the_last_value_and_no_values_is_zero() {
    assert!((nearest_rank(&ramp(1000), 100) - 999.0).abs() < f64::EPSILON);
    assert!(nearest_rank(&[], 95).abs() < f64::EPSILON);
}
