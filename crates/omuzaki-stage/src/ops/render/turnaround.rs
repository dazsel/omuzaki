//! `render.turnaround`: a character from the front, three-quarter, side and back, in one row, stored as a PNG (spec v3
//! §11). It renders any character of the project, built or imported, through the same stills as `render.preview`; the
//! print-quality sheet with labels and annotations is `sheet.*`'s (EX-10).

use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, VariantId};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::SystemClock;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{DEFAULT_LIGHT_RIG, RenderQuality, check_size, cost, still_light, with_renderer};
use super::picture::{canvas, paste, store, straight};
use super::still::{Shot, StillSpec, render_body_stills};
use crate::body::{BodyServices, load_body};
use crate::scene::frame::frame;
use crate::sheet::turnaround::View;

/// The gap between views, in pixels.
pub const GUTTER: u32 = 8;
/// One view's size when none is given: a portrait cell.
const DEFAULT_SIZE: [u32; 2] = [384, 640];

fn default_size() -> [u32; 2] {
    DEFAULT_SIZE
}

fn default_light_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

/// Renders a character from four sides in one row and stores it as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "framing": "bust", "size": [256, 256], "quality": "live"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct Turnaround {
    /// The character, built from the kit or imported.
    pub character: CharId,
    /// A variant of a kit character to render instead of its base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// The shot of every view: `full` (the default), `bust` or `face`.
    #[serde(default)]
    pub framing: Framing,
    /// The light rig, by name (`viewer-studio` by default).
    #[serde(default = "default_light_rig")]
    pub light_rig: String,
    /// One view's width and height in pixels, each 16 to 4096.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline` (the default, the same bytes on every run).
    #[serde(default)]
    pub quality: RenderQuality,
}

/// The stored row of views.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TurnaroundResult {
    /// `omuzaki://render/<hash>.png` of the row.
    pub uri: String,
    /// Its content hash.
    pub hash: String,
    /// Its width and height in pixels.
    pub size: [u32; 2],
    /// The views from left to right: `front`, `three_quarter`, `side`, `back`. The side views show the character's left.
    pub views: Vec<String>,
    /// `kit` for a character built from parts, `imported` for an imported avatar.
    pub body: String,
}

/// A view's name in the result.
fn view_name(view: View) -> &'static str {
    match view {
        View::Front => "front",
        View::ThreeQuarter => "three_quarter",
        View::Side => "side",
        View::Back => "back",
    }
}

impl Turnaround {
    fn check(&self) -> Result<UVec2, OpError> {
        let size = check_size(self.size)?;
        if matches!(self.framing, Framing::Free | Framing::Hands) {
            return Err(OpError::invalid("framing", "a turnaround frames the character: full, bust or face"));
        }
        still_light(&self.light_rig)?;
        Ok(size)
    }
}

impl Op for Turnaround {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.turnaround",
            summary: "Render a character (built or imported) from the front, three-quarter, side and back in one row, \
                      and store it as a PNG; returns its omuzaki://render/ URI.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (the PNG goes to the content store), so its empty patch is undone
            // exactly and nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let size = self.check()?;
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        Ok(Plan { cost: Some(cost(size, 4, self.quality)), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = self.check()?;
        let light = still_light(&self.light_rig)?;
        let clock = SystemClock::default();
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>()?,
            job.service::<BuildServices>()?,
            job.service::<dyn Store>()?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        job.checkpoint("loading", 0.0)?;
        let body = job.read(|doc| load_body(doc, &services, &self.character, self.variant.as_ref(), self.quality.detail()))?;
        let kind = match &body {
            crate::body::Body::Kit(_) => "kit",
            crate::body::Body::Avatar(_) => "imported",
        };
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("framing", e.to_string()))?;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = size.x as f32 / size.y as f32;
        let mut shots = Vec::with_capacity(View::ALL.len());
        for view in View::ALL {
            let mut camera = CameraDef::preset(self.framing);
            // The sheet turns the character; the camera goes the other way round it to see the same side.
            camera.yaw = if view == View::Back { 180.0 } else { -view.yaw_deg() };
            let placed = frame(self.framing, &landmarks, &camera, aspect).map_err(|e| OpError::invalid("framing", e.to_string()))?;
            shots.push(Shot::new(placed));
        }
        job.checkpoint("rendering", 0.2)?;
        let spec = StillSpec::new(light, size, self.quality.quality());
        let stills = with_renderer(job, |renderer| render_body_stills(renderer, body, &spec, &shots))?;
        let views = u32::try_from(stills.len()).unwrap_or(u32::MAX);
        let mut row = canvas("size", UVec2::new(views * size.x + (views - 1) * GUTTER, size.y))?;
        for (i, still) in (0_u32..).zip(stills) {
            let image = straight(still.image.size, crate::ops::render::still::straight_rgba(&still.image))?;
            paste(&mut row, &image, UVec2::new(i * (size.x + GUTTER), 0));
        }
        let stored = store(job, &row)?;
        job.checkpoint("done", 1.0)?;
        let result = TurnaroundResult {
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: [row.size.x, row.size.y],
            views: View::ALL.into_iter().map(|v| view_name(v).to_owned()).collect(),
            body: kind.into(),
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the turnaround: {e}")))
    }
}

register_op!(Turnaround);
