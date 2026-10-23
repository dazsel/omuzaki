//! Turns a stream of per-frame speech probabilities into speech start, held and end events.
//!
//! This is the same hysteresis idea as Silero's reference `get_speech_timestamps` (two thresholds, a minimum speech
//! length, a minimum silence length, padding), but pushed one probability at a time so it can run live: `push` does
//! no allocation and the state is a few integers.
//!
//! **Held** (`ai-performer.md` §5.4) is the barge-in signal: it fires once per segment, on the frame that completes a
//! run of [`SegmenterConfig::barge_in_frames`] consecutive frames at or above the threshold (96 ms at the default 3),
//! long before `min_speech_ms` decides whether the segment is speech at all. Two would cut on clicks; four would take
//! the barge-in budget past 200 ms.

use crate::SAMPLE_RATE;

/// Samples per VAD frame: Silero VAD v5/v6 take 512 samples (32 ms) at 16 kHz.
pub const FRAME: usize = 512;

/// Converts milliseconds to a whole number of samples at [`SAMPLE_RATE`].
pub(crate) const fn ms_to_samples(ms: u32) -> usize {
    (ms as usize * SAMPLE_RATE as usize) / 1000
}

/// Tuning for the [`Segmenter`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmenterConfig {
    /// A frame at or above this probability is speech and starts a segment.
    pub threshold: f32,
    /// Inside a segment, a frame below this probability counts as silence. Lower than `threshold` so a probability
    /// hovering around 0.5 does not chatter.
    pub neg_threshold: f32,
    /// Segments shorter than this are reported as [`Event::Rejected`] (coughs, clicks).
    pub min_speech_ms: u32,
    /// How long probabilities must stay below `neg_threshold` before a segment ends. This is the end-of-speech
    /// latency, so it is the main knob between snappy turn-taking and cutting a speaker off mid-sentence.
    pub min_silence_ms: u32,
    /// Audio kept before and after a segment's speech in [`Segment`], so Whisper sees the word edges.
    pub pad_ms: u32,
    /// A segment is cut and a new one started when it reaches this length, staying inside Whisper's 30 s window.
    pub max_speech_ms: u32,
    /// Consecutive frames at or above `threshold` that make [`Event::Held`]. At least 2 (the start frame reports
    /// [`Event::Start`]); smaller values count as 2.
    pub barge_in_frames: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            neg_threshold: 0.35,
            min_speech_ms: 250,
            min_silence_ms: 200,
            pad_ms: 100,
            max_speech_ms: 28_000,
            barge_in_frames: omuzaki_performer_api::limits::BARGE_IN_FRAMES,
        }
    }
}

/// A stretch of speech, as sample indices into the stream (end exclusive), padding included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// First sample.
    pub start: usize,
    /// One past the last sample.
    pub end: usize,
}

impl Segment {
    /// Length in samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// True for a zero-length segment.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

/// What the segmenter reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Speech began at this sample (first frame at or above the threshold, no padding).
    Start {
        /// First sample of the frame that crossed the threshold.
        at: usize,
    },
    /// The run of speech frames reached `barge_in_frames`: someone really is talking. Once per segment. Fire barge-in
    /// from this.
    Held {
        /// One past the last sample of the frame that completed the run.
        at: usize,
    },
    /// A segment ended and is long enough to keep: transcribe it.
    End(Segment),
    /// A segment ended but was shorter than `min_speech_ms`.
    Rejected(Segment),
}

#[derive(Clone, Copy, Debug)]
enum State {
    Idle,
    Speech {
        start_frame: usize,
        last_speech_frame: usize,
        /// Consecutive frames at or above the threshold, up to the current one; saturates once `Held` fired.
        run: u32,
        held: bool,
    },
}

/// The streaming state machine. Feed it one probability per [`FRAME`] with [`push`](Self::push), then call
/// [`finish`](Self::finish) at the end of the stream.
#[derive(Debug)]
pub struct Segmenter {
    cfg: SegmenterConfig,
    next_frame: usize,
    state: State,
}

