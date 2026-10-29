//! [`LeadAnalysis`]: the AI performer's voice through the same analyser as the person's microphone, so the stage's
//! existing voice path ([`LipSync`](crate::LipSync) and the life layer's speech cues) moves the character's mouth and
//! head while it talks.
//!
//! ```text
//!  TTS pump ──VoiceLead::push(at, pcm)──▶ VoiceAnalyzer ──(program time, frame)──▶ queue
//!                                                                                    │ poll(now): frames whose audio
//!  cut probe ──────────────────────────────────────────────────────────────────────▶ │ has played, never past a cut
//!                                                                                    ▼
//!                                                                    voice mailbox ──▶ perform thread (set_voice)
//! ```
//!
//! The pump hands over PCM about half a second before it plays; each 10 ms frame is stamped with the program time its
//! audio is predicted to play (the pump's `at` plus its offset in the PCM) and published only when that time comes,
//! so the mouth moves with the sound, not ahead of it. A cut drops every frame after it and closes the mouth; private
//! speech moves nothing (§4.3).
//!
//! **Phones** (#2351). When the TTS model reports the phonemes it spoke ([`VoiceLead::line`]), the line's phones are
//! aligned to its audio ([`align`](crate::align::align)) before its first sample is pushed, and every frame inside the
//! line takes its visemes and closure from the [`PhonemeMouth`] at the moment the frame's audio plays: the right vowel
//! shape, and lips shut on `p b m` even when the voice runs through them. Loudness, voicing and onsets still come from
//! the audio. A line without phonemes, or a model that reports none, keeps the analyser's own mouth. [`MouthDrive`](crate::MouthDrive) is the learned-model path that replaces this when a
//! `voice_to_face@1` model is installed; this one needs no model.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_performer_api::voice::{CutProbe, CutState, VoiceLead};

use crate::align::align;
use crate::analyzer::{HOP_S, VoiceAnalyzer, VoiceFrame};
use crate::mailbox::VoiceWriter;
use crate::phoneme::{PhonemeMouth, phones};
use crate::viseme::VowelProfile;

/// Frames kept ahead of playback: 4 s, far more than the pump's lead.
pub const QUEUE_FRAMES: usize = 400;
/// With nothing more to play for this long after the last frame, the mouth is closed.
pub const CLOSE_AFTER_S: f64 = 0.05;
/// Lines kept with their phones: the TTS host writes at most a few ahead of the one playing.
const LINES: usize = 8;
/// How far ahead of its frame's sound the phone mouth is read, seconds: the line is known in full, so the mouth can
/// start toward each shape early by [`LipSync`](crate::LipSync)'s attack and land on the sound.
pub const PHONE_LEAD_S: f64 = 0.03;

/// A line whose phones are known: feed samples `[start, end)` at `rate`, and its mouth.
struct Line {
    start: u64,
    end: u64,
    rate: u32,
    mouth: PhonemeMouth,
}

struct Inner {
    profile: VowelProfile,
    analyzer: Option<(u32, VoiceAnalyzer)>,
    /// `(program time, frame)`, rising.
    queue: VecDeque<(f64, VoiceFrame)>,
    writer: VoiceWriter,
    probe: Option<CutProbe>,
    seq: u64,
    /// The program time of the last frame published, and whether it was the closing one.
    last: Option<(f64, bool)>,
    /// The time the newest PCM ends, so the next push's frames never go before it.
    pcm_end: f64,
    /// The pump's last cut, until new PCM arrives.
    cut: Option<f64>,
    frames: u64,
    /// The lines with phones on the attached feed, oldest first.
    lines: VecDeque<Line>,
    /// Frames whose mouth came from a line's phones.
    phoned: u64,
}

/// The analysis of the AI's voice. Clone it: one clone goes to the TTS host as its [`VoiceLead`], another is polled by
/// the performer's thread.
#[derive(Clone)]
pub struct LeadAnalysis(Arc<Mutex<Inner>>);

impl LeadAnalysis {
    /// An analysis publishing into `writer`, classifying vowels with `profile`.
    pub fn new(writer: VoiceWriter, profile: VowelProfile) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            profile,
            analyzer: None,
            queue: VecDeque::new(),
            writer,
            probe: None,
            seq: 0,
            last: None,
            pcm_end: f64::NEG_INFINITY,
            cut: None,
            frames: 0,
            lines: VecDeque::new(),
            phoned: 0,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes the frames whose audio has played by `now`, newest last; drops what a cut silenced and closes the
    /// mouth when the voice has stopped.
    pub fn poll(&self, now: Seconds) {
        let mut inner = self.lock();
        let cut_at = match inner.probe.as_ref().map(CutProbe::state) {
            Some(CutState::Raised) => return,
            Some(CutState::Cut { at, .. }) => Some(at.0),
            _ => inner.cut,
        };
        if let Some(at) = cut_at {
            inner.queue.retain(|(t, _)| *t <= at);
        }
        let mut newest = None;
        while inner.queue.front().is_some_and(|(t, _)| *t <= now.0) {
            newest = inner.queue.pop_front();
        }
        if let Some((t, frame)) = newest {
            inner.publish(t, frame, false);
        }
        let closing = match inner.last {
            Some((t, false)) => cut_at.is_some_and(|at| at <= now.0) || now.0 - t >= CLOSE_AFTER_S,
            _ => false,
        };
        if closing && inner.queue.is_empty() {
            inner.publish(now.0, VoiceFrame::default(), true);
        }
    }

