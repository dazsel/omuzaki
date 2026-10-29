//! The voice of a short: the sound (48 kHz mono), when each line is spoken, and the mouth's voice frames.
//!
//! - **Silent**: each line lasts as long as it takes to say, and the mouth reads the text ([`text_frames`]): every
//!   vowel group of a word is a syllable that opens the mouth in its vowel's shape, and `p`, `b` and `m` close it.
//! - **File**: a recording of the whole script. Its spoken stretches are found by loudness and matched to the lines
//!   in order, and the mouth reads the sound through the same analyser as a live microphone
//!   (`omuzaki-track-voice`).
//!
//! Either way the result is a list of [`VoiceFrame`]s on the short's clock, which the offline perform loop hands to
//! the same [`LipSync`](omuzaki_track_voice::LipSync) the live loop uses.

use std::path::Path;

use omuzaki_track_voice::{VoiceAnalyzer, VoiceFrame, VowelProfile};

use crate::def::{LineDef, ShortDef, VoiceDef};
use crate::error::ShortError;

/// The rate of a short's sound.
pub const RATE: u32 = 48_000;
/// A text syllable's share spent on the vowel; the rest is the consonants around it.
const NUCLEUS_SHARE: f32 = 0.7;
/// How long a closed-lips consonant (`p`, `b`, `m`) shuts the mouth, seconds.
const CLOSURE_S: f32 = 0.07;
/// The step of the text mouth's frames, seconds (the analyser's hop is 10 ms too).
const TEXT_HOP_S: f32 = 0.01;
/// Spoken stretches closer than this are one stretch, seconds.
const JOIN_GAP_S: f32 = 0.22;
/// Spoken stretches shorter than this are noise, seconds.
const MIN_SPEECH_S: f32 = 0.12;

/// When a line is spoken.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Span {
    /// Seconds from the start of the short.
    pub start: f32,
    /// Seconds from the start of the short.
    pub end: f32,
}

impl Span {
    /// Seconds between start and end.
    #[must_use]
    pub fn len(&self) -> f32 {
        self.end - self.start
    }

    /// True when the span has no length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }
}

/// A short's voice, ready to render.
#[derive(Clone, Debug, Default)]
pub struct VoiceTrack {
    /// The sound at [`RATE`], mono; silence for a silent short. As long as the short.
    pub pcm: Vec<f32>,
    /// When each line is spoken, one per line.
    pub spans: Vec<Span>,
    /// The mouth's voice frames on the short's clock, in time order.
    pub frames: Vec<(f32, VoiceFrame)>,
    /// How long the short runs, seconds.
    pub duration: f32,
}

impl VoiceTrack {
    /// The newest voice frame at `t` seconds, or silence before the first.
    #[must_use]
    pub fn frame_at(&self, t: f32) -> VoiceFrame {
        let i = self.frames.partition_point(|(at, _)| *at <= t);
        i.checked_sub(1).and_then(|i| self.frames.get(i)).map(|(_, f)| *f).unwrap_or_default()
    }
}

/// Reads a WAV file into mono samples at [`RATE`].
///
/// # Errors
/// [`ShortError::Voice`] for a file that isn't a readable WAV.
pub fn read_wav(path: &Path) -> Result<Vec<f32>, ShortError> {
    let file = std::fs::File::open(path).map_err(|e| ShortError::Voice(format!("{}: {e}", path.display())))?;
    read_wav_from(std::io::BufReader::new(file), &path.display().to_string())
}

/// Reads WAV bytes from `reader` (`name` is for messages) into mono samples at [`RATE`].
///
/// # Errors
/// [`ShortError::Voice`] for bytes that aren't a readable WAV.
pub fn read_wav_from(reader: impl std::io::Read, name: &str) -> Result<Vec<f32>, ShortError> {
    let mut reader =
        hound::WavReader::new(reader).map_err(|e| ShortError::Voice(format!("{name} doesn't read as WAV: {e}")))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            #[allow(clippy::cast_precision_loss, reason = "bit depths are at most 32")]
            let scale = 1.0 / (1_i64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            #[allow(clippy::cast_precision_loss, reason = "a sample")]
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 * scale)).collect::<Result<_, _>>()
        }
    }
    .map_err(|e| ShortError::Voice(format!("{name} has unreadable samples: {e}")))?;
    #[allow(clippy::cast_precision_loss, reason = "a channel count")]
    let mono: Vec<f32> =
        samples.chunks(channels).map(|frame| frame.iter().sum::<f32>() / frame.len() as f32).collect();
    Ok(resample(&mono, spec.sample_rate, RATE))
}

