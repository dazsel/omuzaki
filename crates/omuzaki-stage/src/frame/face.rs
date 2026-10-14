//! The face system's per-frame update: layer painting, variant selection by head coverage, settling for goldens; stub,
//! not implemented yet.
//!
//! The update is a method of [`Stage`] defined in this child module, so it reaches the stage's private state (the
//! built and uploaded character, the rig, the scene with its camera, the root) without a context struct, and it can
//! re-solve the rig after a variant switch.

use omuzaki_anim_api::Tick;
use omuzaki_render_api::Renderer;

use super::{FrameInput, Stage};
use crate::error::StageError;

impl Stage {
    /// Step 5 of [`tick`](Stage::tick): maps the channels to the face, repaints what changed and selects variants. A
    /// stage with no face system does nothing yet; the face system will fill this in.
    ///
    /// # Errors
    /// Whatever stops the face from painting this frame.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)] // a stub: keeps the signature the face system will fill in
    pub(super) fn update_face(
        &mut self,
        _renderer: &mut dyn Renderer,
        _tick: Tick,
        _input: &FrameInput,
    ) -> Result<(), StageError> {
        Ok(())
    }
}
