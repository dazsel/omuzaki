//! VAD over a recording, plus the scoring the benchmark uses to judge it.

use std::time::Instant;

use crate::segmenter::{Event, FRAME, Segment, Segmenter, SegmenterConfig};
use crate::synth::frame_levels_db;
use crate::vad::SileroVad;
use crate::{SAMPLE_RATE, SpeechError};

/// Everything one VAD pass produced.
#[derive(Debug)]
pub struct Detection {
    /// Speech probability of each [`FRAME`].
    pub probabilities: Vec<f32>,
    /// Inference time of each frame, in milliseconds.
    pub frame_ms: Vec<f64>,
    /// Segmenter events, each with the sample index at which the VAD could first report it (end of the frame that
    /// triggered it; end of stream for events fired by `finish`).
    pub events: Vec<(usize, Event)>,
}

impl Detection {
    /// The segments worth transcribing (kept ones, padding included), clamped to a recording of `total` samples.
    #[must_use]
    pub fn kept_segments(&self, total: usize) -> Vec<Segment> {
        self.events
            .iter()
            .filter_map(|(_, e)| {
                if let Event::End(s) = e { Some(Segment { start: s.start, end: s.end.min(total) }) } else { None }
            })
            .collect()
    }
}

/// Runs the VAD and segmenter over `audio` from a fresh state, timing every frame.
///
/// # Errors
/// As [`SileroVad::probability`].
pub fn detect(vad: &mut SileroVad, audio: &[f32], config: SegmenterConfig) -> Result<Detection, SpeechError> {
    vad.reset();
    let mut segmenter = Segmenter::new(config);
    let frames = audio.len().div_ceil(FRAME);
    let mut detection = Detection {
        probabilities: Vec::with_capacity(frames),
        frame_ms: Vec::with_capacity(frames),
        events: Vec::new(),
    };
    let mut frame = [0.0f32; FRAME];
    for (index, chunk) in audio.chunks(FRAME).enumerate() {
        frame.fill(0.0);
        frame[..chunk.len()].copy_from_slice(chunk);
        let started = Instant::now();
        let probability = vad.probability(&frame)?;
        detection.frame_ms.push(started.elapsed().as_secs_f64() * 1e3);
        detection.probabilities.push(probability);
        if let Some(event) = segmenter.push(probability) {
            detection.events.push(((index + 1) * FRAME, event));
        }
    }
    if let Some(event) = segmenter.finish() {
        detection.events.push((audio.len(), event));
    }
    Ok(detection)
}

/// Frame-level agreement between two sets of speech/non-speech decisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameScores {
    /// Of the frames called speech, the share the reference also calls speech.
    pub precision: f64,
    /// Of the frames the reference calls speech, the share also called speech.
    pub recall: f64,
    /// Harmonic mean of the two.
    pub f1: f64,
}

/// Scores `probabilities >= threshold` against `reference` decisions (the shorter of the two decides the length).
///
/// Used for robustness: the reference is the VAD's own decisions on the clean recording, so the score says how
/// much noise changes the VAD's mind without needing hand labels.
#[must_use]
pub fn frame_scores(reference: &[bool], probabilities: &[f32], threshold: f32) -> FrameScores {
    let (mut tp, mut fp, mut fneg) = (0usize, 0usize, 0usize);
    for (&label, &p) in reference.iter().zip(probabilities) {
        match (p >= threshold, label) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fneg += 1,
            (false, false) => {}
        }
    }
    let ratio = |num: usize, den: usize| if den == 0 { 1.0 } else { num as f64 / den as f64 };
    let (precision, recall) = (ratio(tp, tp + fp), ratio(tp, tp + fneg));
    let f1 = if precision + recall == 0.0 { 0.0 } else { 2.0 * precision * recall / (precision + recall) };
    FrameScores { precision, recall, f1 }
}

/// Samples per block when looking for the acoustic start and end of speech (8 ms).
const BLOCK: usize = 128;
/// A block counts as speech energy when it is this far above the local quiet level.
const RISE_DB: f32 = 12.0;
/// How far back from a boundary the local quiet level is estimated.
const LOOKBACK_FRAMES: usize = 12;