    /// Frames analysed so far.
    pub fn frames(&self) -> u64 {
        self.lock().frames
    }

    /// Frames whose visemes and closure came from their line's phones.
    pub fn phoned_frames(&self) -> u64 {
        self.lock().phoned
    }
}

impl Inner {
    fn publish(&mut self, t: f64, frame: VoiceFrame, closing: bool) {
        self.seq += 1;
        let frame = VoiceFrame { seq: self.seq, ..frame };
        self.writer.publish(&frame);
        self.last = Some((t, closing));
    }
}

impl VoiceLead for LeadAnalysis {
    fn attach(&mut self, probe: CutProbe) {
        let mut inner = self.lock();
        inner.probe = Some(probe);
        inner.queue.clear();
        inner.lines.clear();
        inner.pcm_end = f64::NEG_INFINITY;
    }

    /// Aligns the line's phones to its audio (outside the lock: a few milliseconds for a long line) and keeps them for
    /// the frames of `[feed_sample, feed_sample + pcm.len())`.
    fn line(&mut self, feed_sample: u64, rate: u32, pcm: &[f32], phonemes: &str) {
        let line = phones(phonemes);
        if line.is_empty() || pcm.is_empty() {
            return;
        }
        let profile = self.lock().profile;
        let Ok(mut analyzer) = VoiceAnalyzer::new(rate, profile) else { return };
        let mut frames = Vec::with_capacity(pcm.len() / 160 + 1);
        analyzer.push(pcm, |f| frames.push(*f));
        let mouth = PhonemeMouth::new(align(&line, &frames));
        let mut inner = self.lock();
        let end = feed_sample + pcm.len() as u64;
        inner.lines.retain(|l| l.end <= feed_sample || l.start >= end);
        inner.lines.push_back(Line { start: feed_sample, end, rate, mouth });
        while inner.lines.len() > LINES {
            inner.lines.pop_front();
        }
    }

