//! Turn-taking timing (AP-03, master checklist §11 "full-duplex conversational timing"): how soon the end of a turn is
//! detected, and how often a mid-turn pause is cut, for the fixed hangovers against the [`TurnTaker`].
//!
//! The conversation is synthetic and made in code: phrases of voiced syllables (a harmonic source at a pitch that
//! follows a contour) separated by pauses, with the words a recogniser would return for each phrase. The pitch
//! contours follow the cues the turn-taking literature reports for English (Gravano and Hirschberg 2011): statements
//! fall to the bottom of the speaker's range, questions rise, and a speaker who holds the floor through a pause stops on
//! a level pitch, often after "and" or "um". Two kinds of mid-turn pause are included on purpose:
//!
//! - **hesitations** (level pitch, "and", "um", 0.5 to 1.1 s): the fixed hangovers cut these;
//! - **clause pauses** (a complete, falling sentence, then another, 0.35 to 0.4 s): a place where a listener may
//!   take the turn (a transition-relevance place). Neither prosody nor words can tell these from an ending, and people
//!   often take the turn there too, so they are counted, not failed.
//!
//! The simulation is the host's own logic, frame by frame, with no threads: an energy VAD, the [`Segmenter`] and the
//! [`TurnTaker`], and the recogniser's words delivered [`ASR_MS`] after the speculative cut at [`SPECULATE_MS`]. The
//! numbers rank policies; real speech will move them.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::f32::consts::TAU;

use omuzaki_speech::SAMPLE_RATE;
use omuzaki_speech::segmenter::{Event, FRAME, Segmenter, SegmenterConfig};
use omuzaki_speech::turn::{TurnConfig, TurnTaker};

/// The speculative transcription's cut, ms into a pause.
const SPECULATE_MS: f32 = 96.0;
/// Whisper's time on a short utterance (an assumption for the estimate; `base.en` on a laptop CPU is in this range).
const ASR_MS: f32 = 300.0;
const FS: f32 = SAMPLE_RATE as f32;

/// One phrase: syllables, the pitch at its start and end (with the last syllable's own contour), and what follows.
struct Phrase {
    syllables: usize,
    /// Pitch at the start and at the last syllable's start, Hz.
    from_hz: f32,
    to_hz: f32,
    /// The last syllable's pitch at its end, Hz (a fall or rise inside it).
    end_hz: f32,
    /// The last syllable fades (a soft ending) instead of stopping at full strength.
    taper: bool,
    /// The words so far at the end of this phrase, as a recogniser would give them.
    words: &'static str,
    /// The pause after it, ms, and whether the turn ends there.
    pause_ms: f32,
    turn_ends: bool,
}

/// Where a pause is and what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pause {
    TurnEnd,
    Hesitation,
    Clause,
}

/// The conversation's phrases and their kinds: eight turns from one speaker, about a minute.
fn script() -> Vec<(Phrase, Pause)> {
    let p = |syllables, from_hz, to_hz, end_hz, taper, words, pause_ms, turn_ends| Phrase {
        syllables,
        from_hz,
        to_hz,
        end_hz,
        taper,
        words,
        pause_ms,
        turn_ends,
    };
    use Pause::{Clause, Hesitation, TurnEnd};
    vec![
        (p(8, 150.0, 130.0, 100.0, true, "I just got back from the market.", 2500.0, true), TurnEnd),
        (p(6, 145.0, 150.0, 200.0, false, "Have you tried the new noodle place?", 2500.0, true), TurnEnd),
        (p(5, 150.0, 140.0, 140.0, false, "So I went down to the", 700.0, false), Hesitation),
        (p(6, 140.0, 125.0, 100.0, true, "station, and it was closed.", 2500.0, true), TurnEnd),
        (p(4, 150.0, 140.0, 142.0, false, "And then, um...", 1100.0, false), Hesitation),
        (p(7, 145.0, 128.0, 100.0, true, "And then, um, I walked home instead.", 2500.0, true), TurnEnd),
        (p(5, 148.0, 128.0, 102.0, true, "The rain had stopped.", 400.0, false), Clause),
        (p(6, 140.0, 125.0, 100.0, true, "The rain had stopped. The sky was clear.", 2500.0, true), TurnEnd),
        (p(3, 150.0, 140.0, 140.0, false, "Well, I think", 600.0, false), Hesitation),
        (p(7, 145.0, 150.0, 210.0, false, "Well, I think we should go tomorrow, don't you?", 2500.0, true), TurnEnd),
        (p(2, 140.0, 125.0, 100.0, true, "Yes.", 2500.0, true), TurnEnd),
        (p(6, 150.0, 130.0, 104.0, true, "I liked the first one.", 350.0, false), Clause),
        (p(5, 140.0, 140.0, 141.0, false, "I liked the first one. But the second one was", 800.0, false), Hesitation),
        (
            p(5, 140.0, 125.0, 100.0, true, "I liked the first one. But the second one was better.", 2500.0, true),
            TurnEnd,
        ),
    ]
}

