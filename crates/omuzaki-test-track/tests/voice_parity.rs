//! Voice parity on the real stack: Kokoro reads a fable that a LibriVox volunteer read (public domain, the speech
//! smoke tests' pinned clip), and `voice.compare`'s scoring measures ours against the reading, round after round,
//! adjusting the voice, the speed, the pitch and the pauses between sentences by what the measures say. Then the
//! pitch moves to TD-PSOLA (formants kept, `omuzaki_speech::prosody`) and each sentence gets its own pitch offset and
//! intonation range from the per-phrase measures (#2683): settings set by numbers, never the reading's contour.
//!
//! The reading is only measured against: nothing of it goes into ours (Kokoro speaks the text from its own voices; no
//! cloning, no resynthesis). There is one take of the reader, so there is no takes floor: the targets are the op's
//! defaults and the answer says so. A different speaker's timbre can't reach the reader's, so the mel-cepstral
//! distortion stays above its target; the test prints every measure as it stands and never calls that parity.
//!
//! Needs the speech folders and `OMUZAKI_TTS_MODELS` (see `speech_support`); skips with a message otherwise. With
//! `OMUZAKI_PARITY_OUT` set to a folder outside the repository, it writes each round's answer (JSON) and the last
//! round's spectrogram overlay (PNG) there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss, clippy::doc_markdown)]

mod speech_support;

use omuzaki_ai_api::ModelRegistry as _;
use omuzaki_speech::parity::ops::{Scored, score};
use omuzaki_speech::parity::overlay;
use omuzaki_speech::prosody::{Shape, reshape};
use omuzaki_speech::slots::clips::{CLIPS, decode_16k};
use omuzaki_speech::slots::tts::{TTS_RATE, TtsCall, Utterance, resample};
use omuzaki_speech::slots::{asr_slot, tts_slot};
use serde_json::Value;
use speech_support::{install, registry, tts_folders};

/// How ours is made in a round.
#[derive(Clone, Debug)]
struct Settings {
    voice: &'static str,
    speed: f32,
    pitch_semitones: f64,
    pause_s: f64,
    /// Pitch by TD-PSOLA (formants kept) instead of the declared rate.
    psola: bool,
    /// Per sentence: an extra pitch offset (semitones) and an intonation range; empty for none.
    lines: Vec<(f64, f64)>,
}

impl Settings {
    fn made_with(&self) -> String {
        let mut out = format!(
            "kokoro-82m/{}, speed {:.2}, pitch {:+.1} semitones{}, {:.0} ms between sentences",
            self.voice,
            self.speed,
            self.pitch_semitones,
            if self.psola { " (TD-PSOLA, formants kept)" } else { "" },
            self.pause_s * 1000.0
        );
        if !self.lines.is_empty() {
            let ranges: Vec<String> = self.lines.iter().map(|(p, r)| format!("{p:+.1}/{r:.2}")).collect();
            out = format!("{out}, per sentence pitch/range {}", ranges.join(" "));
        }
        out
    }

    fn line(&self, i: usize) -> (f64, f64) {
        self.lines.get(i).copied().unwrap_or((0.0, 1.0))
    }
}

/// The fable's sentences: split after `.`, `?`, `!` and `:` followed by a space or the end, and after a closing quote
/// that follows one.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        let next = chars.get(i + 1).copied();
        let ends = matches!(c, '.' | '?' | '!' | ':') && (next == Some(' ') || next.is_none());
        let quote_ends = *c == '"' && i > 0 && matches!(chars[i - 1], '.' | '?' | '!') && next == Some(' ');
        if ends || quote_ends {
            out.push(current.trim().to_owned());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// Ours at 16 kHz: each sentence spoken and pitched (by the declared rate, the TTS host's way, with the speed
/// compensating; or by TD-PSOLA), with the pauses between. Also each sentence's span in ours, in samples.
fn speak(
    call: &mut TtsCall,
    session: &mut dyn omuzaki_ai_api::Session,
    lines: &[String],
    s: &Settings,
) -> (Vec<f32>, Vec<(usize, usize)>) {
    let mut out = Vec::new();
    let mut spans = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let (extra, range) = s.line(i);
        let start = out.len();
        if s.psola {
            let pcm =
                call.speak(session, &Utterance { voice: s.voice, speed: s.speed, ..Utterance::plain(line) }).unwrap();
            let shape = Shape { semitones: s.pitch_semitones + extra, range, tempo: 1.0 };
            out.extend(resample(&reshape(pcm, TTS_RATE, &shape), TTS_RATE, 16_000));
        } else {
            let shift = 2f64.powf((s.pitch_semitones + extra) / 12.0);
            let speed = (f64::from(s.speed) / shift) as f32;
            let from = (f64::from(TTS_RATE) * shift).round() as u32;
            let pcm = call.speak(session, &Utterance { voice: s.voice, speed, ..Utterance::plain(line) }).unwrap();
            out.extend(resample(pcm, from, 16_000));
        }
        spans.push((start, out.len()));
        if i + 1 < lines.len() {
            out.extend(std::iter::repeat_n(0.0, (s.pause_s * 16_000.0) as usize));
        }
    }
    (out, spans)
}