    fn push(&mut self, at: Seconds, feed_sample: u64, rate: u32, pcm: &[f32], private: bool) {
        if private || pcm.is_empty() || rate == 0 {
            return;
        }
        let mut inner = self.lock();
        if inner.analyzer.as_ref().is_none_or(|(r, _)| *r != rate) {
            let profile = inner.profile;
            match VoiceAnalyzer::new(rate, profile) {
                Ok(a) => inner.analyzer = Some((rate, a)),
                Err(_) => return,
            }
        }
        inner.cut = None;
        let start = at.0.max(inner.pcm_end);
        // Hop-sized slices, so each frame is stamped within 10 ms of its audio.
        // Rates are at most 192 kHz: the hop is a few thousand samples.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let hop = ((rate as f32 * HOP_S).round() as usize).max(1);
        let mut produced = Vec::new();
        let mut offset = 0usize;
        let Inner { analyzer, lines, phoned, .. } = &mut *inner;
        if let Some((_, analyzer)) = analyzer.as_mut() {
            for slice in pcm.chunks(hop) {
                offset += slice.len();
                #[allow(clippy::cast_precision_loss)]
                let t = start + offset as f64 / f64::from(rate);
                // The feed sample playing at `t`, and the line it belongs to.
                let at_sample = feed_sample + offset as u64;
                let line = lines.iter().find(|l| l.rate == rate && l.start < at_sample && at_sample <= l.end);
                analyzer.push(slice, |frame| {
                    let mut frame = *frame;
                    if let Some(line) = line {
                        #[allow(clippy::cast_precision_loss)] // a line is seconds long
                        let into = (at_sample - line.start) as f64 / f64::from(rate);
                        line.mouth.frame(into + PHONE_LEAD_S).onto(&mut frame);
                        *phoned += 1;
                    }
                    produced.push((t, frame));
                });
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let end = start + pcm.len() as f64 / f64::from(rate);
        inner.pcm_end = end;
        inner.frames += produced.len() as u64;
        inner.queue.extend(produced);
        while inner.queue.len() > QUEUE_FRAMES {
            inner.queue.pop_front();
        }
    }

    fn cut(&mut self, at: Seconds) {
        let mut inner = self.lock();
        inner.lines.clear();
        inner.queue.retain(|(t, _)| *t <= at.0);
        inner.pcm_end = f64::NEG_INFINITY;
        inner.cut = Some(at.0);
    }
}

impl std::fmt::Debug for LeadAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("LeadAnalysis").field("queued", &inner.queue.len()).field("frames", &inner.frames).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::voice_mailbox;
    use crate::synth::{Vowel, vowel as synth_vowel};
    use crate::viseme::Viseme;

    fn vowel(seconds: f32) -> Vec<f32> {
        synth_vowel(Vowel::of(Viseme::Aa, 180.0, 1.0, 0.5), 48_000, seconds, 0.0)
    }

    #[test]
    fn frames_play_with_their_audio_and_the_mouth_closes_after() {
        let (writer, mut reader) = voice_mailbox();
        let lead = LeadAnalysis::new(writer, VowelProfile::default());
        let mut pump = lead.clone();
        // The pump writes 0.5 s of vowel predicted to play from 1.0 s.
        pump.push(Seconds(1.0), 0, 48_000, &vowel(0.5), false);
        lead.poll(Seconds(0.9));
        assert_eq!(reader.poll(), None, "nothing before its audio plays");
        lead.poll(Seconds(1.3));
        let speaking = reader.poll().unwrap();
        assert!(speaking.voiced && speaking.visemes[0] > 0.2, "{speaking:?}");
        lead.poll(Seconds(1.6));
        lead.poll(Seconds(1.7));
        let closed = reader.poll().unwrap();
        assert!(!closed.speaking && closed.visemes.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn a_line_s_phones_shut_the_lips_on_its_closures_and_shape_its_vowels() {
        use crate::phoneme::Phone;
        use crate::synth::{Sound, utterance};
        // "ma, bo pi": a nasal and two stops a voice runs through, each before another vowel.
        let lips = |seconds| Sound::Stop { lips: true, seconds };
        let sounds = [
            Sound::Pause(0.2),
            Sound::Nasal(0.08),
            Sound::Vowel(Viseme::Aa, 0.22),
            Sound::Pause(0.12),
            lips(0.08),
            Sound::Vowel(Viseme::Oh, 0.2),
            lips(0.08),
            Sound::Vowel(Viseme::Ih, 0.2),
            Sound::Pause(0.3),
        ];
        let rate = 24_000;
        let (pcm, truth) = utterance(&sounds, 160.0, rate, 0.5);
        let (writer, mut reader) = voice_mailbox();
        let lead = LeadAnalysis::new(writer, VowelProfile::default());
        let mut pump = lead.clone();
        pump.line(0, rate, &pcm, "mˈɑ, bˈɔ pˈi");
        // The pump writes it in 50 ms pieces, from feed sample 0, playing from 1.0 s.
        for (i, piece) in (0u32..).zip(pcm.chunks(1_200)) {
            let first = i * 1_200;
            pump.push(Seconds(1.0 + f64::from(first) / f64::from(rate)), u64::from(first), rate, piece, false);
        }
        assert!(lead.phoned_frames() > 100, "{} frames took their mouth from the phones", lead.phoned_frames());
        // What the stage reads every 10 ms while it plays.
        let mut shown = Vec::new();
        for k in 0..170 {
            let t = 1.0 + f64::from(k) * 0.01;
            lead.poll(Seconds(t));
            if let Some(frame) = reader.poll() {
                shown.push((t - 1.0, frame));
            }
        }
        let shown = &shown;
        // The phones lead the sound by PHONE_LEAD_S, so that the face's attack lands the shape on it.
        let during = |span: crate::PhoneSpan| {
            let (start, end) = (span.start_s - PHONE_LEAD_S, span.end_s - PHONE_LEAD_S);
            shown.iter().filter(move |(t, _)| *t >= start && *t < end)
        };
        for span in &truth {
            match span.phone {
                Phone::Bilabial => {
                    assert!(during(*span).any(|(_, f)| f.closure > 0.5), "the lips shut on {span:?}");
                }
                Phone::Vowel(v) => {
                    let right = during(*span).filter(|(_, f)| f.closure == 0.0 && f.visemes[v.index()] > 0.5).count();
                    assert!(right >= 5, "{v:?} shows in {right} frames of {span:?}");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn private_speech_moves_nothing() {
        let (writer, mut reader) = voice_mailbox();
        let lead = LeadAnalysis::new(writer, VowelProfile::default());
        let mut pump = lead.clone();
        pump.push(Seconds(0.0), 0, 48_000, &vowel(0.5), true);
        lead.poll(Seconds(1.0));
        assert_eq!(reader.poll(), None);
        assert_eq!(lead.frames(), 0);
    }

    #[test]
    fn a_cut_drops_what_did_not_play() {
        let (writer, mut reader) = voice_mailbox();
        let lead = LeadAnalysis::new(writer, VowelProfile::default());
        let mut pump = lead.clone();
        pump.push(Seconds(0.0), 0, 48_000, &vowel(1.0), false);
        lead.poll(Seconds(0.3));
        assert!(reader.poll().unwrap().voiced);
        pump.cut(Seconds(0.35));
        lead.poll(Seconds(0.5));
        let after = reader.poll().unwrap();
        assert!(!after.voiced, "the cut closed the mouth: {after:?}");
        lead.poll(Seconds(0.9));
        assert_eq!(reader.poll(), None, "nothing after the cut plays");
    }
}
