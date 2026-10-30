//! What a scene looks like around its cast (design D2, D8): `stage.set_backdrop`, `stage.set_ground` and
//! `stage.set_post`.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{BackdropFit, Colour, Tonemap};
use omuzaki_stage_api::{Backdrop, Bars, DepthOfFieldDef, GradeDef, GroundKind, OcclusionDef, SceneId};
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
        {"scene": "night_stream", "tonemap": "none", "grain": "off"},
        {"occlusion": {"radius": 0.005, "strength": 0.6, "contact": 0.015}},
        {"occlusion": "off"}
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
    /// Ambient occlusion and contact shadows (a fold's radius, strength and how far a contact shadow reaches, in
    /// metres; a field left out takes its default: 5 mm, 0.6 and 1.5 cm), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion: Option<OrOff<OcclusionDef>>,
}

impl Op for SetPost {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_post",
            summary: "Change a scene's post look: bloom on or off, tone mapping, depth of field, film grain, colour \
                      grade and ambient occlusion with contact shadows (`off` removes an effect). Only the fields \
                      given change.",
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
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({ "scene": id.as_str(), "changed": changed(&plan.patch) });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetPost);
