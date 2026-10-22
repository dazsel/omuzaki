//! `fx.radio` (#916): the telephone voice's response and the noise bed, measured on the node.

mod common;

use common::{node, run, sine_gain_db};
use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{ConfigValue, NodeConfig};
use omuzaki_audio_fx::radio::RadioKind;
use omuzaki_audio_mock::signals::{VoiceSpec, voice};

#[test]
fn the_telephone_voice_is_band_limited_as_the_spec_says() {
    // A small level keeps the drive in its linear range, so the response is the filters'.
    let at = |hz: f64| sine_gain_db(&mut *node(&RadioKind, &[]), hz, 0.003);
    let reference = at(1000.0);
    let low = at(100.0) - reference;
    let high = at(8000.0) - reference;
    let band: Vec<f64> =
        [400.0, 500.0, 700.0, 1000.0, 1500.0, 2000.0, 2500.0, 3000.0].iter().map(|hz| at(*hz) - reference).collect();
    let (lo, hi) = band.iter().fold((f64::MAX, f64::MIN), |(a, b), g| (a.min(*g), b.max(*g)));
    eprintln!(
        "fx.radio: telephone, relative to 1 kHz: 100 Hz {low:.1} dB, 8 kHz {high:.1} dB, 400 to 3000 Hz from {lo:.2} to {hi:.2} dB ({:.2} dB wide)",
        hi - lo
    );
    assert!(low <= -18.0 && high <= -24.0, "{low} {high}");
    // The issue's ±3.5 dB is read as a 7 dB window (±3.5 dB about its middle): the spec's own filters (two 300 Hz
    // high-passes, two 3.4 kHz low-passes and a +3 dB bell at 1.5 kHz) put 3 kHz 5 dB under 1 kHz.
    assert!(hi - lo <= 7.0, "{lo} {hi}");
}

fn noisy(seed: i64) -> Vec<f32> {
    let mut config = NodeConfig::default();
    config.0.insert("seed".into(), ConfigValue::Int(seed));
    let mut radio = common::node_with(&RadioKind, &config, Channels::Mono, &[("noise_db", -30.0)]);
    let mut x = voice(&VoiceSpec::steady(5, 1.0));
    x.extend(vec![0.0; 3 * 48_000]);
    run(&mut *radio, &x)
}

#[test]
fn the_noise_bed_is_the_same_for_a_seed_and_keyed_by_the_voice() {
    let (a, b, other) = (noisy(9), noisy(9), noisy(10));
    assert_eq!(a, b, "the same seed gives the same noise, bit for bit");
    assert_ne!(a, other, "another seed gives other noise");
    let tail = a[48_000 + 120_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(tail < 1e-6, "silence in is silence out once the key has released: {tail}");
}

#[test]
fn the_radio_mode_sets_its_own_band_and_parameters_override_it() {
    let radio_8k = sine_gain_db(&mut *node(&RadioKind, &[("mode", 1.0)]), 4500.0, 0.003)
        - sine_gain_db(&mut *node(&RadioKind, &[("mode", 1.0)]), 1000.0, 0.003);
    let phone_8k = sine_gain_db(&mut *node(&RadioKind, &[]), 4500.0, 0.003)
        - sine_gain_db(&mut *node(&RadioKind, &[]), 1000.0, 0.003);
    assert!(radio_8k > phone_8k + 6.0, "radio {radio_8k} phone {phone_8k}");
    let wide = sine_gain_db(&mut *node(&RadioKind, &[("mode", 1.0), ("high_hz", 12_000.0)]), 4500.0, 0.003)
        - sine_gain_db(&mut *node(&RadioKind, &[("mode", 1.0), ("high_hz", 12_000.0)]), 1000.0, 0.003);
    assert!(wide > radio_8k, "high_hz set after the mode overrides it: {wide} {radio_8k}");
    let before = sine_gain_db(&mut *node(&RadioKind, &[("high_hz", 12_000.0), ("mode", 1.0)]), 4500.0, 0.003)
        - sine_gain_db(&mut *node(&RadioKind, &[("high_hz", 12_000.0), ("mode", 1.0)]), 1000.0, 0.003);
    assert!((before - wide).abs() < 1e-6, "and set before it, the same: {before} {wide}");
}
