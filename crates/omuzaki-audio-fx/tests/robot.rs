//! `fx.robot` (#915): the ring modulator's sidebands and the comb's echoes, measured on the node.

mod common;

use common::{db, dtft, hann, node, run, sine};
use omuzaki_audio_fx::robot::RobotKind;

#[test]
fn a_440_hz_sine_through_a_60_hz_carrier_becomes_380_and_500_hz() {
    let mut robot = node(&RobotKind, &[("carrier_hz", 60.0), ("ring_mix", 1.0), ("feedback", 0.0), ("mix", 1.0)]);
    let x = sine(96_000, 440.0, 0.5, 0.0);
    let y = run(&mut *robot, &x);
    let spectrum = hann(&y[48_000..]);
    let (low, high, carrier) = (dtft(&spectrum, 380.0), dtft(&spectrum, 500.0), dtft(&spectrum, 440.0));
    let left = db(carrier / low.max(high));
    eprintln!(
        "fx.robot: 380 Hz {:.1} dB, 500 Hz {:.1} dB, 440 Hz left at {left:.1} dB under the sidebands",
        db(low),
        db(high)
    );
    assert!((db(low) - db(high)).abs() < 0.5, "the sidebands are equal");
    assert!(left < -60.0, "{left}");
    for hz in (300..=600).step_by(10).map(f64::from).filter(|h| (h - 380.0).abs() > 15.0 && (h - 500.0).abs() > 15.0) {
        assert!(dtft(&spectrum, hz) < low, "{hz} Hz is not a peak");
    }
}

#[test]
fn the_comb_echoes_at_multiples_of_its_delay_with_powers_of_its_feedback() {
    for (comb_ms, feedback) in [(6.0f32, 0.5f32), (2.0, 0.9), (12.0, 0.3)] {
        let mut robot =
            node(&RobotKind, &[("ring_mix", 0.0), ("comb_ms", comb_ms), ("feedback", feedback), ("mix", 1.0)]);
        let mut x = vec![0.0f32; 48_000];
        x[0] = 1.0;
        let y = run(&mut *robot, &x);
        let d = (f64::from(comb_ms) * 48.0).round() as usize;
        let mut worst = 0.0f64;
        for k in 0..8u32 {
            let expected = f64::from(feedback).powi(k as i32);
            worst = worst.max((f64::from(y[k as usize * d]) - expected).abs());
        }
        let between = y.iter().enumerate().filter(|(n, _)| n % d != 0).fold(0.0f32, |m, (_, s)| m.max(s.abs()));
        eprintln!(
            "fx.robot: comb {comb_ms} ms feedback {feedback}: worst echo error {worst:e}, largest sample between echoes {between:e}"
        );
        assert!(worst < 1e-4 && between < 1e-6, "{worst} {between}");
    }
}

#[test]
fn feedback_at_its_maximum_is_stable_over_60_s_of_noise() {
    let mut robot = node(&RobotKind, &[("feedback", 0.9), ("comb_ms", 2.0)]);
    let x = omuzaki_audio_mock::signals::white_noise(60 * 48_000, 11, 1.0);
    let y = run(&mut *robot, &x);
    let peak = y.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let last = y[59 * 48_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    eprintln!("fx.robot: 60 s of full-scale noise at feedback 0.9: peak {peak:.2}, last second {last:.2}");
    // The comb's gain is at most 1 / (1 - 0.9) = 10.
    assert!(peak.is_finite() && peak <= 10.0 && last <= 10.0, "{peak}");
}
