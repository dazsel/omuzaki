//! `face.underlay.set`: reads the sheet, builds the underlay layer at the head's resolution and hands it to the app.

use omuzaki_core::glam::UVec2;
use omuzaki_face_api::FaceError;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Store, StoreError};
use serde_json::json;

use crate::underlay::{Sheet, Underlay, UnderlaySet, UnderlayTarget, UnderlayView};

impl Op for UnderlaySet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.underlay.set",
            summary: "Show a reference sheet under a character's painted face at an opacity and alignment, to trace \
                      the features; changes the view only, not the project",
            kind: OpKind::Edit,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.validate()?;
        let target = ctx.service::<dyn UnderlayTarget>()?;
        let head = target.head(&self.character)?;
        let sheet_face = self.sheet_face.or(head.sheet_face).ok_or_else(|| {
            OpError::invalid("sheet_face", "the character has no locked sheet, so the sheet's face rectangle is unknown")
                .hint("give sheet_face as {x, y, width, height} in sheet pixels")
        })?;

        let pixels = decode(ctx.service::<dyn Store>()?, self)?;
        let (width, height) = pixels.dimensions();
        let sheet = Sheet::new(UVec2::new(width, height), pixels.as_raw()).map_err(|e| invalid(&e))?;
        let mut layer =
            Underlay::new(&sheet, sheet_face, &head.projection, head.head_centre, head.px_per_m, &self.align)
                .map_err(|e| invalid(&e))?;
        layer.set_opacity(self.opacity).map_err(|e| invalid(&e))?;

        let view = UnderlayView::of(&layer);
        // A dry run builds and checks everything and shows nothing.
        if !ctx.dry_run {
            target.show(&self.character, layer)?;
        }
        Ok(Plan::answer(json!({ "character": self.character.to_string(), "view": view, "dry_run": ctx.dry_run })))
    }
}

register_op!(UnderlaySet);

/// The sheet's pixels: the asset read from the store and decoded.
fn decode(store: &dyn Store, request: &UnderlaySet) -> Result<image::RgbaImage, OpError> {
    let bytes = store.get(&request.asset).map_err(|e| match e {
        StoreError::Missing(_) => OpError::not_found(format!("there is no asset {}", request.asset))
            .hint("give the content hash of an image in the project's asset store"),
        other => OpError::internal(other.to_string()),
    })?;
    let image = image::load_from_memory(&bytes).map_err(|e| {
        OpError::invalid("asset", format!("asset {} is not an image this build reads: {e}", request.asset))
    })?;
    Ok(image.to_rgba8())
}

/// A face error as the op's refusal. The messages already name the field at fault.
fn invalid(error: &FaceError) -> OpError {
    OpError::invalid("underlay", error.to_string())
}
