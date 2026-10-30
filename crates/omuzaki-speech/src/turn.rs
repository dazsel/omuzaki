//! [`TurnTaker`]: when has the person finished their turn? End-of-turn prediction from the voice's prosody and the
//! words so far, so the co-host answers a finished sentence at once and waits through a hesitation (AP-03, the
//! "full-duplex conversational timing" row of `docs/quality/master-checklist.md` §11).
//!
//! A fixed silence timeout is what every open co-host uses (Open-LLM-VTuber, AIRI: a VAD and a hangover), and it is
//! wrong both ways: 600 ms after a finished question feels slow, and 600 ms is too short for "I went to the, um...".
//! People don't wait for silence; they hear the turn ending. The cues are well studied (Gravano and Hirschberg 2011,
//! "Turn-taking cues in task-oriented dialogue"; Ward and Tsukahara 2000 for backchannels):
//!
//! | Cue | Turn ends | Turn goes on |
//! |---|---|---|
//! | final pitch, against the speaker's own range | a fall to the low end, or a high rise (a question) | a level plateau in the middle |
//! | the last syllable's loudness | a soft taper | cut off at full strength |
//! | the last words (the partial transcript, when there is one) | a question mark, a complete clause | "and", "but", "the", "um" |
//! | length | a short answer ("yes", "sure") | |
//!
//! Each pause gets a score from -1 (holding the floor) to +1 (yielding it), recomputed every frame of the pause, so
//! words that arrive mid-pause from a speculative transcription can still lengthen or shorten it. The score sets how
//! long the segmenter waits: [`TurnConfig::yield_ms`] at +1, [`TurnConfig::neutral_ms`] at 0 and
//! [`TurnConfig::hold_ms`] at -1.
//!
//! **Backchannels.** While the person talks, a listener nods where a speaker invites it: Ward and Tsukahara's cue is a
//! stretch of pitch in the lowest quarter of the speaker's range, at least 110 ms long, after at least 700 ms of
//! speech. [`TurnFrame::backchannel`] marks the frame that completes one, at most once every
//! [`TurnConfig::backchannel_gap_ms`], and also a mid-turn pause after a falling phrase (the person stopped at a clause
//! end but keeps the floor).
//!
//! Pitch is a normalised autocorrelation over 70 to 400 Hz on each 32 ms VAD frame, kept as a histogram in half
//! semitones: one for the utterance and one for the speaker that decays over about 30 s. Nothing allocates after
//! [`TurnTaker::new`].

use crate::SAMPLE_RATE;
use crate::segmenter::FRAME;

/// The lowest pitch heard, Hz.
pub const F0_MIN: f32 = 70.0;
/// The highest pitch heard, Hz.
pub const F0_MAX: f32 = 400.0;
/// Histogram bins per semitone.
const BINS_PER_ST: f32 = 2.0;
/// Semitones from [`F0_MIN`] to [`F0_MAX`], rounded up.
const SPAN_ST: usize = 31;
/// Histogram bins.
const BINS: usize = SPAN_ST * 2 + 1;
/// A frame is periodic above this normalised autocorrelation peak.
const PERIODIC: f32 = 0.5;
/// Frames of the most recent voiced pitch kept for the end of a phrase.
const TAIL: usize = 4;
/// The speaker histogram's decay per voiced frame (about 30 s of voiced speech).
const SPEAKER_DECAY: f32 = 0.999;
/// Ward and Tsukahara's low-pitch region: the lowest 26% of the speaker's range.
const LOW_PERCENTILE: f32 = 0.26;
/// ... lasting this long, ms.
const LOW_REGION_MS: u32 = 110;
/// ... after this much speech, ms.
const BACKCHANNEL_AFTER_MS: u32 = 700;
/// A mid-turn pause this long after a falling phrase invites a nod, ms.
const PAUSE_NOD_MS: u32 = 250;

/// Settings for the [`TurnTaker`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurnConfig {
    /// The wait after a pause that sounds finished (score +1), ms.
    pub yield_ms: u32,
    /// The wait when the cues say nothing either way (score 0), ms.
    pub neutral_ms: u32,
    /// The wait after a pause that sounds unfinished (score -1), ms.
    pub hold_ms: u32,
    /// Backchannel cues at most this often, ms.
    pub backchannel_gap_ms: u32,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self { yield_ms: 160, neutral_ms: 420, hold_ms: 1100, backchannel_gap_ms: 1800 }
    }
}

/// Why a pause got its score: each cue's share, for the transcript and for tuning.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnCues {
    /// The phrase's last pitch against the speaker's median, semitones; `None` when too little was voiced.
    pub final_pitch_st: Option<f32>,
    /// The pitch cue's part of the score.
    pub pitch: f32,
    /// The loudness cue's part.
    pub taper: f32,
    /// The words' part.
    pub words: f32,
    /// The length cue's part.
    pub length: f32,
    /// The sum, clamped to -1..1.
    pub score: f32,
}

