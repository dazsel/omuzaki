//! The live control ops (`docs/design/drivers.md` §5): `act.expression`, `act.look`, `act.channels`, `act.clip`,
//! `act.release`, `act.list`, and `act.react` ([`react`]), which knocks the body. Anything that can run an op (a
//! person's panel, an agent over MCP, a script, a show's automation, the AI performer) moves a performing character
//! with them: each plays an act on the member's live score
//! ([`PerformHost::act`]), which the score layer blends under the tracker unless the member's drive map gives it the
//! channels, or the call asks for `over_tracking` (`act.expression`, `act.channels`), which wins on its channels.
//!
//! `act.expression` plays an expression the way its `playback` says when the call doesn't: its own fade-in and
//! fade-out (`0` is a hard cut), and its `auto_stop_s` as the hold.
//!
//! They are live actions, like `trigger.press`: logged, scope `perform`, nothing to undo (the act fades by itself or
//! on `act.release`). A member left out means the scene's subject.

pub mod react;

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim::poses::{ClipLibrary, find_clip};
use omuzaki_char_api::CharId;
use omuzaki_anim_api::Channel;
use omuzaki_core::text::nearest;
use omuzaki_face_api::{ExpressionId, Playback, narrow};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::ids::MemberId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::perform::{host, op_error};
use crate::perform::cast::LiveCast;
use crate::perform::host::{ActRequest, PerformHost};
use crate::perform::score::{Envelope, MAX_ACT_S};

/// Seconds an act fades in and out when the call gives none.
const DEFAULT_FADE_S: f32 = 0.2;
/// Seconds an act holds when the call gives no `hold_s` and does not hold it.
const DEFAULT_HOLD_S: f32 = 2.0;
/// How much the head follows a look when the call does not say.
const DEFAULT_HEAD: f32 = 0.35;

/// A member's performer: borrowed from the services, or shared out of the cast.
pub(crate) enum Performing<'a> {
    /// The app's single performer, the subject.
    Subject(&'a dyn PerformHost),
    /// A member of the live cast.
    Member(MemberId, Arc<dyn PerformHost>),
}

impl Performing<'_> {
    /// Its host.
    pub(crate) fn host(&self) -> &dyn PerformHost {
        match self {
            Self::Subject(host) => *host,
            Self::Member(_, host) => host.as_ref(),
        }
    }

    /// The member it is, when the cast says.
    pub(crate) fn member(&self) -> Option<&MemberId> {
        match self {
            Self::Subject(_) => None,
            Self::Member(member, _) => Some(member),
        }
    }
}

/// The performer of `member` (the subject's with `None`): from the live cast when the app runs one, else the app's
/// single performer, which only the subject can name by leaving `member` out.
pub(crate) fn performing<'a>(ctx: &OpCtx<'a>, member: Option<&MemberId>) -> Result<Performing<'a>, OpError> {
    if let Ok(cast) = ctx.service::<dyn LiveCast>() {
        return match cast.performer(member) {
            Some((id, host)) => Ok(Performing::Member(id, host)),
            None => Err(not_performing(member, &cast.members())),
        };
    }
    match member {
        None => host(ctx).map(Performing::Subject),
        Some(member) => Err(not_performing(Some(member), &[])),
    }
}

/// The refusal for a member that does not perform, with the ones that do.
fn not_performing(member: Option<&MemberId>, members: &[MemberId]) -> OpError {
    let names: Vec<&str> = members.iter().map(MemberId::as_str).collect();
    let error = match member {
        Some(member) => OpError::not_found(format!("member '{member}' is not performing")),
        None => OpError::unavailable("PerformHost"),
    };
    match (member.and_then(|m| nearest(m.as_str(), names.iter().copied())), names.is_empty()) {
        (_, true) => error.hint("start the live performance first (the Live workspace), then ask again"),
        (Some(near), false) => error.hint(format!("did you mean '{near}'? performing: {}", names.join(", "))),
        (None, false) => error.hint(format!("performing: {}", names.join(", "))),
    }
}

/// How long an act holds and fades, as the calls give it (`hold_s`, `held`, `fade_s`).
struct Timing {
    hold_s: Option<f32>,
    held: bool,
    fade_s: Option<f32>,
}

impl Timing {
    /// The envelope, with `hold` when the call gives no `hold_s`; refuses a time that is not a number from 0 to 600.
    fn envelope(&self, hold: Option<f32>) -> Result<Envelope, OpError> {
        let fade = seconds("fade_s", self.fade_s.unwrap_or(DEFAULT_FADE_S))?;
        let hold = match (self.held, self.hold_s) {
            (true, _) => None,
            (false, Some(s)) => Some(seconds("hold_s", s)?),
            (false, None) => hold,
        };
        Ok(Envelope::new(fade, hold))
    }
}

