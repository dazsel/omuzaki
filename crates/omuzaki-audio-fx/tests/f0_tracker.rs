//! The shifter's analysis (#917): the F0 track and the pitch marks, against the synthetic voice's known F0 and
//! glottal closures.

use omuzaki_audio_fx::pitch::analysis::{Analysis, HOP};
use omuzaki_audio_fx::pitch::f0::{DECIMATION, SPAN, WINDOW};
use omuzaki_audio_fx::pitch::marks::Mark;
use omuzaki_audio_mock::signals::{Trajectory, VoiceSpec, voice_with_closures};

const FS: f64 = 48_000.0;

/// Runs the analysis over `x`, giving `(time in s, estimated F0, voiced)` per hop and every mark placed.
fn analyse(x: &[f32]) -> (Vec<(f64, f64, bool)>, Vec<Mark>) {
    let mut analysis = Analysis::new(1);
    let (mut track, mut marks) = (Vec::new(), Vec::new());
    for (n, s) in x.iter().enumerate() {
        analysis.push(f64::from(*s), false);
        while (marks.len() as u64) < analysis.marks.count() {
            marks.push(analysis.marks.get(marks.len() as u64).expect("a recent mark is kept"));
        }
        if n as i64 % HOP == 0 {
            let e = analysis.estimate();
            let lag = e.period / DECIMATION as f64;
            // The middle of what the estimate compared: the integration window and its copy one period later.
            let centre = n as f64 - (DECIMATION * SPAN) as f64 + DECIMATION as f64 * (WINDOW as f64 + lag) / 2.0;
            track.push((centre / FS, FS / e.period, e.voiced));
        }
    }
    (track, marks)
}

fn spec(seed: u64, seconds: f64, f0: Trajectory) -> VoiceSpec {
    let mut spec = VoiceSpec::steady(seed, seconds);
    spec.f0 = f0;
    spec
}

fn cases() -> Vec<(String, VoiceSpec)> {
    let mut cases: Vec<(String, VoiceSpec)> = [60.0, 80.0, 120.0, 200.0, 300.0, 400.0]
        .iter()
        .map(|f| (format!("steady {f} Hz"), spec(3, 1.5, Trajectory::constant(*f))))
        .collect();
    for (from, to) in [(60.0, 120.0), (120.0, 240.0), (200.0, 400.0), (400.0, 200.0), (240.0, 120.0)] {
        cases.push((format!("glide {from} to {to} Hz in 1 s"), spec(5, 1.5, Trajectory::new(vec![(0.25, from), (1.25, to)]))));
    }
    cases
}

#[test]
fn the_f0_track_has_under_1_percent_gross_errors_and_under_10_cents_rms() {
    for (name, spec) in cases() {
        let (x, _) = voice_with_closures(&spec);
        let (track, _) = analyse(&x);
        let (mut voiced, mut gross, mut squares, mut fine) = (0usize, 0usize, 0.0f64, 0usize);
        for (t, f, v) in track {
            if !v || t < 0.1 || t > spec.seconds - 0.05 {
                continue;
            }
            let truth = spec.f0.at(t);
            voiced += 1;
            if (f / truth - 1.0).abs() > 0.2 {
                gross += 1;
            } else {
                let c = 1200.0 * (f / truth).log2();
                squares += c * c;
                fine += 1;
            }
        }
        let rms = (squares / fine.max(1) as f64).sqrt();
        let share = gross as f64 / voiced.max(1) as f64;
        eprintln!("f0: {name}: {voiced} voiced hops, {gross} gross errors ({:.2}%), fine error {rms:.2} cents RMS", 100.0 * share);
        assert!(voiced > 200 && share < 0.01 && rms < 10.0, "{name}");
    }
}

#[test]
fn marks_sit_within_a_quarter_millisecond_of_the_glottal_closures() {
    for (name, spec) in cases() {
        let (x, closures) = voice_with_closures(&spec);
        let (_, marks) = analyse(&x);
        let voiced: Vec<i64> = marks.iter().filter(|m| m.voiced).map(|m| m.at).collect();
        let (mut near, mut total) = (0usize, 0usize);
        let mut offsets = Vec::new();
        for c in closures.iter().map(|c| *c as i64).filter(|c| *c > 4800 && *c < x.len() as i64 - 4800) {
            total += 1;
            let nearest = voiced.iter().map(|m| m - c).min_by_key(|d| d.abs());
            if let Some(d) = nearest {
                offsets.push(d);
                if d.abs() <= 12 {
                    near += 1;
                }
            }
        }
        let far: Vec<i64> = offsets.iter().copied().filter(|d| d.abs() > 12).collect();
        if !far.is_empty() { eprintln!("   far: {far:?}"); }
        offsets.sort_unstable();
        let share = near as f64 / total as f64;
        eprintln!(
            "marks: {name}: {near} of {total} closures have a mark within 0.25 ms ({:.1}%), median offset {} samples",
            100.0 * share,
            offsets.get(offsets.len() / 2).copied().unwrap_or_default()
        );
        assert!(share >= 0.95, "{name}");
    }
}
