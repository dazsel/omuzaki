//! `stage.measure`: a character's proportions as the stage sees them, whichever way it was made: the height, the head
//! (chin to crown, hair included, and its width), heads tall, the eye line and every humanoid joint in the bind pose,
//! with the lengths and widths a figure-drawing chart reads (shoulders, hips, arms, legs, torso, neck).
//!
//! The same numbers for a kit character and an imported one, in the same frame (metres, standing on the floor at
//! `y = 0` as the stage draws it, facing +Z), so two characters can be set side by side: a person matching a kit
//! character to their own model's proportions, or an agent checking that a build reads as tall as it should, reads
//! one table for both. A query: it builds the character once and changes nothing.

use std::collections::BTreeMap;

use omuzaki_anim_api::{HumanoidBone, Pose};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::{BuildServices, BuiltCharacter, CharId, CharacterBuilder, JointRef, Quality as BuildQuality};
use omuzaki_core::glam::Vec3;
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::body::{Body, BodyServices, load_body};

/// Measures a character: height, head, heads tall, joints and the lengths between them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"character": "kit"}], "x-omuzaki-fixture" = "character"))]
pub struct Measure {
    /// The character, built from the kit or imported.
    pub character: CharId,
}

/// The head as a viewer sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeadMeasure {
    /// The bottom of the head mesh, metres above the floor.
    pub chin: f32,
    /// The top of the head with its hair or hat.
    pub crown: f32,
    /// Chin to crown.
    pub height: f32,
    /// Side to side, hair included.
    pub width: f32,
    /// Front to back, hair included.
    pub depth: f32,
}

/// The lengths and widths of the figure, metres, between joint centres.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Lengths {
    /// Between the upper-arm joints.
    pub shoulder_width: Option<f32>,
    /// Between the upper-leg joints.
    pub hip_width: Option<f32>,
    /// Upper arm, from the shoulder joint to the elbow, then on to the wrist: the left side's.
    pub arm: Option<f32>,
    /// Upper leg to the ankle: the left side's.
    pub leg: Option<f32>,
    /// Hips to the base of the neck.
    pub torso: Option<f32>,
    /// The base of the neck to the head joint.
    pub neck: Option<f32>,
}

/// What `stage.measure` gives back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MeasureResult {
    /// The character.
    pub character: CharId,
    /// `kit` or `imported`.
    pub body: String,
    /// The lowest point to the highest, hair and shoes included.
    pub height: f32,
    /// How far the character stands raised on its lowest point (soles and heels), included in every height here.
    pub lift: f32,
    /// The head; `None` for a character without one.
    pub head: Option<HeadMeasure>,
    /// The height over the head's: how many heads tall the figure is.
    pub heads_tall: Option<f32>,
    /// The eye line.
    pub eyes: Option<f32>,
    /// Each humanoid joint the skeleton has, by its VRM name, in the bind pose.
    pub joints: BTreeMap<String, [f32; 3]>,
    /// The chart's lengths.
    pub lengths: Lengths,
}

impl Op for Measure {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.measure",
            summary: "Measure a character, kit or imported, in one frame: height, head, heads tall, eye line, every \
                      humanoid joint and the lengths between them.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let cost = Some(CostHint { seconds: Some(1.0), gpu_mib: Some(0), usd: None });
        let (Ok(builder), Ok(build), Ok(store)) =
            (ctx.service::<dyn CharacterBuilder>(), ctx.service::<BuildServices>(), ctx.service::<dyn Store>())
        else {
            // A host without a character builder: the character is there, and nothing can be measured.
            if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
                return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                    .hint("check the id; characters live under characters/<id>"));
            }
            return Ok(Plan { cost, ..Plan::default() });
        };
        let mtoon = ctx.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon);
        let services = BodyServices::new(builder, build, store, mtoon, ctx.clock);
        let body = load_body(ctx.doc, &services, &self.character, None, BuildQuality::Live)?;
        let result = measure(&self.character, &body)?;
        let output = serde_json::to_value(result).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan { output: Some(output), cost, ..Plan::default() })
    }
}

register_op!(Measure);