/// `pcm` at `from` Hz, linearly resampled to `to` Hz.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn resample(pcm: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || pcm.is_empty() || from == 0 {
        return pcm.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let len = (pcm.len() as f64 / ratio).floor() as usize;
    (0..len)
        .map(|i| {
            let x = i as f64 * ratio;
            let j = x.floor() as usize;
            let frac = (x - j as f64) as f32;
            let a = pcm.get(j).copied().unwrap_or(0.0);
            let b = pcm.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

/// The voice for `def`, reading a recording through `read` (a path relative to the project, opened for reading).
///
/// # Errors
/// [`ShortError::Voice`] when the recording doesn't read, or holds no speech.
pub fn voice_for(def: &ShortDef, read: impl FnOnce(&str) -> Result<Vec<f32>, ShortError>) -> Result<VoiceTrack, ShortError> {
    match &def.voice {
        VoiceDef::Silent { words_per_minute } => Ok(silent(def, *words_per_minute)),
        VoiceDef::File { path } => {
            let pcm = read(path)?;
            from_recording(def, pcm)
        }
    }
}

/// How long `text` takes to say at `wpm` words per minute: its words, plus a breath at each comma or full stop.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "word counts")]
pub fn spoken_seconds(text: &str, wpm: f32) -> f32 {
    let words = text.split_whitespace().count() as f32;
    let breaks = text.chars().filter(|c| matches!(c, ',' | ';' | ':' | '.' | '!' | '?' | '…')).count() as f32;
    (words * 60.0 / wpm + 0.12 * breaks).max(0.45)
}

/// Lays the lines out one after another from `def.lead`: each starts at its `at` or after the line before and its
/// pause, and lasts its `duration` or `length(line)`.
fn lay_out(def: &ShortDef, mut length: impl FnMut(&LineDef) -> f32) -> Vec<Span> {
    let mut t = def.lead;
    def.lines
        .iter()
        .map(|line| {
            let start = line.at.unwrap_or(t);
            let end = start + line.duration.unwrap_or_else(|| length(line));
            t = end + line.pause;
            Span { start, end }
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a short is at most 600 s")]
fn samples(seconds: f32) -> usize {
    (seconds.max(0.0) * RATE as f32).round() as usize
}

fn silent(def: &ShortDef, wpm: f32) -> VoiceTrack {
    let spans = lay_out(def, |line| spoken_seconds(&line.spoken(), wpm));
    let duration = spans.iter().map(|s| s.end).fold(0.0, f32::max) + def.tail;
    let mut frames = Vec::new();
    let mut onsets = 0;
    for (line, span) in def.lines.iter().zip(&spans) {
        text_frames(&line.spoken(), *span, &mut onsets, &mut frames);
    }
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    VoiceTrack { pcm: vec![0.0; samples(duration)], spans, frames, duration }
}

/// A syllable of text: the viseme its vowel makes, whether a closed-lips consonant follows, and whether it is stressed.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Syllable {
    viseme: usize,
    closes: bool,
    stressed: bool,
}

/// The viseme index (`aa`, `ih`, `ou`, `ee`, `oh`) of a vowel letter.
fn viseme_of(c: char) -> Option<usize> {
    match c.to_ascii_lowercase() {
        'a' | 'ä' | 'à' | 'á' | 'â' => Some(0),
        'i' | 'y' | 'ì' | 'í' | 'î' => Some(1),
        'u' | 'ü' | 'ù' | 'ú' | 'û' | 'w' => Some(2),
        'e' | 'è' | 'é' | 'ê' | 'ë' => Some(3),
        'o' | 'ö' | 'ò' | 'ó' | 'ô' => Some(4),
        _ => None,
    }
}

fn syllables(text: &str) -> Vec<Syllable> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
        let shouted = letters.len() > 1 && letters.iter().all(|c| !c.is_lowercase());
        let first = out.len();
        let mut i = 0;
        while i < letters.len() {
            if let Some(viseme) = viseme_of(letters[i]) {
                let mut j = i;
                while j < letters.len() && viseme_of(letters[j]).is_some() {
                    j += 1;
                }
                let closes = letters[j..].iter().take_while(|c| viseme_of(**c).is_none()).any(|c| matches!(c.to_ascii_lowercase(), 'p' | 'b' | 'm'));
                out.push(Syllable { viseme, closes, stressed: false });
                i = j;
            } else {
                i += 1;
            }
        }
        if out.len() == first && !letters.is_empty() {
            // A word with no vowel letter ("hmm", "pst") still takes a beat.
            out.push(Syllable { viseme: 3, closes: true, stressed: false });
        }
        if let Some(s) = out.get_mut(first) {
            s.stressed = shouted || letters.len() > 6 || word.ends_with('!');
        }
    }
    out
}

/// Appends voice frames for `text` spoken over `span`: one syllable after another, the mouth open in each vowel's
/// shape for [`NUCLEUS_SHARE`] of it, shut for [`CLOSURE_S`] before a `p`, `b` or `m`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn text_frames(text: &str, span: Span, onsets: &mut u32, out: &mut Vec<(f32, VoiceFrame)>) {
    let syllables = syllables(text);
    if syllables.is_empty() || span.is_empty() {
        return;
    }
    let each = span.len() / syllables.len() as f32;
    let loud = if text.chars().filter(|c| c.is_alphabetic()).all(|c| !c.is_lowercase()) { 0.95 } else { 0.7 };
    let hops = (span.len() / TEXT_HOP_S).ceil() as usize;
    let mut seq = out.last().map_or(0, |(_, f)| f.seq);
    for h in 0..=hops {
        let t = span.start + h as f32 * TEXT_HOP_S;
        let k = (((t - span.start) / each) as usize).min(syllables.len() - 1);
        let s = syllables[k];
        let into = t - span.start - k as f32 * each;
        let voiced = into < each * NUCLEUS_SHARE && !(s.closes && into > each - CLOSURE_S) && t < span.end;
        if into < TEXT_HOP_S && s.stressed {
            *onsets += 1;
        }
        let mut visemes = [0.0; 5];
        if voiced {
            // A little of the neighbouring shape, as real vowels glide.
            visemes[s.viseme] = 0.85;
            visemes[(s.viseme + 1) % 5] = 0.1;
        }
        seq += 1;
        out.push((
            t,
            VoiceFrame {
                seq,
                speaking: t < span.end,
                voiced,
                loudness: if voiced { loud } else { loud * 0.4 },
                visemes,
                onsets: *onsets,
                formants: [0.0, 0.0],
            },
        ));
    }
    // A last silent frame closes the mouth after the line.
    seq += 1;
    out.push((span.end + TEXT_HOP_S, VoiceFrame { seq, onsets: *onsets, ..VoiceFrame::default() }));
}

/// The spoken stretches of `pcm` at [`RATE`]: 20 ms windows louder than a tenth of the loudest, joined across gaps
/// shorter than [`JOIN_GAP_S`], stretches shorter than [`MIN_SPEECH_S`] dropped.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn speech_spans(pcm: &[f32]) -> Vec<Span> {
    let window = (RATE / 50) as usize;
    let rms: Vec<f32> = pcm.chunks(window).map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt()).collect();
    let loudest = rms.iter().copied().fold(0.0, f32::max);
    if loudest <= 1e-5 {
        return Vec::new();
    }
    let threshold = loudest * 0.1;
    let seconds = |i: usize| i as f32 * window as f32 / RATE as f32;
    let mut spans: Vec<Span> = Vec::new();
    for (i, level) in rms.iter().enumerate() {
        if *level < threshold {
            continue;
        }
        let (start, end) = (seconds(i), seconds(i + 1));
        match spans.last_mut() {
            Some(last) if start - last.end < JOIN_GAP_S => last.end = end,
            _ => spans.push(Span { start, end }),
        }
    }
    spans.retain(|s| s.len() >= MIN_SPEECH_S);
    spans
}