impl Segmenter {
    /// A segmenter at the start of a stream.
    #[must_use]
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self { cfg, next_frame: 0, state: State::Idle }
    }

    /// Consumes the probability of the next frame and returns at most one event.
    pub fn push(&mut self, probability: f32) -> Option<Event> {
        let frame = self.next_frame;
        self.next_frame += 1;
        match self.state {
            State::Idle => {
                if probability < self.cfg.threshold {
                    return None;
                }
                self.state = State::Speech { start_frame: frame, last_speech_frame: frame, run: 1, held: false };
                Some(Event::Start { at: frame * FRAME })
            }
            State::Speech { start_frame, last_speech_frame, run, held } => {
                let last_speech_frame = if probability >= self.cfg.neg_threshold { frame } else { last_speech_frame };
                let run = if probability >= self.cfg.threshold { run.saturating_add(1) } else { 0 };
                let silent = (frame - last_speech_frame) * FRAME;
                let length = (frame + 1 - start_frame) * FRAME;
                if silent >= ms_to_samples(self.cfg.min_silence_ms) || length >= ms_to_samples(self.cfg.max_speech_ms) {
                    self.state = State::Idle;
                    return Some(self.close(start_frame, last_speech_frame));
                }
                let now_held = !held && run >= self.cfg.barge_in_frames.max(2);
                self.state = State::Speech { start_frame, last_speech_frame, run, held: held || now_held };
                now_held.then_some(Event::Held { at: (frame + 1) * FRAME })
            }
        }
    }

    /// Forgets a segment in progress without reporting it (the microphone was muted: nothing was heard). The stream
    /// position carries on.
    pub fn abandon(&mut self) {
        self.state = State::Idle;
    }

    /// Ends the stream: closes a segment still open (speech right up to the last frame).
    pub fn finish(&mut self) -> Option<Event> {
        let State::Speech { start_frame, last_speech_frame, .. } = self.state else {
            return None;
        };
        self.state = State::Idle;
        Some(self.close(start_frame, last_speech_frame))
    }

    /// Builds the end event for a segment whose speech spans `start_frame..=last_speech_frame`.
    fn close(&self, start_frame: usize, last_speech_frame: usize) -> Event {
        let pad = ms_to_samples(self.cfg.pad_ms);
        let segment =
            Segment { start: (start_frame * FRAME).saturating_sub(pad), end: (last_speech_frame + 1) * FRAME + pad };
        let speech = (last_speech_frame + 1 - start_frame) * FRAME;
        if speech < ms_to_samples(self.cfg.min_speech_ms) { Event::Rejected(segment) } else { Event::End(segment) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a probability script and collects the events with the frame index that produced each.
    fn run_all(cfg: SegmenterConfig, probs: &[f32]) -> Vec<(usize, Event)> {
        let mut seg = Segmenter::new(cfg);
        let mut out: Vec<(usize, Event)> =
            probs.iter().enumerate().filter_map(|(i, &p)| seg.push(p).map(|e| (i, e))).collect();
        out.extend(seg.finish().map(|e| (probs.len(), e)));
        out
    }

    /// [`run_all`] without the `Held` events, for the tests of start and end.
    fn run(cfg: SegmenterConfig, probs: &[f32]) -> Vec<(usize, Event)> {
        run_all(cfg, probs).into_iter().filter(|(_, e)| !matches!(e, Event::Held { .. })).collect()
    }

    fn cfg() -> SegmenterConfig {
        // 64 ms minimum silence = 2 frames, no padding: easy to count in frames.
        SegmenterConfig { min_speech_ms: 96, min_silence_ms: 64, pad_ms: 0, ..SegmenterConfig::default() }
    }

    #[test]
    fn silence_makes_no_events() {
        assert_eq!(run(cfg(), &[0.1; 50]), []);
    }

    #[test]
    fn speech_block_starts_on_first_frame_and_ends_after_min_silence() {
        let mut probs = vec![0.05; 4];
        probs.extend([0.9; 10]);
        probs.extend([0.05; 6]);
        let events = run(cfg(), &probs);
        assert_eq!(events[0], (4, Event::Start { at: 4 * FRAME }));
        // Speech frames 4..=13; frame 15 is the second silent frame, so the end fires there.
        assert_eq!(events[1], (15, Event::End(Segment { start: 4 * FRAME, end: 14 * FRAME })));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn short_silence_inside_speech_does_not_split() {
        let mut probs = vec![0.9; 5];
        probs.push(0.1); // one silent frame, below min_silence
        probs.extend([0.9; 5]);
        probs.extend([0.0; 4]);
        let ends: Vec<_> = run(cfg(), &probs).into_iter().filter(|(_, e)| matches!(e, Event::End(_))).collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].1, Event::End(Segment { start: 0, end: 11 * FRAME }));
    }

    #[test]
    fn hysteresis_keeps_a_wobbling_probability_in_one_segment() {
        // Between neg_threshold (0.35) and threshold (0.5) counts as speech once started.
        let probs = [0.9, 0.45, 0.4, 0.45, 0.9, 0.0, 0.0, 0.0];
        let events = run(cfg(), &probs);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].1, Event::End(Segment { start: 0, end: 5 * FRAME }));
    }

    #[test]
    fn blip_shorter_than_min_speech_is_rejected() {
        let probs = [0.0, 0.9, 0.9, 0.0, 0.0, 0.0];
        let events = run_all(cfg(), &probs);
        assert_eq!(events[1].1, Event::Rejected(Segment { start: FRAME, end: 3 * FRAME }));
        assert!(!events.iter().any(|(_, e)| matches!(e, Event::Held { .. })), "two frames never hold: {events:?}");
    }

    #[test]
    fn held_fires_once_on_the_third_consecutive_speech_frame() {
        let mut probs = vec![0.05; 3];
        probs.extend([0.9; 12]);
        probs.extend([0.05; 4]);
        let events = run_all(SegmenterConfig::default(), &probs);
        let held: Vec<_> = events.iter().filter(|(_, e)| matches!(e, Event::Held { .. })).collect();
        // Speech starts on frame 3; frames 3, 4 and 5 make the run, so Held fires on frame 5, at its end.
        assert_eq!(held, [&(5, Event::Held { at: 6 * FRAME })]);
        assert_eq!(events[0], (3, Event::Start { at: 3 * FRAME }));
    }

    #[test]
    fn held_needs_the_run_unbroken_and_counts_only_frames_over_the_threshold() {
        // Over the start threshold, then between the two thresholds (still speech, but no run), then a click pattern.
        let probs = [0.9, 0.9, 0.45, 0.9, 0.9, 0.2, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let events = run_all(SegmenterConfig { min_silence_ms: 64, ..SegmenterConfig::default() }, &probs);
        let held: Vec<usize> =
            events.iter().filter(|(_, e)| matches!(e, Event::Held { .. })).map(|(i, _)| *i).collect();
        assert_eq!(held, [8], "{events:?}");
        // A barge_in_frames of 1 would collide with Start: it counts as 2.
        let events = run_all(SegmenterConfig { barge_in_frames: 1, ..SegmenterConfig::default() }, &[0.9, 0.9, 0.0]);
        assert_eq!(events[1], (1, Event::Held { at: 2 * FRAME }));
    }

    #[test]
    fn abandon_drops_the_segment_and_keeps_the_stream_position() {
        let mut seg = Segmenter::new(cfg());
        assert_eq!(seg.push(0.9), Some(Event::Start { at: 0 }));
        seg.abandon();
        assert_eq!(seg.finish(), None);
        assert_eq!(seg.push(0.9), Some(Event::Start { at: FRAME }));
    }

    #[test]
    fn padding_extends_both_sides_and_clamps_at_zero() {
        let cfg = SegmenterConfig { pad_ms: 64, ..cfg() };
        let probs = [0.9, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0];
        let Some((_, Event::End(s))) = run(cfg, &probs).into_iter().nth(1) else {
            panic!("expected an End event");
        };
        assert_eq!(s, Segment { start: 0, end: 4 * FRAME + 1024 });
    }

    #[test]
    fn speech_open_at_end_of_stream_is_closed_by_finish() {
        let events = run(cfg(), &[0.9; 8]);
        assert_eq!(events[1], (8, Event::End(Segment { start: 0, end: 8 * FRAME })));
    }

    #[test]
    fn long_speech_is_cut_at_max_length_and_continues_as_a_new_segment() {
        let cfg = SegmenterConfig { max_speech_ms: 320, ..cfg() }; // 10 frames
        let events = run(cfg, &[0.9; 25]);
        let ends: Vec<Segment> =
            events.iter().filter_map(|(_, e)| if let Event::End(s) = e { Some(*s) } else { None }).collect();
        assert_eq!(ends[0], Segment { start: 0, end: 10 * FRAME });
        assert_eq!(ends[1], Segment { start: 10 * FRAME, end: 20 * FRAME });
        assert_eq!(ends.len(), 3); // the last 5 frames close in finish()
    }
}
