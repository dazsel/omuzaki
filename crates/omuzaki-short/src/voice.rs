//! The voice of a short: the sound (48 kHz mono), when each line is spoken, and the mouth's voice frames.
//!
//! - **Silent**: each line lasts as long as it takes to say, and the mouth reads the text ([`text_frames`]): every
//!   vowel group of a word is a syllable that opens the mouth in its vowel's shape, and `p`, `b` and `m` close it.
//! - **File**: a recording of the whole script. Its spoken stretches are found by loudness and matched to the lines
//!   in order, and the mouth reads the sound through the same analyser as a live microphone
//!   (`omuzaki-track-voice`).
//! - **Speech**: each line said by a [`Speaker`] (the project's local text-to-speech in the ops). Lines are laid out
//!   with their real synthesised lengths, their sound placed at their spans, and the mouth reads the sound through the
//!   same analyser. A cheap plan may [estimate](Synthesis::Estimate) instead: lines timed from their words at the
//!   voice's speed, the mouth from the text, no sound.
//!
//! Every way, the result the result is a list of [`VoiceFrame`]s on the short's clock, which the offline perform loop hands to
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
/// About the pace of the local voices at speed 1, words per minute: what an estimate times a spoken line by. Kokoro's
/// `af_heart` said two stream lines at 200 to 240 words a minute (`tests/speech_voice.rs`); the lower end keeps an
/// estimate from promising a shorter short than the render makes.
pub const SPEECH_WPM: f32 = 200.0;
/// The fade at the end of a line's sound cut short by its `duration`, seconds.
const CUT_FADE_S: f32 = 0.01;

/// One line's speech, as a [`Speaker`] says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spoken {
    /// Mono samples at `rate`, with no long silence before or after the words.
    pub pcm: Vec<f32>,
    /// Samples per second.
    pub rate: u32,
}

/// Says a line in a voice: the project's local text-to-speech in the ops, a stub in tests.
pub trait Speaker {
    /// `text` said in `voice` (`<model>/<voice>`, as `voice.list` names it) at `speed` (1 is the voice's own pace).
    ///
    /// # Errors
    /// [`ShortError::Voice`] when the voice can't be found or the synthesis fails.
    fn speak(&mut self, text: &str, voice: &str, speed: f32) -> Result<Spoken, ShortError>;
}

/// How a spoken short ([`VoiceDef::Speech`]) gets its sound; silent and recorded shorts ignore it.
pub enum Synthesis<'a> {
    /// Each line is said by the speaker.
    Speak(&'a mut dyn Speaker),
    /// Nothing is said: each line is timed from its words at [`SPEECH_WPM`] times the voice's speed and the mouth reads
    /// the text, as for a silent short. For a cheap plan.
    Estimate,
}

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
    let mono: Vec<f32> = samples.chunks(channels).map(|frame| frame.iter().sum::<f32>() / frame.len() as f32).collect();
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

