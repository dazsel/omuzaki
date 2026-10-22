//! `fx.pitch` (#1443): pitch and formant shifts measured on the output by code the shifter does not share.

mod common;

use common::{cents, db, dtft, formants, hann, node, run, yin_track};
use omuzaki_audio_fx::pitch::PitchKind;
use omuzaki_audio_fx::pitch::synth::LATENCY;
use omuzaki_audio_mock::signals::{Trajectory, VoiceSpec, voice};

fn steady(f0: f64, seconds: f64) -> Vec<f32> {
    let mut spec = VoiceSpec::steady(41, seconds);
    spec.f0 = Trajectory::constant(f0);
    spec.jitter = 0.002;
    voice(&spec)
}

/// The median F0 of the voiced frames after the first 0.3 s, and the share of them within `cents` of `target`.
fn measure(y: &[f32], target: f64, within: f64) -> (f64, f64) {
    let track: Vec<f64> = yin_track(&y[14_400..]).into_iter().flatten().collect();
    let mut sorted = track.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let close = track.iter().filter(|f| cents(**f, target).abs() <= within).count() as f64 / track.len() as f64;
    (median, close)
}

/// The formants are read by LPC from the output and compared with the input's. At +12 semitones the output's
/// harmonics (240 Hz apart) are too sparse for any envelope estimate to hold /a/'s F1 and F2 (360 Hz apart) as two
/// peaks, but they fall exactly on every other harmonic of the input, so there the envelope is compared directly:
/// harmonic by harmonic from 480 to 3,600 Hz, after the overall level.
#[test]
fn shifts_land_within_10_cents_and_keep_the_formants() {
    let x = steady(120.0, 2.0);
    let input = formants(&x[14_400..]);
    for semitones in [5.0f32, -5.0, 12.0, -12.0] {
        let mut shifter = node(&PitchKind, &[("semitones", semitones)]);
        let y = run(&mut *shifter, &x);
        let target = 120.0 * (f64::from(semitones) / 12.0).exp2();
        let (median, close) = measure(&y[LATENCY as usize..], target, 10.0);
        let output = formants(&y[LATENCY as usize + 14_400..]);
        eprintln!(
            "fx.pitch: {semitones:+} st: median F0 {median:.2} Hz ({:+.2} cents off {target:.2}), {:.1}% of voiced frames within 10 cents; LPC formants in {input:.0?}, out {output:.0?}",
            cents(median, target),
            100.0 * close
        );
        assert!(cents(median, target).abs() <= 10.0, "{semitones}: {median}");
        if semitones == 12.0 {
            let (xs, ys) = (hann(&x[48_000..72_000]), hann(&y[48_000 + LATENCY as usize..72_000 + LATENCY as usize]));
            let differences: Vec<f64> =
                (2..=15).map(|k| db(dtft(&ys, 240.0 * f64::from(k))) - db(dtft(&xs, 240.0 * f64::from(k)))).collect();
            let mut sorted = differences.clone();
            sorted.sort_by(f64::total_cmp);
            let level = sorted[sorted.len() / 2];
            let worst = differences.iter().map(|d| (d - level).abs()).fold(0.0, f64::max);
            eprintln!(
                "fx.pitch: +12 st: the output's harmonics 480 to 3,600 Hz follow the input's envelope within {worst:.2} dB (level {level:+.2} dB)"
            );
            assert!(worst <= 1.5, "{differences:?}");
        } else {
            for (a, b) in input.iter().zip(&output) {
                assert!((b / a - 1.0).abs() <= 0.05, "{semitones}: formants {input:?} became {output:?}");
            }
        }
    }
}

#[test]
fn a_formant_factor_of_1_2_moves_f1_to_f3_up_20_percent_and_keeps_the_pitch() {
    let x = steady(120.0, 2.0);
    let input = formants(&x[14_400..]);
    let mut shifter = node(&PitchKind, &[("formant", 1.2)]);
    let y = run(&mut *shifter, &x);
    let output = formants(&y[LATENCY as usize + 14_400..]);
    let (median, _) = measure(&y[LATENCY as usize..], 120.0, 10.0);
    let moved: Vec<f64> = input.iter().zip(&output).map(|(a, b)| b / a).collect();
    eprintln!(
        "fx.pitch: formant 1.2: F1 to F3 {input:.0?} became {output:.0?} (x{moved:.3?}); F0 {median:.2} Hz ({:+.2} cents)",
        cents(median, 120.0)
    );
    assert!(cents(median, 120.0).abs() <= 10.0, "{median}");
    for m in moved {
        assert!((m - 1.2).abs() <= 0.06, "{m}");
    }
}

#[test]
fn a_glide_shifted_7_semitones_has_almost_no_octave_errors() {
    let mut spec = VoiceSpec::steady(43, 2.0);
    spec.f0 = Trajectory::new(vec![(0.0, 100.0), (2.0, 300.0)]);
    let x = voice(&spec);
    let mut shifter = node(&PitchKind, &[("semitones", 7.0)]);
    let y = run(&mut *shifter, &x);
    let ratio = (7.0f64 / 12.0).exp2();
    let track = yin_track(&y[LATENCY as usize..]);
    let mut voiced = 0;
    let mut octave = 0;
    for (i, f) in track.iter().enumerate() {
        let Some(f) = f else { continue };
        let at = (i * 480 + 1024) as f64 / 48_000.0;
        if at < 0.3 {
            continue;
        }
        let target = ratio * (100.0 + 100.0 * at);
        voiced += 1;
        if cents(*f, target).abs() > 600.0 {
            octave += 1;
        }
    }
    let share = f64::from(octave) / f64::from(voiced);
    eprintln!(
        "fx.pitch: 100 to 300 Hz glide +7 st: {octave} octave errors in {voiced} voiced frames ({:.2}%)",
        100.0 * share
    );
    assert!(share < 0.01, "{octave} of {voiced}");
}

/// The largest second difference of `x[from..]`.
fn curvature(x: &[f32], from: usize) -> f64 {
    x[from..].windows(3).map(|w| f64::from((w[2] - 2.0 * w[1] + w[0]).abs())).fold(0.0, f64::max)
}

#[test]
fn shifted_speech_has_no_clicks() {
    let mut spec = VoiceSpec::steady(47, 3.0);
    spec.f0 = Trajectory::new(vec![(0.0, 110.0), (1.0, 180.0), (2.0, 95.0), (3.0, 140.0)]);
    spec.vowels = vec![
        (0.0, omuzaki_audio_mock::signals::Vowel::A),
        (1.0, omuzaki_audio_mock::signals::Vowel::I),
        (2.0, omuzaki_audio_mock::signals::Vowel::O),
        (3.0, omuzaki_audio_mock::signals::Vowel::E),
    ];
    let x = voice(&spec);
    let natural = curvature(&x, 0);
    for (semitones, formant) in [(5.0f32, 1.0f32), (-5.0, 1.0), (0.0, 0.85), (0.0, 1.2), (7.0, 1.15)] {
        let mut shifter = node(&PitchKind, &[("semitones", semitones), ("formant", formant)]);
        let y = run(&mut *shifter, &x);
        let level = common::rms(&y[LATENCY as usize..]) / common::rms(&x);
        let found = curvature(&y, LATENCY as usize);
        // The conformance suite's rule: three times the curvature the signal already has (here scaled to the output's
        // level).
        let allowed = 3.0 * natural * level;
        eprintln!(
            "fx.pitch: {semitones:+} st, formant {formant}: largest second difference {found:.4} against {allowed:.4} allowed"
        );
        assert!(found <= allowed, "{semitones} {formant}: {found} > {allowed}");
    }
}