/// Each sentence's pitch offset from the rest of the line (cents) and its range ratio, from the per-phrase measures:
/// a phrase counts for the sentence its middle falls in, weighted by its frames.
fn per_sentence(scored: &Scored, ours: &[f32], spans: &[(usize, usize)]) -> Vec<(Option<f64>, Option<f64>)> {
    let first = omuzaki_speech::parity::analyse(ours).speech_span().map_or(0, |(s, _)| s) as f64 * 10.0;
    let line_bias = scored.parity.measures.f0_bias_cents;
    let mut acc = vec![(0.0, 0.0, 0.0, 0.0); spans.len()];
    for seg in &scored.parity.segments {
        let middle = (first + seg.ours_ms[0].midpoint(seg.ours_ms[1])) * 16.0;
        let Some(i) = spans.iter().position(|(a, b)| (*a as f64..*b as f64).contains(&middle)) else { continue };
        let w = seg.frames as f64;
        acc[i].0 += (seg.f0_bias_cents - line_bias) * w;
        acc[i].1 += w;
        if let Some(r) = seg.f0_range_ratio {
            acc[i].2 += r.ln() * w;
            acc[i].3 += w;
        }
    }
    acc.iter().map(|&(b, wb, r, wr)| ((wb > 0.0).then(|| b / wb), (wr > 0.0).then(|| (r / wr).exp()))).collect()
}

