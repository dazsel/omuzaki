//! The lip-sync evaluation (#2351) on synthetic lines with known phones: the microphone mouth with closures against the
//! vowel-only mouth it replaces, and the mouth from a line's own phones, aligned to its audio.
//!
//! open-annie's bar (real TTS speech, scored against TTS durations): 40.9% closure recall and 34.6% five-vowel accuracy
//! for its best mouth, 13.6% and 26.4% for its vowel-only one. These lines are synthetic, so the numbers are not the same
//! corpus; the floors here hold the shape of the result: closures caught, vowels right, nothing worse than before.

use omuzaki_face_api::expression::ExpressionSet;
use omuzaki_track_voice::align::align;
use omuzaki_track_voice::eval::{LipSyncScore, frames_of, mic_mouth, per_hop, score};
use omuzaki_track_voice::synth::{Sound, utterance};
use omuzaki_track_voice::{LipSyncConfig, Phone, PhoneSpan, PhonemeMouth, Viseme};

const RATE: u32 = 16_000;

/// "ma, ba pa", "mio mee", "bu po sa", "abba", "toma", "pima": closures between every vowel kind, stops and nasals.
fn line() -> Vec<Sound> {
    use Sound::{Fricative, Nasal, Pause, Stop, Vowel};
    use Viseme::{Aa, Ee, Ih, Oh, Ou};
    let lips = |seconds| Stop { lips: true, seconds };
    let tongue = |seconds| Stop { lips: false, seconds };
    vec![
        Pause(0.3),
        Nasal(0.08),
        Vowel(Aa, 0.22),
        Pause(0.15),
        lips(0.07),
        Vowel(Aa, 0.18),
        lips(0.08),
        Vowel(Aa, 0.2),
        Pause(0.3),
        Nasal(0.08),
        Vowel(Ih, 0.15),
        Vowel(Oh, 0.2),
        Nasal(0.09),
        Vowel(Ee, 0.25),
        Pause(0.25),
        lips(0.07),
        Vowel(Ou, 0.18),
        lips(0.08),
        Vowel(Oh, 0.2),
        Fricative(0.12),
        Vowel(Aa, 0.2),
        Pause(0.3),
        Vowel(Aa, 0.15),
        lips(0.1),
        Vowel(Aa, 0.2),
        Pause(0.2),
        tongue(0.07),
        Vowel(Oh, 0.16),
        Nasal(0.08),
        Vowel(Aa, 0.2),
        Pause(0.2),
        lips(0.07),
        Vowel(Ih, 0.16),
        Nasal(0.08),
        Vowel(Aa, 0.22),
        Pause(0.4),
    ]
}

struct Run {
    vowel_only: LipSyncScore,
    closures: LipSyncScore,
    phonemes: LipSyncScore,
}

fn run() -> Run {
    let set = ExpressionSet::starter();
    let (pcm, truth) = utterance(&line(), 140.0, RATE, 0.5);
    let end = truth.last().map_or(0.0, |s| s.end_s);
    let frames = frames_of(&pcm, RATE).unwrap();
    let mic = |config| {
        let track = mic_mouth(&frames, &set, config);
        score(&truth, end, &per_hop(&track))
    };
    let vowel_only = mic(LipSyncConfig::default().with_closure(false));
    let closures = mic(LipSyncConfig::default());
    // The line's phones as a TTS engine knows them (no times), aligned to its audio.
    let phones: Vec<Phone> = truth.iter().map(|s| s.phone).filter(|p| *p != Phone::Silence).collect();
    let mouth = PhonemeMouth::new(align(&phones, &frames));
    let phonemes = score(&truth, end, &|t| mouth.frame(t));
    Run { vowel_only, closures, phonemes }
}

#[test]
fn closures_shut_the_microphone_mouth_without_shutting_its_vowels() {
    let Run { vowel_only, closures, .. } = run();
    println!("vowel-only mouth: {vowel_only:?}\nwith closures:    {closures:?}");
    assert!(closures.closure_recall >= 0.8, "closures caught: {closures:?}");
    assert!(closures.closure_recall > vowel_only.closure_recall + 0.3, "{vowel_only:?} then {closures:?}");
    assert!(closures.vowels_shut <= vowel_only.vowels_shut + 0.05, "vowels kept open: {closures:?}");
    assert!(closures.vowel_accuracy >= vowel_only.vowel_accuracy - 0.05, "{vowel_only:?} then {closures:?}");
    assert!(closures.silence >= 0.9, "{closures:?}");
    assert!(closures.offset_ms.abs() <= 40.0, "{closures:?}");
}

#[test]
fn the_mouth_from_a_line_s_own_phones_is_exact_where_the_words_are_known() {
    let Run { closures, phonemes, .. } = run();
    println!("from the phones: {phonemes:?}");
    assert!(phonemes.closure_recall >= 0.9, "{phonemes:?}");
    assert!(phonemes.vowel_accuracy >= 0.85, "{phonemes:?}");
    assert!(phonemes.vowel_accuracy >= closures.vowel_accuracy, "{closures:?} vs {phonemes:?}");
    assert!(phonemes.vowels_shut <= 0.05, "{phonemes:?}");
    assert!(phonemes.silence >= 0.9, "{phonemes:?}");
    assert!(phonemes.offset_ms.abs() <= 20.0, "{phonemes:?}");
}

#[test]
fn alignment_finds_the_spans_within_a_few_hops() {
    let (pcm, truth) = utterance(&line(), 140.0, RATE, 0.5);
    let frames = frames_of(&pcm, RATE).unwrap();
    let phones: Vec<Phone> = truth.iter().map(|s| s.phone).filter(|p| *p != Phone::Silence).collect();
    let got: Vec<PhoneSpan> = align(&phones, &frames).into_iter().filter(|s| s.phone != Phone::Silence).collect();
    let want: Vec<&PhoneSpan> = truth.iter().filter(|s| s.phone != Phone::Silence).collect();
    assert_eq!(got.len(), want.len());
    let mut errors: Vec<f64> = got.iter().zip(&want).map(|(g, w)| (g.start_s - w.start_s).abs()).collect();
    errors.sort_by(f64::total_cmp);
    let median = errors[errors.len() / 2];
    println!("alignment: median start error {:.0} ms, worst {:.0} ms", median * 1000.0, errors[errors.len() - 1] * 1000.0);
    assert!(median <= 0.03, "median start error {median} s");
}