/// The audio, at 16 kHz, and each pause's span in samples with its kind and the words before it.
fn render(script: &[(Phrase, Pause)]) -> (Vec<f32>, Vec<(usize, usize, Pause, &'static str)>) {
    let syllable_s = 0.19f32;
    let mut out = vec![0.0f32; (0.5 * FS) as usize];
    let mut pauses = Vec::new();
    let mut phase = 0.0f32;
    for (phrase, kind) in script {
        let n = (syllable_s * FS) as usize;
        for k in 0..phrase.syllables {
            let last = k + 1 == phrase.syllables;
            let t0 = k as f32 / (phrase.syllables - 1).max(1) as f32;
            let start_hz = phrase.from_hz + (phrase.to_hz - phrase.from_hz) * t0;
            for i in 0..n {
                let u = i as f32 / n as f32;
                let hz = if last { start_hz + (phrase.end_hz - start_hz) * u } else { start_hz };
                // A syllable's loudness: a quick rise, a dip at the boundary; the last one tapers or stops.
                let env = if last && phrase.taper {
                    (u * 8.0).min(1.0) * (1.0 - u).powf(1.5)
                } else {
                    (u * 8.0).min(1.0) * (0.75 + 0.25 * (std::f32::consts::PI * u).sin())
                };
                phase += TAU * hz / FS;
                let mut s = 0.0f32;
                for h in 1..=12u8 {
                    let h = f32::from(h);
                    s += (phase * h).sin() / h;
                }
                out.push(0.12 * env * s);
            }
        }
        let start = out.len();
        out.extend(std::iter::repeat_n(0.0f32, (phrase.pause_ms / 1000.0 * FS) as usize));
        pauses.push((start, out.len(), *kind, phrase.words));
    }
    (out, pauses)
}

/// How a policy waits: a fixed hangover, or the turn taker with or without the words.
#[derive(Clone, Copy, Debug)]
enum Policy {
    Fixed(u32),
    Prosody,
    ProsodyAndWords,
}

/// What a run measured.
#[derive(Debug, Default)]
struct Timing {
    /// Ms from the end of speech to the end event, at each true turn end.
    end_delay_ms: Vec<f32>,
    /// Ms from the end of speech to the transcript, estimated with [`ASR_MS`] (and the speculative cut for the turn
    /// taker).
    text_ms: Vec<f32>,
    /// Mid-turn pauses that were cut, by kind, and how many there were.
    hesitations_cut: usize,
    hesitations: usize,
    clauses_cut: usize,
    clauses: usize,
    turn_ends_missed: usize,
}

fn energy_vad(frame: &[f32]) -> f32 {
    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
    if rms > 0.02 { 0.9 } else { 0.0 }
}

fn simulate(policy: Policy) -> Timing {
    let script = script();
    let (audio, pauses) = render(&script);
    let base = SegmenterConfig { min_silence_ms: 600, pad_ms: 0, ..SegmenterConfig::default() };
    let cfg = match policy {
        Policy::Fixed(ms) => SegmenterConfig { min_silence_ms: ms, ..base },
        _ => base,
    };
    let mut segmenter = Segmenter::new(cfg);
    let mut turn = TurnTaker::new(TurnConfig::default());
    let mut ends: Vec<usize> = Vec::new();
    let frame_of = |sample: usize| sample / FRAME;
    for (i, frame) in audio.chunks_exact(FRAME).enumerate() {
        let frame: &[f32; FRAME] = frame.try_into().unwrap();
        let p = energy_vad(frame);
        if !matches!(policy, Policy::Fixed(_)) {
            let open = segmenter.is_open();
            let speech = p >= if open { cfg.neg_threshold } else { cfg.threshold };
            // The words arrive `SPECULATE_MS + ASR_MS` into a pause.
            if matches!(policy, Policy::ProsodyAndWords)
                && let Some((.., words)) = pauses
                    .iter()
                    .find(|(start, ..)| frame_of(*start + ((SPECULATE_MS + ASR_MS) / 1000.0 * FS) as usize) == i)
            {
                turn.words(words);
            }
            let told = turn.frame(frame, open || speech, speech);
            segmenter.set_min_silence_ms(told.wait_ms);
        }
        if let Some(Event::End(_)) = segmenter.push(p) {
            ends.push((i + 1) * FRAME);
        }
    }
    let mut timing = Timing::default();
    for (start, end, kind, _) in &pauses {
        let cut = ends.iter().find(|e| **e > *start && **e <= *end);
        match kind {
            Pause::TurnEnd => match cut {
                Some(at) => {
                    let delay = (*at - start) as f32 / FS * 1000.0;
                    timing.end_delay_ms.push(delay);
                    let text = match policy {
                        Policy::Fixed(_) => delay + ASR_MS,
                        // The speculative cut at `SPECULATE_MS` is ready `ASR_MS` later, or at the end if that is later.
                        _ => delay.max(SPECULATE_MS + ASR_MS),
                    };
                    timing.text_ms.push(text);
                }
                None => timing.turn_ends_missed += 1,
            },
            Pause::Hesitation => {
                timing.hesitations += 1;
                timing.hesitations_cut += usize::from(cut.is_some());
            }
            Pause::Clause => {
                timing.clauses += 1;
                timing.clauses_cut += usize::from(cut.is_some());
            }
        }
    }
    timing
}

fn median(v: &[f32]) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[s.len() / 2]
}