/// Makes `spans` exactly `n` long: merges the pair with the shortest gap while there are too many, and splits the
/// longest stretch in two at its middle while there are too few.
fn fit_spans(mut spans: Vec<Span>, n: usize) -> Vec<Span> {
    while spans.len() > n && spans.len() > 1 {
        let i = (0..spans.len() - 1)
            .min_by(|&a, &b| (spans[a + 1].start - spans[a].end).total_cmp(&(spans[b + 1].start - spans[b].end)))
            .unwrap_or(0);
        let next = spans.remove(i + 1);
        spans[i].end = next.end;
    }
    while spans.len() < n {
        let Some(i) = (0..spans.len()).max_by(|&a, &b| spans[a].len().total_cmp(&spans[b].len())) else { break };
        let s = spans[i];
        let mid = s.start + s.len() * 0.5;
        spans[i].end = mid;
        spans.insert(i + 1, Span { start: mid, end: s.end });
    }
    spans
}

fn from_recording(def: &ShortDef, mut pcm: Vec<f32>) -> Result<VoiceTrack, ShortError> {
    let found = speech_spans(&pcm);
    if found.is_empty() {
        return Err(ShortError::Voice("the recording is silent: no speech found".into()));
    }
    let fitted = fit_spans(found, def.lines.len());
    let spans: Vec<Span> = def
        .lines
        .iter()
        .zip(fitted)
        .map(|(line, found)| match (line.at, line.duration) {
            (Some(start), Some(d)) => Span { start, end: start + d },
            (Some(start), None) => Span { start, end: start + found.len() },
            (None, Some(d)) => Span { start: found.start, end: found.start + d },
            (None, None) => found,
        })
        .collect();
    let end = spans.iter().map(|s| s.end).fold(0.0, f32::max) + def.tail;
    #[allow(clippy::cast_precision_loss)]
    let duration = end.max(pcm.len() as f32 / RATE as f32);
    pcm.resize(samples(duration), 0.0);
    let mut analyzer = VoiceAnalyzer::new(RATE, VowelProfile::default()).map_err(|e| ShortError::Voice(e.to_string()))?;
    let mut frames = Vec::new();
    let block = (RATE / 100) as usize;
    for (i, chunk) in pcm.chunks(block).enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = (i + 1) as f32 * block as f32 / RATE as f32;
        analyzer.push(chunk, |frame| frames.push((t, *frame)));
    }
    Ok(VoiceTrack { pcm, spans, frames, duration })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(lines: &[&str]) -> ShortDef {
        let mut text = String::from("character = \"lumi\"\n");
        for line in lines {
            text.push_str(&format!("[[line]]\ntext = \"{line}\"\n"));
        }
        ShortDef::from_toml(&text).unwrap()
    }

    #[test]
    fn silent_lines_follow_each_other_with_their_pauses() {
        let def = short(&["One two three four.", "Five!"]);
        let voice = voice_for(&def, |_| unreachable!()).unwrap();
        assert_eq!(voice.spans.len(), 2);
        assert!((voice.spans[0].start - def.lead).abs() < 1e-6);
        assert!((voice.spans[1].start - (voice.spans[0].end + 0.25)).abs() < 1e-5);
        assert!((voice.duration - (voice.spans[1].end + def.tail)).abs() < 1e-5);
        #[allow(clippy::cast_precision_loss)]
        let pcm_s = voice.pcm.len() as f32 / RATE as f32;
        assert!((pcm_s - voice.duration).abs() < 1e-3);
    }

    #[test]
    fn the_text_mouth_opens_on_vowels_and_shuts_after_the_line() {
        let def = short(&["Papa mama"]);
        let voice = voice_for(&def, |_| unreachable!()).unwrap();
        let span = voice.spans[0];
        let open = voice.frames.iter().filter(|(t, f)| *t < span.end && f.voiced).count();
        let shut = voice.frames.iter().filter(|(t, f)| *t < span.end && !f.voiced).count();
        assert!(open > 0 && shut > 0, "open {open}, shut {shut}");
        assert!(voice.frames.iter().filter(|(_, f)| f.voiced).all(|(_, f)| f.visemes[0] > 0.5), "a's are aa");
        assert!(!voice.frame_at(span.end + 0.5).speaking);
    }

    #[test]
    fn recorded_stretches_fit_the_line_count() {
        let tone = |seconds: f32| -> Vec<f32> {
            (0..samples(seconds)).map(|i| (i as f32 * 0.05).sin() * 0.5).collect()
        };
        let quiet = |seconds: f32| vec![0.0_f32; samples(seconds)];
        let pcm: Vec<f32> = [quiet(0.3), tone(0.8), quiet(0.5), tone(1.2), quiet(0.6), tone(0.5), quiet(0.4)].concat();
        let found = speech_spans(&pcm);
        assert_eq!(found.len(), 3);
        assert!((found[0].start - 0.3).abs() < 0.03 && (found[0].end - 1.1).abs() < 0.03, "{found:?}");
        assert_eq!(fit_spans(found.clone(), 2).len(), 2);
        assert_eq!(fit_spans(found, 5).len(), 5);
        let voice = from_recording(&short(&["a", "b", "c"]), pcm).unwrap();
        assert_eq!(voice.spans.len(), 3);
        assert!(voice.frames.iter().any(|(_, f)| f.speaking));
    }
}
