//! The guided calibration's fit (spec v3 PF-20): [`GuidedCalibrator`], the [`Calibrator`] that turns about forty
//! seconds of prompts ([`CalibrationPrompt::ROUTINE`]) into per-channel ranges and a rest head pose for one performer on
//! one device.
//!
//! The fit is the contract's: `lo` is each channel's **median** at rest; `hi` is the **95th percentile** within each
//! prompt, and the largest of those, because a channel fires only in its own prompt and pooling the prompts would
//! dilute it to noise. `hi` is never closer to `lo` than [`MIN_FIT_SPAN`]. The first [`REACTION_SECS`] of every prompt
//! are dropped (the person is still reacting), and a prompt with fewer than [`MIN_PROMPT_FRAMES`] tracked frames is
//! reported as skipped: a channel no measured prompt moved keeps the starting calibration's span above its new `lo`.
//! The rest head pose is the median of each angle (and of the position, when every rest frame has one).
//!
//! Frames are kept until [`finish`](Calibrator::finish): a cold path, about a megabyte for the whole routine at 60 fps.

use omuzaki_anim_api::{
    ARKIT_COUNT, Calibration, CalibrationError, CalibrationOutcome, CalibrationPrompt, Calibrator, Channel,
    ChannelRange, HeadPose, MIN_FIT_SPAN, TrackFrame, TrackStatus,
};
use omuzaki_core::glam::Vec3;

/// Seconds at the start of each prompt that are not measured: the person is still reacting.
pub const REACTION_SECS: f64 = 0.5;

/// The fewest tracked frames a prompt needs to be measured (the rest prompt included).
pub const MIN_PROMPT_FRAMES: usize = 5;

/// The percentile a prompt's peak is read at, so one glitched frame does not set it.
const PEAK_PERCENTILE: f32 = 0.95;

const PROMPTS: usize = CalibrationPrompt::ROUTINE.len();

/// Collects the guided routine's frames and fits a [`Calibration`] from them (see the module docs).
#[derive(Clone, Debug)]
pub struct GuidedCalibrator {
    /// The calibration fitted ranges replace: the device class's default, or the performer's last one.
    base: Calibration,
    /// When each prompt's first frame arrived, tracked or not: its start, for the reaction time.
    starts: [Option<f64>; PROMPTS],
    /// The tracked frames past each prompt's reaction time.
    frames: [Vec<TrackFrame>; PROMPTS],
}

impl GuidedCalibrator {
    /// A calibrator whose fitted ranges and rest pose replace those of `base`; channels the routine cannot measure
    /// keep `base`'s.
    pub fn new(base: Calibration) -> Self {
        Self { base, starts: [None; PROMPTS], frames: std::array::from_fn(|_| Vec::new()) }
    }

    /// Tracked frames kept for `prompt` so far (past its reaction time).
    pub fn frames_in(&self, prompt: CalibrationPrompt) -> usize {
        self.frames[slot(prompt)].len()
    }
}

/// `prompt`'s place in the routine.
fn slot(prompt: CalibrationPrompt) -> usize {
    CalibrationPrompt::ROUTINE.iter().position(|p| *p == prompt).unwrap_or(0)
}

impl Calibrator for GuidedCalibrator {
    fn push(&mut self, prompt: CalibrationPrompt, frame: &TrackFrame) {
        let i = slot(prompt);
        let start = *self.starts[i].get_or_insert(frame.time.0);
        if frame.status == TrackStatus::Tracking && frame.time.0 - start >= REACTION_SECS {
            self.frames[i].push(*frame);
        }
    }

