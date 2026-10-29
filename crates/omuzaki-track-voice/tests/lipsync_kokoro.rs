//! The lip-sync evaluation on real speech (#2351): Kokoro-82M's own voice, scored against the spans its duration
//! predictor gave each phoneme, the ground truth open-annie scored against. `tools/lipsync-eval/` makes the corpus
//! (its README has the steps); set `OMUZAKI_LIPSYNC_CORPUS` to its folder. Without it the test says so and passes.
//!
//! Four mouths, each over every line of the corpus:
//! - the microphone mouth without closures (the analyser's vowels alone, before #2351);
//! - the microphone mouth with closures (the analyser's stop gaps and nasal murmurs);
//! - the line's own phones, aligned to its audio, as the voice lead shows them: through `LipSync`, frame by frame,
//!   each frame shown from the moment its window ends, so the smoothing and the latency are in the score;
//! - the same phones on the truth's own spans, the most any phone mouth can score here.
//!
//! The bar is open-annie's best: 40.9% closure recall and 34.6% five-vowel accuracy (chance is 20%).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)] // sample and step counts are small and positive

use std::path::{Path, PathBuf};

use omuzaki_face_api::expression::ExpressionSet;
use omuzaki_track_voice::align::align;
use omuzaki_track_voice::analyzer::{HOP_S, WINDOW_S};
use omuzaki_track_voice::eval::{LipSyncScore, frames_of, mic_mouth, per_hop, score};
use omuzaki_track_voice::lead::PHONE_LEAD_S;
use omuzaki_track_voice::phoneme::phones;
use omuzaki_track_voice::{LipSyncConfig, MouthFrame, Phone, PhoneSpan, PhonemeMouth, VoiceFrame};

/// The silence put between lines when they are scored as one recording.
const GAP_S: f64 = 0.4;

/// One line of the corpus: its audio, its phonemes and the truth's spans, from its start.
struct Line {
    pcm: Vec<f32>,
    rate: u32,
    phonemes: String,
    truth: Vec<PhoneSpan>,
}

/// The truth's spans from the tokens Kokoro gave durations: a diphthong's frames split between its two vowels, a
/// stress or length mark's frames go to the phone after it, a space's to the phone before it (a word break is not a
/// pause), punctuation is a pause, and the audio before the first token and after the last is silence.
fn truth(tokens: &[(char, f64, f64)], end_s: f64) -> Vec<PhoneSpan> {
    let mut spans: Vec<PhoneSpan> = Vec::new();
    let mut carry: Option<f64> = None;
    let first = tokens.first().map_or(0.0, |t| t.1);
    if first > 0.0 {
        spans.push(PhoneSpan { phone: Phone::Silence, start_s: 0.0, end_s: first });
    }
    for &(c, start, end) in tokens {
        let start = carry.take().unwrap_or(start);
        if c.is_whitespace() {
            if let Some(last) = spans.last_mut() {
                last.end_s = end;
            }
            continue;
        }
        if c.is_ascii_punctuation() || "—…“”".contains(c) {
            spans.push(PhoneSpan { phone: Phone::Silence, start_s: start, end_s: end });
            continue;
        }
        let these = phones(&c.to_string());
        if these.is_empty() {
            carry = Some(start);
            continue;
        }
        let step = (end - start) / these.len() as f64;
        for (i, phone) in these.into_iter().enumerate() {
            let s = start + step * i as f64;
            spans.push(PhoneSpan { phone, start_s: s, end_s: s + step });
        }
    }
    let last = spans.last().map_or(0.0, |s| s.end_s);
    if end_s > last {
        spans.push(PhoneSpan { phone: Phone::Silence, start_s: last, end_s });
    }
    spans
}

fn read_line(json: &Path) -> Line {
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
    let rate = u32::try_from(record["rate"].as_u64().unwrap()).unwrap();
    let bytes = std::fs::read(json.with_extension("f32")).unwrap();
    let pcm: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let tokens: Vec<(char, f64, f64)> = record["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let c = t[0].as_str().unwrap().chars().next().unwrap();
            (c, t[1].as_f64().unwrap(), t[2].as_f64().unwrap())
        })
        .collect();
    let end_s = pcm.len() as f64 / f64::from(rate);
    Line { pcm, rate, phonemes: record["phonemes"].as_str().unwrap().to_owned(), truth: truth(&tokens, end_s) }
}

fn corpus(dir: &Path) -> Vec<Line> {
    let mut jsons: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    jsons.sort();
    jsons.iter().map(|p| read_line(p)).collect()
}

/// The step of the sync search, seconds.
const SYNC_STEP_S: f64 = 0.005;
/// The furthest the durations may run from the audio, seconds either way.
const SYNC_MAX_S: f64 = 0.1;

