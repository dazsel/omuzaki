//! `fx.eq` (#1158): each band shape's measured magnitude response matches the analytic RBJ cookbook response within
//! 0.1 dB at 20 frequencies. The response is measured by a direct DTFT of the node's impulse response; the analytic
//! answer is computed here from the cookbook's formulas, independently of `omuzaki_audio_fx::biquad`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

mod common;

use std::f64::consts::PI;

use common::{FS, db, dtft, node, run};
use omuzaki_audio_fx::eq::EqKind;

/// The cookbook's `(b, a)` for `shape` (0 to 4), written out again here.
fn cookbook(shape: u8, f0: f64, q: f64, gain_db: f64) -> ([f64; 3], [f64; 3]) {
    let w0 = 2.0 * PI * f0 / FS;
    let (c, s) = (w0.cos(), w0.sin());
    let alpha = s / (2.0 * q);
    let a = 10f64.powf(gain_db / 40.0);
    let sq = 2.0 * a.sqrt() * alpha;
    match shape {
        0 => ([(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha]),
        1 => ([f64::midpoint(1.0, c), -(1.0 + c), f64::midpoint(1.0, c)], [1.0 + alpha, -2.0 * c, 1.0 - alpha]),
        2 => (
            [
                a * ((a + 1.0) - (a - 1.0) * c + sq),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - sq),
            ],
            [(a + 1.0) + (a - 1.0) * c + sq, -2.0 * ((a - 1.0) + (a + 1.0) * c), (a + 1.0) + (a - 1.0) * c - sq],
        ),
        3 => (
            [
                a * ((a + 1.0) + (a - 1.0) * c + sq),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - sq),
            ],
            [(a + 1.0) - (a - 1.0) * c + sq, 2.0 * ((a - 1.0) - (a + 1.0) * c), (a + 1.0) - (a - 1.0) * c - sq],
        ),
        _ => ([1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a]),
    }
}

/// `|H(e^jw)|` in dB at `hz`.
fn analytic_db((b, a): ([f64; 3], [f64; 3]), hz: f64) -> f64 {
    let w = 2.0 * PI * hz / FS;
    let eval = |k: [f64; 3]| {
        let re = k[0] + k[1] * w.cos() + k[2] * (2.0 * w).cos();
        let im = -(k[1] * w.sin() + k[2] * (2.0 * w).sin());
        re.hypot(im)
    };
    db(eval(b) / eval(a))
}

/// 20 test frequencies, log-spaced from 30 Hz to 18 kHz.
fn frequencies() -> Vec<f64> {
    (0..20).map(|i| 30.0 * (18_000.0f64 / 30.0).powf(f64::from(i) / 19.0)).collect()
}

#[test]
fn every_shape_matches_the_cookbook_within_a_tenth_of_a_db() {
    let cases = [
        (0u8, 2000.0, 0.707, 0.0),
        (1, 200.0, 2.0, 0.0),
        (2, 300.0, 0.9, 9.0),
        (3, 4000.0, 0.707, -12.0),
        (4, 1000.0, 4.0, 15.0),
    ];
    let mut worst = 0.0f64;
    for (shape, freq, q, gain) in cases {
        let mut eq = node(
            &EqKind,
            &[("b1.type", f32::from(shape)), ("b1.freq", freq as f32), ("b1.q", q as f32), ("b1.gain_db", gain as f32)],
        );
        let mut impulse = vec![0.0f32; 1 << 15];
        impulse[0] = 1.0;
        let response = run(&mut *eq, &impulse);
        let reference = cookbook(shape, f64::from(freq as f32), f64::from(q as f32), f64::from(gain as f32));
        for hz in frequencies() {
            let measured = db(dtft(&response, hz));
            let expected = analytic_db(reference, hz);
            // Far down a pass filter's stopband the float audio itself is the floor; compare where the response is
            // above -80 dB.
            if expected > -80.0 {
                let error = (measured - expected).abs();
                worst = worst.max(error);
                assert!(
                    error < 0.1,
                    "shape {shape} at {hz:.0} Hz: measured {measured:.3} dB, cookbook {expected:.3} dB"
                );
            }
        }
    }
    eprintln!("fx.eq: worst error against the cookbook over 5 shapes x 20 frequencies: {worst:.5} dB");
}

#[test]
fn a_new_eq_changes_nothing_and_bands_stack() {
    let mut flat = node(&EqKind, &[]);
    let x = common::sine(4800, 440.0, 0.5, 0.0);
    assert_eq!(run(&mut *flat, &x), x, "eight flat bells pass the signal bit for bit");
    let mut two = node(&EqKind, &[("b1.gain_db", 6.0), ("b2.gain_db", 6.0)]);
    let g = common::sine_gain_db(&mut *two, 1000.0, 0.1);
    assert!((g - 12.0).abs() < 0.05, "two +6 dB bells at 1 kHz give {g} dB");
}

#[test]
fn a_switched_off_band_is_flat() {
    let mut off = node(&EqKind, &[("b1.gain_db", 12.0), ("b1.on", 0.0)]);
    let g = common::sine_gain_db(&mut *off, 1000.0, 0.1);
    assert!(g.abs() < 1e-4, "{g}");
}
