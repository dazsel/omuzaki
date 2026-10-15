//! `render.preview`: a still of a character, stored as a PNG. The camera is a framing preset measured on the character or
//! a free camera; the light rig and the expression are named. It is a job, so nothing renders while it is planned.

use omuzaki_char_api::{CharId, VariantId, paths};
use omuzaki_core::glam::Vec3;
use omuzaki_ops_api::{JobCtx, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_face_api::ExpressionId;
use omuzaki_render_api::{Camera, Projection};
use omuzaki_stage_api::{Anchor, CameraDef, Framing};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::job::{
    DEFAULT_LIGHT_RIG, RenderQuality, build_character, check_size, cost, still_light, store_png, with_renderer,
};
use super::still::{Shot, StillSpec, encode_png, render_stills};
use crate::scene::frame::frame;
use crate::scene::landmarks::Landmarks;

/// Near plane of a free camera, in the anchor's units.
const FREE_NEAR: f32 = 0.05;
/// Far plane of a free camera, in the anchor's units.
const FREE_FAR: f32 = 50.0;

fn full_front() -> CameraDef {
    CameraDef::preset(Framing::Full)
}

fn default_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

fn default_size() -> [u32; 2] {
    [1024, 1024]
}

/// Renders a still of a character and stores it as a PNG. The camera is a preset measured on the character, or a free
/// camera; it takes a light rig and an expression. A job: nothing renders while planning, and the document never changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderPreview {
    /// The character to render.
    pub character: CharId,
    /// One of its variants; absent renders the character as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// The camera: a framing preset (`face`, `bust`, `full`, `hands`) measured on the character, or a `free` camera
    /// with a position and a target. Default: the full-length preset.
    #[serde(default = "full_front")]
    pub camera: CameraDef,
    /// The light rig, by name (`viewer-studio`, `viewer-candle`, ...). Default `viewer-studio`.
    #[serde(default = "default_rig")]
    pub light_rig: String,
    /// An expression of the character's face at full weight. Absent: the neutral face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionId>,
    /// Width and height in pixels, each 16 to 4096. Default 1024 x 1024.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline` (the same bytes on every run). Default `offline`.
    #[serde(default)]
    pub quality: RenderQuality,
}

/// The camera `def` gives on a character with `landmarks`, at `aspect` (width / height). A preset uses
/// `scene::frame::frame`. A `free` camera is `Camera::look_at(position, target, Vec3::Y, ..)` with the def's `fov`, in the
/// anchor's units: metres for `stage`, multiplied by `landmarks.height_m` for `subject`. Public so the other render ops share it.
///
/// # Errors
/// `OpError::invalid("camera", ..)` for a free camera without a position or target, a `member:<id>` anchor (a
/// preview has no cast), or a framing the character can't take (the `StageError::Framing` reason).
pub fn camera_for(def: &CameraDef, landmarks: &Landmarks, aspect: f32) -> Result<Camera, OpError> {
    check_camera(def)?;
    let member = |anchor: &Anchor| matches!(anchor, Anchor::Member(_));
    if member(&def.subject) || def.anchor.as_ref().is_some_and(member) {
        return Err(OpError::invalid("camera", "a member anchor has no cast in a preview: frame the subject instead"));
    }
    if def.framing == Framing::Free {
        return free_camera(def, landmarks);
    }
    frame(def.framing, landmarks, def, aspect).map_err(|e| OpError::invalid("camera", e.to_string()))
}

/// The free camera `def` places, measured against its anchor.
fn free_camera(def: &CameraDef, landmarks: &Landmarks) -> Result<Camera, OpError> {
    let (Some(position), Some(target)) = (def.position, def.target) else {
        return Err(OpError::invalid("camera", "a free camera needs a position and a target")
            .hint("give position and target as [x, y, z], measured against the anchor"));
    };
    let scale = if def.free_anchor() == Anchor::Subject { landmarks.height_m } else { 1.0 };
    let projection = Projection::Perspective { fov_y_deg: def.fov, near: FREE_NEAR * scale, far: FREE_FAR * scale };
    let camera = Camera::look_at(Vec3::from(position) * scale, Vec3::from(target) * scale, Vec3::Y, projection);
    camera.validate().map_err(|e| OpError::invalid("camera", e.to_string()))?;
    Ok(camera)
}

/// Refuses a camera whose numbers can't make a view: a field of view outside `(0, 180)`, a zoom not above zero, or a
/// number that isn't finite.
fn check_camera(def: &CameraDef) -> Result<(), OpError> {
    if !(def.fov.is_finite() && def.fov > 0.0 && def.fov < 180.0) {
        return Err(OpError::invalid("fov", format!("fov {} is not usable: give degrees above 0 and below 180", def.fov)));
    }
    if !(def.zoom.is_finite() && def.zoom > 0.0) {
        return Err(OpError::invalid("zoom", format!("zoom {} is not usable: give a number above 0", def.zoom)));
    }
    let finite = def.yaw.is_finite() && def.pitch.is_finite() && def.shift.iter().all(|s| s.is_finite());
    if !finite {
        return Err(OpError::invalid("camera", "yaw, pitch and shift must be finite numbers"));
    }
    Ok(())
}

impl RenderPreview {
    /// The framing's name, for the plan's output.
    fn framing_name(&self) -> &'static str {
        match self.camera.framing {
            Framing::Face => "face",
            Framing::Bust => "bust",
            Framing::Full => "full",
            Framing::Hands => "hands",
            Framing::Free => "free",
        }
    }
}

#[allow(clippy::cast_precision_loss)] // sides are at most 4096, exact in an f32
impl omuzaki_ops_api::Op for RenderPreview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.preview",
            summary: "Render a still of a character with a camera preset or a free camera, a light rig and an expression, and store it as a PNG.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    /// Checks the arguments against the document without building or rendering anything.
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let size = check_size(self.size)?;
        still_light(&self.light_rig)?;
        check_camera(&self.camera)?;
        Ok(Plan {
            output: Some(json!({
                "character": self.character.to_string(),
                "size": self.size,
                "framing": self.framing_name(),
            })),
            cost: Some(cost(size, 1, self.quality)),
            ..Plan::default()
        })
    }

    /// Builds the character, frames it, renders one still and stores it.
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = check_size(self.size)?;
        let built = build_character(job, &self.character, self.variant.as_ref(), self.quality)?;
        let landmarks = Landmarks::measure(&built).map_err(|e| OpError::invalid("character", e.to_string()))?;
        let aspect = size.x as f32 / size.y as f32;
        let camera = camera_for(&self.camera, &landmarks, aspect)?;
        let light = still_light(&self.light_rig)?;
        let mut shot = Shot::new(camera);
        if let Some(expression) = &self.expression {
            shot = shot.with_expression(expression.clone(), 1.0);
        }
        let spec = StillSpec::new(light, size, self.quality.quality());
        let stills = with_renderer(job, |renderer| render_stills(renderer, built, &spec, &[shot]))?;
        let still = stills.into_iter().next().ok_or_else(|| OpError::internal("the render returned no still"))?;
        let png = encode_png(&still.image).map_err(|e| OpError::internal(e.to_string()))?;
        let stored = store_png(job, &png)?;
        let report = still.report;
        Ok(json!({
            "hash": stored.hash.to_string(),
            "uri": stored.uri,
            "size": [size.x, size.y],
            "report": {
                "render_size": [report.render_size.x, report.render_size.y],
                "draw_calls": report.draw_calls,
                "triangles": report.triangles,
            },
        }))
    }
}

register_op!(RenderPreview);