/// How far the duration predictor's spans run behind the sound itself, seconds (negative: ahead): the lag that best
/// lines the truth's vowels up with the audio's loudness, over every line. A duration frame is 25 ms and the decoder
/// has its own delay, so the spans and the sound need not start together; the mouth is judged against the sound.
fn sync(lines: &[Line]) -> f64 {
    let steps = (SYNC_MAX_S / SYNC_STEP_S).round() as i64;
    let mut totals = vec![0.0f64; (2 * steps + 1) as usize];
    for line in lines {
        let hop = (SYNC_STEP_S * f64::from(line.rate)).round() as usize;
        let level: Vec<f64> = line
            .pcm
            .chunks(hop)
            .map(|c| (c.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / c.len() as f64).sqrt().max(1e-4).ln())
            .collect();
        let vowel: Vec<f64> = (0..level.len())
            .map(|i| {
                let t = (i as f64 + 0.5) * SYNC_STEP_S;
                let on = line.truth.iter().any(|s| matches!(s.phone, Phone::Vowel(_)) && s.start_s <= t && t < s.end_s);
                if on { 1.0 } else { 0.0 }
            })
            .collect();
        for (slot, lag) in totals.iter_mut().zip(-steps..=steps) {
            // The truth `lag` steps later than the sound: vowel[i + lag] against level[i].
            let pairs: Vec<(f64, f64)> = (0..level.len())
                .filter_map(|i| usize::try_from(i as i64 + lag).ok().and_then(|j| vowel.get(j)).map(|v| (*v, level[i])))
                .collect();
            *slot += correlation(&pairs);
        }
    }
    let best = totals.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(steps as usize, |(i, _)| i);
    (best as f64 - steps as f64) * SYNC_STEP_S
}

fn correlation(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len().max(1) as f64;
    let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (a, b) in pairs {
        ab += (a - ma) * (b - mb);
        aa += (a - ma).powi(2);
        bb += (b - mb).powi(2);
    }
    if aa > 0.0 && bb > 0.0 { ab / (aa * bb).sqrt() } else { 0.0 }
}

/// The four scores.
struct Scores {
    vowel_only: LipSyncScore,
    closures: LipSyncScore,
    phones: LipSyncScore,
    oracle: LipSyncScore,
    lines: usize,
    seconds: f64,
    closures_in_truth: usize,
    /// Each aligned phone's start against the truth's, seconds (lines whose phones match the truth's one for one).
    start_errors: Vec<f64>,
    /// How far the durations ran behind the sound, seconds; the truth was moved back by it.
    sync_s: f64,
}

/// Every line as one recording, `GAP_S` of silence after each, with the four mouths over it.
fn evaluate(lines: &[Line], sync_s: f64) -> Scores {
    let set = ExpressionSet::starter();
    let early =
        |s: &PhoneSpan| PhoneSpan { start_s: (s.start_s - sync_s).max(0.0), end_s: (s.end_s - sync_s).max(0.0), ..*s };
    let lines: Vec<Line> = lines
        .iter()
        .map(|l| Line {
            pcm: l.pcm.clone(),
            rate: l.rate,
            phonemes: l.phonemes.clone(),
            truth: l.truth.iter().map(early).collect(),
        })
        .collect();
    let rate = lines[0].rate;
    let gap = vec![0.0f32; (GAP_S * f64::from(rate)) as usize];
    let (mut pcm, mut truth, mut aligned, mut frames) = (Vec::new(), Vec::new(), Vec::new(), Vec::<VoiceFrame>::new());
    let mut start_errors = Vec::new();
    for line in &lines {
        assert_eq!(line.rate, rate, "one rate across the corpus");
        let offset = pcm.len() as f64 / f64::from(rate);
        let shift = |s: &PhoneSpan| PhoneSpan { start_s: s.start_s + offset, end_s: s.end_s + offset, ..*s };
        truth.extend(line.truth.iter().map(shift));
        // Each line is aligned on its own audio, as the voice lead aligns each TTS line.
        let own = frames_of(&line.pcm, rate).unwrap();
        let spans = align(&phones(&line.phonemes), &own);
        let voiced = |v: &[PhoneSpan]| v.iter().filter(|s| s.phone != Phone::Silence).copied().collect::<Vec<_>>();
        let (got, want) = (voiced(&spans), voiced(&line.truth));
        if got.len() == want.len() {
            start_errors.extend(got.iter().zip(&want).map(|(g, w)| g.start_s - w.start_s));
        }
        aligned.extend(spans.iter().map(shift));
        pcm.extend_from_slice(&line.pcm);
        let pause = PhoneSpan { phone: Phone::Silence, start_s: pcm.len() as f64 / f64::from(rate), end_s: 0.0 };
        pcm.extend_from_slice(&gap);
        truth.push(PhoneSpan { end_s: pcm.len() as f64 / f64::from(rate), ..pause });
    }
    frames.extend(frames_of(&pcm, rate).unwrap());
    let end = pcm.len() as f64 / f64::from(rate);
    let mic = |config| score(&truth, end, &per_hop(&mic_mouth(&frames, &set, config)));

    // The phones on the analyser's frames, each at the moment its window ends, then through LipSync.
    let mouth = PhonemeMouth::new(aligned);
    let phoned: Vec<VoiceFrame> = frames
        .iter()
        .enumerate()
        .map(|(k, f)| {
            let mut f = *f;
            mouth.frame(f64::from(WINDOW_S) + f64::from(HOP_S) * k as f64 + PHONE_LEAD_S).onto(&mut f);
            f
        })
        .collect();
    let phones_track = mic_mouth(&phoned, &set, LipSyncConfig::default());
    let oracle_mouth = PhonemeMouth::new(truth.clone());
    Scores {
        vowel_only: mic(LipSyncConfig::default().with_closure(false)),
        closures: mic(LipSyncConfig::default()),
        phones: score(&truth, end, &per_hop(&phones_track)),
        oracle: score(&truth, end, &|t| -> MouthFrame { oracle_mouth.frame(t) }),
        lines: lines.len(),
        seconds: end,
        closures_in_truth: truth.iter().filter(|s| s.phone.is_closure()).count(),
        start_errors,
        sync_s,
    }
}