    fn finish(&mut self) -> Result<CalibrationOutcome, CalibrationError> {
        let rest = &self.frames[slot(CalibrationPrompt::Rest)];
        if rest.len() < MIN_PROMPT_FRAMES {
            return Err(CalibrationError::NotEnoughRest { got: rest.len(), need: MIN_PROMPT_FRAMES });
        }
        let measured: Vec<&[TrackFrame]> = CalibrationPrompt::ROUTINE
            .iter()
            .zip(&self.frames)
            .filter(|(prompt, frames)| !prompt.is_rest() && frames.len() >= MIN_PROMPT_FRAMES)
            .map(|(_, frames)| frames.as_slice())
            .collect();
        let skipped = CalibrationPrompt::ROUTINE
            .iter()
            .zip(&self.frames)
            .filter(|(prompt, frames)| !prompt.is_rest() && frames.len() < MIN_PROMPT_FRAMES)
            .map(|(prompt, _)| *prompt)
            .collect();
        let mut calibration = self.base;
        let mut values = Vec::with_capacity(rest.len());
        for info in Channel::arkit() {
            let channel = info.channel;
            gather(rest, channel, &mut values);
            let Some(lo) = percentile(&mut values, 0.5) else { continue };
            let peak = measured
                .iter()
                .filter_map(|frames| {
                    gather(frames, channel, &mut values);
                    percentile(&mut values, PEAK_PERCENTILE)
                })
                .reduce(f32::max);
            let base = self.base.range(channel);
            let hi = peak.map_or(lo + (base.hi - base.lo), |peak| peak.max(lo + MIN_FIT_SPAN));
            calibration.set_range(channel, ChannelRange { lo, hi });
        }
        calibration.head_rest = head_rest(rest, &mut values).unwrap_or(self.base.head_rest);
        Ok(CalibrationOutcome { calibration, skipped })
    }
}

/// Fills `out` with `channel`'s values in the frames that have it.
fn gather(frames: &[TrackFrame], channel: Channel, out: &mut Vec<f32>) {
    out.clear();
    let i = channel.index();
    if i < ARKIT_COUNT {
        out.extend(frames.iter().filter(|f| f.present.contains(channel)).map(|f| f.blend[i]));
    }
    out.retain(|v| v.is_finite());
}

/// The nearest-rank `q` percentile of `values` (reordered); `None` when it is empty.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a rank within a short list
fn percentile(values: &mut [f32], q: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f32::total_cmp);
    let rank = ((values.len() - 1) as f32 * q).round() as usize;
    values.get(rank.min(values.len() - 1)).copied()
}

