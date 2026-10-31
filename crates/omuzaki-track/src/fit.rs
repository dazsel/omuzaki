//! Fitting a known character's pose to a picture of it, by rendering and comparing (analysis by synthesis).
//!
//! A tracker built on people barely sees a drawn character, and even a good solve lands degrees off; but when the
//! character in the picture is one Omuzaki can render (the person's own avatar in a recorded stream, the kit in a test),
//! the picture itself is the answer key. [`fit`] starts from a pose (the tracker's, or the rest pose), renders the
//! character through a [`Synth`], measures the picture's difference pixel by pixel, and walks each bone's rotation
//! toward the pose whose render matches, coarse to fine, until no pixel differs or the steps run out.
//!
//! - **What is fitted.** Each bone in [`FitConfig::bones`] turns by a rotation vector on top of its starting rotation,
//!   one axis at a time, parents before children. A step that lowers the total difference is kept and repeated while
//!   it keeps helping; the step halves when a sweep finds nothing. The search never sees the true pose, only pixels.
//! - **Two levels.** Big steps are judged on the [`Level::Coarse`] picture (small and cheap), small ones on the
//!   [`Level::Fine`] one (the frame itself), where a fit is exact only when its map is 0.
//! - **Blurred while far.** A raw difference gives an arm no pull toward a target it doesn't overlap, so the search
//!   settles where it overlaps a little (a bent elbow putting the hand in place). Big steps compare both pictures
//!   blurred, by a radius that shrinks with the step, which turns a displaced part into a slope toward its place.
//! - **Several starts.** The tracker's pose, and any others the caller gives (the character's relaxed stance for a
//!   frame the tracker missed); the one that scores best on the most blurred picture is the one walked.
//! - **Deterministic.** The search order is fixed, so the same frame and start give the same fit, render for render.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};

/// Which picture a render is judged against.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Level {
    /// The small, cheap picture the big steps are judged on.
    Coarse,
    /// The picture itself.
    Fine,
}

/// Renders the character in a pose, for [`fit`].
pub trait Synth {
    /// `pose` rendered at `level`, as RGB rows with no padding, the size of that level's [`Targets`] picture.
    ///
    /// # Errors
    /// Whatever stopped the render, in words.
    fn render(&mut self, pose: &HumanoidPose, level: Level) -> Result<&[u8], String>;
}

/// The picture being fitted, at each level, as RGB rows with no padding.
#[derive(Copy, Clone, Debug)]
pub struct Targets<'a> {
    /// The small picture.
    pub coarse: &'a [u8],
    /// Its width in pixels.
    pub coarse_width: u32,
    /// The picture itself.
    pub fine: &'a [u8],
}

/// How far a render is from its target.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score {
    /// The sum of every channel's absolute difference: what the search lowers.
    pub sum: u64,
    /// Pixels that differ in any channel: 0 is an exact fit.
    pub differ: u64,
}

impl Score {
    /// `ours` against `target`, both RGB.
    pub fn of(ours: &[u8], target: &[u8]) -> Self {
        let mut score = Self::default();
        for (a, b) in ours.as_chunks::<3>().0.iter().zip(target.as_chunks::<3>().0) {
            let d = [a[0].abs_diff(b[0]), a[1].abs_diff(b[1]), a[2].abs_diff(b[2])];
            score.sum += u64::from(d[0]) + u64::from(d[1]) + u64::from(d[2]);
            score.differ += u64::from(d != [0; 3]);
        }
        score
    }
}

/// What [`fit`] searches and for how long.
#[derive(Clone, Debug)]
pub struct FitConfig {
    /// The bones it turns, parents first.
    pub bones: Vec<HumanoidBone>,
    /// The step sizes in degrees, largest first.
    pub steps_deg: Vec<f32>,
    /// Steps at or above this many degrees are judged on the coarse picture.
    pub coarse_from_deg: f32,
    /// The most sweeps over every bone at one step size.
    pub sweeps: u32,
    /// The most renders in all; the fit stops with what it has.
    pub renders: u32,
    /// The coarse blur's radius, in coarse pixels per degree of step.
    pub blur_per_deg: f32,
    /// The largest blur radius.
    pub max_blur: u32,
}

impl Default for FitConfig {
    fn default() -> Self {
        use HumanoidBone as B;
        Self {
            bones: vec![
                B::Spine,
                B::Chest,
                B::UpperChest,
                B::Neck,
                B::Head,
                B::LeftShoulder,
                B::RightShoulder,
                B::LeftUpperArm,
                B::RightUpperArm,
                B::LeftLowerArm,
                B::RightLowerArm,
                B::LeftHand,
                B::RightHand,
            ],
            steps_deg: vec![
                32.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.12, 0.06, 0.03, 0.015, 0.008, 0.004, 0.002, 0.001,
            ],
            coarse_from_deg: 0.25,
            sweeps: 4,
            renders: 6000,
            blur_per_deg: 0.25,
            max_blur: 8,
        }
    }
}

