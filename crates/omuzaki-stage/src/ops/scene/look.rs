//! What a scene looks like around its cast (design D2, D8): `stage.set_backdrop`, `stage.set_ground`,
//! `stage.set_post`, and how its cast moves as drawn animation: `stage.set_timing`, `stage.set_squash` and
//! `stage.set_smears`.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{BackdropFit, Colour, Glare, Stylize, Tonemap};
use omuzaki_stage_api::{
    Backdrop, Bars, DepthOfFieldDef, GradeDef, GroundKind, OcclusionDef, SceneId, Smears, Squash,
};
use omuzaki_store_api::{ContentHash, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{OrOff, SceneEdit, changed};

/// Which backdrop.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackdropKind {
    /// The lighting's background colour, so a rig brings its own mood.
    Lighting,
    /// A colour of the scene's own (a green screen is `#00b140` with the ground off).
    Colour,
    /// Nothing: alpha, for OBS and other compositors.
    Transparent,
    /// The lighting's environment: its sky, turned and as bright as it lights the cast (`stage.set_lighting` sets one).
    Environment,
    /// A vertical gradient from `top` to `bottom`, shown as authored.
    Gradient,
    /// A picture from the asset store (`asset`, a PNG or JPEG), shown as authored, placed by `fit` (`cover` fills the
    /// frame, `contain` shows all of it with bars, coloured by `top` and `bottom` or the lighting's background).
    Image,
}

/// Sets what shows behind a scene's cast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"kind": "transparent"}, {"kind": "colour", "colour": "#00b140"}, {"scene": "night_stream", "kind": "lighting"}, {"kind": "environment"}, {"kind": "gradient", "top": "#1d2b4f", "bottom": "#e8a87c"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetBackdrop {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// `lighting`, `colour` (with `colour`), `transparent`, `environment`, `gradient` (with `top` and `bottom`) or
    /// `image` (with `asset`, and optionally `fit` and the bars' `top` and `bottom`).
    pub kind: BackdropKind,
    /// The colour, for `colour` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colour>,
    /// The top colour: of a `gradient` (required), or of an `image`'s bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<Colour>,
    /// The bottom colour: of a `gradient` (required), or of an `image`'s bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<Colour>,
    /// The picture's content hash in the asset store, for `image` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<ContentHash>,
    /// How an `image` meets a frame of another shape: `cover` (the default) or `contain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<BackdropFit>,
}

impl SetBackdrop {
    /// Refuses a field `kind` doesn't take, naming it.
    fn refuse_foreign(&self) -> Result<(), OpError> {
        let takes: &[&str] = match self.kind {
            BackdropKind::Colour => &["colour"],
            BackdropKind::Gradient => &["top", "bottom"],
            BackdropKind::Image => &["asset", "fit", "top", "bottom"],
            BackdropKind::Lighting | BackdropKind::Transparent | BackdropKind::Environment => &[],
        };
        let given = [
            ("colour", self.colour.is_some()),
            ("top", self.top.is_some()),
            ("bottom", self.bottom.is_some()),
            ("asset", self.asset.is_some()),
            ("fit", self.fit.is_some()),
        ];
        match given.iter().find(|(field, set)| *set && !takes.contains(field)) {
            Some((field, _)) => {
                Err(OpError::invalid(*field, format!("a `{}` backdrop doesn't take `{field}`", self.kind.name())).hint(
                    "`colour` takes colour; `gradient` takes top and bottom; `image` takes asset, fit, top and bottom",
                ))
            }
            None => Ok(()),
        }
    }
}

impl BackdropKind {
    fn name(self) -> &'static str {
        match self {
            Self::Lighting => "lighting",
            Self::Colour => "colour",
            Self::Transparent => "transparent",
            Self::Environment => "environment",
            Self::Gradient => "gradient",
            Self::Image => "image",
        }
    }
}

