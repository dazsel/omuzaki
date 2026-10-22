//! Scene recipes: the camera, lights and ground a shot is staged with, loaded from the golden manifest and the light-rig
//! library. This file holds the recipe type, the reader for a manifest case and the pose; the parts are in [`camera`]
//! (the lens), [`lights`] (the rigs, which also give the post and the background) and [`ground`].

pub mod camera;
pub mod ground;
pub mod lights;
pub mod preset;

use omuzaki_anim_api::skeleton::{HUMANOID_COUNT, HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_render_api::{Background, Camera, Light, Post};
use serde::Deserialize;
use std::collections::BTreeMap;

pub use camera::CameraSpec;
pub use ground::GroundRecipe;
pub use lights::LightRig;
pub use preset::RenderPreset;

/// Anti-aliasing samples per pixel for every recipe (the manifest's `stage.msaa`).
pub const MSAA_SAMPLES: u8 = 4;

/// Why a recipe, a rig or a camera could not be built. Each variant says what to fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RecipeError {
    /// The light-rig library has no rig of that name.
    #[error("no light rig named '{0}' in the library (see LightRig::names for the shipped rigs)")]
    UnknownRig(String),
    /// A file or a manifest entry does not have the shape a recipe needs.
    #[error("{what} does not parse: {message}")]
    Parse {
        /// What was being read.
        what: String,
        /// The parser's message.
        message: String,
    },
    /// A value is out of range or missing, so the recipe would not light or frame the shot as written.
    #[error("{what} is not usable: {reason}")]
    Invalid {
        /// The rig, camera or part that is wrong.
        what: String,
        /// The first rule it breaks.
        reason: String,
    },
}

impl RecipeError {
    /// A [`RecipeError::Parse`] for `what`, from any parser's error.
    pub(crate) fn parse(what: &str, error: &impl std::fmt::Display) -> Self {
        Self::Parse { what: what.to_owned(), message: error.to_string() }
    }

    /// A [`RecipeError::Invalid`] for `what`.
    pub(crate) fn invalid(what: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid { what: what.into(), reason: reason.into() }
    }
}

/// Everything one golden case stages its shot with: the camera, the lights, the look, the ground, the simulation's
/// settle settings and the pose. It is a plain value, so the stage can rebuild it or compare two.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneRecipe {
    /// The light rig's name (`viewer-studio`).
    pub rig: String,
    /// The viewpoint, from the case's `spec.camera`.
    pub camera: Camera,
    /// The lights, in the order [`LightRig::to_lights`] gives them.
    pub lights: Vec<Light>,
    /// The frame's look: the rig's exposure and tone mapping, with bloom only when the case enables it.
    pub post: Post,
    /// The flat background.
    pub background: Background,
    /// The ground disc under the feet.
    pub ground: GroundRecipe,
    /// Anti-aliasing samples per pixel ([`MSAA_SAMPLES`]).
    pub samples: u8,
    /// The simulation step, seconds (`sim.dt`).
    pub dt: f32,
    /// How many steps the hair settles for before the frame is taken (`sim.settleSteps`).
    pub settle_steps: u32,
    /// The hips' absolute translation in the normalised rig, metres (`resolved.bones.hips.position`). Compare it with
    /// the rest hips through [`SceneRecipe::pose_for`].
    pub hips_translation: Vec3,
    /// The parent-local rotation of every humanoid bone (identity where the manifest lists none).
    pub rotations: [Quat; HUMANOID_COUNT],
}

