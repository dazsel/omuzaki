//! A deterministic stand-in for the real retarget and calibrator.
//!
//! It implements the contract's *meaning*: ranges normalise, responses and exaggeration apply, mirroring swaps sides,
//! head pose is measured from the calibrated rest pose, lost faces fade out. It does **not** smooth (no One-Euro) and
//! it uses plain arithmetic only (no libm), so its output stays far inside decision 0009's cross-platform tolerance,
//! which makes it a good fixture for the pipeline's determinism tests.

use omuzaki_anim_api::{
    BlendMode, Calibration, CalibrationError, CalibrationOutcome, CalibrationPrompt, Calibrator, Channel, ChannelBuf,
    ChannelMask, ChannelRange, DeviceProfile, EXAGGERATED, Fade, HeadPose, LOOK_FULL_RAD, Layer, MIN_FIT_SPAN,
    Response, Retarget, RetargetConfig, TrackFrame, TrackStatus,
};

/// See the module docs.
#[derive(Clone, Debug)]
pub struct MockRetarget {
    calibration: Calibration,
    config: RetargetConfig,
    /// The output: the last tracked values, faded to rest while the face stays lost.
    values: ChannelBuf,
    /// The last tracked values, kept so the fade is a straight line from where the face was.
    held: ChannelBuf,
    mask: ChannelMask,
    weight: Fade,
    lost_for: f32,
}

impl MockRetarget {
    /// A retarget with an identity calibration for `profile` and default settings.
    pub fn new(profile: DeviceProfile) -> Self {
        Self::with(Calibration::identity(profile), RetargetConfig::default())
    }

    /// A retarget with the given calibration and settings.
    pub fn with(calibration: Calibration, config: RetargetConfig) -> Self {
        Self {
            calibration,
            config,
            values: ChannelBuf::NEUTRAL,
            held: ChannelBuf::NEUTRAL,
            mask: ChannelMask::EMPTY,
            weight: Fade(1.0),
            lost_for: 0.0,
        }
    }