/// What [`fit`] found.
#[derive(Clone, Debug)]
pub struct Fit {
    /// The fitted pose.
    pub pose: HumanoidPose,
    /// Its score on the fine picture.
    pub score: Score,
    /// The fine score of the starting pose.
    pub start: Score,
    /// Renders made, at each level.
    pub renders: [u32; 2],
    /// Each step size walked, with the score it ended on at its level and the renders made by then.
    pub trace: Vec<(f32, Level, Score, u32)>,
}

/// The pose whose render best matches the synth's target, searched from the best of `starts` (at least one). See the
/// module docs.
///
/// # Errors
/// A render that failed, in words; no start.
pub fn fit(
    synth: &mut dyn Synth,
    targets: Targets<'_>,
    starts: &[HumanoidPose],
    config: &FitConfig,
) -> Result<Fit, String> {
    let first_start = *starts.first().ok_or("no starting pose")?;
    let mut search = Search {
        synth,
        targets,
        blurred: Vec::new(),
        start: first_start,
        offsets: vec![Vec3::ZERO; config.bones.len()],
        renders: [0; 2],
    };
    let first = search.score(&config.bones, Level::Fine, 0)?;
    let widest = config.steps_deg.first().map_or(0, |&step| config.blur(step));
    let mut chosen = (search.score(&config.bones, Level::Coarse, widest)?, first_start);
    for start in &starts[1..] {
        search.start = *start;
        let score = search.score(&config.bones, Level::Coarse, widest)?;
        if score.sum < chosen.0.sum {
            chosen = (score, *start);
        }
    }
    search.start = chosen.1;
    let mut best = Score::default();
    let mut level = None;
    let mut trace = Vec::new();
    'steps: for &step_deg in &config.steps_deg {
        let now = if step_deg >= config.coarse_from_deg { Level::Coarse } else { Level::Fine };
        let blur = if now == Level::Coarse { config.blur(step_deg) } else { 0 };
        if level != Some((now, blur)) {
            level = Some((now, blur));
            best = search.score(&config.bones, now, blur)?;
        }
        let step = step_deg.to_radians();
        for _ in 0..config.sweeps {
            let mut moved = false;
            for bone in 0..config.bones.len() {
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    for sign in [1.0, -1.0] {
                        let mut went = false;
                        loop {
                            if search.renders.iter().sum::<u32>() >= config.renders {
                                break 'steps;
                            }
                            search.offsets[bone] += sign * step * axis;
                            let score = search.score(&config.bones, now, blur)?;
                            if score.sum < best.sum {
                                (best, went, moved) = (score, true, true);
                            } else {
                                search.offsets[bone] -= sign * step * axis;
                                break;
                            }
                        }
                        if went {
                            break;
                        }
                    }
                }
            }
            if !moved || (now == Level::Fine && best.differ == 0) {
                break;
            }
        }
        trace.push((step_deg, now, best, search.renders.iter().sum()));
        if now == Level::Fine && best.differ == 0 {
            break;
        }
    }
    let score = search.score(&config.bones, Level::Fine, 0)?;
    Ok(Fit { pose: search.pose(&config.bones), score, start: first, renders: search.renders, trace })
}

impl FitConfig {
    /// The coarse blur radius for a step of `step_deg`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small, clamped
    fn blur(&self, step_deg: f32) -> u32 {
        (step_deg * self.blur_per_deg).round().clamp(0.0, self.max_blur as f32) as u32
    }
}

/// `rgb` (`width` wide) box-blurred by `radius` along each axis, each channel as 16 times its mean.
fn blur(rgb: &[u8], width: usize, radius: usize) -> Vec<u32> {
    let height = rgb.len() / 3 / width.max(1);
    let pass = |src: &[u32], along_x: bool| -> Vec<u32> {
        let mut out = vec![0; src.len()];
        for y in 0..height {
            for x in 0..width {
                let (at, len) = if along_x { (x, width) } else { (y, height) };
                let (lo, hi) = (at.saturating_sub(radius), (at + radius).min(len - 1));
                for c in 0..3 {
                    let sum: u32 = (lo..=hi)
                        .map(|i| if along_x { src[(y * width + i) * 3 + c] } else { src[(i * width + x) * 3 + c] })
                        .sum();
                    out[(y * width + x) * 3 + c] = sum / u32::try_from(hi - lo + 1).unwrap_or(1);
                }
            }
        }
        out
    };
    let wide: Vec<u32> = rgb.iter().map(|&v| 16 * u32::from(v)).collect();
    pass(&pass(&wide, true), false)
}