/// Measures `body` as `character`.
///
/// # Errors
/// `Invalid` (field `character`) for a body the framing can't measure.
pub fn measure(character: &CharId, body: &Body) -> Result<MeasureResult, OpError> {
    use HumanoidBone as B;
    let landmarks = body.landmarks().map_err(|e| OpError::invalid("character", e.to_string()))?;
    let up = Vec3::new(0.0, landmarks.lift, 0.0);
    let joints: BTreeMap<HumanoidBone, Vec3> = match body {
        Body::Kit(built) => kit_joints(built),
        Body::Avatar(avatar) => avatar_joints(avatar),
    }
    .into_iter()
    .map(|(bone, p)| (bone, p + up))
    .collect();
    let height = landmarks.top() - landmarks.bottom();
    let head = landmarks.head.map(|h| HeadMeasure {
        chin: h.min.y,
        crown: h.max.y,
        height: h.height(),
        width: h.max.x - h.min.x,
        depth: h.max.z - h.min.z,
    });
    let heads_tall = head.as_ref().filter(|h| h.height > 0.0).map(|h| height / h.height);
    let at = |bone: HumanoidBone| joints.get(&bone).copied();
    let between = |a: HumanoidBone, b: HumanoidBone| Some(at(a)?.distance(at(b)?));
    let chain = |bones: &[HumanoidBone]| -> Option<f32> {
        bones.windows(2).map(|w| between(w[0], w[1])).sum::<Option<f32>>()
    };
    let neck_base = at(B::Neck);
    let lengths = Lengths {
        shoulder_width: between(B::LeftUpperArm, B::RightUpperArm),
        hip_width: between(B::LeftUpperLeg, B::RightUpperLeg),
        arm: chain(&[B::LeftUpperArm, B::LeftLowerArm, B::LeftHand]),
        leg: chain(&[B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot]),
        torso: neck_base.zip(at(B::Hips)).map(|(n, h)| n.distance(h)),
        neck: between(B::Neck, B::Head),
    };
    Ok(MeasureResult {
        character: character.clone(),
        body: match body {
            Body::Kit(_) => "kit",
            Body::Avatar(_) => "imported",
        }
        .to_owned(),
        height,
        lift: landmarks.lift,
        head,
        heads_tall,
        eyes: landmarks.eyes,
        joints: joints.iter().map(|(bone, p)| (bone.name().to_owned(), round(*p))).collect(),
        lengths,
    })
}

/// Millimetre precision: the numbers are for reading and comparing, and a stable answer diffs cleanly.
fn round(p: Vec3) -> [f32; 3] {
    let mm = |v: f32| (v * 1000.0).round() / 1000.0;
    [mm(p.x), mm(p.y), mm(p.z)]
}

/// A kit character's joints where its meshes were built: the inverse of each bone's inverse bind.
fn kit_joints(built: &BuiltCharacter) -> BTreeMap<HumanoidBone, Vec3> {
    let inverse_bind = built.rig.inverse_bind();
    HumanoidBone::ALL
        .iter()
        .filter_map(|&bone| {
            let id = built.rig.bone(&JointRef::Humanoid(bone))?;
            let m = inverse_bind.get(id.index())?.inverse();
            Some((bone, m.w_axis.truncate()))
        })
        .collect()
}

/// An imported avatar's joints where its meshes are stored: from the inverse bind of a mesh skinned to the bone, else
/// the skeleton's rest, as framing reads them.
fn avatar_joints(avatar: &ImportedAvatar) -> BTreeMap<HumanoidBone, Vec3> {
    let skeleton = &avatar.skeleton;
    let resolve = |joint: &JointRef| match joint {
        JointRef::Humanoid(bone) => skeleton.humanoid_bone(*bone),
        JointRef::Named(name) => skeleton.find(name.as_str()),
    };
    let mut rest_world = Box::new(Pose::IDENTITY);
    skeleton.world_pose(&skeleton.rest_pose(), &mut rest_world);
    HumanoidBone::ALL
        .iter()
        .filter_map(|&bone| {
            let id = skeleton.humanoid_bone(bone)?;
            let from_mesh = avatar.meshes.iter().find_map(|m| {
                let k = m.mesh.joint_bones.iter().position(|j| resolve(j) == Some(id))?;
                m.inverse_bind.get(k).map(|ibm| ibm.inverse().w_axis.truncate())
            });
            let p = from_mesh.or_else(|| rest_world.local.get(id.index()).map(|t| t.translation))?;
            Some((bone, p))
        })
        .collect()
}
