//! `render.turnaround`: a character from the front, three-quarter, side and back with one framing, each view stored as a
//! PNG. All four come from one staging and one render, so they share the character's warm-up and its light.

use omuzaki_char_api::{CharId, VariantId, paths};
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{JobCtx, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{CameraDef, Framing};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::job::{
    DEFAULT_LIGHT_RIG, RenderQuality, build_character, check_size, cost, still_light, store_png, with_renderer,
};
use super::preview::camera_for;
use super::still::{Shot, StillSpec, encode_png, render_stills};
use crate::scene::landmarks::Landmarks;

/// The views and their yaw in degrees, in this order.
pub const VIEWS: [(&str, f32); 4] = [("front", 0.0), ("three_quarter", 45.0), ("side", 90.0), ("back", 180.0)];

/// The lowest pitch a turnaround takes, degrees from above.
const MIN_PITCH: f32 = -30.0;
/// The highest pitch a turnaround takes, degrees from above.
const MAX_PITCH: f32 = 60.0;

fn full() -> Framing {
    Framing::Full
}

fn default_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

fn default_size() -> [u32; 2] {
    [768, 1024]
}

/// Renders a character from the front, three-quarter, side and back with one framing, and stores the four PNGs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderTurnaround {
    /// The character to render.
    pub character: CharId,
    /// One of its variants; absent renders the character as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// `face`, `bust`, `full` or `hands`; `free` is refused. Default `full`.
    #[serde(default = "full")]
    pub framing: Framing,
    /// Degrees from above, -30 to 60. Default 0.
    #[serde(default)]
    pub pitch: f32,
    /// The light rig, by name. Default `viewer-studio`.
    #[serde(default = "default_rig")]
    pub light_rig: String,
    /// An expression at full weight, on every view. Absent: the neutral face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionId>,
    /// Pixels per view, each side 16 to 4096. Default 768 x 1024.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline`. Default `offline`.
    #[serde(default)]
    pub quality: RenderQuality,
}

/// Refuses a framing a turnaround can't take: `free` has its own position, not a preset.
fn check_framing(framing: Framing) -> Result<(), OpError> {
    if framing == Framing::Free {
        return Err(OpError::invalid("framing", "a turnaround takes a preset framing, not free")
            .hint("choose face, bust, full or hands"));
    }
    Ok(())
}

/// Refuses a pitch outside the range a turnaround takes; NaN is outside it too.
fn check_pitch(pitch: f32) -> Result<(), OpError> {
    if (MIN_PITCH..=MAX_PITCH).contains(&pitch) {
        return Ok(());
    }
    Err(OpError::invalid("pitch", format!("pitch {pitch} is outside {MIN_PITCH} to {MAX_PITCH} degrees"))
        .hint("a pitch from above; negative looks up from below"))
}

#[allow(clippy::cast_precision_loss)] // sides are at most 4096, exact in an f32
impl omuzaki_ops_api::Op for RenderTurnaround {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.turnaround",
            summary: "Render a character from the front, three-quarter, side and back with one framing, and store the four PNGs.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    /// Checks the arguments against the document without building or rendering anything.
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_framing(self.framing)?;
        check_pitch(self.pitch)?;
        if ctx.doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let size = check_size(self.size)?;
        still_light(&self.light_rig)?;
        let images = 4;
        Ok(Plan {
            output: Some(json!({
                "character": self.character.to_string(),
                "size": self.size,
                "framing": self.framing,
                "views": VIEWS.len(),
            })),
            cost: Some(cost(size, images, self.quality)),
            ..Plan::default()
        })
    }

    /// Builds the character once, renders the four views in one call and stores each.
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = check_size(self.size)?;
        let built = build_character(job, &self.character, self.variant.as_ref(), self.quality)?;
        let landmarks = Landmarks::measure(&built).map_err(|e| OpError::invalid("character", e.to_string()))?;
        let aspect = size.x as f32 / size.y as f32;
        let mut shots = Vec::with_capacity(VIEWS.len());
        for (_, yaw) in VIEWS {
            let camera = CameraDef { yaw, pitch: self.pitch, ..CameraDef::preset(self.framing) };
            let mut shot = Shot::new(camera_for(&camera, &landmarks, aspect)?);
            if let Some(expression) = &self.expression {
                shot = shot.with_expression(expression.clone(), 1.0);
            }
            shots.push(shot);
        }
        let spec = StillSpec::new(still_light(&self.light_rig)?, size, self.quality.quality());
        let stills = with_renderer(job, |renderer| render_stills(renderer, built, &spec, &shots))?;
        if stills.len() != VIEWS.len() {
            return Err(OpError::internal(format!("the render gave {} stills for {} views", stills.len(), VIEWS.len())));
        }
        let mut views = Vec::with_capacity(VIEWS.len());
        for (still, (view, yaw)) in stills.iter().zip(VIEWS) {
            let png = encode_png(&still.image).map_err(|e| OpError::internal(e.to_string()))?;
            let stored = store_png(job, &png)?;
            views.push(json!({ "view": view, "yaw": yaw, "hash": stored.hash.to_string(), "uri": stored.uri }));
        }
        Ok(json!({ "views": views, "size": [size.x, size.y] }))
    }
}

register_op!(RenderTurnaround);