    fn track(&mut self, frame: &TrackFrame) {
        let mut values = ChannelBuf::NEUTRAL;
        let mut mask = frame.present;
        for c in frame.present {
            let shaped = self.calibration.range(c).normalize(frame.get(c));
            let response = self.config.responses.get(&c).copied().unwrap_or_else(|| Response::built_in(c));
            let boost = if EXAGGERATED.contains(c) { self.config.exaggeration } else { 1.0 };
            values[c] = response.apply(shaped, boost);
        }
        if let Some(head) = frame.head {
            mask |= self.head(&mut values, head);
        }
        if let Some(eyes) = frame.eyes
            && !mask.intersects(ChannelMask::GAZE)
        {
            // Both eyes' mean turn toward image-right is a look toward the character's left; pitch is down-positive.
            let x = f32::midpoint(eyes.left_yaw, eyes.right_yaw) / LOOK_FULL_RAD;
            let y = -f32::midpoint(eyes.left_pitch, eyes.right_pitch) / LOOK_FULL_RAD;
            values.set_gaze(x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
            mask |= ChannelMask::GAZE;
        }
        if self.config.mirror {
            values.mirror();
            mask = mask.mirrored();
        }
        (self.values, self.held, self.mask) = (values, values, mask);
    }

    /// Writes the head channels and returns the ones written.
    fn head(&self, values: &mut ChannelBuf, head: HeadPose) -> ChannelMask {
        let (rest, gain) = (self.calibration.head_rest, self.config.head_gain);
        values[Channel::HEAD_YAW] = (head.yaw - rest.yaw) * gain;
        values[Channel::HEAD_PITCH] = (head.pitch - rest.pitch) * gain;
        values[Channel::HEAD_ROLL] = (head.roll - rest.roll) * gain;
        let mut written = ChannelMask::from_channels(&[Channel::HEAD_YAW, Channel::HEAD_PITCH, Channel::HEAD_ROLL]);
        if let (Some(p), Some(p0)) = (head.position, rest.position) {
            values[Channel::HEAD_SHIFT_X] = p.x - p0.x;
            values[Channel::HEAD_SHIFT_Y] = p.y - p0.y;
            values[Channel::LEAN] = (p.z - p0.z) * self.config.lean_per_metre;
            written |= ChannelMask::from_channels(&[Channel::HEAD_SHIFT_X, Channel::HEAD_SHIFT_Y, Channel::LEAN]);
        }
        written
    }
}

impl Retarget for MockRetarget {
    fn apply(&mut self, frame: &TrackFrame, dt: f32, out: &mut Layer) {
        let lost = self.config.lost;
        if frame.status == TrackStatus::Tracking {
            self.lost_for = 0.0;
            self.track(frame);
            self.weight.toward(1.0, dt, lost.fade_s);
        } else {
            self.lost_for += dt;
            if self.lost_for > lost.grace_s {
                // Ease the held values to rest while the weight fades, so the hand-off to the life layer is smooth.
                let progress = ((self.lost_for - lost.grace_s) / lost.fade_s.max(f32::EPSILON)).clamp(0.0, 1.0);
                for c in self.mask {
                    self.values[c] = self.held[c] * (1.0 - progress);
                }
                self.weight.toward(0.0, dt, lost.fade_s);
            }
        }
        out.values = self.values;
        out.mask = self.mask;
        out.weight = self.weight.0;
        out.mode = BlendMode::Override;
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    fn set_calibration(&mut self, calibration: Calibration) {
        self.calibration = calibration;
    }

    fn config(&self) -> &RetargetConfig {
        &self.config
    }

    fn set_config(&mut self, config: RetargetConfig) {
        self.config = config;
    }

    fn reset(&mut self) {
        (self.values, self.mask, self.weight, self.lost_for) =
            (ChannelBuf::NEUTRAL, ChannelMask::EMPTY, Fade(1.0), 0.0);
    }
}

/// Fits a [`Calibration`] from the guided routine, as the [`Calibrator`] docs describe.
#[derive(Clone, Debug, Default)]
pub struct MockCalibrator {
    prompts: Vec<PromptSamples>,
    profile: Option<DeviceProfile>,
}

/// What was captured while one prompt showed.
#[derive(Clone, Debug)]
struct PromptSamples {
    prompt: CalibrationPrompt,
    /// Time of the first frame seen for the prompt; the settle period counts from here.
    started: f64,
    /// Tracked frames after the settle period.
    frames: Vec<TrackFrame>,
}

/// Frames at the start of a prompt that are the person reacting, not performing.
const SETTLE_S: f64 = 0.5;
/// Fewest tracked frames the rest prompt needs.
const MIN_REST_FRAMES: usize = 10;
/// Fewest tracked frames an active prompt needs to count.
const MIN_ACTIVE_FRAMES: usize = 5;

impl MockCalibrator {
    /// An empty calibrator.
    pub fn new() -> Self {
        Self::default()
    }

    fn frames(&self, prompt: CalibrationPrompt) -> &[TrackFrame] {
        self.prompts.iter().find(|p| p.prompt == prompt).map_or(&[], |p| p.frames.as_slice())
    }
}

/// The value at fraction `p` of the sorted samples (nearest rank below).
fn quantile(sorted: &[f32], p: f32) -> f32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small indices
    let i = (p * (sorted.len().saturating_sub(1)) as f32).floor() as usize;
    sorted.get(i).copied().unwrap_or(0.0)
}

fn sorted_channel(frames: &[TrackFrame], c: Channel) -> Vec<f32> {
    let mut v: Vec<f32> = frames.iter().filter(|f| f.present.contains(c)).map(|f| f.get(c)).collect();
    v.sort_by(f32::total_cmp);
    v
}

impl Calibrator for MockCalibrator {
    fn push(&mut self, prompt: CalibrationPrompt, frame: &TrackFrame) {
        if frame.status != TrackStatus::Tracking {
            return;
        }
        self.profile.get_or_insert(frame.profile);
        let index = self.prompts.iter().position(|p| p.prompt == prompt).unwrap_or_else(|| {
            self.prompts.push(PromptSamples { prompt, started: frame.time.0, frames: Vec::new() });
            self.prompts.len() - 1
        });
        let samples = &mut self.prompts[index];
        if frame.time.0 - samples.started >= SETTLE_S {
            samples.frames.push(*frame);
        }
    }

