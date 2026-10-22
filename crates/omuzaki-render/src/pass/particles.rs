//! The `Particles` pass (particles.md D4 to D6). A stub until the pass lands: it never runs, and `supported` keeps it
//! out of `Capabilities::passes`.

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::shared::Shared;

/// See the module docs.
pub(crate) struct Particles;

impl Particles {
    pub(crate) fn new(_shared: &Shared) -> Self {
        Self
    }
}

impl FramePass for Particles {
    fn pass(&self) -> Pass {
        Pass::Particles
    }

    fn supported(&self) -> bool {
        false
    }

    fn record(&mut self, _cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        Ok(Outcome::Skipped)
    }
}