impl SceneRecipe {
    /// Builds the recipe for one entry of the golden manifest's `cases`: its rig from the library, and its camera, sim
    /// and pose from the case itself.
    ///
    /// # Errors
    /// [`RecipeError::UnknownRig`] for a rig the library lacks, [`RecipeError::Parse`] for a case missing a field the
    /// recipe reads, and [`RecipeError::Invalid`] for a value out of range.
    pub fn from_manifest_case(case: &serde_json::Value) -> Result<Self, RecipeError> {
        let case = ManifestCase::deserialize(case).map_err(|e| RecipeError::parse("manifest case", &e))?;
        let rig = LightRig::load(&case.rig)?;
        let rotations = humanoid_rotations(&case.resolved.bones)?;
        let hips = case.resolved.bones.get("hips").ok_or_else(|| {
            RecipeError::invalid(&case.rig, "resolved.bones has no hips, so there is no hips translation")
        })?;
        Ok(Self {
            rig: case.rig.clone(),
            camera: case.spec.camera.to_camera()?,
            lights: rig.to_lights()?,
            post: rig.post(case.spec.render.bloom.enabled),
            background: rig.background(),
            ground: GroundRecipe::STAGE,
            samples: MSAA_SAMPLES,
            dt: case.spec.sim.dt,
            settle_steps: case.spec.sim.settle_steps,
            hips_translation: Vec3::from(hips.position),
            rotations,
        })
    }

    /// The pose to animate a character with: the recipe's rotations, with the hips moved by how far they sit from the
    /// rest hips of the character being staged.
    pub fn pose_for(&self, rest_hips: Vec3) -> HumanoidPose {
        HumanoidPose { hips_offset: self.hips_translation - rest_hips, rotations: self.rotations }
    }
}

/// The parts of a golden case the recipe reads.
#[derive(Deserialize)]
struct ManifestCase {
    /// The rig's name.
    rig: String,
    /// The case's inputs.
    spec: CaseSpec,
    /// What the inputs resolved to.
    resolved: Resolved,
}

/// `spec`: the camera, the render settings and the simulation settings.
#[derive(Deserialize)]
struct CaseSpec {
    /// The viewpoint.
    camera: CameraSpec,
    /// Render settings; only bloom matters here.
    render: RenderSpec,
    /// The simulation's settle settings.
    sim: SimSpec,
}

/// `spec.render`.
#[derive(Deserialize)]
struct RenderSpec {
    /// Whether this case turns bloom on.
    bloom: BloomFlag,
}

/// `spec.render.bloom`.
#[derive(Deserialize)]
struct BloomFlag {
    /// On or off for this case.
    enabled: bool,
}

/// `spec.sim`.
#[derive(Deserialize)]
struct SimSpec {
    /// The step, seconds.
    dt: f32,
    /// The number of settle steps.
    #[serde(rename = "settleSteps")]
    settle_steps: u32,
}

/// `resolved`: the bones the animator produced.
#[derive(Deserialize)]
struct Resolved {
    /// Bone name (VRM spelling) to its local transform.
    bones: BTreeMap<String, BoneLocal>,
}

/// One bone's local transform: where it sits and how it is turned.
#[derive(Deserialize)]
struct BoneLocal {
    /// Local translation, metres.
    position: [f32; 3],
    /// Local rotation, `[x, y, z, w]`.
    quaternion: [f32; 4],
}

/// How far a quaternion's length may be from 1 and still be a rotation. The manifest stores float64 values, so their
/// length is 1 to within float32 rounding.
const UNIT_TOLERANCE: f32 = 1e-4;

/// Maps the manifest's bones onto a [`HumanoidPose`]'s rotations. A bone the manifest does not name keeps the identity
/// rotation, which is the rest pose.
fn humanoid_rotations(bones: &BTreeMap<String, BoneLocal>) -> Result<[Quat; HUMANOID_COUNT], RecipeError> {
    let mut rotations = [Quat::IDENTITY; HUMANOID_COUNT];
    for (name, bone) in bones {
        let Some(humanoid) = HumanoidBone::from_name(name) else {
            return Err(RecipeError::invalid("resolved.bones", format!("'{name}' is not a VRM humanoid bone")));
        };
        let q = Quat::from_array(bone.quaternion);
        // A rotation is a unit quaternion. A NaN length is refused too: NaN is neither at most the tolerance nor below it.
        let deviation = (q.length() - 1.0).abs();
        if deviation.is_nan() || deviation > UNIT_TOLERANCE {
            let path = format!("resolved.bones.{name}.quaternion");
            let reason = format!("{:?} has length {}, but a rotation needs length 1", bone.quaternion, q.length());
            return Err(RecipeError::invalid(path, reason));
        }
        rotations[humanoid.index()] = q;
    }
    Ok(rotations)
}
