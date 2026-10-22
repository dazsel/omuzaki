//! `fx.compressor` (#914): the static curve and the attack and release times, measured on the node.

mod common;

use common::{db, node, rms, run, sine};
use omuzaki_audio_fx::compressor::{CompressorKind, curve};

#[test]
fn the_static_curve_holds_within_half_a_db_from_minus_40_to_0() {
    let mut worst = 0.0f64;
    for step in 0..=20 {
        let peak_db = -40.0 + 2.0 * f64::from(step);
        let mut c = node(&CompressorKind, &[]);
        let x = sine(48_000, 1000.0, 10f64.powf(peak_db / 20.0), 0.0);
        let y = run(&mut *c, &x);
        let measured = db(rms(&y[24_000..]) / rms(&x[24_000..]));
        // The RMS detector reads a sine 3.01 dB under its peak.
        let level = db(rms(&x[24_000..]));
        let expected = curve(level, -18.0, 3.0, 6.0) - level;
        worst = worst.max((measured - expected).abs());
        assert!((measured - expected).abs() < 0.5, "{peak_db} dBFS: {measured:.3} dB, formula {expected:.3} dB");
    }
    eprintln!("fx.compressor: worst static-curve error from -40 to 0 dBFS: {worst:.4} dB");
}

/// The times to 63% of the final gain change after a +20 dB step up and back down, in ms.
fn times(attack_ms: f32, release_ms: f32) -> (f64, f64) {
    let params = [
        ("threshold_db", -30.0),
        ("ratio", 4.0),
        ("knee_db", 0.0),
        ("attack_ms", attack_ms),
        ("release_ms", release_ms),
        ("detector", 0.0),
    ];
    let mut c = node(&CompressorKind, &params);
    // DC at -40 dBFS (no reduction), +20 dB to -20 dBFS (7.5 dB of reduction), and back.
    let (low, high, len) = (0.01f32, 0.1f32, 96_000usize);
    let mut x = vec![low; len];
    x.extend(vec![high; len]);
    x.extend(vec![low; len]);
    let y = run(&mut *c, &x);
    let gain = |n: usize| db(f64::from(y[n]) / f64::from(x[n]));
    let time_to = |from: usize| {
        let start = gain(from - 1);
        let end = gain(from + len - 1);
        let mark = start + 0.632 * (end - start);
        (from..from + len).position(|n| if end < start { gain(n) <= mark } else { gain(n) >= mark }).unwrap() as f64
            / 48.0
    };
    (time_to(len), time_to(2 * len))
}

#[test]
fn attack_and_release_reach_63_percent_within_ten_percent_of_their_times() {
    for (attack, release) in [(5.0f32, 120.0f32), (1.0, 50.0), (20.0, 500.0)] {
        let (a, r) = times(attack, release);
        eprintln!("fx.compressor: attack {attack} ms measured {a:.3} ms; release {release} ms measured {r:.3} ms");
        assert!((a - f64::from(attack)).abs() <= 0.1 * f64::from(attack), "attack {attack}: {a}");
        assert!((r - f64::from(release)).abs() <= 0.1 * f64::from(release), "release {release}: {r}");
    }
}

#[test]
fn the_channels_are_linked() {
    use omuzaki_audio_api::graph::Channels;
    use omuzaki_audio_api::node::{BlockMut, NodeConfig};
    let mut c = common::node_with(&CompressorKind, &NodeConfig::default(), Channels::Stereo, &[]);
    let mut l = sine(48_000, 1000.0, 0.9, 0.0);
    let mut r = sine(48_000, 1000.0, 0.01, 0.0);
    let (l0, r0) = (l.clone(), r.clone());
    for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        c.process(&mut BlockMut::stereo(a, b, 0));
    }
    let gl = db(rms(&l[24_000..]) / rms(&l0[24_000..]));
    let gr = db(rms(&r[24_000..]) / rms(&r0[24_000..]));
    assert!(gl < -3.0 && (gl - gr).abs() < 1e-3, "left {gl} right {gr}");
}
