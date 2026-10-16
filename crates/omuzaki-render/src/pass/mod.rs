//! The frame graph's passes, one module each.
//!
//! The graph is fixed by the contract: eight passes in the order of [`Pass::ALL`]. Each is a type in its own file
//! implementing [`FramePass`]; [`build`] lists them in order, one constructor per line, all with the same
//! `new(&Shared)` signature, so a task that fills in a pass edits that pass's file and nothing here.
//!
//! | Pass | File | State |
//! |---|---|---|
//! | `PaintTextures` | `paint_textures.rs` | done: rebuilds the mips of repainted textures |
//! | `Shadows` | `shadows.rs` | done: one fitted, texel-snapped directional map for the first casting light, redrawn every `shadow_interval`-th live frame; the lookup is in `shader/toon.wgsl` |
//! | `Opaque` | `opaque.rs` | done: toon shading (ramp, tinted shade, rim, skin, lights) over base colour, vertex colours, textures, overlays and skinning |
//! | `Outlines` | `outlines.rs` | done: inverted hull on welded smooth normals, width by distance; `MToon`'s other width modes are not implemented yet |
//! | `Transparent` | `transparent.rs` | done: blended instances sorted by `Instance::order`, then back to front, depth-tested without writing depth; the opaque shader and program hooks with a blend state |
//! | `Bloom` | `bloom.rs` | done: the prototype's `UnrealBloomPass`, 12 small passes publishing a glow image |
//! | `DepthOfField` | `dof.rs` | done: prepass, foreground search and gather on half-size images |
//! | `Resolve` | `resolve.rs` | done: exposure, tone map, output transform; folds in the glow and blur, grade, grain, dither |
//!
//! **To implement a stub:** replace its body, delete its `supported` override (that is what lists the pass in
//! `Capabilities::passes`, and what makes the conformance suite require its checks), read the frame through [`PassCx`],
//! and own your pipelines, buffers, shader file and anything you derive from the draws. Your instances are
//! `cx.frame.draws.for_pass(Pass::X)`, already filtered; the quality is `cx.frame.quality`. A product another pass
//! reads (the shadow map `Opaque` samples) is published through `frame/attachments.rs`.

mod bloom;
mod dof;
mod opaque;
mod outlines;
mod paint_textures;
mod resolve;
pub(crate) mod shadows;
mod transparent;

use std::time::Duration;

use omuzaki_render_api::{Pass, RenderError, TargetDesc};

use crate::frame::FrameView;
use crate::frame::attachments::Attachments;
use crate::paint::PaintLog;
use crate::resource::SceneResources;
use crate::shared::Shared;

/// Whether a pass had work this frame. `Skipped` shows as `None` in the frame report, which is not the same as having
/// run in zero time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ran,
    Skipped,
}

/// Draw calls and triangles submitted so far this frame.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct FrameStats {
    pub(crate) draw_calls: u32,
    pub(crate) triangles: u64,
}

/// Everything a pass may touch while recording one frame.
pub(crate) struct PassCx<'a> {
    pub(crate) shared: &'a Shared,
    /// Meshes, textures and materials, read-only. Draw lists name slots in these.
    pub(crate) resources: SceneResources<'a>,
    /// The scene, quality, sizes, group 0 and the draw lists.
    pub(crate) frame: &'a FrameView<'a>,
    /// Scene colour, depth and the resolved colour of the target being drawn.
    pub(crate) attachments: &'a mut Attachments,
    /// The target's output texture, in `OUTPUT_FORMAT`.
    pub(crate) output: &'a wgpu::TextureView,
    pub(crate) output_desc: TargetDesc,
    pub(crate) paints: &'a PaintLog,
    pub(crate) encoder: &'a mut wgpu::CommandEncoder,
    pub(crate) stats: FrameStats,
    /// CPU time the pass did outside `record` (painters ran before the frame); charged to the pass in the report.
    pub(crate) extra_cpu: Duration,
}

impl<'a> PassCx<'a> {
    /// Group 3 of the lit passes: the scene's environment, or the stand-in that lights nothing. It borrows the resources,
    /// not the context, so it can be fetched while a render pass holds the encoder.
    pub(crate) fn environment_group(&self) -> &'a wgpu::BindGroup {
        let (shared, resources) = (self.shared, self.resources);
        self.frame
            .draws
            .environment
            .and_then(|slot| resources.environments.at(slot))
            .unwrap_or(&shared.no_environment)
            .bind_group()
    }
}

/// One pass of the frame graph.
pub(crate) trait FramePass: Send {
    /// Which pass this is. [`build`] must return them in [`Pass::ALL`] order.
    fn pass(&self) -> Pass;

    /// Whether this pass is implemented. Stubs say no; the answer fills `Capabilities::passes`.
    fn supported(&self) -> bool {
        true
    }

    /// Records the pass into `cx.encoder`. Passes record render or compute passes of their own; the graph brackets the
    /// call with timestamps, so there is nothing to wire up for timing.
    ///
    /// # Errors
    /// Anything the frame cannot continue after; the graph drops the frame's commands.
    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError>;
}

/// The eight passes, in frame order.
pub(crate) fn build(shared: &Shared) -> [Box<dyn FramePass>; Pass::COUNT] {
    [
        Box::new(paint_textures::PaintTextures::new(shared)),
        Box::new(shadows::Shadows::new(shared)),
        Box::new(opaque::Opaque::new(shared)),
        Box::new(outlines::Outlines::new(shared)),
        Box::new(transparent::Transparent::new(shared)),
        Box::new(bloom::Bloom::new(shared)),
        Box::new(dof::DepthOfField::new(shared)),
        Box::new(resolve::Resolve::new(shared)),
    ]
}