fn row(name: &str, s: &LipSyncScore) -> String {
    format!(
        "| {name} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:+.0} ms |",
        100.0 * s.vowel_accuracy,
        100.0 * s.vowels_shut,
        100.0 * s.silence,
        100.0 * s.closure_recall,
        s.offset_ms
    )
}

#[test]
fn on_kokoro_speech_the_phone_mouth_beats_open_annie_s_bar() {
    let Some(dir) = std::env::var_os("OMUZAKI_LIPSYNC_CORPUS").map(PathBuf::from) else {
        eprintln!("OMUZAKI_LIPSYNC_CORPUS is not set: skipping the real-speech lip-sync evaluation");
        return;
    };
    let lines = corpus(&dir);
    assert!(!lines.is_empty(), "no lines in {}", dir.display());
    let measured = sync(&lines);
    let raw = evaluate(&lines, 0.0);
    let s = evaluate(&lines, measured);
    println!("{} lines, {:.1} s, {} lip closures", s.lines, s.seconds, s.closures_in_truth);
    let mut errors: Vec<f64> = s.start_errors.iter().map(|e| e.abs()).collect();
    errors.sort_by(f64::total_cmp);
    let mean = s.start_errors.iter().sum::<f64>() / s.start_errors.len().max(1) as f64;
    println!(
        "alignment against the sound: {} phones, median start error {:.0} ms, 90th percentile {:.0} ms, mean signed \
         {:+.0} ms",
        errors.len(),
        1000.0 * errors.get(errors.len() / 2).copied().unwrap_or(0.0),
        1000.0 * errors.get(errors.len() * 9 / 10).copied().unwrap_or(0.0),
        1000.0 * mean
    );
    for (title, scores) in [
        ("Against the durations as Kokoro gives them".to_owned(), &raw),
        (
            format!(
                "Against the sound (the durations run {:+.0} ms behind it, so the truth moves back by that)",
                1000.0 * s.sync_s
            ),
            &s,
        ),
    ] {
        println!(
            "\n{title}\n\n| Mouth | Vowels right | Vowels shut | Pauses shut | Closure recall | Offset |\n|---|---|---|---|---|---|"
        );
        for (name, score) in [
            ("Microphone, vowels only", &scores.vowel_only),
            ("Microphone, with closures", &scores.closures),
            ("The line's phones, aligned, through LipSync", &scores.phones),
            ("The phones on the truth's spans (ceiling)", &scores.oracle),
        ] {
            println!("{}", row(name, score));
        }
    }
    // open-annie's best mouth on Kokoro speech: 40.9% closure recall, 34.6% five-vowel accuracy.
    assert!(s.phones.closure_recall >= 0.409, "closure recall {:?}", s.phones);
    assert!(s.phones.vowel_accuracy >= 0.346, "vowel accuracy {:?}", s.phones);
    assert!(s.closures.closure_recall > s.vowel_only.closure_recall, "{:?} vs {:?}", s.closures, s.vowel_only);
    assert!(s.phones.offset_ms.abs() <= 40.0, "{:?}", s.phones);
}