impl Op for SetBackdrop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_backdrop",
            summary: "Set what shows behind a scene's cast: the lighting's background, a colour of its own (a green \
                      screen), transparent for compositing in OBS, the lighting's environment (its sky), a gradient, \
                      or a picture from the asset store.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.refuse_foreign()?;
        let backdrop = match self.kind {
            BackdropKind::Colour => Backdrop::Colour {
                colour: self.colour.ok_or_else(|| {
                    OpError::invalid("colour", "a `colour` backdrop needs a colour")
                        .hint("give `colour` as #rrggbb, for a green screen #00b140")
                })?,
            },
            BackdropKind::Gradient => match (self.top, self.bottom) {
                (Some(top), Some(bottom)) => Backdrop::Gradient { top, bottom },
                (None, _) => return Err(OpError::invalid("top", "a `gradient` backdrop needs `top` and `bottom`")),
                (_, None) => return Err(OpError::invalid("bottom", "a `gradient` backdrop needs `top` and `bottom`")),
            },
            BackdropKind::Image => {
                let asset = self.asset.ok_or_else(|| {
                    OpError::invalid("asset", "an `image` backdrop needs the picture's `asset`")
                        .hint("store a PNG or JPEG first (with its provenance) and give its content hash")
                })?;
                // Where the op runs with the asset store, an asset that isn't in it is refused now, not at the render.
                if let Ok(store) = ctx.service::<dyn Store>()
                    && !store.contains(&asset)
                {
                    return Err(OpError::invalid("asset", format!("the asset store has no {asset}"))
                        .hint("store the picture first; its provenance and licence stay with it in the store"));
                }
                let bars = match (self.top, self.bottom) {
                    (Some(top), Some(bottom)) => Some(Bars { top, bottom }),
                    (Some(colour), None) | (None, Some(colour)) => Some(Bars { top: colour, bottom: colour }),
                    (None, None) => None,
                };
                Backdrop::Image { asset, fit: self.fit.unwrap_or_default(), bars }
            }
            BackdropKind::Lighting => Backdrop::Lighting {},
            BackdropKind::Transparent => Backdrop::Transparent {},
            BackdropKind::Environment => Backdrop::Environment {},
        };
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        edit.def.scene.backdrop = backdrop;
        let output = json!({ "scene": edit.id.as_str(), "backdrop": edit.def.scene.backdrop });
        edit.finish(ctx, output)
    }
}

register_op!(SetBackdrop);

/// `fit` in a ground's radius: size the ground to the cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Fit the cast plus a margin.
    Fit,
}

/// A radius, or `fit`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Radius {
    /// Fit the cast.
    Fit(Fit),
    /// This radius, in subject units.
    Units(f32),
}

/// Changes a scene's ground: only the fields given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"opacity": 0.4}, {"kind": "none"}, {"kind": "floor", "colour": "#26242e"}, {"scene": "night_stream", "radius": 1.5}, {"radius": "fit"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetGround {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// `shadow` (a disc that catches the cast's shadow), `floor` (a lit stage floor the lights pool on) or `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<GroundKind>,
    /// How dark the shadow is, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// The disc's radius in subject units, or `fit` to size it to the cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<Radius>,
    /// A floor's colour, `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<omuzaki_render_api::Colour>,
}

impl Op for SetGround {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_ground",
            summary: "Change a scene's ground: a shadow-catcher disc, a lit floor or none, its shadow's opacity, its \
                      radius (or `fit` to size it to the cast) and a floor's colour. Only the fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let ground = &mut edit.def.scene.ground;
        ground.kind = self.kind.unwrap_or(ground.kind);
        ground.opacity = self.opacity.unwrap_or(ground.opacity);
        ground.colour = self.colour.or(ground.colour);
        match self.radius {
            Some(Radius::Fit(Fit::Fit)) => ground.radius = None,
            Some(Radius::Units(r)) => ground.radius = Some(r),
            None => {}
        }
        let output = json!({ "scene": edit.id.as_str(), "ground": edit.def.scene.ground });
        edit.finish(ctx, output)
    }
}

register_op!(SetGround);

