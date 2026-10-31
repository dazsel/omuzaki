//! `measure.arm_room` (#2683): how much room a character's body leaves its arms. The arm guard keeps the forearms and
//! hands out of the body, measured from the character's own meshes; this says what it measured (each part's size, a
//! skirt as its own flared hull), how close the arms come down when asked to hang straight, and, for a clip or an
//! imported take, how far the body moves the arms off what the clip asks, frame by frame.
//!
//! An agent reads it before it picks a clip for a character (a take that hangs the arms close fits a slim character,
//! not one in a wide skirt) and after it changes the body or the outfit. It changes nothing.

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::find_clip;
use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, IkGoals, Tick};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, Quality as BuildQuality, RegistryBuilder};
use omuzaki_io_ops::ops::avatar::is_imported;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_rig::ik::guard::BodyPart;
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::body::{Body, BodyServices, load_body};
use crate::frame::rig::RigFrame;
use crate::ops::render::job::read_definition;

/// The rate a clip is stepped at, frames a second: the stage's.
const FPS: f32 = 60.0;
/// The longest clip measured, seconds.
const MAX_SECONDS: f32 = 600.0;

/// Measure how much room a character's body leaves its arms, and how far it moves a clip's arms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "clip": "wave"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ArmRoom {
    /// The character.
    pub character: CharId,
    /// A clip to play on it: one of the character's own (`motion.import` stores takes there) or of the pose library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
}

/// One part of the body the arms are kept out of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PartRoom {
    /// What it is: `waist`, `torso`, `pelvis`, `left_thigh`, `right_thigh`, `head`, `skirt`. A tapering part comes as
    /// two pieces of the same name.
    pub part: String,
    /// Its half width at its upper end, millimetres.
    pub width_mm: f32,
    /// Its half depth, front to back, at its upper end, millimetres.
    pub depth_mm: f32,
    /// Its half width at its lower end, millimetres (the same as `width_mm` for a straight part).
    pub end_width_mm: f32,
}

/// A spread of values over a clip's frames and both arms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Spread {
    /// The mean.
    pub mean: f32,
    /// The 95th percentile.
    pub p95: f32,
    /// The largest.
    pub max: f32,
}

/// How far the body moves a clip's arms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClipRoom {
    /// The clip.
    pub clip: String,
    /// Frames measured, at 60 a second.
    pub frames: usize,
    /// The share of frames on which the body moved either arm by more than a centimetre at the wrist, 0 to 1.
    pub moved_share: f32,
    /// How far the wrists are from where the clip puts them, centimetres.
    pub wrist_off_cm: Spread,
    /// How far each arm's line (shoulder to wrist) turns off the clip's, degrees.
    pub arm_off_deg: Spread,
    /// When the arms are moved most, seconds into the clip.
    pub worst_s: f32,
}

/// What `measure.arm_room` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArmRoomResult {
    /// The character.
    pub character: CharId,
    /// Whether the body was measured and guards its arms (a body without humanoid arms, or one with no mesh on its
    /// torso, is not, and its arms go wherever they are put).
    pub guarded: bool,
    /// The parts, as measured.
    pub parts: Vec<PartRoom>,
    /// Asked to hang straight down, how far out the arms stay, degrees from straight down (shoulder to wrist): what
    /// the hips, the thighs or a skirt leave them.
    pub hang_deg: f32,
    /// The clip, if one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<ClipRoom>,
}

/// The name a part goes by in the result.
fn part_name(part: BodyPart) -> &'static str {
    match part {
        BodyPart::Torso => "torso",
        BodyPart::Waist => "waist",
        BodyPart::Pelvis => "pelvis",
        BodyPart::LeftThigh => "left_thigh",
        BodyPart::RightThigh => "right_thigh",
        BodyPart::Head => "head",
        BodyPart::Skirt => "skirt",
    }
}

/// The rig of a loaded body with its guard, and a twin without one: what the clip asks.
fn frames(body: &Body) -> Result<(RigFrame, RigFrame), OpError> {
    let rig_error = |e: crate::error::StageError| OpError::invalid("character", format!("its rig doesn't build: {e}"));
    let build = || match body {
        Body::Kit(built) => RigFrame::for_character(built),
        Body::Avatar(avatar) => RigFrame::for_skeleton(&avatar.skeleton, &avatar.springs, &avatar.constraints)
            .map(|frame| frame.guarding(avatar)),
    };
    let guarded = build().map_err(rig_error)?;
    let mut free = build().map_err(rig_error)?;
    free.without_guard();
    Ok((guarded, free))
}

/// Each arm's shoulder and wrist in the last solve of `frame`, `[left, right]`.
fn arms(frame: &RigFrame) -> [(Vec3, Vec3); 2] {
    let at = |bone| frame.joint(bone).unwrap_or(Vec3::ZERO);
    [(at(B::LeftUpperArm), at(B::LeftHand)), (at(B::RightUpperArm), at(B::RightHand))]
}

/// The mean, 95th percentile and largest of `values`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // counts
fn spread(values: &mut [f32]) -> Spread {
    if values.is_empty() {
        return Spread::default();
    }
    values.sort_by(f32::total_cmp);
    let n = values.len();
    Spread {
        mean: values.iter().sum::<f32>() / n as f32,
        p95: values[((n - 1) as f32 * 0.95).round() as usize],
        max: values[n - 1],
    }
}

