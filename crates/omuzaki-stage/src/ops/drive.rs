//! The driver binding ops (`docs/design/drivers.md` §4): `drive.set` and `drive.list`, and `drive.sync`, which undo
//! and redo use.
//!
//! A member's drive map is part of the scene (`cast/<member>.toml`, `[drive]`), so it is saved with the project and
//! undone like any scene edit. When the member is performing, `drive.set` also hands the new map to its performer at
//! once, and records the call that puts the old one back, so an undo reaches the live performer too.

use omuzaki_ops_api::{Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::drive::{DriveGroup, DriveMap, DriverKind};
use omuzaki_stage_api::{MemberId, SceneDef, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::act::performing;
use super::perform::op_error;
use super::scene::edit::{SceneEdit, unknown};

/// The live performer of `member` in `scene`, if it performs. The app's single performer counts as the subject's.
fn live_host<'a>(
    ctx: &OpCtx<'a>,
    def: &SceneDef,
    member: &MemberId,
) -> Option<super::act::Performing<'a>> {
    match performing(ctx, Some(member)) {
        Ok(found) => Some(found),
        // No cast: the app's one performer is the subject's.
        Err(_) if def.scene.subject.as_ref() == Some(member) || is_only(def, member) => performing(ctx, None).ok(),
        Err(_) => None,
    }
}

/// Whether `member` is the scene's only member, the subject a scene with no subject set has.
fn is_only(def: &SceneDef, member: &MemberId) -> bool {
    def.scene.subject.is_none() && def.cast.len() == 1 && def.cast.contains_key(member)
}

/// The call that hands `drive` to `member`'s performer.
fn sync_call(scene: &SceneId, member: &MemberId, drive: &DriveMap) -> Result<OpCall, OpError> {
    let args = Sync { scene: Some(scene.clone()), member: member.clone(), drive: drive.clone() };
    let args = serde_json::to_value(args).map_err(|e| OpError::internal(e.to_string()))?;
    Ok(OpCall { op: "drive.sync".into(), args })
}

/// How strongly one kind of driver moves one group of a cast member's channels: `weight` 0 never, 1 fully (the
/// default). The groups are `brows`, `eyes` (lids), `gaze` (where the eyes look), `mouth`, `head`, `body`, `extras`
/// and `custom`; the drivers are `score` (agents, scripts and the AI performer, through `act.*`), `tracker` (webcam,
/// phone, VMC), `voice` (lip sync) and `keys` (hotkeys and triggers). A higher driver wins where both move a channel:
/// keys over voice over tracker over score. So `gaze`, `tracker`, 0 lets an agent's `act.look` point a webcam-driven
/// character's eyes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"member": "kit", "group": "gaze", "driver": "tracker", "weight": 0.0},
        {"scene": "main", "member": "kit", "group": "head", "driver": "tracker", "weight": 0.5}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct Set {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The cast member.
    pub member: MemberId,
    /// The group of channels.
    pub group: DriveGroup,
    /// The kind of driver.
    pub driver: DriverKind,
    /// How strongly, 0 to 1.
    pub weight: f32,
}

impl Op for Set {
    fn meta() -> OpMeta {
        OpMeta {
            name: "drive.set",
            summary: "Set how strongly a kind of driver (score, tracker, voice, keys) moves a group of a cast member's \
                      channels (brows, eyes, gaze, mouth, head, body, extras, custom), 0 to 1.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Explicit,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(self.weight.is_finite() && (0.0..=1.0).contains(&self.weight)) {
            return Err(OpError::invalid("weight", "must be a number from 0 to 1"));
        }
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let Some(member) = edit.def.cast.get_mut(&self.member) else {
            return Err(unknown("member", "member", self.member.as_str(), edit.def.cast.keys().map(ToString::to_string)));
        };
        let before = member.drive.clone();
        member.drive.set(self.group, self.driver, self.weight);
        let after = member.drive.clone();
        let scene = edit.id.clone();
        let live = live_host(ctx, &edit.def, &self.member);
        let output = json!({
            "scene": scene.as_str(),
            "member": self.member.as_str(),
            "drive": after,
            "live": live.is_some(),
        });
        let mut plan = edit.finish(ctx, output)?;
        if let Some(live) = live.filter(|_| !ctx.dry_run && before != after) {
            live.host().set_drive(after.clone()).map_err(op_error)?;
            plan.effect = Some(Effect {
                summary: format!("{} drives {}'s {} at {}", self.driver.name(), self.member, self.group.name(), self.weight),
                undo: sync_call(&scene, &self.member, &before)?,
                redo: sync_call(&scene, &self.member, &after)?,
            });
        }
        Ok(plan)
    }
}

register_op!(Set);

/// Hand a drive map to a performing member's performer, without changing the scene: what undo and redo of
/// `drive.set` run. A member that is not performing has nothing to change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"member": "kit", "drive": {"gaze": {"tracker": 0.0}}}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct Sync {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The cast member.
    pub member: MemberId,
    /// The map: per group, per driver, a weight from 0 to 1; what it leaves out drives at full strength.
    #[serde(default)]
    pub drive: DriveMap,
}

impl Op for Sync {
    fn meta() -> OpMeta {
        OpMeta {
            name: "drive.sync",
            summary: "Hand a drive map to a performing cast member's performer without changing the scene (what undo \
                      of drive.set runs).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if let Some((group, driver, _)) = self.drive.invalid() {
            return Err(OpError::invalid(
                format!("drive.{}.{}", group.name(), driver.name()),
                "a driver's weight is a number from 0 to 1",
            ));
        }
        let edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let live = live_host(ctx, &edit.def, &self.member);
        if let Some(live) = live.as_ref().filter(|_| !ctx.dry_run) {
            live.host().set_drive(self.drive.clone()).map_err(op_error)?;
        }
        Ok(Plan::answer(json!({ "member": self.member.as_str(), "live": live.is_some() })))
    }
}

register_op!(Sync);

/// List who drives each cast member of a scene: its drive map as saved (what it leaves out drives at full strength),
/// and the map its performer uses when it is performing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"member": "kit"}], "x-omuzaki-fixture" = "scenes"))]
pub struct List {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// One member; every member when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Op for List {
    fn meta() -> OpMeta {
        OpMeta {
            name: "drive.list",
            summary: "List which drivers move which channel groups of each cast member, as saved and as performing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        if let Some(member) = &self.member
            && !edit.def.cast.contains_key(member)
        {
            return Err(unknown("member", "member", member.as_str(), edit.def.cast.keys().map(ToString::to_string)));
        }
        let members: Vec<Value> = edit
            .def
            .cast
            .iter()
            .filter(|(id, _)| self.member.as_ref().is_none_or(|m| m == *id))
            .map(|(id, member)| {
                let live = live_host(ctx, &edit.def, id).map(|live| live.host().drive());
                json!({ "member": id.as_str(), "drive": member.drive, "performing": live })
            })
            .collect();
        let groups: Vec<&str> = DriveGroup::ALL.iter().map(|g| g.name()).collect();
        let drivers: Vec<&str> = DriverKind::ALL.iter().map(|d| d.name()).collect();
        Ok(Plan::answer(json!({
            "scene": edit.id.as_str(),
            "members": members,
            "groups": groups,
            "drivers": drivers,
        })))
    }
}

register_op!(List);