/// `value`, refused unless it is a number of seconds from 0 to [`MAX_ACT_S`].
fn seconds(field: &str, value: f32) -> Result<f32, OpError> {
    if value.is_finite() && (0.0..=MAX_ACT_S).contains(&value) {
        Ok(value)
    } else {
        Err(OpError::invalid(field, format!("must be a number of seconds from 0 to {MAX_ACT_S}")))
    }
}

/// `value`, refused unless it is a number in `lo..=hi`.
fn within(field: &str, value: f32, lo: f32, hi: f32) -> Result<f32, OpError> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(value)
    } else {
        Err(OpError::invalid(field, format!("must be a number from {lo} to {hi}")))
    }
}

/// Plays `request` on `member`'s live score and answers with its id; a dry run checks and answers what would play.
fn play(ctx: &OpCtx<'_>, member: Option<&MemberId>, request: ActRequest, envelope: Envelope) -> Result<Plan, OpError> {
    let performing = performing(ctx, member)?;
    let member = performing.member().map(MemberId::as_str);
    if ctx.dry_run {
        return Ok(Plan::answer(json!({ "dry_run": true, "member": member, "act": request, "envelope": envelope })));
    }
    let host = performing.host();
    let id = host.act(request, envelope).map_err(op_error)?;
    // The envelope as it plays: a one-shot clip holds for its own length.
    let playing = host.acts().into_iter().find(|info| info.id == id).map_or(envelope, |info| info.envelope);
    Ok(Plan::answer(json!({ "act": id, "member": member, "envelope": playing })))
}

/// A live action's facts.
const fn live(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
}

/// Show one of the character's expressions for a while, live, over the rest face: it fades in, holds and fades out
/// (or holds until `act.release`). A second expression takes over from the first. On a member a webcam drives, the
/// webcam wins on the channels it reads unless the member's drive map (`drive.set`) gives them to the score.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "expression": "happy" },
    { "expression": "surprised", "intensity": 0.7, "hold_s": 1.5 },
    { "expression": "sad", "held": true, "fade_s": 0.5 },
    { "expression": "surprised", "fade_s": 0, "hold_s": 1, "over_tracking": true }
]))]
pub struct Expression {
    /// An expression of the character's face, such as `happy` (`act.list` lists them).
    pub expression: String,
    /// Strength, 0 to 1. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// Seconds to hold at full strength before fading out. Default: the expression's `auto_stop_s` (counted from
    /// when it starts), else 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_s: Option<f32>,
    /// Hold until `act.release` instead of for `hold_s`. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Seconds to fade in and out, 0 to snap (a hard cut). Default: the expression's own `fade_in_s` and
    /// `fade_out_s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_s: Option<f32>,
    /// Win over the webcam, lip sync and hotkeys on the channels the expression sets (eyes forced shut while
    /// tracking), instead of playing under them. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub over_tracking: bool,
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Expression {
    /// The envelope: the call's times where it gives them, else the expression's `playback`.
    fn envelope(&self, playback: &Playback) -> Result<Envelope, OpError> {
        let (fade_in, fade_out) = match self.fade_s {
            Some(fade) => {
                let fade = seconds("fade_s", fade)?;
                (fade, fade)
            }
            None => (narrow(playback.fade_in_s), narrow(playback.fade_out_s)),
        };
        let hold = match (self.held, self.hold_s, playback.auto_stop_s) {
            (true, _, _) => None,
            (false, Some(s), _) => Some(seconds("hold_s", s)?),
            // An auto-stop counts from the show, so the hold is what is left of it after the fade in.
            (false, None, Some(stop)) => Some((narrow(stop) - fade_in).max(0.0)),
            (false, None, None) => Some(DEFAULT_HOLD_S),
        };
        Ok(Envelope { fade_in, hold, fade_out }.clamped())
    }
}

impl Op for Expression {
    fn meta() -> OpMeta {
        live("act.expression", "Show one of the character's expressions live for a while (or until act.release).")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let expression =
            ExpressionId::new(&self.expression).map_err(|e| OpError::invalid("expression", e.to_string()))?;
        let intensity = within("intensity", self.intensity.unwrap_or(1.0), 0.0, 1.0)?;
        let performing = performing(ctx, self.member.as_ref())?;
        let expressions = performing.host().expressions();
        let Some(found) = expressions.0.get(&expression) else {
            let names = expressions.0.keys().map(ExpressionId::as_str);
            let error = OpError::not_found(format!("no expression '{}' on this character", self.expression));
            return Err(match nearest(&self.expression, names) {
                Some(near) => error.hint(format!("did you mean '{near}'? act.list lists the expressions")),
                None => error.hint("act.list lists the expressions"),
            });
        };
        let envelope = self.envelope(&found.playback)?;
        let request = ActRequest::Expression { expression, intensity, over_tracking: self.over_tracking };
        play(ctx, self.member.as_ref(), request, envelope)
    }
}

register_op!(Expression);

/// Where a look points, by name, as the viewer sees it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LookAt {
    /// Into the camera.
    Camera,
    /// Toward the left of the picture.
    Left,
    /// Toward the right of the picture.
    Right,
    /// Up.
    Up,
    /// Down.
    Down,
}

