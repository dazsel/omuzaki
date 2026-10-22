//! A `Step` track as linear keys (`docs/design/vrma-export.md` V5).
//!
//! three-vrm 3.5.5 drops a sampler's interpolation, so a `STEP` sampler plays as a ramp. The exporter writes every
//! sampler `LINEAR` and encodes a hold as two keys an epsilon apart: before each key `i > 0` whose value differs from
//! the one before, it inserts `(h_i, v_{i-1})` with `h_i = t_i - e`, `e = min(HOLD_S, (t_i - t_{i-1}) / 2)` computed in
//! `f64` and rounded to `f32` once.
//!
//! Rounding can land on a neighbour, so `h_i` must lie strictly between `f32(t_{i-1})` and `f32(t_i)`; when it does
//! not, it is the largest `f32` below `t_i`, and when that is not above `t_{i-1}` no `f32` fits between the keys and the
//! track is refused. The hold's width `t_i - h_i` must also stay within `HOLD_S + ulp(t_i) / 2`, the most that nearest
//! rounding gives, or the fallback's whole ulp would make a visibly long hold at large times: refused too. Both
//! refusals are `anim_export.step_unrepresentable`, an error, since a clip with a hold missing plays wrong.

use omuzaki_io_api::{Finding, Interpolation, Keys, Report};

/// The hold's width, seconds: short enough to read as a cut, long enough to survive `f32` times of a long clip.
pub const HOLD_S: f64 = 1e-4;

/// Half the spacing of `f32` values at `t` (`t` finite and positive).
fn half_ulp(t: f32) -> f64 {
    (f64::from(t.next_up()) - f64::from(t)) / 2.0
}

/// The keys to write for `keys`: as they are for a linear track; for a step track, with a hold before every change.
/// A hold that cannot be placed adds an `anim_export.step_unrepresentable` error at `at` to `report` and leaves the
/// key without one (the caller refuses the export).
pub fn expand_steps<T: Copy + PartialEq>(keys: &Keys<T>, at: &str, report: &mut Report) -> (Vec<f32>, Vec<T>) {
    if keys.interpolation == Interpolation::Linear || keys.times.len() < 2 {
        return (keys.times.clone(), keys.values.clone());
    }
    let mut times = Vec::with_capacity(keys.times.len() * 2);
    let mut values = Vec::with_capacity(keys.values.len() * 2);
    for (i, (&t, &v)) in keys.times.iter().zip(&keys.values).enumerate() {
        if i > 0 && keys.values[i - 1] != v {
            let before = keys.times[i - 1];
            match hold(before, t) {
                Some(h) => {
                    times.push(h);
                    values.push(keys.values[i - 1]);
                }
                None => report.push(
                    Finding::error(
                        "anim_export.step_unrepresentable",
                        format!("no hold fits between the keys at {before} s and {t} s in 32-bit time"),
                    )
                    .at(format!("{at}/times/{i}"))
                    .hint("move the keys further apart or make the track linear"),
                ),
            }
        }
        times.push(t);
        values.push(v);
    }
    (times, values)
}

/// The hold time before the key at `t` that follows the key at `before`, if one can be written.
fn hold(before: f32, t: f32) -> Option<f32> {
    let e = HOLD_S.min((f64::from(t) - f64::from(before)) / 2.0);
    #[allow(clippy::cast_possible_truncation, reason = "the one rounding to f32 the design asks for")]
    let mut h = (f64::from(t) - e) as f32;
    if !(before < h && h < t) {
        h = t.next_down();
    }
    let fits = before < h && f64::from(t) - f64::from(h) <= HOLD_S + half_ulp(t);
    fits.then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(times: &[f32], values: &[f32]) -> (Vec<f32>, Vec<f32>, Report) {
        let mut report = Report::new();
        let (t, v) = expand_steps(&Keys::step(times.to_vec(), values.to_vec()), "tracks/0", &mut report);
        (t, v, report)
    }

    #[test]
    fn a_change_gets_a_hold_a_tenth_of_a_millisecond_before_it() {
        let (t, v, report) = expand(&[0.0, 1.0, 2.0, 3.0], &[0.0, 1.0, 1.0, 0.0]);
        assert!(report.findings.is_empty());
        assert_eq!(v, [0.0, 0.0, 1.0, 1.0, 1.0, 0.0]);
        assert_eq!(t.len(), 6);
        assert!((f64::from(t[1]) - (1.0 - HOLD_S)).abs() < 1e-7 && (f64::from(t[4]) - (3.0 - HOLD_S)).abs() < 1e-6);
        assert!(t.windows(2).all(|w| w[0] < w[1]), "strictly increasing: {t:?}");
    }

    #[test]
    fn close_keys_split_their_gap_and_linear_tracks_pass_through() {
        let (t, _, report) = expand(&[0.0, 0.0001], &[0.0, 1.0]);
        assert!(report.findings.is_empty());
        assert!((t[1] - 0.00005).abs() < 1e-9, "{t:?}");
        let mut r = Report::new();
        let linear = Keys::linear(vec![0.0, 1.0], vec![0.0, 1.0]);
        assert_eq!(expand_steps(&linear, "x", &mut r), (vec![0.0, 1.0], vec![0.0, 1.0]));
    }

    #[test]
    fn keys_one_ulp_apart_are_refused() {
        let (_, _, report) = expand(&[0.0, 0.999_999_94, 1.0], &[0.0, 0.0, 1.0]);
        assert!(report.has_rule("anim_export.step_unrepresentable"), "the midpoint rounds onto a key");
        assert_eq!(report.findings[0].location.as_deref(), Some("tracks/0/times/2"));
    }

    #[test]
    fn a_fallback_hold_wider_than_the_bound_is_refused() {
        let (_, _, report) = expand(&[0.0, 3599.0, 3600.0], &[0.0, 0.0, 1.0]);
        assert!(report.has_rule("anim_export.step_unrepresentable"), "0.244 ms is over the 0.222 ms bound");
    }

    #[test]
    fn long_clips_keep_their_hold_within_the_bound() {
        for t in [200.0_f32, 599.0, 600.0] {
            let (times, _, report) = expand(&[0.0, t - 1.0, t], &[0.0, 0.0, 1.0]);
            assert!(report.findings.is_empty(), "{t}");
            assert!(times.windows(2).all(|w| w[0] < w[1]));
            let width = f64::from(t) - f64::from(times[2]);
            assert!(width > 0.0 && width <= HOLD_S + half_ulp(t), "{t}: {width}");
        }
    }
}