/// `ours` against `target`, both from [`blur`]: the sum of the channels' absolute differences, and the pixels that
/// differ at all.
fn blurred_score(ours: &[u32], target: &[u32]) -> Score {
    let mut score = Score::default();
    for (a, b) in ours.as_chunks::<3>().0.iter().zip(target.as_chunks::<3>().0) {
        let d = [a[0].abs_diff(b[0]), a[1].abs_diff(b[1]), a[2].abs_diff(b[2])];
        score.sum += u64::from(d[0]) + u64::from(d[1]) + u64::from(d[2]);
        score.differ += u64::from(d != [0; 3]);
    }
    score
}

struct Search<'a> {
    synth: &'a mut dyn Synth,
    targets: Targets<'a>,
    /// The coarse target blurred, by radius.
    blurred: Vec<(u32, Vec<u32>)>,
    start: HumanoidPose,
    /// Each bone's rotation vector on top of its starting rotation.
    offsets: Vec<Vec3>,
    renders: [u32; 2],
}

impl Search<'_> {
    fn pose(&self, bones: &[HumanoidBone]) -> HumanoidPose {
        let mut pose = self.start;
        for (&bone, &offset) in bones.iter().zip(&self.offsets) {
            pose.set(bone, (self.start.get(bone) * Quat::from_scaled_axis(offset)).normalize());
        }
        pose
    }

    /// The current pose's score at `level`, the coarse picture blurred by `radius` when it is above 0.
    fn score(&mut self, bones: &[HumanoidBone], level: Level, radius: u32) -> Result<Score, String> {
        let pose = self.pose(bones);
        self.renders[usize::from(level == Level::Fine)] += 1;
        if level == Level::Fine {
            return Ok(Score::of(self.synth.render(&pose, level)?, self.targets.fine));
        }
        if radius == 0 {
            return Ok(Score::of(self.synth.render(&pose, level)?, self.targets.coarse));
        }
        let width = self.targets.coarse_width as usize;
        let at = self.blurred.iter().position(|(r, _)| *r == radius).unwrap_or_else(|| {
            self.blurred.push((radius, blur(self.targets.coarse, width, radius as usize)));
            self.blurred.len() - 1
        });
        let ours = blur(self.synth.render(&pose, level)?, width, radius as usize);
        Ok(blurred_score(&ours, &self.blurred[at].1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synth whose "render" is each fitted bone's rotation drawn as pixels: a pixel per quaternion component, so the
    /// picture matches exactly when the rotations do (to the drawing's resolution).
    struct Dial {
        bones: Vec<HumanoidBone>,
        out: Vec<u8>,
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=255 first
    fn draw(bones: &[HumanoidBone], pose: &HumanoidPose) -> Vec<u8> {
        bones
            .iter()
            .flat_map(|&b| pose.get(b).to_array())
            .flat_map(|c| {
                let v = (c.clamp(-1.0, 1.0) * 127.0 + 128.0).round() as u8;
                [v, v, v]
            })
            .collect()
    }

    impl Synth for Dial {
        fn render(&mut self, pose: &HumanoidPose, _: Level) -> Result<&[u8], String> {
            self.out = draw(&self.bones, pose);
            Ok(&self.out)
        }
    }

    #[test]
    fn a_fit_walks_from_rest_to_the_pose_its_picture_shows() {
        let bones = vec![HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::Head];
        let mut truth = HumanoidPose::REST;
        truth.set(HumanoidBone::LeftUpperArm, Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.2, -0.4, 1.1));
        truth.set(HumanoidBone::LeftLowerArm, Quat::from_rotation_y(-1.3));
        truth.set(HumanoidBone::Head, Quat::from_rotation_x(0.15));
        let target = draw(&bones, &truth);
        let mut synth = Dial { bones: bones.clone(), out: Vec::new() };
        let config = FitConfig { bones, ..FitConfig::default() };
        let targets = Targets { coarse: &target, coarse_width: 4, fine: &target };
        let fit = fit(&mut synth, targets, &[HumanoidPose::REST], &config).unwrap();
        assert!(fit.start.differ > 0);
        assert_eq!(fit.score.differ, 0, "{fit:?}");
        for bone in [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::Head] {
            assert!(fit.pose.get(bone).angle_between(truth.get(bone)) < 0.02, "{bone:?}");
        }
    }

    #[test]
    fn the_score_counts_every_differing_pixel_once() {
        let a = [0, 0, 0, 10, 10, 10, 5, 0, 0];
        let b = [0, 0, 0, 10, 12, 10, 0, 0, 3];
        assert_eq!(Score::of(&a, &b), Score { sum: 2 + 5 + 3, differ: 2 });
    }
}
