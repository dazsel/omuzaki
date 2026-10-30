//! `render.eye`: a close-up of a character's photoreal 3D eye (`face/eye3d`), stored as a PNG. It is how an agent sees
//! the eye it designs with `face.set_style`: the design is built and lit as a macro still by
//! `omuzaki_face::eye3d::closeup`, the same builder the eye's lab test draws through. A face without a 3D eye shows
//! the design one starts from. The document never changes.

use omuzaki_char_api::{CharId, paths};
use omuzaki_core::glam::UVec2;
use omuzaki_doc_api::DocRead;
use omuzaki_face::eye3d::EyeDesign;
use omuzaki_face::eye3d::closeup::{Closeup, Shot, View};
use omuzaki_face_api::FaceProfile;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{AlphaEncoding, RendererExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{RenderQuality, check_size, cost, store_png, with_renderer};
use super::still::encode_png;
use crate::error::StageError;

/// The size of a close-up when none is given: a macro photograph's proportions.
const DEFAULT_SIZE: [u32; 2] = [1024, 744];

fn default_size() -> [u32; 2] {
    DEFAULT_SIZE
}

/// Renders a close-up of a character's 3D eye and stores it as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "view": "front", "size": [512, 512], "quality": "live"},
        {"character": "kit", "shot": {"camera": [-0.03, -0.012, 0.05], "look": [-0.002, -0.0008, 0.01], "roll": -18,
                                      "fov": 32, "gaze": [-0.2, 0.17, 0.92], "depth_of_field": true}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct EyeCloseup {
    /// The character whose eye to show.
    pub character: CharId,
    /// `macro` (the default: three-quarter from the temple's side and below, rolled, the iris in focus and the lashes
    /// and skin falling out of it) or `front` (straight on, everything in focus).
    #[serde(default)]
    pub view: View,
    /// A shot of its own (camera, look, roll, field of view, gaze, depth of field, in the eye's frame) in place of the
    /// view's: `face.eye3d.fit` returns one that frames the eye as a photograph does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<Shot>,
    /// Width and height in pixels, each 16 to 4096.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline` (the default, the same bytes on every run). `live` builds the eye's textures at half
    /// the detail.
    #[serde(default)]
    pub quality: RenderQuality,
}

/// What a close-up gives back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EyeCloseupResult {
    /// `omuzaki://render/<hash>.png`.
    pub uri: String,
    /// The PNG's content hash in the asset store.
    pub hash: String,
    /// Width and height in pixels.
    pub size: [u32; 2],
    /// The view it was shot from.
    pub view: View,
    /// The shot it was taken with (the view's, unless one was given).
    pub shot: Shot,
    /// Whether the face has a 3D eye of its own; `false` shows the design one starts from.
    pub in_use: bool,
}

impl EyeCloseup {
    /// The side of the iris atlas and the skin texture at this quality.
    fn texture(&self) -> u32 {
        match self.quality {
            RenderQuality::Live => 1024,
            RenderQuality::High | RenderQuality::Offline => 2048,
        }
    }

    /// The character's eye design, and whether the face has one of its own.
    fn design(&self, doc: &dyn DocRead) -> Result<(EyeDesign, bool), OpError> {
        if doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let Some(face) = doc.get(&paths::face(&self.character)) else {
            return Ok((EyeDesign::default(), false));
        };
        let face = FaceProfile::deserialize(face).map_err(|e| {
            OpError::invalid("character", format!("the face of '{}' doesn't read: {e}", self.character))
                .hint("`face.describe_style` names what to fix")
        })?;
        let in_use = face.eye3d.is_some();
        Ok((face.eye3d.unwrap_or_default(), in_use))
    }

    /// The shot to take: the one given, or the view's.
    fn shot(&self) -> Result<Shot, OpError> {
        let shot = self.shot.clone().unwrap_or_else(|| self.view.shot());
        shot.check().map_err(|e| OpError::invalid("shot", e))?;
        Ok(shot)
    }

    /// The bytes the eye's textures take on the GPU: the iris atlas (two squares) and the skin texture, RGBA.
    fn texture_bytes(&self) -> u64 {
        let side = u64::from(self.texture());
        3 * side * side * 4
    }
}

impl Op for EyeCloseup {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.eye",
            summary: "Render a close-up of a character's photoreal 3D eye (its `face/eye3d` design, set with \
                      face.set_style) as a macro photograph lit by a studio softbox, from the `macro` or `front` view, \
                      and store it as a PNG; returns its omuzaki://render/ URI. A face without a 3D eye shows the \
                      design one starts from. The document never changes.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // Its finish plans no change (the PNG goes to the content store), so its empty patch is undone exactly and
            // nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.design(ctx.doc)?;
        self.shot()?;
        let size = check_size(self.size, self.quality)?;
        Ok(Plan { cost: Some(cost(size, 1, self.quality, self.texture_bytes())), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size: UVec2 = check_size(self.size, self.quality)?;
        let (design, in_use) = job.read(|doc| self.design(doc))?;
        let shot = self.shot()?;
        job.checkpoint("building", 0.0)?;
        let quality = self.quality.quality();
        let image = with_renderer(job, |renderer| {
            let closeup = Closeup::shoot(renderer, &design, &shot, self.texture())
                .map_err(|e| StageError::render("the eye", e))?;
            let image = renderer.render_image(size, AlphaEncoding::Straight, closeup.scene(), &quality);
            let released = closeup.release(renderer);
            let image = image.map_err(|e| StageError::render("the eye close-up", e))?;
            released.map_err(|e| StageError::render("releasing the eye", e))?;
            Ok(image)
        })?;
        job.checkpoint("encoding", 0.9)?;
        let png = encode_png(&image).map_err(|e| OpError::internal(e.to_string()))?;
        let stored = store_png(job, &png)?;
        job.checkpoint("done", 1.0)?;
        let result = EyeCloseupResult {
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: self.size,
            view: self.view,
            shot,
            in_use,
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the close-up: {e}")))
    }
}

register_op!(EyeCloseup);