    fn finish(&mut self) -> Result<CalibrationOutcome, CalibrationError> {
        let rest = self.frames(CalibrationPrompt::Rest);
        if rest.len() < MIN_REST_FRAMES {
            return Err(CalibrationError::NotEnoughRest { got: rest.len(), need: MIN_REST_FRAMES });
        }
        let mut calibration = Calibration::identity(self.profile.unwrap_or(DeviceProfile::Arkit));
        let active: Vec<_> = CalibrationPrompt::ROUTINE.iter().filter(|p| !p.is_rest()).collect();
        for info in Channel::arkit() {
            let c = info.channel;
            let lo = quantile(&sorted_channel(rest, c), 0.5);
            let hi = active
                .iter()
                .map(|p| self.frames(**p))
                .filter(|f| f.len() >= MIN_ACTIVE_FRAMES)
                .map(|f| quantile(&sorted_channel(f, c), 0.95))
                .fold(lo + MIN_FIT_SPAN, f32::max);
            calibration.set_range(c, ChannelRange { lo, hi });
        }
        calibration.head_rest = median_head(rest);
        let skipped = active.iter().filter(|p| self.frames(***p).len() < MIN_ACTIVE_FRAMES).map(|p| **p).collect();
        Ok(CalibrationOutcome { calibration, skipped })
    }
}

fn median_head(frames: &[TrackFrame]) -> HeadPose {
    let median = |f: &dyn Fn(&HeadPose) -> f32| {
        let mut v: Vec<f32> = frames.iter().filter_map(|t| t.head.as_ref().map(f)).collect();
        v.sort_by(f32::total_cmp);
        quantile(&v, 0.5)
    };
    let positions: Vec<_> = frames.iter().filter_map(|t| t.head.and_then(|h| h.position)).collect();
    let position = (!positions.is_empty()).then(|| {
        let axis = |f: &dyn Fn(&omuzaki_core::glam::Vec3) -> f32| {
            let mut v: Vec<f32> = positions.iter().map(f).collect();
            v.sort_by(f32::total_cmp);
            quantile(&v, 0.5)
        };
        omuzaki_core::glam::Vec3::new(axis(&|p| p.x), axis(&|p| p.y), axis(&|p| p.z))
    });
    HeadPose { yaw: median(&|h| h.yaw), pitch: median(&|h| h.pitch), roll: median(&|h| h.roll), position }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;
    use crate::track::{SyntheticSpec, synthetic_track};

    fn frame(jaw: f32) -> TrackFrame {
        let mut f = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit);
        f.set(Channel::JAW_OPEN, jaw);
        f
    }

    fn run(r: &mut MockRetarget, f: &TrackFrame, dt: f32) -> Layer {
        let mut out = Layer::absent(BlendMode::Override);
        r.apply(f, dt, &mut out);
        out
    }