/// The voice for `def`, reading a recording through `read` (a path relative to the project, opened for reading) and
/// saying spoken lines as `synthesis` says.
///
/// # Errors
/// [`ShortError::Voice`] when the recording doesn't read or holds no speech, or the speaker fails.
pub fn voice_for(
    def: &ShortDef,
    read: impl FnOnce(&str) -> Result<Vec<f32>, ShortError>,
    synthesis: Synthesis<'_>,
) -> Result<VoiceTrack, ShortError> {
    match &def.voice {
        VoiceDef::Silent { words_per_minute } => Ok(silent(def, *words_per_minute)),
        VoiceDef::File { path } => {
            let pcm = read(path)?;
            from_recording(def, pcm)
        }
        VoiceDef::Speech { voice, speed } => match synthesis {
            Synthesis::Speak(speaker) => spoken(def, voice, *speed, speaker),
            Synthesis::Estimate => Ok(silent(def, SPEECH_WPM * speed)),
        },
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
fn lay_out(def: &ShortDef, mut length: impl FnMut(usize, &LineDef) -> f32) -> Vec<Span> {
    let mut t = def.lead;
    def.lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let start = line.at.unwrap_or(t);
            let end = start + line.duration.unwrap_or_else(|| length(i, line));
            t = end + line.pause;
            Span { start, end }
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a short is at most 600 s"
)]
fn samples(seconds: f32) -> usize {
    (seconds.max(0.0) * RATE as f32).round() as usize
}

fn silent(def: &ShortDef, wpm: f32) -> VoiceTrack {
    let spans = lay_out(def, |_, line| spoken_seconds(&line.spoken(), wpm));
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
                let closes = letters[j..]
                    .iter()
                    .take_while(|c| viseme_of(**c).is_none())
                    .any(|c| matches!(c.to_ascii_lowercase(), 'p' | 'b' | 'm'));
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
    let rms: Vec<f32> =
        pcm.chunks(window).map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt()).collect();
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
    let frames = mouth(&pcm)?;
    Ok(VoiceTrack { pcm, spans, frames, duration })
}

/// The mouth's voice frames for `pcm` at [`RATE`]: the live microphone's analyser, fed 10 ms blocks, each frame at the
/// end of its block.
fn mouth(pcm: &[f32]) -> Result<Vec<(f32, VoiceFrame)>, ShortError> {
    let mut analyzer =
        VoiceAnalyzer::new(RATE, VowelProfile::default()).map_err(|e| ShortError::Voice(e.to_string()))?;
    let mut frames = Vec::new();
    let block = (RATE / 100) as usize;
    for (i, chunk) in pcm.chunks(block).enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = (i + 1) as f32 * block as f32 / RATE as f32;
        analyzer.push(chunk, |frame| frames.push((t, *frame)));
    }
    Ok(frames)
}

/// Says every line of `def` in `voice` at `speed`, lays the lines out with the lengths of their sound, and places each
/// line's sound at its span: from its start, cut (with a short fade) at its end when a `duration` makes it shorter
/// than the sound, mixed where an `at` makes lines overlap.
fn spoken(def: &ShortDef, voice: &str, speed: f32, speaker: &mut dyn Speaker) -> Result<VoiceTrack, ShortError> {
    let mut said = Vec::with_capacity(def.lines.len());
    for (i, line) in def.lines.iter().enumerate() {
        let text = line.spoken();
        let spoken = speaker.speak(&text, voice, speed)?;
        let pcm = resample(&spoken.pcm, spoken.rate, RATE);
        if pcm.is_empty() {
            return Err(ShortError::Voice(format!("line[{i}]: {voice} said nothing for {text:?}")));
        }
        said.push(pcm);
    }
    #[allow(clippy::cast_precision_loss, reason = "a line is seconds long")]
    let spans = lay_out(def, |i, _| said.get(i).map_or(0.0, |pcm| pcm.len() as f32 / RATE as f32));
    let duration = spans.iter().map(|s| s.end).fold(0.0, f32::max) + def.tail;
    let mut pcm = vec![0.0; samples(duration)];
    for (sound, span) in said.iter().zip(&spans) {
        place(&mut pcm, sound, *span);
    }
    let frames = mouth(&pcm)?;
    Ok(VoiceTrack { pcm, spans, frames, duration })
}