impl ArmRoom {
    /// The character's body as the stage loads it. A host without the builder services builds a kit character with
    /// the registry's own builder; an imported one needs the store.
    fn load(&self, ctx: &OpCtx<'_>) -> Result<Body, OpError> {
        let fallback = BuildServices::default();
        let builder: &dyn CharacterBuilder = ctx.service::<dyn CharacterBuilder>().unwrap_or(&RegistryBuilder);
        let with = ctx.service::<BuildServices>().unwrap_or(&fallback);
        if let Ok(store) = ctx.service::<dyn Store>() {
            let mtoon = ctx.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon);
            let services = BodyServices::new(builder, with, store, mtoon, ctx.clock);
            return load_body(ctx.doc, &services, &self.character, None, BuildQuality::Live);
        }
        if is_imported(ctx.doc, &self.character) {
            return Err(OpError::unavailable("the content store, which holds an imported character's model"));
        }
        let def = read_definition(ctx.doc, &self.character, None)?;
        let options = BuildOptions { variant: None, quality: BuildQuality::Live, rig_export: false };
        let built = builder.build(&self.character, &def, &options, with).map_err(|e| {
            OpError::invalid("character", format!("character '{}' doesn't build: {e}", self.character))
        })?;
        Ok(Body::Kit(Box::new(built)))
    }

    /// Measures `body`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // frame counts
    fn measure(&self, ctx: &OpCtx<'_>, body: &Body) -> Result<ArmRoomResult, OpError> {
        let (mut guarded, mut free) = frames(body)?;
        let parts: Vec<PartRoom> = guarded
            .guard_parts()
            .map(|p| PartRoom {
                part: part_name(p.part).to_owned(),
                width_mm: p.radius * 1e3,
                depth_mm: p.depth * 1e3,
                end_width_mm: p.radius * p.flare * 1e3,
            })
            .collect();

        // Straight down: each upper arm turned from the T-pose to hang, the rest as the body stands.
        let mut hang = body.base_pose();
        hang.set(B::LeftUpperArm, Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2));
        hang.set(B::RightUpperArm, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        for bone in [B::LeftLowerArm, B::RightLowerArm] {
            hang.set(bone, Quat::IDENTITY);
        }
        guarded.solve(Tick::at_rate(0, FPS), &hang, &IkGoals::NONE);
        let hang_deg = arms(&guarded)
            .iter()
            .map(|(shoulder, wrist)| (*wrist - *shoulder).angle_between(Vec3::NEG_Y).to_degrees())
            .sum::<f32>()
            / 2.0;

        let clip = match &self.clip {
            None => None,
            Some(id) => {
                let clip = find_clip(ctx.doc, Some(&self.character), id)
                    .map_err(|e| OpError { field: e.field.or_else(|| Some("clip".to_owned())), ..e })?;
                let rest = match body {
                    Body::Kit(built) => HandRest::of(&built.rig.skeleton),
                    Body::Avatar(avatar) => HandRest::of(&avatar.skeleton),
                }
                .unwrap_or_else(|_| HandRest::nominal());
                let count = ((clip.length().min(MAX_SECONDS) * FPS).round() as usize).max(1);
                guarded.reset();
                let (mut wrist, mut angle) = (Vec::with_capacity(count * 2), Vec::with_capacity(count * 2));
                let (mut moved, mut worst) = (0_usize, (0.0_f32, 0.0_f32));
                for i in 0..count {
                    let t = i as f32 / FPS;
                    let pose: HumanoidPose = clip.sample(t, &rest);
                    let tick = Tick::at_rate(i as u64, FPS);
                    guarded.solve(tick, &pose, &IkGoals::NONE);
                    free.solve(tick, &pose, &IkGoals::NONE);
                    let mut frame_worst = 0.0_f32;
                    for ((s_g, w_g), (s_f, w_f)) in arms(&guarded).into_iter().zip(arms(&free)) {
                        let off = w_g.distance(w_f) * 100.0;
                        frame_worst = frame_worst.max(off);
                        wrist.push(off);
                        angle.push((w_g - s_g).angle_between(w_f - s_f).to_degrees());
                    }
                    moved += usize::from(frame_worst > 1.0);
                    if frame_worst > worst.0 {
                        worst = (frame_worst, t);
                    }
                }
                Some(ClipRoom {
                    clip: clip.id.clone(),
                    frames: count,
                    moved_share: moved as f32 / count as f32,
                    wrist_off_cm: spread(&mut wrist),
                    arm_off_deg: spread(&mut angle),
                    worst_s: worst.1,
                })
            }
        };
        Ok(ArmRoomResult { character: self.character.clone(), guarded: !parts.is_empty(), parts, hang_deg, clip })
    }
}

impl Op for ArmRoom {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.arm_room",
            summary: "Measure how much room a character's body leaves its arms: the body's parts as the arm guard keeps \
                      the arms out of them (a skirt as its own hull), how close the arms hang when asked to hang \
                      straight, and how far the body moves a clip's arms off what the clip asks.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let body = self.load(ctx)?;
        let result = self.measure(ctx, &body)?;
        let output = serde_json::to_value(result).map_err(|e| OpError::internal(format!("the arm room: {e}")))?;
        Ok(Plan {
            output: Some(output),
            cost: Some(CostHint { seconds: Some(1.0), gpu_mib: Some(0), usd: None }),
            ..Plan::default()
        })
    }
}

register_op!(ArmRoom);