    #[test]
    fn present_channels_are_ranged_and_claimed() {
        let mut cal = Calibration::identity(DeviceProfile::Webcam);
        cal.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.0, hi: 0.5 });
        let mut r = MockRetarget::with(cal, RetargetConfig::default());
        let out = run(&mut r, &frame(0.25), 1.0 / 30.0);
        assert_eq!(out.values[Channel::JAW_OPEN], 0.5);
        assert_eq!(out.mask, ChannelMask::of(Channel::JAW_OPEN), "absent channels are not claimed");
        assert_eq!((out.weight, out.mode), (1.0, BlendMode::Override));
    }

    #[test]
    fn head_is_measured_from_the_calibrated_rest_pose_and_mirror_flips_it() {
        let mut cal = Calibration::identity(DeviceProfile::Arkit);
        cal.head_rest =
            HeadPose { yaw: 0.1, position: Some(omuzaki_core::glam::Vec3::new(0.0, 0.0, 0.5)), ..HeadPose::default() };
        let mut f = frame(0.0);
        f.head = Some(HeadPose {
            yaw: 0.3,
            pitch: 0.1,
            roll: 0.0,
            position: Some(omuzaki_core::glam::Vec3::new(0.02, 0.0, 0.6)),
        });
        let mut r = MockRetarget::with(cal, RetargetConfig::default());
        let out = run(&mut r, &f, 0.033);
        assert!((out.values[Channel::HEAD_YAW] - 0.2).abs() < 1e-6);
        assert_eq!(out.values[Channel::HEAD_PITCH], 0.1);
        assert!(
            (out.values[Channel::LEAN] - 0.1 * 90.0_f32.to_radians()).abs() < 1e-6,
            "leaning toward the camera is positive"
        );
        assert!((out.values[Channel::HEAD_SHIFT_X] - 0.02).abs() < 1e-6);

        let mut mirrored = MockRetarget::with(cal, RetargetConfig { mirror: true, ..RetargetConfig::default() });
        let m = run(&mut mirrored, &f, 0.033);
        assert!((m.values[Channel::HEAD_YAW] + 0.2).abs() < 1e-6, "yaw flips");
        assert_eq!(m.values[Channel::HEAD_PITCH], 0.1, "pitch does not");
    }

    #[test]
    fn mirror_swaps_sides_in_values_and_mask() {
        let mut f = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit);
        f.set(Channel::EYE_BLINK_LEFT, 1.0);
        let mut r = MockRetarget::with(
            Calibration::identity(DeviceProfile::Arkit),
            RetargetConfig { mirror: true, ..RetargetConfig::default() },
        );
        let out = run(&mut r, &f, 0.033);
        assert_eq!(out.values[Channel::EYE_BLINK_RIGHT], 1.0);
        assert_eq!(out.mask, ChannelMask::of(Channel::EYE_BLINK_RIGHT));
    }

    #[test]
    fn gaze_angles_become_look_channels_only_when_the_source_has_no_blendshapes_for_it() {
        let mut f = frame(0.0);
        f.eyes =
            Some(omuzaki_anim_api::EyeAngles { left_yaw: 0.35, left_pitch: 0.0, right_yaw: 0.35, right_pitch: 0.0 });
        let mut r = MockRetarget::new(DeviceProfile::Arkit);
        let out = run(&mut r, &f, 0.033);
        let (x, y) = out.values.gaze();
        assert!(
            (x - 0.35 / LOOK_FULL_RAD).abs() < 1e-5 && y == 0.0,
            "toward image-right is toward the character's left"
        );
        f.set(Channel::EYE_LOOK_UP_LEFT, 0.5);
        assert!(!run(&mut r, &f, 0.033).mask.contains(Channel::EYE_LOOK_OUT_LEFT), "blendshapes win over angles");
    }

    #[test]
    fn a_lost_face_holds_through_the_grace_period_then_fades_out() {
        let mut r = MockRetarget::new(DeviceProfile::Arkit);
        run(&mut r, &frame(0.8), 0.033);
        let lost = TrackFrame::lost(1, Seconds(0.1), DeviceProfile::Arkit);
        let held = run(&mut r, &lost, 0.1);
        assert_eq!(
            (held.values[Channel::JAW_OPEN], held.weight),
            (0.8, 1.0),
            "inside the grace period nothing changes"
        );
        let mut faded = held;
        for _ in 0..30 {
            faded = run(&mut r, &lost, 0.05);
        }
        assert_eq!((faded.weight, faded.values[Channel::JAW_OPEN]), (0.0, 0.0), "faded to rest");
        let back = run(&mut r, &frame(0.4), 0.05);
        assert!(back.weight > 0.0 && back.weight < 1.0, "and fades back in when the face returns");
    }

    #[test]
    fn calibrating_a_webcam_track_recovers_usable_ranges() {
        let mut cal = MockCalibrator::new();
        let rest = synthetic_track(&SyntheticSpec { seconds: 4.0, ..SyntheticSpec::default() });
        let acting = synthetic_track(&SyntheticSpec { seed: 9, seconds: 3.0, ..SyntheticSpec::default() });
        for f in &rest.frames {
            cal.push(CalibrationPrompt::Rest, f);
        }
        for f in &acting.frames {
            cal.push(CalibrationPrompt::MouthOpen, f);
        }
        let outcome = cal.finish().unwrap();
        let jaw = outcome.calibration.range(Channel::JAW_OPEN);
        assert!(jaw.hi >= jaw.lo + MIN_FIT_SPAN && jaw.lo >= 0.0);
        assert_eq!(outcome.calibration.profile, DeviceProfile::Webcam);
        assert_eq!(outcome.skipped.len(), 11, "every other prompt had no frames");
        assert!(!outcome.skipped.contains(&CalibrationPrompt::MouthOpen));
    }

    #[test]
    fn calibration_needs_a_visible_face_at_rest() {
        let mut cal = MockCalibrator::new();
        cal.push(CalibrationPrompt::Rest, &frame(0.1)); // inside the settle period
        cal.push(CalibrationPrompt::Rest, &TrackFrame::lost(1, Seconds(1.0), DeviceProfile::Arkit));
        assert_eq!(cal.finish(), Err(CalibrationError::NotEnoughRest { got: 0, need: 10 }));
    }
}