/// How late the VAD reports speech boundaries, measured against the acoustic start and end of the speech.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoundaryLag {
    /// For each `Start`: milliseconds from the acoustic onset (energy rising 12 dB above the local quiet level) to
    /// the moment the event is available.
    pub onset_ms: Vec<f64>,
    /// For each `End`/`Rejected`: milliseconds from the last block of speech energy to the moment the event is
    /// available. This includes the `min_silence_ms` hangover by design.
    pub end_ms: Vec<f64>,
}

fn block_levels_db(audio: &[f32], from: usize, to: usize) -> Vec<f32> {
    frame_levels_db(&audio[from.min(audio.len())..to.min(audio.len())], BLOCK)
}

/// The 20th percentile of `levels`: a robust "quiet" level even when part of the window is speech.
fn quiet_level(levels: &[f32]) -> f32 {
    let mut sorted = levels.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted.get(sorted.len() / 5).copied().unwrap_or(f32::NEG_INFINITY)
}

/// Finds each event's acoustic boundary in `audio` and reports how late the VAD delivered it.
///
/// This is a reference without hand labels: the acoustic boundary is where short-term energy leaves or reaches the
/// local quiet level, which is well defined for read speech after a pause and fuzzy for soft word onsets. Read the
/// numbers as "time from the sound to the event", not as an error against a transcript.
#[must_use]
pub fn boundary_lag(audio: &[f32], events: &[(usize, Event)]) -> BoundaryLag {
    let to_ms = |samples: usize| samples as f64 * 1000.0 / f64::from(SAMPLE_RATE);
    let mut lag = BoundaryLag::default();
    for &(emitted, event) in events {
        match event {
            Event::Start { at } => {
                let from = (at + FRAME).saturating_sub(LOOKBACK_FRAMES * FRAME) / BLOCK * BLOCK;
                let levels = block_levels_db(audio, from, at + FRAME);
                let threshold = quiet_level(&levels) + RISE_DB;
                // Energy that stays up for three blocks (24 ms), so a single click is not an onset.
                let onset = levels.windows(3).position(|w| w.iter().all(|&l| l >= threshold));
                lag.onset_ms.extend(onset.map(|b| to_ms(emitted.saturating_sub(from + b * BLOCK))));
            }
            Event::End(_) | Event::Rejected(_) => {
                let from = emitted.saturating_sub(LOOKBACK_FRAMES * FRAME) / BLOCK * BLOCK;
                let levels = block_levels_db(audio, from, emitted);
                let threshold = quiet_level(&levels) + RISE_DB;
                let last = levels.iter().rposition(|&l| l >= threshold);
                lag.end_ms.extend(last.map(|b| to_ms(emitted.saturating_sub(from + (b + 1) * BLOCK))));
            }
        }
    }
    lag
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::white_noise;

    #[test]
    fn frame_scores_count_each_quadrant() {
        let labels = [true, true, false, false, true];
        let probs = [0.9, 0.1, 0.9, 0.1, 0.9];
        let s = frame_scores(&labels, &probs, 0.5);
        assert!((s.precision - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.recall - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.f1 - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn boundary_lag_is_measured_from_the_sound_to_the_event() {
        // 1 s of quiet noise, then 1 s of loud noise (onset at exactly 16000), then 1 s quiet again.
        let mut audio = white_noise(1.0, 0.001, 1);
        audio.extend(white_noise(1.0, 0.3, 2));
        audio.extend(white_noise(1.0, 0.001, 3));
        // The VAD reports Start at frame 31 (samples 15872..16384), available when that frame completes, and the
        // end 6 frames after the speech stops (sample 32000).
        let start_at = 31 * FRAME;
        let end_emitted = 32_000 + 6 * FRAME;
        let events = vec![
            (start_at + FRAME, Event::Start { at: start_at }),
            (end_emitted, Event::End(Segment { start: 15_000, end: 33_000 })),
        ];
        let lag = boundary_lag(&audio, &events);
        assert_eq!(lag.onset_ms.len(), 1);
        // Available at 16384, onset at 16000: 384 samples = 24 ms (to within one 8 ms block).
        assert!((lag.onset_ms[0] - 24.0).abs() <= 8.0, "onset lag {:?}", lag.onset_ms);
        assert_eq!(lag.end_ms.len(), 1);
        assert!((lag.end_ms[0] - 192.0).abs() <= 8.0, "end lag {:?}", lag.end_ms);
    }
}