/// Changes a scene's post look: only the fields given; `off` removes an effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"bloom": false},
        {"grain": 0.15, "grade": {"saturation": 1.1, "contrast": 1.05}},
        {"depth_of_field": {"focus": "subject", "range": 0.3, "max_blur": 0.01}},
        {"depth_of_field": {"focus": "subject", "range": 0.2, "max_blur": 0.03, "bokeh": 3}},
        {"scene": "night_stream", "tonemap": "none", "grain": "off"},
        {"occlusion": {"radius": 0.005, "strength": 0.6, "contact": 0.015}},
        {"occlusion": "off"},
        {"glare": {"streaks": 6, "length": 0.4, "rotation_deg": 15, "halo": 0.3}},
        {"glare": "off"}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetPost {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// Whether the lighting's bloom shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<bool>,
    /// How light maps to the display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tonemap: Option<Tonemap>,
    /// Depth of field (focus `subject` or metres, range, max blur, and `bokeh`: out-of-focus lights drawn as discs,
    /// 0 to 8), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<OrOff<DepthOfFieldDef>>,
    /// Film grain, 0 to 1, or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<OrOff<f32>>,
    /// A colour grade (saturation and contrast, 1 leaves them alone), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<OrOff<GradeDef>>,
    /// Ambient occlusion and contact shadows (a fold's radius, strength and how far a contact shadow reaches, in
    /// metres; a field left out takes its default: 5 mm, 0.6 and 1.5 cm), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion: Option<OrOff<OcclusionDef>>,
    /// The drawn look laid over the frame, or `off`: halftone dots, ink hatching, posterized tones, paper, a vignette,
    /// a lens fringe and split toning (each part optional; sizes are fractions of the frame's height). Replaces the
    /// scene's whole stylize block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stylize: Option<OrOff<Stylize>>,
    /// Lens glare from the brightest lights, or `off`: a star of `streaks` (0, 2, 4, 6 or 8) `length` of the frame's
    /// height long at `rotation_deg`, a `halo` ring and `ghosts` across the centre, tinted, from what is brighter than
    /// `threshold` (linear luminance). A field left out takes its default (strength 1, threshold 1.5, 4 streaks, 0.3
    /// long at 20 degrees, white, no halo or ghosts). Replaces the scene's whole glare block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glare: Option<OrOff<Glare>>,
}

impl Op for SetPost {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_post",
            summary: "Change a scene's post look: bloom on or off, tone mapping, depth of field, film grain, colour \
                      grade, ambient occlusion with contact shadows and the drawn stylize look (halftone, hatching, \
                      posterize, paper, vignette, lens fringe, split toning) and lens glare (streaks, halo, ghosts); \
                      `off` removes an effect. Only the fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let post = &mut edit.def.scene.post;
        post.bloom = self.bloom.unwrap_or(post.bloom);
        post.tonemap = self.tonemap.unwrap_or(post.tonemap);
        if let Some(dof) = &self.depth_of_field {
            post.depth_of_field = dof.clone().into_option();
        }
        if let Some(grain) = &self.grain {
            post.grain = grain.clone().into_option();
        }
        if let Some(grade) = &self.grade {
            post.grade = grade.clone().into_option();
        }
        if let Some(occlusion) = &self.occlusion {
            post.occlusion = occlusion.clone().into_option();
        }
        if let Some(stylize) = &self.stylize {
            post.stylize = stylize.clone().into_option();
        }
        if let Some(glare) = &self.glare {
            post.glare = glare.clone().into_option();
        }
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({ "scene": id.as_str(), "changed": changed(&plan.patch) });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetPost);

/// Sets how a scene's cast moves in time: every frame, or held as drawn animation is. Only the poses hold; the camera,
/// lights, effects and render keep the frame rate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"body": "twos"}, {"body": "threes", "face": "twos", "hold_secondary": false}, {"body": "ones"}, {"scene": "night_stream", "body_hz": 6}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetTiming {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The body's timing by name: `ones` (every frame), `twos` (12 poses a second, as at 24 fps on twos) or `threes`
    /// (8 a second).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Beat>,
    /// The body's poses per second, `0..=60` (0 is every frame), in place of `body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hz: Option<f32>,
    /// The face's timing by name, as `body`. Left out: the scene's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<Beat>,
    /// The face's updates per second, `0..=60`, in place of `face`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_hz: Option<f32>,
    /// Whether hair and cloth hold with the body (the drawn look) or keep moving between held poses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_secondary: Option<bool>,
}

/// A named animation beat at 24 fps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Beat {
    /// Every frame: smooth.
    Ones,
    /// A new pose every second frame at 24 fps: 12 a second.
    Twos,
    /// A new pose every third frame at 24 fps: 8 a second.
    Threes,
}

impl Beat {
    /// Poses per second.
    pub fn hz(self) -> f32 {
        match self {
            Self::Ones => 0.0,
            Self::Twos => 12.0,
            Self::Threes => 8.0,
        }
    }
}