impl LookAt {
    /// `(x, y)` in look units: `x` toward the right of the picture (the character's own left), `y` up.
    const fn direction(self) -> (f32, f32) {
        match self {
            Self::Camera => (0.0, 0.0),
            Self::Left => (-0.8, 0.0),
            Self::Right => (0.8, 0.0),
            Self::Up => (0.0, 0.7),
            Self::Down => (0.0, -0.7),
        }
    }
}

/// Point the character's eyes somewhere for a while, live, with the head following a little: into the camera, to a
/// side, or to `x`/`y` (each -1 to 1; `x` toward the right of the picture, `y` up). On a member a webcam drives, the
/// webcam's eyes win unless the member's drive map gives the gaze to the score (`drive.set` gaze tracker 0).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "at": "camera" },
    { "at": "left", "hold_s": 1.0 },
    { "x": 0.4, "y": -0.2, "head": 0.0, "held": true }
]))]
pub struct Look {
    /// Where, by name: `camera`, `left`, `right`, `up` or `down`. Default `camera`; `x` and `y` override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<LookAt>,
    /// Toward the right of the picture (the character's own left), -1 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    /// Up, -1 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    /// How much the head turns with the eyes, 0 (the eyes alone) to 1. Default 0.35.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<f32>,
    /// Seconds to hold at full strength before fading out. Default 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_s: Option<f32>,
    /// Hold until `act.release` instead of for `hold_s`. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Seconds to fade in and out, 0 to snap. Default 0.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_s: Option<f32>,
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Look {
    fn timing(&self) -> Timing {
        Timing { hold_s: self.hold_s, held: self.held, fade_s: self.fade_s }
    }
}

impl Op for Look {
    fn meta() -> OpMeta {
        live("act.look", "Point the character's eyes (and a little of the head) somewhere live for a while.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (named_x, named_y) = self.at.unwrap_or(LookAt::Camera).direction();
        let x = within("x", self.x.unwrap_or(named_x), -1.0, 1.0)?;
        let y = within("y", self.y.unwrap_or(named_y), -1.0, 1.0)?;
        let head = within("head", self.head.unwrap_or(DEFAULT_HEAD), 0.0, 1.0)?;
        let envelope = self.timing().envelope(Some(DEFAULT_HOLD_S))?;
        play(ctx, self.member.as_ref(), ActRequest::Look { x, y, head }, envelope)
    }
}

register_op!(Look);

/// Set some of the character's channels to values for a while, live, by name (`ARKit` blendshapes such as `jawOpen`,
/// head angles such as `headYaw` in radians, and the anime extras). Each value is held to its channel's range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "channels": { "jawOpen": 0.4, "mouthSmileLeft": 0.6, "mouthSmileRight": 0.6 } },
    { "channels": { "headRoll": 0.2 }, "held": true, "fade_s": 0.5 }
]))]
pub struct Channels {
    /// Channel names and their values.
    pub channels: BTreeMap<String, f32>,
    /// Seconds to hold at full strength before fading out. Default 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_s: Option<f32>,
    /// Hold until `act.release` instead of for `hold_s`. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Seconds to fade in and out, 0 to snap. Default 0.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_s: Option<f32>,
    /// Win over the webcam, lip sync and hotkeys on these channels instead of playing under them. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub over_tracking: bool,
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Channels {
    fn timing(&self) -> Timing {
        Timing { hold_s: self.hold_s, held: self.held, fade_s: self.fade_s }
    }
}

impl Op for Channels {
    fn meta() -> OpMeta {
        live("act.channels", "Set some of the character's channels by name live, for a while or until act.release.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.channels.is_empty() {
            return Err(OpError::invalid("channels", "name at least one channel").hint("such as {\"jawOpen\": 0.4}"));
        }
        let mut values = Vec::with_capacity(self.channels.len());
        for (name, value) in &self.channels {
            let field = format!("channels.{name}");
            let Some(channel) = Channel::from_name(name) else {
                let names = Channel::arkit().iter().map(|info| info.name);
                let error = OpError::invalid(&field, format!("there is no channel '{name}'"));
                return Err(match nearest(name, names) {
                    Some(near) => error.hint(format!("did you mean '{near}'?")),
                    None => error,
                });
            };
            if !value.is_finite() {
                return Err(OpError::invalid(&field, "must be a number"));
            }
            values.push((channel, *value));
        }
        let envelope = self.timing().envelope(Some(DEFAULT_HOLD_S))?;
        play(ctx, self.member.as_ref(), ActRequest::Channels { values, over_tracking: self.over_tracking }, envelope)
    }
}

register_op!(Channels);

/// Play a body clip live, such as `wave` or `hand-on-chest`, faded over the body (the tracked head and the face go on as
/// before): a character's own clip (`clip.set`) when `character` names one, else the clip library's. A clip that plays
/// once (`yawn`) ends with itself; a looping one (`wave`) plays for `hold_s` (default 2), or until `act.release` with
/// `held`. A clip's hands land where its keys reach on this body (a hand on the chest, palms together).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "clip": "wave" },
    { "clip": "wave-both", "hold_s": 3.0, "weight": 0.8 },
    { "clip": "hand-on-chest", "held": true }
]))]
pub struct Clip {
    /// The clip (`clip.list` lists them; `act.list` lists the library's).
    pub clip: String,
    /// The character whose own clips (`clip.set`) are looked in first. Left out, only the library's clips play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
    /// How strongly it moves the body, 0 to 1. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
    /// Seconds to hold at full strength before fading out. Default: a one-shot clip's own length, 2 for a looping one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_s: Option<f32>,
    /// Hold until `act.release` instead of for `hold_s`. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Seconds to fade in and out, 0 to snap. Default 0.25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_s: Option<f32>,
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

/// The clip library's ids.
fn clip_ids() -> Result<Vec<&'static str>, OpError> {
    let library = ClipLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
    Ok(library.iter().map(|clip| clip.id.as_str()).collect())
}