/// What one frame says about the turn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnFrame {
    /// How long the segmenter should wait in silence before it ends the utterance, ms.
    pub wait_ms: u32,
    /// This frame completes a backchannel cue: a listener would nod now.
    pub backchannel: bool,
}

/// How the last words of a partial transcript lean.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lean {
    /// A question: the person hands the turn over.
    Question,
    /// Ends on a word that can't end a sentence ("and", "the", "um"): the person isn't done.
    Unfinished,
    /// Anything else.
    Neutral,
}

/// Words a sentence doesn't end on: conjunctions, articles, prepositions, fillers and the openings of a clause.
const UNFINISHED: &[&str] = &[
    "a", "an", "and", "as", "at", "because", "but", "by", "cause", "er", "erm", "for", "from", "hmm", "i", "if", "in",
    "into", "is", "it's", "like", "my", "of", "on", "or", "so", "that", "the", "their", "then", "to", "uh", "um",
    "umm", "was", "we", "well", "were", "what", "when", "where", "which", "while", "who", "with", "you", "your",
];

/// How `text`, the person's words so far, leans.
#[must_use]
pub fn lean(text: &str) -> Lean {
    let trimmed = text.trim_end();
    if trimmed.ends_with('?') {
        return Lean::Question;
    }
    // Whisper ends most partials with a full stop or an ellipsis whether or not the sentence is done, so the last
    // word decides, not the punctuation; an ellipsis is a trailing-off.
    if trimmed.ends_with("...") || trimmed.ends_with('…') || trimmed.ends_with(',') || trimmed.ends_with('-') {
        return Lean::Unfinished;
    }
    let last = trimmed
        .rsplit(|c: char| c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase();
    if UNFINISHED.contains(&last.as_str()) { Lean::Unfinished } else { Lean::Neutral }
}

/// A pitch histogram in half semitones above [`F0_MIN`].
#[derive(Clone, Debug)]
struct Histogram {
    bins: [f32; BINS],
    total: f32,
}

impl Histogram {
    const fn new() -> Self {
        Self { bins: [0.0; BINS], total: 0.0 }
    }

    fn add(&mut self, st: f32, weight: f32) {
        let bin = ((st * BINS_PER_ST).round().max(0.0) as usize).min(BINS - 1);
        self.bins[bin] += weight;
        self.total += weight;
    }

    fn decay(&mut self, by: f32) {
        for b in &mut self.bins {
            *b *= by;
        }
        self.total *= by;
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    /// The `p` quantile, semitones above [`F0_MIN`]; `None` while nearly empty.
    fn quantile(&self, p: f32) -> Option<f32> {
        if self.total < 8.0 {
            return None;
        }
        let want = p * self.total;
        let mut seen = 0.0;
        for (i, b) in self.bins.iter().enumerate() {
            seen += b;
            if seen >= want {
                return Some(i as f32 / BINS_PER_ST);
            }
        }
        Some((BINS - 1) as f32 / BINS_PER_ST)
    }
}

/// The streaming end-of-turn predictor. Feed it every VAD frame with [`frame`](Self::frame), and the words of the
/// utterance in progress, when there are any, with [`words`](Self::words).
#[derive(Clone, Debug)]
pub struct TurnTaker {
    cfg: TurnConfig,
    speaker: Histogram,
    utterance: Histogram,
    /// The last voiced frames' pitch (semitones above `F0_MIN`) and level (dB), oldest first.
    tail: [(f32, f32); TAIL],
    tail_len: usize,
    /// The utterance's loudest voiced frame, dB.
    peak_db: f32,
    /// Frames since the utterance began, and since its last voiced frame.
    speech_frames: u32,
    pause_frames: u32,
    /// Consecutive voiced frames in the low-pitch region.
    low_run: u32,
    /// Frames since the last backchannel cue.
    since_backchannel: u32,
    /// Whether the open pause already gave its nod.
    pause_nodded: bool,
    lean: Lean,
    open: bool,
    cues: TurnCues,
    /// Scratch for the autocorrelation: one frame of samples, mean removed.
    scratch: [f32; FRAME],
}

impl TurnTaker {
    /// A predictor that has heard nobody yet.
    #[must_use]
    pub fn new(cfg: TurnConfig) -> Self {
        Self {
            cfg,
            speaker: Histogram::new(),
            utterance: Histogram::new(),
            tail: [(0.0, -120.0); TAIL],
            tail_len: 0,
            peak_db: -120.0,
            speech_frames: 0,
            pause_frames: 0,
            low_run: 0,
            since_backchannel: u32::MAX / 2,
            pause_nodded: false,
            lean: Lean::Neutral,
            open: false,
            cues: TurnCues::default(),
            scratch: [0.0; FRAME],
        }
    }

    /// The settings.
    #[must_use]
    pub const fn config(&self) -> TurnConfig {
        self.cfg
    }

    /// The cues of the pause in progress (or the last one).
    #[must_use]
    pub const fn cues(&self) -> TurnCues {
        self.cues
    }

    /// The words of the utterance in progress so far (a partial transcript). Ignored between utterances.
    pub fn words(&mut self, text: &str) {
        if self.open {
            self.lean = lean(text);
        }
    }

    /// One VAD frame: its 16 kHz samples, and whether the segmenter has an utterance open after it (`open`) and
    /// counts the frame as speech (`speech`).
    pub fn frame(&mut self, samples: &[f32; FRAME], open: bool, speech: bool) -> TurnFrame {
        self.since_backchannel = self.since_backchannel.saturating_add(1);
        if !open {
            if self.open {
                self.end_utterance();
            }
            return TurnFrame { wait_ms: self.cfg.neutral_ms, backchannel: false };
        }
        if !self.open {
            self.open = true;
            self.lean = Lean::Neutral;
        }
        self.speech_frames += 1;
        let mut backchannel = false;
        let (f0, level_db) = self.pitch(samples);
        match (speech, f0) {
            (true, Some(st)) => {
                if self.pause_frames > 0 {
                    // Speech again after a pause: the words heard at that pause no longer describe the ending.
                    self.lean = Lean::Neutral;
                }
                self.pause_frames = 0;
                self.pause_nodded = false;
                self.utterance.add(st, 1.0);
                self.speaker.decay(SPEAKER_DECAY);
                self.speaker.add(st, 1.0);
                self.tail.rotate_left(1);
                self.tail[TAIL - 1] = (st, level_db);
                self.tail_len = (self.tail_len + 1).min(TAIL);
                self.peak_db = self.peak_db.max(level_db);
                let low = self.speaker.quantile(LOW_PERCENTILE).is_some_and(|q| st <= q);
                self.low_run = if low { self.low_run + 1 } else { 0 };
                if self.low_run == frames(LOW_REGION_MS).max(1)
                    && self.speech_frames >= frames(BACKCHANNEL_AFTER_MS)
                    && self.backchannel_due()
                {
                    backchannel = true;
                }
            }
            (true, None) => {
                // A consonant or a breath inside speech: no pitch, and not a pause either.
                self.pause_frames = 0;
                self.low_run = 0;
            }
            (false, _) => {
                self.pause_frames += 1;
                self.low_run = 0;
            }
        }
        self.score();
        if !speech
            && !self.pause_nodded
            && self.pause_frames == frames(PAUSE_NOD_MS)
            && self.cues.pitch > 0.2
            && self.speech_frames >= frames(BACKCHANNEL_AFTER_MS)
            && self.wait_ms() > PAUSE_NOD_MS + 200
            && self.backchannel_due()
        {
            self.pause_nodded = true;
            backchannel = true;
        }
        if backchannel {
            self.since_backchannel = 0;
        }
        TurnFrame { wait_ms: self.wait_ms(), backchannel }
    }

    fn backchannel_due(&self) -> bool {
        self.since_backchannel >= frames(self.cfg.backchannel_gap_ms)
    }

    fn end_utterance(&mut self) {
        self.open = false;
        self.utterance.clear();
        self.tail_len = 0;
        self.peak_db = -120.0;
        self.speech_frames = 0;
        self.pause_frames = 0;
        self.low_run = 0;
        self.lean = Lean::Neutral;
    }

    /// The wait the score gives, ms.
    fn wait_ms(&self) -> u32 {
        let s = self.cues.score;
        let (to, from) =
            if s >= 0.0 { (self.cfg.yield_ms, self.cfg.neutral_ms) } else { (self.cfg.hold_ms, self.cfg.neutral_ms) };
        let t = s.abs().min(1.0);
        (from as f32 + (to as f32 - from as f32) * t).round() as u32
    }

    /// Scores the phrase so far as if it ended now.
    fn score(&mut self) {
        let mut cues = TurnCues::default();
        let median = self.speaker.quantile(0.5);
        if let (Some(median), true) = (median, self.tail_len >= 2) {
            let tail = &self.tail[TAIL - self.tail_len..];
            // The last two voiced frames, against the speaker's median: the phrase-final pitch.
            let last = tail.iter().rev().take(2).map(|t| t.0).sum::<f32>() / 2.0;
            let rel = last - median;
            cues.final_pitch_st = Some(rel);
            let low = self.speaker.quantile(0.2).map_or(-2.0, |q| (q - median).min(-1.0));
            let high = self.speaker.quantile(0.9).map_or(3.0, |q| (q - median).max(2.0));
            let rising = rel >= high * 0.8;
            cues.pitch = if rel <= low * 0.5 {
                // A fall toward the low end of the range: a statement ending.
                0.3 + 0.3 * (rel / low).clamp(0.0, 1.5) / 1.5
            } else if rising {
                // A high rise: a question.
                0.6
            } else {
                // Level and mid-range: a continuation.
                -0.5
            };
            // The last syllable's loudness against the utterance's peak.
            let taper_db = self.peak_db - tail[tail.len() - 1].1;
            cues.taper = if taper_db >= 9.0 {
                0.15
            } else if taper_db <= 3.0 && !rising {
                // Stopped at full strength (a question's rise is allowed to).
                -0.15
            } else {
                0.0
            };
        }
        cues.words = match self.lean {
            Lean::Question => 0.5,
            Lean::Unfinished => -0.8,
            Lean::Neutral => 0.0,
        };
        let spoken_ms = self.speech_frames.saturating_sub(self.pause_frames) * frame_ms();
        cues.length = if spoken_ms < 600 { 0.25 } else { 0.0 };
        cues.score = (cues.pitch + cues.taper + cues.words + cues.length).clamp(-1.0, 1.0);
        self.cues = cues;
    }

    /// The frame's pitch in semitones above [`F0_MIN`] (`None` when it isn't periodic) and its level in dBFS.
    fn pitch(&mut self, samples: &[f32; FRAME]) -> (Option<f32>, f32) {
        let mean = samples.iter().sum::<f32>() / FRAME as f32;
        let mut energy = 0.0f32;
        for (s, x) in self.scratch.iter_mut().zip(samples) {
            *s = x - mean;
            energy += *s * *s;
        }
        let level_db = 10.0 * (energy / FRAME as f32).max(1e-12).log10();
        if level_db < -55.0 {
            return (None, level_db);
        }
        let fs = SAMPLE_RATE as f32;
        let (min_lag, max_lag) = ((fs / F0_MAX) as usize, (fs / F0_MIN) as usize);
        let x = &self.scratch;
        let mut best = (0usize, 0.0f32);
        for lag in min_lag..=max_lag.min(FRAME - 1) {
            let n = FRAME - lag;
            let (mut xy, mut xx, mut yy) = (0.0f32, 0.0f32, 0.0f32);
            for i in 0..n {
                xy += x[i] * x[i + lag];
                xx += x[i] * x[i];
                yy += x[i + lag] * x[i + lag];
            }
            let r = xy / (xx * yy).sqrt().max(1e-12);
            if r > best.1 {
                best = (lag, r);
            }
        }
        if best.1 < PERIODIC || best.0 == 0 {
            return (None, level_db);
        }
        // Octave check: a peak at twice the period is as strong as the true one; prefer the shortest strong lag.
        let mut lag = best.0;
        for div in [3usize, 2] {
            let short = lag / div;
            if short >= min_lag {
                let n = FRAME - short;
                let (mut xy, mut xx, mut yy) = (0.0f32, 0.0f32, 0.0f32);
                for i in 0..n {
                    xy += x[i] * x[i + short];
                    xx += x[i] * x[i];
                    yy += x[i + short] * x[i + short];
                }
                if xy / (xx * yy).sqrt().max(1e-12) > 0.9 * best.1 {
                    lag = short;
                    break;
                }
            }
        }
        let f0 = fs / lag as f32;
        (Some(12.0 * (f0 / F0_MIN).log2()), level_db)
    }
}

/// One VAD frame, ms.
const fn frame_ms() -> u32 {
    (FRAME as u32 * 1000) / SAMPLE_RATE
}

/// Frames in `ms`, rounded up.
const fn frames(ms: u32) -> u32 {
    ms.div_ceil(frame_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_words_lean_the_turn() {
        assert_eq!(lean("Can you hear me?"), Lean::Question);
        assert_eq!(lean("I went to the"), Lean::Unfinished);
        assert_eq!(lean("so I said, um..."), Lean::Unfinished);
        assert_eq!(lean("and then, uh."), Lean::Unfinished);
        assert_eq!(lean("That was the best day."), Lean::Neutral);
        assert_eq!(lean(""), Lean::Neutral);
    }

    #[test]
    fn a_histogram_gives_its_quantiles() {
        let mut h = Histogram::new();
        for i in 0..20 {
            h.add(i as f32, 1.0);
        }
        assert_eq!(h.quantile(0.5), Some(9.5f32.floor()));
        assert!(h.quantile(0.1).unwrap() <= 2.0);
    }
}