/// `named` or `hz` (not both), checked to `0..=60`.
fn rate(field: &str, named: Option<Beat>, hz: Option<f32>) -> Result<Option<f32>, OpError> {
    match (named, hz) {
        (Some(_), Some(_)) => Err(OpError::invalid(format!("{field}_hz"), format!("give `{field}` or `{field}_hz`, not both"))),
        (Some(beat), None) => Ok(Some(beat.hz())),
        (None, Some(hz)) if hz.is_finite() && (0.0..=60.0).contains(&hz) => Ok(Some(hz)),
        (None, Some(hz)) => Err(OpError::invalid(format!("{field}_hz"), format!("{hz} is outside 0 to 60 a second"))
            .hint("0 moves every frame; 12 is on twos and 8 on threes at 24 fps")),
        (None, None) => Ok(None),
    }
}

impl Op for SetTiming {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_timing",
            summary: "Time a scene's cast like drawn animation: poses held on twos or threes (or any rate) while the \
                      camera and render stay smooth, the face on its own beat, hair and cloth held or flowing. `ones` \
                      moves every frame again.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let body = rate("body", self.body, self.body_hz)?;
        let face = rate("face", self.face, self.face_hz)?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let timing = &mut edit.def.scene.timing;
        timing.body_hz = body.unwrap_or(timing.body_hz);
        timing.face_hz = face.unwrap_or(timing.face_hz);
        timing.hold_secondary = self.hold_secondary.unwrap_or(timing.hold_secondary);
        let now = *timing;
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({
            "scene": id.as_str(),
            "body_hz": now.body_hz,
            "face_hz": now.face_hz,
            "hold_secondary": now.hold_secondary,
            "changed": changed(&plan.patch),
        });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetTiming);

/// Sets how a scene's cast squashes and stretches: bodies stretch as they move up or down and squash when that motion
/// stops or turns, keeping their volume. Drawn only: the rig, the physics and the face see the body as posed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"preset": "cartoon"}, {"preset": "subtle"}, {"preset": "rubber", "limit": 0.3}, {"stretch": 0.08, "squash": 0.15}, {"preset": "off"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetSquash {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// A starting point by name: `off`, `subtle` (a lively body that still reads as solid), `cartoon` (the classic
    /// bounce) or `rubber` (a soft, jiggling one). The fields below adjust it; left out, they adjust the scene's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<SquashPreset>,
    /// Stretch for each metre per second the body moves up or down, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stretch: Option<f32>,
    /// Squash for each metre per second of the weight's lag at a sudden stop or start, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squash: Option<f32>,
    /// How fast the weight bounces back after an impact, hertz, `0.5..=12`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounce_hz: Option<f32>,
    /// How quickly the bounce dies, `0.05..=1` (1 settles with no wobble).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f32>,
    /// The most the height changes either way, as a share of it, `0..=0.6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f32>,
}

/// A named squash and stretch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SquashPreset {
    /// None: the body as posed.
    Off,
    /// A lively body that still reads as solid.
    Subtle,
    /// The classic cartoon bounce.
    Cartoon,
    /// A soft body that jiggles after every landing.
    Rubber,
}

impl SquashPreset {
    /// The settings it names.
    pub fn settings(self) -> Squash {
        match self {
            Self::Off => Squash::OFF,
            Self::Subtle => Squash { stretch: 0.04, squash: 0.08, bounce_hz: 3.5, damping: 0.55, limit: 0.1 },
            Self::Cartoon => Squash { stretch: 0.1, squash: 0.2, bounce_hz: 3.0, damping: 0.35, limit: 0.25 },
            Self::Rubber => Squash { stretch: 0.16, squash: 0.35, bounce_hz: 4.5, damping: 0.18, limit: 0.4 },
        }
    }
}

/// `value` if given, checked to `lo..=hi`.
fn ranged(field: &str, value: Option<f32>, lo: f32, hi: f32) -> Result<Option<f32>, OpError> {
    match value {
        Some(v) if !(v.is_finite() && (lo..=hi).contains(&v)) => {
            Err(OpError::invalid(field, format!("{v} is outside {lo} to {hi}")))
        }
        _ => Ok(value),
    }
}

