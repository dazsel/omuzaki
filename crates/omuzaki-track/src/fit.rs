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
            steps_deg: vec![32.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.12, 0.06, 0.03, 0.015, 0.008],
            coarse_from_deg: 2.0,
            sweeps: 4,
            renders: 6000,
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
}

/// The pose whose render best matches the synth's target, searched from `start`. See the module docs.
///
/// # Errors
/// A render that failed, in words.
pub fn fit(
    synth: &mut dyn Synth,
    targets: Targets<'_>,
    start: &HumanoidPose,
    config: &FitConfig,
) -> Result<Fit, String> {
    let mut search =
        Search { synth, targets, start: *start, offsets: vec![Vec3::ZERO; config.bones.len()], renders: [0; 2] };
    let first = search.score(&config.bones, Level::Fine)?;
    let mut best = first;
    let mut level = None;
    'steps: for &step_deg in &config.steps_deg {
        let now = if step_deg >= config.coarse_from_deg { Level::Coarse } else { Level::Fine };
        if level != Some(now) {
            level = Some(now);
            best = search.score(&config.bones, now)?;
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
                            let score = search.score(&config.bones, now)?;
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
        if now == Level::Fine && best.differ == 0 {
            break;
        }
    }
    let score = search.score(&config.bones, Level::Fine)?;
    Ok(Fit { pose: search.pose(&config.bones), score, start: first, renders: search.renders })
}

struct Search<'a> {
    synth: &'a mut dyn Synth,
    targets: Targets<'a>,
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

    fn score(&mut self, bones: &[HumanoidBone], level: Level) -> Result<Score, String> {
        let pose = self.pose(bones);
        self.renders[usize::from(level == Level::Fine)] += 1;
        let target = if level == Level::Fine { self.targets.fine } else { self.targets.coarse };
        Ok(Score::of(self.synth.render(&pose, level)?, target))
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
        let targets = Targets { coarse: &target, fine: &target };
        let fit = fit(&mut synth, targets, &HumanoidPose::REST, &config).unwrap();
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
