//! `face.underlay.set`: reads the sheet, builds the underlay layer at the head's resolution and hands it to the app.
//!
//! It is a job: `plan` validates and prices (no store read, no decode), and `work` does the read, the decode and the
//! resample on a job worker without the document lock. A call that changes only the opacity of what is already shown
//! sets that opacity and builds nothing.

use omuzaki_core::glam::UVec2;
use omuzaki_face_api::{FaceProjection, UnderlayHead, UnderlayTarget, UnderlayTargetError, narrow};
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::PixelRect;
use omuzaki_store_api::{Store, StoreError};
use serde_json::{Value, json};

use super::decode;
use crate::underlay::{Geometry, Sheet, Underlay, UnderlaySet, UnderlayView};

impl Op for UnderlaySet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.underlay.set",
            summary: "Show a reference sheet under a character's painted face at an opacity and alignment, to trace \
                      the features; changes the view only, not the project",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let request = Request::resolve(self, ctx.service::<dyn UnderlayTarget>()?)?;
        let rebuild = request.target.shown(&request.character).as_deref() != Some(request.key.as_str());
        Ok(Plan::answer(json!({
            "character": request.character,
            "view": UnderlayView::new(request.geometry.window(), narrow(self.opacity)),
            "rebuild": rebuild,
            "dry_run": ctx.dry_run,
        })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let request = Request::resolve(self, job.service::<dyn UnderlayTarget>()?)?;
        let opacity = narrow(self.opacity);
        let view = UnderlayView::new(request.geometry.window(), opacity);
        let answer = |rebuilt: bool| json!({ "character": request.character, "view": view, "rebuilt": rebuilt });

        // Unchanged sheet and alignment: only the opacity moves, a material update with nothing read or built.
        if request.target.shown(&request.character).as_deref() == Some(request.key.as_str()) {
            match request.target.set_opacity(&request.character, opacity) {
                Ok(()) => return Ok(answer(false)),
                // What was shown went away since the check: build it again.
                Err(UnderlayTargetError::NotFound(_)) => {}
                Err(other) => return Err(app_error(other)),
            }
        }

        job.checkpoint("reading the sheet", 0.1)?;
        let bytes = job.service::<dyn Store>()?.get(&self.asset).map_err(|e| match e {
            StoreError::Missing(_) => OpError::not_found(format!("there is no asset {}", self.asset))
                .hint("give the content hash of an image in the project's asset store"),
            other => OpError::internal(other.to_string()),
        })?;
        job.checkpoint("decoding the sheet", 0.3)?;
        let pixels = decode::rgba(&bytes, &self.asset.to_string())?;
        job.checkpoint("building the underlay", 0.6)?;
        let (width, height) = pixels.dimensions();
        let sheet = Sheet::new(UVec2::new(width, height), pixels.as_raw())?;
        let mut layer = Underlay::new(&sheet, request.sheet_face, &request.geometry, &self.align)?;
        layer.set_opacity(self.opacity)?;
        job.checkpoint("showing the underlay", 0.95)?;
        request.target.show(&request.character, layer.into_image(request.key)).map_err(app_error)?;
        Ok(answer(true))
    }
}

register_op!(UnderlaySet);

/// What a request comes to once the app has been asked about the head: everything that needs no store read.
struct Request<'a> {
    target: &'a dyn UnderlayTarget,
    character: String,
    sheet_face: PixelRect,
    geometry: Geometry,
    /// What the pixels depend on; equal keys mean equal pixels.
    key: String,
}

impl<'a> Request<'a> {
    fn resolve(set: &UnderlaySet, target: &'a dyn UnderlayTarget) -> Result<Self, OpError> {
        set.validate()?;
        let character = set.character.to_string();
        let UnderlayHead { projection, head_centre, px_per_m, sheet_face } = target.head(&character).map_err(app_error)?;
        let sheet_face = set.sheet_face.or_else(|| sheet_face.map(rect)).ok_or_else(|| {
            OpError::invalid("sheet_face", "the character has no locked sheet, so the sheet's face rectangle is unknown")
                .hint("give sheet_face as {x, y, width, height} in sheet pixels")
        })?;
        let geometry = Geometry::new(&projection, head_centre, px_per_m)?;
        let key = key(set, sheet_face, &projection, head_centre.to_array(), px_per_m);
        Ok(Self { target, character, sheet_face, geometry, key })
    }
}

/// The inputs of the pixels as text. `f64` and `f32` print the same way for the same bits, so equal inputs give equal
/// keys.
fn key(set: &UnderlaySet, sheet_face: PixelRect, projection: &FaceProjection, centre: [f32; 3], px_per_m: f32) -> String {
    format!("{}|{sheet_face:?}|{:?}|{projection:?}|{centre:?}|{px_per_m}", set.asset, set.align)
}

fn rect([x, y, width, height]: [u32; 4]) -> PixelRect {
    PixelRect { x, y, width, height }
}

/// The app's refusal as the op's.
fn app_error(error: UnderlayTargetError) -> OpError {
    match error {
        UnderlayTargetError::NotFound(message) => OpError::not_found(message),
        UnderlayTargetError::Failed(message) => OpError::internal(message),
    }
}