impl Op for SetSquash {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_squash",
            summary: "Give a scene's cast squash and stretch: bodies stretch as they rise or fall and squash on a landing \
                      or a push-off, keeping their volume, then bounce back. A preset (`subtle`, `cartoon`, `rubber`) or \
                      the gains by hand; `off` draws bodies as posed.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stretch = ranged("stretch", self.stretch, 0.0, 1.0)?;
        let squash = ranged("squash", self.squash, 0.0, 1.0)?;
        let bounce = ranged("bounce_hz", self.bounce_hz, 0.5, 12.0)?;
        let damping = ranged("damping", self.damping, 0.05, 1.0)?;
        let limit = ranged("limit", self.limit, 0.0, 0.6)?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let q = &mut edit.def.scene.squash;
        if let Some(preset) = self.preset {
            *q = preset.settings();
        }
        q.stretch = stretch.unwrap_or(q.stretch);
        q.squash = squash.unwrap_or(q.squash);
        q.bounce_hz = bounce.unwrap_or(q.bounce_hz);
        q.damping = damping.unwrap_or(q.damping);
        q.limit = limit.unwrap_or(q.limit);
        let now = *q;
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({
            "scene": id.as_str(),
            "stretch": now.stretch,
            "squash": now.squash,
            "bounce_hz": now.bounce_hz,
            "damping": now.damping,
            "limit": now.limit,
            "on": !now.is_off(),
            "changed": changed(&plan.patch),
        });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetSquash);

/// Sets how a scene's cast smears on fast moves: a part that whips across the frame is drawn as one streak from where it
/// was a moment ago to where it is, the way animated films draw a swing, and slow moves stay crisp. Drawn only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"preset": "cartoon"}, {"preset": "subtle"}, {"preset": "wild", "jag": 1.0}, {"amount": 1.2, "length_s": 0.06, "min_speed": 1.5}, {"preset": "off"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetSmears {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// A starting point by name: `off`, `subtle` (short streaks on the fastest moves), `cartoon` (the classic smear
    /// on a swing) or `wild` (long, ragged streaks). The fields below adjust it; left out, they adjust the scene's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<SmearPreset>,
    /// How far back the streak reaches: `1` to where the part was `length_s` ago, `2` as far again, `0..=2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f32>,
    /// How far back in time the earlier pose is taken, seconds, `0.01..=0.25`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_s: Option<f32>,
    /// The speed a part must pass before it smears, metres per second, `0..=10`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_speed: Option<f32>,
    /// How ragged the streak's edge is, `0..=1` (0 one clean shape, 1 uneven fingers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jag: Option<f32>,
}

/// A named smear.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmearPreset {
    /// None: every frame drawn as posed.
    Off,
    /// Short, clean streaks on the fastest moves only.
    Subtle,
    /// The classic smear on a swing.
    Cartoon,
    /// Long, ragged streaks on anything quick.
    Wild,
}

impl SmearPreset {
    /// The settings it names.
    pub fn settings(self) -> Smears {
        match self {
            Self::Off => Smears::OFF,
            Self::Subtle => Smears { amount: 0.6, length_s: 0.033, min_speed: 1.5, jag: 0.2 },
            Self::Cartoon => Smears { amount: 1.0, length_s: 0.05, min_speed: 1.0, jag: 0.5 },
            Self::Wild => Smears { amount: 1.6, length_s: 0.08, min_speed: 0.6, jag: 0.9 },
        }
    }
}

impl Op for SetSmears {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_smears",
            summary: "Give a scene's cast smear frames: a fast move is drawn as one streak from where a part was a moment \
                      ago, the animated-film way, while slow moves stay crisp. A preset (`subtle`, `cartoon`, `wild`) \
                      or the streak's reach, time, speed threshold and raggedness by hand; `off` draws every frame as \
                      posed.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let amount = ranged("amount", self.amount, 0.0, 2.0)?;
        let length = ranged("length_s", self.length_s, 0.01, 0.25)?;
        let speed = ranged("min_speed", self.min_speed, 0.0, 10.0)?;
        let jag = ranged("jag", self.jag, 0.0, 1.0)?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let m = &mut edit.def.scene.smears;
        if let Some(preset) = self.preset {
            *m = preset.settings();
        }
        m.amount = amount.unwrap_or(m.amount);
        m.length_s = length.unwrap_or(m.length_s);
        m.min_speed = speed.unwrap_or(m.min_speed);
        m.jag = jag.unwrap_or(m.jag);
        let now = *m;
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({
            "scene": id.as_str(),
            "amount": now.amount,
            "length_s": now.length_s,
            "min_speed": now.min_speed,
            "jag": now.jag,
            "on": !now.is_off(),
            "changed": changed(&plan.patch),
        });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetSmears);
