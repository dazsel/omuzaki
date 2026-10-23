//! One frame's shared state: what `render` builds from the scene before any pass runs.
//!
//! The order of a frame, owned by [`crate::renderer`]:
//! 1. validate the scene and turn its handles into slot indices ([`FrameState::prepare`], which fails before the GPU is
//!    touched, so an error leaves the target's previous frame intact);
//! 2. upload the frame uniform, the instances and the joints, and tag every draw with the passes that want it
//!    ([`draws`]);
//! 3. run the passes in order ([`graph`]), bracketing each with GPU timestamps ([`timing`]); a supersampled frame runs
//!    the geometry passes once per round and averages the rounds ([`supersample`]);
//! 4. submit, and collect timings that have arrived.
//!
//! A pass reads the result through [`PassCx`](crate::pass::PassCx) and never rebuilds any of it.

pub(crate) mod attachments;
pub(crate) mod draws;
pub(crate) mod graph;
pub(crate) mod supersample;
pub(crate) mod timing;
pub(crate) mod uniform;

#[cfg(test)]
mod layout_probe;

pub(crate) use draws::FrameState;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{Quality, Scene};

/// What a pass needs to know about the frame being drawn. Borrowed for the duration of one `render` call.
pub(crate) struct FrameView<'a> {
    pub(crate) scene: &'a Scene,
    /// The quality the caller asked for. `samples` and `render_scale` are already applied (below); the rest (shadow
    /// quality and interval, post effects, live or offline, particle density) is for the passes that honour it.
    pub(crate) quality: &'a Quality,
    /// The internal resolution: the target's size times `render_scale`.
    pub(crate) internal_size: UVec2,
    /// The target's size.
    pub(crate) target_size: UVec2,
    /// Hardware MSAA sample count in use (1, 2, 4 or 8).
    pub(crate) samples: u32,
    /// Group 0 of `shader/common.wgsl` (frame uniform, instances, joints) for each round the geometry passes draw: one,
    /// or more when the frame supersamples ([`supersample`]). A pass binds [`PassCx::frame_group`](crate::pass::PassCx::frame_group),
    /// which picks the round being drawn.
    pub(crate) bind_groups: &'a [wgpu::BindGroup],
    /// The frame uniforms behind those groups, one per round, for a pass that binds them in a group of its own.
    pub(crate) uniforms: &'a draws::FrameUniforms,
    /// The scene's draws, validated and tagged with the passes that draw them ([`draws::Draws::for_pass`]).
    pub(crate) draws: &'a draws::Draws,
}

impl FrameView<'_> {
    /// The scene's background as a premultiplied linear clear colour. The environment is drawn over opaque black by the
    /// `Opaque` pass.
    pub(crate) fn clear_colour(&self) -> wgpu::Color {
        use omuzaki_render_api::Background;
        match self.scene.background {
            Background::Solid { colour } => {
                let (rgb, a) = (colour.to_linear(), f64::from(colour.alpha()));
                wgpu::Color { r: f64::from(rgb.x) * a, g: f64::from(rgb.y) * a, b: f64::from(rgb.z) * a, a }
            }
            // A gradient or a picture is laid under the frame by the `Resolve` pass, so the scene renders over nothing.
            Background::Transparent | Background::Gradient { .. } | Background::Texture { .. } => {
                wgpu::Color::TRANSPARENT
            }
            Background::Environment => wgpu::Color::BLACK,
        }
    }
}
