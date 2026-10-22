//! What a scene looks like around its cast (design D2, D8): `stage.set_backdrop`, `stage.set_ground` and
//! `stage.set_post`.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Colour, Tonemap};
use omuzaki_stage_api::{Backdrop, DepthOfFieldDef, GradeDef, GroundKind, SceneId};
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
}

/// Sets what shows behind a scene's cast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"kind": "transparent"}, {"kind": "colour", "colour": "#00b140"}, {"scene": "night_stream", "kind": "lighting"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetBackdrop {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// `lighting`, `colour` (with `colour`) or `transparent`.
    pub kind: BackdropKind,
    /// The colour, for `colour` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colour>,
}

impl Op for SetBackdrop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_backdrop",
            summary: "Set what shows behind a scene's cast: the lighting's background, a colour of its own (a green \
                      screen), or transparent for compositing in OBS.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let backdrop = match (self.kind, self.colour) {
            (BackdropKind::Colour, Some(colour)) => Backdrop::Colour { colour },
            (BackdropKind::Colour, None) => {
                return Err(OpError::invalid("colour", "a `colour` backdrop needs a colour")
                    .hint("give `colour` as #rrggbb, for a green screen #00b140"));
            }
            (_, Some(_)) => {
                return Err(OpError::invalid("colour", "only a `colour` backdrop takes a colour")
                    .hint("a `lighting` backdrop shows the lighting's background: stage.set_lighting sets it"));
            }
            (BackdropKind::Lighting, None) => Backdrop::Lighting {},
            (BackdropKind::Transparent, None) => Backdrop::Transparent {},
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
    "examples" = [{"opacity": 0.4}, {"kind": "none"}, {"scene": "night_stream", "radius": 1.5}, {"radius": "fit"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetGround {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// `shadow` (a disc that catches the cast's shadow) or `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<GroundKind>,
    /// How dark the shadow is, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// The disc's radius in subject units, or `fit` to size it to the cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<Radius>,
}

impl Op for SetGround {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_ground",
            summary: "Change a scene's ground: a shadow-catcher disc or none, its shadow's opacity, and its radius (or \
                      `fit` to size it to the cast). Only the fields given change.",
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
        {"scene": "night_stream", "tonemap": "none", "grain": "off"}
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
    /// Depth of field (focus `subject` or metres, range, max blur), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<OrOff<DepthOfFieldDef>>,
    /// Film grain, 0 to 1, or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<OrOff<f32>>,
    /// A colour grade (saturation and contrast, 1 leaves them alone), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<OrOff<GradeDef>>,
}

impl Op for SetPost {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_post",
            summary: "Change a scene's post look: bloom on or off, tone mapping, depth of field, film grain and colour \
                      grade (`off` removes an effect). Only the fields given change.",
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
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({ "scene": id.as_str(), "changed": changed(&plan.patch) });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetPost);