/// The median head pose of the rest frames that have one; the position only when every one of them has it.
fn head_rest(rest: &[TrackFrame], scratch: &mut Vec<f32>) -> Option<HeadPose> {
    let heads: Vec<HeadPose> = rest.iter().filter_map(|f| f.head).collect();
    if heads.len() < MIN_PROMPT_FRAMES {
        return None;
    }
    let mut median = |read: &dyn Fn(&HeadPose) -> Option<f32>| {
        scratch.clear();
        scratch.extend(heads.iter().filter_map(read).filter(|v| v.is_finite()));
        percentile(scratch, 0.5)
    };
    let yaw = median(&|h| Some(h.yaw))?;
    let pitch = median(&|h| Some(h.pitch))?;
    let roll = median(&|h| Some(h.roll))?;
    let position = if heads.iter().all(|h| h.position.is_some()) {
        let x = median(&|h| h.position.map(|p| p.x));
        let y = median(&|h| h.position.map(|p| p.y));
        let z = median(&|h| h.position.map(|p| p.z));
        x.zip(y).zip(z).map(|((x, y), z)| Vec3::new(x, y, z))
    } else {
        None
    };
    Some(HeadPose { yaw, pitch, roll, position })
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // test frame counters
mod tests {
    use omuzaki_anim_api::DeviceProfile;
    use omuzaki_core::time::Seconds;

    use super::*;

    /// Plays `prompt` for `seconds` at 30 fps from `start`, each frame made by `make(i)`; returns the end time.
    fn play(
        cal: &mut GuidedCalibrator,
        prompt: CalibrationPrompt,
        start: f64,
        seconds: f64,
        make: impl Fn(usize) -> TrackFrame,
    ) -> f64 {
        let n = (seconds * 30.0) as usize;
        for i in 0..n {
            let mut frame = make(i);
            frame.time = Seconds(start + i as f64 / 30.0);
            cal.push(prompt, &frame);
        }
        start + seconds
    }

    fn face(values: &[(Channel, f32)]) -> TrackFrame {
        let mut frame = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
        for info in Channel::arkit() {
            frame.set(info.channel, 0.0);
        }
        for &(channel, value) in values {
            frame.set(channel, value);
        }
        frame.head = Some(HeadPose { yaw: 0.1, pitch: -0.05, roll: 0.02, position: Some(Vec3::new(0.0, 0.0, 0.6)) });
        frame
    }

    fn base() -> Calibration {
        Calibration::from_gains(DeviceProfile::Webcam, |_| 2.0)
    }

    #[test]
    fn rest_sets_lo_each_prompt_sets_hi_and_the_reaction_time_is_ignored() {
        let mut cal = GuidedCalibrator::new(base());
        // At rest the jaw idles around 0.05 and the smile at 0.1.
        let t = play(&mut cal, CalibrationPrompt::Rest, 0.0, 4.0, |i| {
            let wobble = if i % 2 == 0 { 0.01 } else { -0.01 };
            face(&[(Channel::JAW_OPEN, 0.05 + wobble), (Channel::MOUTH_SMILE_LEFT, 0.1)])
        });
        // The mouth opens to 0.6, after half a second of reacting with it still shut.
        let t = play(&mut cal, CalibrationPrompt::MouthOpen, t, 3.0, |i| {
            let jaw = if i < 15 { 0.0 } else { 0.6 };
            face(&[(Channel::JAW_OPEN, jaw), (Channel::MOUTH_SMILE_LEFT, 0.1)])
        });
        play(&mut cal, CalibrationPrompt::BigSmile, t, 3.0, |_| {
            face(&[(Channel::JAW_OPEN, 0.1), (Channel::MOUTH_SMILE_LEFT, 0.12)])
        });
        assert_eq!(cal.frames_in(CalibrationPrompt::MouthOpen), 75, "the first 15 frames were the reaction");
        let outcome = cal.finish().unwrap();
        let jaw = outcome.calibration.range(Channel::JAW_OPEN);
        assert!((jaw.lo - 0.05).abs() < 0.011 && (jaw.hi - 0.6).abs() < 1e-6, "{jaw:?}");
        // The smile barely moved: its span is held open at MIN_FIT_SPAN, never amplified into noise.
        let smile = outcome.calibration.range(Channel::MOUTH_SMILE_LEFT);
        assert!((smile.lo - 0.1).abs() < 1e-6 && (smile.hi - smile.lo - MIN_FIT_SPAN).abs() < 1e-6, "{smile:?}");
        let head = outcome.calibration.head_rest;
        assert!((head.yaw - 0.1).abs() < 1e-6 && head.position.is_some(), "{head:?}");
        // Every prompt but the three played was skipped.
        assert_eq!(outcome.skipped.len(), CalibrationPrompt::ROUTINE.len() - 3);
        assert!(!outcome.skipped.contains(&CalibrationPrompt::MouthOpen));
    }

    #[test]
    fn a_channel_no_prompt_measured_keeps_its_span_above_the_new_rest() {
        let mut cal = GuidedCalibrator::new(base());
        play(&mut cal, CalibrationPrompt::Rest, 0.0, 4.0, |_| face(&[(Channel::BROW_INNER_UP, 0.2)]));
        let outcome = cal.finish().unwrap();
        let brow = outcome.calibration.range(Channel::BROW_INNER_UP);
        assert!((brow.lo - 0.2).abs() < 1e-6 && (brow.hi - 0.7).abs() < 1e-6, "base span 0.5 kept: {brow:?}");
        assert!(outcome.skipped.contains(&CalibrationPrompt::BrowsUp));
    }

    #[test]
    fn a_rest_with_no_face_is_refused_and_lost_frames_are_not_counted() {
        let mut cal = GuidedCalibrator::new(base());
        play(&mut cal, CalibrationPrompt::Rest, 0.0, 4.0, |_| TrackFrame::lost(0, Seconds(0.0), DeviceProfile::Webcam));
        assert_eq!(cal.finish().unwrap_err(), CalibrationError::NotEnoughRest { got: 0, need: MIN_PROMPT_FRAMES });
    }

    #[test]
    fn channels_the_source_never_sends_keep_the_base_range() {
        let mut cal = GuidedCalibrator::new(base());
        play(&mut cal, CalibrationPrompt::Rest, 0.0, 4.0, |_| {
            let mut frame = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
            frame.set(Channel::JAW_OPEN, 0.05);
            frame
        });
        let outcome = cal.finish().unwrap();
        assert_eq!(outcome.calibration.range(Channel::TONGUE_OUT), base().range(Channel::TONGUE_OUT));
        assert_eq!(outcome.calibration.head_rest, base().head_rest, "no head in the frames: the base rest stays");
    }
}