fn metric(answer: &Value, name: &str) -> f64 {
    answer["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap()["value"].as_f64().unwrap()
}

/// One number for choosing between rounds: each measure over its target, summed.
fn badness(answer: &Value) -> f64 {
    answer["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["value"].as_f64().unwrap() / m["target"].as_f64().unwrap().max(1e-6))
        .sum()
}

#[test]
#[allow(clippy::too_many_lines, reason = "one study, round after round, read top to bottom")]
fn kokoro_is_measured_against_a_public_domain_reading_and_tuned_by_the_measures() {
    let Some(files) = tts_folders() else { return };
    let dir = std::env::temp_dir().join(format!("omuzaki-voice-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = files.0.clone();
    let registry = registry(files, &dir);
    // Kokoro's smoke test transcribes what it says, so Whisper goes in first.
    install(&registry, &asr_slot(), "whisper-tiny-en");
    let kokoro = install(&registry, &tts_slot(), "kokoro-82m");
    let mut session = registry.open(&tts_slot(), Some(&kokoro)).unwrap();
    let mut call = TtsCall::new();

    // The reading: the fable from its title on (the volunteer's own preamble before it isn't in the text).
    let clip = CLIPS[0];
    let name = clip.url.rsplit('/').next().unwrap();
    let path = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()).expect("the clip is in OMUZAKI_SPEECH_DATA");
    let audio = decode_16k(&path).unwrap();
    let from = ((clip.speech[clip.story].0 - 0.3).max(0.0) * 16_000.0) as usize;
    let reference = &audio[from..];
    let lines = sentences(clip.text);
    eprintln!("{} sentences: {lines:?}", lines.len());

    let out = std::env::var_os("OMUZAKI_PARITY_OUT").map(std::path::PathBuf::from);
    let mut s =
        Settings { voice: "af_heart", speed: 1.0, pitch_semitones: 0.0, pause_s: 0.4, psola: false, lines: Vec::new() };
    let mut rounds: Vec<(Settings, Value, Scored)> = Vec::new();
    let mut run = |s: &Settings, rounds: &mut Vec<(Settings, Value, Scored)>| {
        let (ours, spans) = speak(&mut call, session.as_mut(), &lines, s);
        let scored = score(&ours, reference, &[]).unwrap();
        let answer = scored.answer(&s.made_with(), &[]);
        eprintln!(
            "| {} | {:.2} | {:.0} | {:.0} | {:.2} | {:.1} | {:.0} | {:.0} | {:.2} |",
            s.made_with(),
            metric(&answer, "mcd_db"),
            metric(&answer, "f0_bias_cents"),
            metric(&answer, "f0_shape_cents"),
            metric(&answer, "f0_range_factor"),
            metric(&answer, "duration_error_pct"),
            metric(&answer, "timing_rms_ms"),
            metric(&answer, "phrase_onset_ms"),
            badness(&answer),
        );
        rounds.push((s.clone(), answer, scored));
        (ours, spans)
    };
    eprintln!("| ours | MCD dB | pitch bias c | intonation c | range × | duration % | rhythm ms | onset ms | total |");
    eprintln!("|---|---|---|---|---|---|---|---|---|");
    let (heart, _) = run(&s, &mut rounds);

    // Each voice at the starting settings; keep the closest.
    let others: Vec<(&str, Vec<f32>)> = ["af_bella", "am_michael", "bf_emma"]
        .into_iter()
        .map(|voice| (voice, run(&Settings { voice, ..s.clone() }, &mut rounds).0))
        .collect();
    s = rounds.iter().min_by(|a, b| badness(&a.1).total_cmp(&badness(&b.1))).map(|r| r.0.clone()).unwrap();

    // Then three rounds of following the measures: the pitch by its bias, the speed by the duration over the speech,
    // the pauses by how far the phrase onsets drift.
    for _ in 0..3 {
        let (_, answer, scored) = rounds.iter().find(|r| r.0.made_with() == s.made_with()).unwrap();
        let m = &scored.parity.measures;
        let bias = answer["measures"]["f0_bias_cents"].as_f64().unwrap();
        let drift: f64 = {
            let segs = &scored.parity.segments;
            if segs.len() > 1 {
                (segs[segs.len() - 1].offset_ms[0] - segs[0].offset_ms[0]) / (segs.len() - 1) as f64 / 1000.0
            } else {
                0.0
            }
        };
        s = Settings {
            pitch_semitones: (s.pitch_semitones - bias / 100.0).clamp(-4.0, 4.0),
            speed: (f64::from(s.speed) * m.duration_ratio).clamp(0.7, 1.4) as f32,
            pause_s: (s.pause_s - drift * 0.5).clamp(0.1, 1.2),
            ..s
        };
        run(&s, &mut rounds);
    }

    // The same settings with the pitch by TD-PSOLA: the formants stay Kokoro's.
    s = rounds.iter().skip(4).min_by(|a, b| badness(&a.1).total_cmp(&badness(&b.1))).map(|r| r.0.clone()).unwrap();
    s = Settings { psola: true, ..s };
    let (mut ours, mut spans) = run(&s, &mut rounds);

    // Then each sentence by its own phrases: its pitch offset from the rest of the line, its intonation range.
    for _ in 0..3 {
        let scored = &rounds.last().unwrap().2;
        let line_bias = scored.parity.measures.f0_bias_cents;
        let wanted = per_sentence(scored, &ours, &spans);
        let lines_now: Vec<(f64, f64)> = (0..lines.len()).map(|i| s.line(i)).collect();
        s = Settings {
            pitch_semitones: (s.pitch_semitones - line_bias / 100.0).clamp(-4.0, 4.0),
            lines: lines_now
                .iter()
                .zip(&wanted)
                .map(|(&(p, r), &(bias, ratio))| {
                    ((p - bias.unwrap_or(0.0) / 100.0).clamp(-3.0, 3.0), (r / ratio.unwrap_or(1.0)).clamp(0.3, 3.0))
                })
                .collect(),
            ..s
        };
        (ours, spans) = run(&s, &mut rounds);
    }

    // What the timbre measures read between takes of one voice (af_heart again at another speed) and between voices,
    // with no reference in it: where a same-speaker target has to sit in this feature space.
    let (again, _) = speak(&mut call, session.as_mut(), &lines, &Settings { speed: 1.15, ..rounds[0].0.clone() });
    let heart_f = omuzaki_speech::parity::analyse(&heart);
    eprintln!("| pair | MCD dB | spectral dB | voicing |\n|---|---|---|---|");
    let pairs =
        std::iter::once(("af_heart, another take at speed 1.15", &again)).chain(others.iter().map(|(v, a)| (*v, a)));
    for (what, other) in pairs {
        let m = omuzaki_speech::parity::compare(&omuzaki_speech::parity::analyse(other), &heart_f).unwrap().measures;
        eprintln!("| af_heart against {what} | {:.2} | {:.2} | {:.2} |", m.mcd_db, m.spectral_db, m.voicing_error);
    }

    let first = &rounds[0].1;
    let last = rounds.iter().min_by(|a, b| badness(&a.1).total_cmp(&badness(&b.1))).unwrap();
    eprintln!("closest: {}\n{}", last.0.made_with(), serde_json::to_string_pretty(&last.1).unwrap());
    if let Some(out) = &out {
        std::fs::create_dir_all(out).unwrap();
        for (i, (_, answer, _)) in rounds.iter().enumerate() {
            std::fs::write(out.join(format!("round-{i:02}.json")), serde_json::to_string_pretty(answer).unwrap())
                .unwrap();
        }
        let image = overlay(&last.2.parity);
        image::RgbImage::from_raw(image.width, image.height, image.rgb).unwrap().save(out.join("closest.png")).unwrap();
    }
    // The measures moved ours toward the reading on what the settings can reach: pitch and speed.
    assert!(metric(&last.1, "f0_bias_cents") <= metric(first, "f0_bias_cents").max(50.0));
    assert!(metric(&last.1, "duration_error_pct") <= metric(first, "duration_error_pct").max(5.0));
    assert!(badness(&last.1) <= badness(first));
    std::fs::remove_dir_all(&dir).unwrap();
}