/// Mixes `sound` into `pcm` from `span`'s start, no longer than the span, fading out over [`CUT_FADE_S`] where it is
/// cut short.
#[allow(clippy::cast_precision_loss, reason = "fade positions")]
fn place(pcm: &mut [f32], sound: &[f32], span: Span) {
    let start = samples(span.start);
    let len = sound.len().min(samples(span.len())).min(pcm.len().saturating_sub(start));
    let fade = if len < sound.len() { samples(CUT_FADE_S).min(len) } else { 0 };
    let Some(out) = pcm.get_mut(start..start + len) else { return };
    for (i, (out, s)) in out.iter_mut().zip(sound).enumerate() {
        let left = len - i;
        let gain = if left <= fade { left as f32 / fade.max(1) as f32 } else { 1.0 };
        *out = (*out + s * gain).clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn short(lines: &[&str]) -> ShortDef {
        let mut text = String::from("character = \"lumi\"\n");
        for line in lines {
            let _ = writeln!(text, "[[line]]\ntext = \"{line}\"");
        }
        ShortDef::from_toml(&text).unwrap()
    }

    #[test]
    fn silent_lines_follow_each_other_with_their_pauses() {
        let def = short(&["One two three four.", "Five!"]);
        let voice = voice_for(&def, |_| unreachable!(), Synthesis::Estimate).unwrap();
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
        let voice = voice_for(&def, |_| unreachable!(), Synthesis::Estimate).unwrap();
        let span = voice.spans[0];
        let open = voice.frames.iter().filter(|(t, f)| *t < span.end && f.voiced).count();
        let shut = voice.frames.iter().filter(|(t, f)| *t < span.end && !f.voiced).count();
        assert!(open > 0 && shut > 0, "open {open}, shut {shut}");
        assert!(voice.frames.iter().filter(|(_, f)| f.voiced).all(|(_, f)| f.visemes[0] > 0.5), "a's are aa");
        assert!(!voice.frame_at(span.end + 0.5).speaking);
    }

    /// Says every line as a 300 Hz tone at 24 kHz, 50 ms per spoken character, and remembers what it was asked.
    #[derive(Default)]
    struct Stub {
        asked: Vec<(String, String, f32)>,
    }

    const STUB_RATE: u32 = 24_000;

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn stub_seconds(text: &str, speed: f32) -> f32 {
        // Whole samples at the stub's rate, as the stub makes them.
        let n = (text.chars().count() as f32 * 0.05 / speed * STUB_RATE as f32).round();
        n / STUB_RATE as f32
    }

    impl Speaker for Stub {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        fn speak(&mut self, text: &str, voice: &str, speed: f32) -> Result<Spoken, ShortError> {
            self.asked.push((text.to_owned(), voice.to_owned(), speed));
            if text.contains("fail") {
                return Err(ShortError::Voice("the stub won't".into()));
            }
            let n = (stub_seconds(text, speed) * STUB_RATE as f32).round() as usize;
            let pcm =
                (0..n).map(|i| (std::f32::consts::TAU * 300.0 * i as f32 / STUB_RATE as f32).sin() * 0.5).collect();
            Ok(Spoken { pcm, rate: STUB_RATE })
        }
    }

    fn spoken_short(lines: &str) -> ShortDef {
        let text = format!(
            "character = \"lumi\"\n[voice]\nkind = \"speech\"\nvoice = \"kokoro-82m/af_heart\"\nspeed = 1.25\n{lines}"
        );
        ShortDef::from_toml(&text).unwrap()
    }

    fn rms(pcm: &[f32], from: f32, to: f32) -> f32 {
        let part = &pcm[samples(from).min(pcm.len())..samples(to).min(pcm.len())];
        #[allow(clippy::cast_precision_loss)]
        let n = part.len().max(1) as f32;
        (part.iter().map(|s| s * s).sum::<f32>() / n).sqrt()
    }

    #[test]
    fn spoken_lines_last_as_long_as_their_sound_and_follow_with_their_pauses() {
        let def = spoken_short(
            "[[line]]\ntext = \"Hello {surprised} there!\"\n[[line]]\ntext = \"Second line.\"\npause = 0.5\n\
             [[line]]\ntext = \"Third.\"",
        );
        let mut stub = Stub::default();
        let voice = voice_for(&def, |_| unreachable!(), Synthesis::Speak(&mut stub)).unwrap();
        let said: Vec<&str> = stub.asked.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(said, ["Hello there!", "Second line.", "Third."], "the beats are not said");
        assert!(stub.asked.iter().all(|(_, v, s)| v == "kokoro-82m/af_heart" && (*s - 1.25).abs() < 1e-6));
        let [a, b, c] = voice.spans[..] else { panic!("{:?}", voice.spans) };
        assert!((a.start - def.lead).abs() < 1e-6);
        for (span, text) in [(a, "Hello there!"), (b, "Second line."), (c, "Third.")] {
            assert!((span.len() - stub_seconds(text, 1.25)).abs() < 1e-3, "{span:?} for {text:?}");
        }
        assert!((b.start - (a.end + def.lines[0].pause)).abs() < 1e-5);
        assert!((c.start - (b.end + 0.5)).abs() < 1e-5);
        assert!((voice.duration - (c.end + def.tail)).abs() < 1e-5);
        #[allow(clippy::cast_precision_loss)]
        let pcm_s = voice.pcm.len() as f32 / RATE as f32;
        assert!((pcm_s - voice.duration).abs() < 1e-3);
        // The sound is where the spans are, resampled to the short's rate, and quiet between them.
        assert!(rms(&voice.pcm, a.start + 0.02, a.end - 0.02) > 0.3);
        assert!(rms(&voice.pcm, b.end + 0.02, c.start - 0.02) < 1e-4);
        assert!(rms(&voice.pcm, 0.0, a.start - 0.01) < 1e-4);
        // The mouth reads the sound: speaking inside the lines, closed in the pause.
        assert!(voice.frame_at(b.start + b.len() * 0.5).speaking);
        assert!(!voice.frame_at(c.start - 0.05).voiced);
    }

    #[test]
    fn a_spoken_line_keeps_its_own_start_and_length() {
        let def = spoken_short(
            "[[line]]\ntext = \"A rather long first line of words.\"\nduration = 0.5\n\
             [[line]]\ntext = \"Later.\"\nat = 3.0",
        );
        let voice = voice_for(&def, |_| unreachable!(), Synthesis::Speak(&mut Stub::default())).unwrap();
        let [a, b] = voice.spans[..] else { panic!("{:?}", voice.spans) };
        assert!((a.len() - 0.5).abs() < 1e-6 && (b.start - 3.0).abs() < 1e-6, "{:?}", voice.spans);
        assert!((b.len() - stub_seconds("Later.", 1.25)).abs() < 1e-3);
        // The first line's sound is cut at its end, faded rather than clicked off.
        assert!(rms(&voice.pcm, a.end + 0.005, a.end + 0.5) < 1e-4);
        let edge = samples(a.end) - 1;
        assert!(voice.pcm[edge].abs() < 0.01, "faded out: {}", voice.pcm[edge]);
    }

    #[test]
    fn an_estimate_times_spoken_lines_from_their_words_at_the_voice_speed() {
        let def = spoken_short("[[line]]\ntext = \"One two three four five six.\"");
        let voice = voice_for(&def, |_| unreachable!(), Synthesis::Estimate).unwrap();
        let expected = spoken_seconds("One two three four five six.", SPEECH_WPM * 1.25);
        assert!((voice.spans[0].len() - expected).abs() < 1e-5);
        assert!(voice.pcm.iter().all(|s| *s == 0.0), "an estimate says nothing");
        assert!(voice.frames.iter().any(|(_, f)| f.voiced), "the mouth reads the text");
    }

    #[test]
    fn a_speaker_failure_is_the_voice_error() {
        let def = spoken_short("[[line]]\ntext = \"This will fail.\"");
        let err = voice_for(&def, |_| unreachable!(), Synthesis::Speak(&mut Stub::default())).unwrap_err();
        assert!(matches!(err, ShortError::Voice(_)), "{err}");
    }

    #[test]
    fn recorded_stretches_fit_the_line_count() {
        let tone = |seconds: f32| -> Vec<f32> {
            #[allow(clippy::cast_precision_loss, reason = "a sample index")]
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