#[test]
fn the_turn_taker_answers_sooner_and_cuts_fewer_hesitations() {
    let policies = [Policy::Fixed(600), Policy::Fixed(200), Policy::Prosody, Policy::ProsodyAndWords];
    println!(
        "| policy | end detected, median | transcript ready, median (est.) | hesitations cut | clause pauses cut | turn ends missed |"
    );
    println!("|---|---|---|---|---|---|");
    let mut runs = Vec::new();
    for policy in policies {
        let t = simulate(policy);
        println!(
            "| {policy:?} | {:.0} ms | {:.0} ms | {}/{} | {}/{} | {} |",
            median(&t.end_delay_ms),
            median(&t.text_ms),
            t.hesitations_cut,
            t.hesitations,
            t.clauses_cut,
            t.clauses,
            t.turn_ends_missed
        );
        runs.push(t);
    }
    let (fixed600, fixed200, _prosody, words) = (&runs[0], &runs[1], &runs[2], &runs[3]);
    // The fixed hangovers trade one failure for the other.
    assert!(fixed200.hesitations_cut >= 2, "{fixed200:?}");
    // (Measured from the end of the phrase's audio; the last syllable's taper falls under the VAD a little earlier.)
    assert!(median(&fixed600.end_delay_ms) >= 500.0, "{fixed600:?}");
    // The turn taker with the words: no hesitation cut, every turn end found, and the end found in under half the
    // 600 ms default's time.
    assert_eq!(words.turn_ends_missed, 0, "{words:?}");
    assert_eq!(words.hesitations_cut, 0, "{words:?}");
    assert!(median(&words.end_delay_ms) <= 300.0, "{words:?}");
    assert!(median(&words.text_ms) < median(&fixed600.text_ms) - 400.0, "{words:?} vs {fixed600:?}");
}

#[test]
fn backchannels_come_while_the_person_talks_and_not_too_often() {
    let (audio, _) = render(&script());
    let mut segmenter =
        Segmenter::new(SegmenterConfig { min_silence_ms: 600, pad_ms: 0, ..SegmenterConfig::default() });
    let mut turn = TurnTaker::new(TurnConfig::default());
    let mut cues = Vec::new();
    for (i, frame) in audio.chunks_exact(FRAME).enumerate() {
        let frame: &[f32; FRAME] = frame.try_into().unwrap();
        let p = energy_vad(frame);
        let open = segmenter.is_open();
        let speech = p >= if open { 0.35 } else { 0.5 };
        let told = turn.frame(frame, open || speech, speech);
        segmenter.set_min_silence_ms(told.wait_ms);
        let _ = segmenter.push(p);
        if told.backchannel {
            cues.push(i as f32 * FRAME as f32 / FS);
        }
    }
    println!("backchannel cues at {cues:.2?} s");
    assert!(cues.len() >= 3, "{cues:?}");
    let gaps: Vec<f32> = cues.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|g| *g >= 1.7), "{gaps:?}");
}
