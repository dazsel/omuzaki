//! Measured voice parity (`voice.compare`): the aligner and the measures on a synthetic speaker, and the loop it is
//! for. A source-filter synthesiser (a glottal pulse train through vowel formants, noise bursts for consonants, a
//! falling intonation per phrase) reads one line as a "reference speaker", then three more takes with the natural
//! variation between takes, which set the floor. Then "ours" starts as a different voice (higher, slower, a longer
//! pause, other formants) and is fixed one step at a time by what the `worst` list says, toward the floor. Everything
//! is generated here: no recording, no reference that isn't the project's own.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_speech::parity::ops::score;
use omuzaki_speech::parity::{HOP, analyse, compare, overlay};
use serde_json::Value;
use source_filter::{SPEAKER, Voice, read};

mod source_filter;

fn metric(answer: &Value, name: &str) -> (f64, f64, bool) {
    let m = answer["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap();
    (m["value"].as_f64().unwrap(), m["target"].as_f64().unwrap(), m["pass"].as_bool().unwrap())
}

#[test]
fn the_aligner_finds_a_slower_reading_and_a_late_phrase() {
    let reference = analyse(&read(SPEAKER));
    let slow = analyse(&read(Voice { tempo: 1.2, pause: [0.25, 0.0], ..SPEAKER }));
    let p = compare(&slow, &reference).unwrap();
    let m = &p.measures;
    println!("{m:?}\n{:?}", p.segments);
    // About 1.2 times as long (the pauses don't stretch, the extra pause adds).
    assert!(m.duration_ratio > 1.12 && m.duration_ratio < 1.3, "{m:?}");
    // The second phrase starts later by the first phrase's stretch plus the pause.
    assert_eq!(p.segments.len(), 3, "{:?}", p.segments);
    let late = p.segments[1].offset_ms[0];
    assert!(late > 300.0 && late < 600.0, "{late}");
    assert!(m.f0_bias_cents.abs() < 20.0, "{m:?}");
}

#[test]
fn the_overlay_has_a_column_per_10_ms_of_the_reference() {
    let r = read(SPEAKER);
    let reference = analyse(&r);
    let p = compare(&analyse(&read(Voice { seed: 2, ..SPEAKER })), &reference).unwrap();
    let (first, last) = reference.speech_span().unwrap();
    let image = overlay(&p);
    assert_eq!(image.width as usize, last - first);
    assert_eq!(image.rgb.len(), (image.width * image.height * 3) as usize);
    assert_eq!(HOP, 160);
}

#[test]
fn a_copy_of_the_reference_is_refused() {
    let r = read(SPEAKER);
    let e = score(&r, &r, &[]).unwrap_err();
    assert!(e.message.contains("near-copy"), "{}", e.message);
}

/// The loop the op is for: measure, fix what the worst list names, measure again, until every measure is within the
/// floor. Every failing measure is named in the worst list with what to change.
#[test]
fn ours_is_fixed_step_by_step_toward_the_floor() {
    let reference = read(SPEAKER);
    let takes: Vec<Vec<f32>> = (2..5).map(|seed| read(Voice { seed, ..SPEAKER })).collect();
    let takes: Vec<&[f32]> = takes.iter().map(Vec::as_slice).collect();
    let steps = [
        (
            "a different voice: higher, slower, a longer pause, other formants",
            Voice { f0: 240.0, formants: 1.12, tempo: 1.18, pause: [0.25, 0.0], seed: 7, ..SPEAKER },
        ),
        (
            "pitch shifted down as the hint says",
            Voice { f0: 200.0, formants: 1.12, tempo: 1.18, pause: [0.25, 0.0], seed: 7, ..SPEAKER },
        ),
        ("speed and the pause fixed", Voice { formants: 1.12, seed: 7, ..SPEAKER }),
        ("a voice with the reference speaker's formants", Voice { seed: 7, ..SPEAKER }),
        (
            "the pause before phrase 3 shortened by the 50 ms the hint names",
            Voice { pause: [0.0, -0.05], seed: 7, ..SPEAKER },
        ),
    ];
    println!("| step | mcd dB | f0 bias cents | duration % | onset ms | done | worst |");
    println!("|---|---|---|---|---|---|---|");
    let mut answers = Vec::new();
    for (what, voice) in steps {
        let scored = score(&read(voice), &reference, &takes).unwrap();
        let answer = scored.answer(what, &[]);
        let worst = answer["worst"].get(0).map_or(String::from("none"), |w| {
            format!(
                "{} {}: {}",
                w["scope"].as_str().unwrap(),
                w["metric"].as_str().unwrap(),
                w["hint"].as_str().unwrap()
            )
        });
        println!(
            "| {what} | {:.2} | {:.0} | {:.1} | {:.0} | {} | {worst} |",
            metric(&answer, "mcd_db").0,
            metric(&answer, "f0_bias_cents").0,
            metric(&answer, "duration_error_pct").0,
            metric(&answer, "phrase_onset_ms").0,
            answer["done"]
        );
        answers.push(answer);
    }
    println!("floor: {}", answers[0]["floor"]);
    // Step 1: the pitch, the speed and the onset all fail, and the worst list says how far to shift the pitch.
    let first = &answers[0];
    assert!(!metric(first, "f0_bias_cents").2 && !metric(first, "duration_error_pct").2);
    let hints: Vec<&str> = first["worst"].as_array().unwrap().iter().filter_map(|w| w["hint"].as_str()).collect();
    assert!(hints.iter().any(|h| h.contains("semitones")), "{hints:?}");
    // Every step is closer than the one before on what it fixed.
    assert!(metric(&answers[1], "f0_bias_cents").0 < metric(first, "f0_bias_cents").0 / 4.0);
    assert!(metric(&answers[2], "duration_error_pct").0 < metric(&answers[1], "duration_error_pct").0 / 4.0);
    assert!(metric(&answers[3], "mcd_db").0 < metric(&answers[2], "mcd_db").0 / 2.0);
    // Step 4 is close but not there: the worst list names the late phrase, and step 5 follows it. What is left is
    // the rhythm inside the phrases, a little wider than the speaker's own other takes, and the answer says so: not
    // done until that is fixed too (it needs per-word timing, which this synthetic voice doesn't have).
    let last = &answers[4];
    let failing: Vec<&str> = last["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["pass"] == false)
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(metric(last, "phrase_onset_ms").2 && metric(last, "duration_error_pct").2, "{failing:?}");
    assert!(failing.len() <= 1, "{failing:?}");
    assert_eq!(last["done"], failing.is_empty());
    for name in failing {
        assert!(last["worst"].as_array().unwrap().iter().any(|w| w["metric"] == name), "{}", last["worst"]);
    }
}

/// A reading with half the intonation range: every phrase measures about half as wide, and the worst list says to
/// widen it, naming the expressiveness that would (1.0 for twice the range). Following it brings the range back.
#[test]
fn a_flat_reading_is_told_how_much_wider_to_swing() {
    use omuzaki_speech::prosody::{Shape, reshape};
    let reference = read(SPEAKER);
    let take = read(Voice { seed: 2, ..SPEAKER });
    let flat = reshape(&take, omuzaki_speech::parity::RATE, &Shape { range: 0.5, ..Shape::IDENTITY });
    let scored = score(&flat, &reference, &[]).unwrap();
    let ratios: Vec<f64> = scored.parity.segments.iter().filter_map(|s| s.f0_range_ratio).collect();
    println!("range ratios {ratios:?}, line {:.2}", scored.parity.measures.f0_range_ratio);
    assert!(!ratios.is_empty() && ratios.iter().all(|r| (0.35..0.7).contains(r)), "{ratios:?}");
    let answer = scored.answer("a take at half the range", &[]);
    let hints: Vec<&str> = answer["worst"].as_array().unwrap().iter().filter_map(|w| w["hint"].as_str()).collect();
    println!("{hints:#?}");
    assert!(
        hints.iter().any(|h| h.contains("widen it (expressiveness 0.9") || h.contains("widen it (expressiveness 1.0")),
        "{hints:?}"
    );
    // Following the hint: expressiveness 1.0 is twice the range.
    let range = Shape::range_for_expressiveness(1.0);
    let fixed = reshape(&flat, omuzaki_speech::parity::RATE, &Shape { range, ..Shape::IDENTITY });
    let after = score(&fixed, &reference, &[]).unwrap().parity.measures;
    println!("after: range {:.2}, shape {:.0} cents", after.f0_range_ratio, after.f0_shape_cents);
    assert!((after.f0_range_ratio - 1.0).abs() < 0.15, "{after:?}");
    assert!(after.f0_shape_cents < scored.parity.measures.f0_shape_cents, "{after:?}");
}
