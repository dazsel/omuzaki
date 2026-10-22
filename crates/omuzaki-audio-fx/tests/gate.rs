//! `fx.gate` (#913): attenuation, the opening time, hysteresis and hold, measured on the node.

mod common;

use common::{db, rms};
use omuzaki_audio_api::node::{BlockMut, Node};
use omuzaki_audio_fx::gate::{Gate, GateState};
use omuzaki_audio_mock::signals::{VoiceSpec, voice, white_noise};

fn feed(gate: &mut Gate, x: &[f32]) -> Vec<f32> {
    let mut y = x.to_vec();
    for (k, chunk) in y.chunks_mut(256).enumerate() {
        gate.process(&mut BlockMut::mono(chunk, (k * 256) as u64));
    }
    y
}

#[test]
fn noise_at_minus_60_comes_out_at_least_78_db_lower() {
    let mut gate = Gate::new();
    let x = white_noise(3 * 48_000, 7, 0.001);
    let y = feed(&mut gate, &x);
    let (input, output) = (rms(&x[96_000..]), rms(&y[96_000..]));
    let drop = db(input) - db(output);
    eprintln!("fx.gate: -60 dBFS noise comes out {drop:.2} dB lower");
    assert!(drop >= 78.0, "{drop}");
}

#[test]
fn a_voice_at_minus_20_passes_at_full_gain_within_1_ms_of_its_onset() {
    let mut gate = Gate::new();
    let mut spec = VoiceSpec::steady(3, 0.5);
    spec.peak = 0.1;
    let mut x = vec![0.0f32; 4800];
    x.extend(voice(&spec));
    // The onset: the first sample over the opening threshold (-45 dBFS), where the voice becomes audible to the gate.
    let onset = x.iter().position(|s| f64::from(s.abs()) > 10f64.powf(-45.0 / 20.0)).unwrap();
    let mut full = None;
    for (n, s) in x.iter().enumerate() {
        let mut one = [*s];
        gate.process(&mut BlockMut::mono(&mut one, n as u64));
        if n >= onset && gate.gain() == 1.0 && full.is_none() {
            full = Some(n - onset);
        }
    }
    let full = full.expect("the gate opened");
    eprintln!("fx.gate: full gain {:.3} ms after the voice's onset", full as f64 / 48.0);
    assert!(full <= 48, "{full} samples");
}

/// A 1 kHz tone whose peak alternates between `a` and `b` dBFS every 20 ms, for `seconds`.
fn alternating(a: f64, b: f64, seconds: f64) -> Vec<f32> {
    let len = (seconds * 48_000.0) as usize;
    (0..len)
        .map(|n| {
            let level = if (n / 960) % 2 == 0 { a } else { b };
            (10f64.powf(level / 20.0) * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / 48_000.0).sin()) as f32
        })
        .collect()
}

#[test]
fn a_level_between_the_thresholds_changes_the_state_at_most_once() {
    // From open: a burst opens the gate, then the level hovers between the thresholds.
    let mut gate = Gate::new();
    feed(&mut gate, &alternating(-20.0, -20.0, 0.1));
    assert_eq!(gate.state(), GateState::Open);
    let before = gate.changes();
    feed(&mut gate, &alternating(-47.0, -49.0, 2.0));
    let opened = gate.changes() - before;
    // From closed: the same level never opens it.
    let mut closed = Gate::new();
    feed(&mut closed, &alternating(-47.0, -49.0, 2.0));
    eprintln!(
        "fx.gate: -47/-49 dB every 20 ms for 2 s: {opened} change(s) from open, {} from closed",
        closed.changes()
    );
    assert!(opened <= 1 && closed.changes() <= 1, "{opened} {}", closed.changes());
}

#[test]
fn hold_is_kept_to_the_millisecond() {
    for hold_ms in [0.0f32, 30.0, 80.0, 250.0] {
        let mut gate = Gate::new();
        gate.set_param(omuzaki_audio_api::node::ParamIndex(2), hold_ms);
        let mut x = alternating(-20.0, -20.0, 0.1);
        x.extend(vec![0.0; 48_000]);
        let (mut fell, mut closing) = (None, None);
        for (n, s) in x.iter().enumerate() {
            let before = gate.state();
            let mut one = [*s];
            gate.process(&mut BlockMut::mono(&mut one, n as u64));
            if before == GateState::Open && gate.state() != GateState::Open {
                fell = Some(n);
            }
            if gate.state() == GateState::Closing && closing.is_none() {
                closing = Some(n);
            }
        }
        // The hold is measured from the sample where the detector fell under close_db to the one where the gain
        // starts to fall.
        let measured = (closing.unwrap() - fell.unwrap()) as f64 / 48.0;
        eprintln!("fx.gate: hold {hold_ms} ms measured {measured:.3} ms");
        assert!((measured - f64::from(hold_ms)).abs() <= 1.0, "{hold_ms}: {measured}");
    }
}