impl Clip {
    fn timing(&self) -> Timing {
        Timing { hold_s: self.hold_s, held: self.held, fade_s: self.fade_s }
    }
}

impl Op for Clip {
    fn meta() -> OpMeta {
        live("act.clip", "Play a body clip live (wave, hand-on-chest, a character's own clip), faded over the body.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let weight = within("weight", self.weight.unwrap_or(1.0), 0.0, 1.0)?;
        let clip = find_clip(ctx.doc, self.character.as_ref(), &self.clip).map_err(|e| match e.field {
            None => OpError { field: Some("clip".to_owned()), ..e },
            Some(_) => e,
        })?;
        // No hold given: a one-shot clip plays once (the host holds it for its own length) and a looping one holds
        // for the default.
        let mut envelope = self.timing().envelope(clip.looped().then_some(DEFAULT_HOLD_S))?;
        if self.fade_s.is_none() {
            envelope = Envelope::new(0.25, envelope.hold);
        }
        play(ctx, self.member.as_ref(), ActRequest::clip_def(clip, weight), envelope)
    }
}

register_op!(Clip);

/// Let go of a live act (its id from the call that played it), or of every act with none: each fades out from where
/// it is over its own fade.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Release {
    /// The act's id; every act of the member when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<u64>,
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Op for Release {
    fn meta() -> OpMeta {
        live("act.release", "Let go of a live act by its id, or of every act, so it fades out.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let performing = performing(ctx, self.member.as_ref())?;
        let member = performing.member().map(MemberId::as_str);
        if let Some(id) = self.act
            && !performing.host().acts().iter().any(|info| info.id == id)
        {
            return Err(OpError::not_found(format!("act {id} is not playing")).hint("act.list lists what plays"));
        }
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "member": member, "act": self.act })));
        }
        performing.host().release_act(self.act).map_err(op_error)?;
        Ok(Plan::answer(json!({ "released": self.act.map_or(json!("all"), Value::from), "member": member })))
    }
}

register_op!(Release);

/// List what plays live on a member (its acts, with their ids), what it can show (its expressions and the clips),
/// and which members perform.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct List {
    /// The cast member; the scene's subject when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
}

impl Op for List {
    fn meta() -> OpMeta {
        OpMeta {
            name: "act.list",
            summary: "List a performing member's live acts, the expressions and clips it can play, and who performs.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let performing = performing(ctx, self.member.as_ref())?;
        let host = performing.host();
        let expressions: Vec<String> = host.expressions().0.keys().map(|id| id.as_str().to_owned()).collect();
        let members: Vec<String> = ctx
            .service::<dyn LiveCast>()
            .map(|cast| cast.members().iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        let acts = serde_json::to_value(host.acts()).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(json!({
            "member": performing.member().map(MemberId::as_str),
            "acts": acts,
            "expressions": expressions,
            "clips": clip_ids()?,
            "members": members,
        })))
    }
}

register_op!(List);
