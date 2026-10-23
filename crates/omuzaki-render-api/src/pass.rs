//! The frame graph and its timings.
//!
//! The graph is **fixed and linear** on purpose: nine named passes in one documented order. It is not a
//! programmable render graph. A fixed order is what makes frames reproducible, lets the scheduler reason about cost
//! per pass, and keeps every renderer (GPU, mock, web) interchangeable. An implementation may fuse or reorder work
//! internally as long as the *observable* order holds and it reports each logical pass.

use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;

/// One logical step of a frame, in execution order ([`Pass::ALL`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pass {
    /// Painters fill dynamic textures (the face atlas, §8 "face paint"). Runs first so nothing samples a stale
    /// texture. Budget: [`budget::FACE_PAINT_GPU`](crate::budget::FACE_PAINT_GPU).
    PaintTextures,
    /// Shadow maps for shadow-casting lights. Skipped when shadows are off, no light casts, or in live mode between
    /// `shadow_interval` redraws.
    Shadows,
    /// Opaque and alpha-masked instances: lit toon shading into colour and depth.
    Opaque,
    /// Inverted-hull outlines of every instance whose material has one. After `Opaque`, so they hide behind it.
    Outlines,
    /// Particles (particles.md D4): alpha-to-coverage sprites that test and write depth, so they need no sorting.
    /// After `Opaque` and `Outlines` so they hide behind the characters and their lines, before `Transparent` so blended
    /// surfaces test against them, and before `Bloom` and `DepthOfField` so glowing particles bloom and blur. Skipped
    /// when the scene has no spawn batches.
    Particles,
    /// Blended instances, sorted back to front (`Instance::order`, then distance), depth-tested, not writing depth.
    Transparent,
    /// Bloom. Needs the lit colour, so it follows all geometry.
    Bloom,
    /// Depth of field. Needs depth, so it follows all geometry.
    DepthOfField,
    /// Exposure, tone mapping, grade, grain, sample resolve and the output transform to the target's alpha encoding.
    Resolve,
}

impl Pass {
    /// How many passes there are.
    pub const COUNT: usize = 9;

    /// Every pass in the order a frame runs them.
    pub const ALL: [Self; Self::COUNT] = [
        Self::PaintTextures,
        Self::Shadows,
        Self::Opaque,
        Self::Outlines,
        Self::Particles,
        Self::Transparent,
        Self::Bloom,
        Self::DepthOfField,
        Self::Resolve,
    ];

    /// Position in [`Pass::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// A stable lower-case name for logs, the Build Theatre and budget reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PaintTextures => "paint_textures",
            Self::Shadows => "shadows",
            Self::Opaque => "opaque",
            Self::Outlines => "outlines",
            Self::Particles => "particles",
            Self::Transparent => "transparent",
            Self::Bloom => "bloom",
            Self::DepthOfField => "depth_of_field",
            Self::Resolve => "resolve",
        }
    }
}

/// A time per pass. `None` means the pass did not run this frame (no work, switched off by quality, unsupported, or a
/// skipped shadow update), which the scheduler needs to tell apart from "ran in zero time".
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct PassTimes([Option<Seconds>; Pass::COUNT]);

impl PassTimes {
    /// Records that `pass` ran and took `time`.
    pub fn set(&mut self, pass: Pass, time: Seconds) {
        self.0[pass.index()] = Some(time);
    }

    /// The time `pass` took, or `None` if it did not run.
    pub fn get(&self, pass: Pass) -> Option<Seconds> {
        self.0[pass.index()]
    }

    /// The passes that ran, in frame order, with their times.
    pub fn iter(&self) -> impl Iterator<Item = (Pass, Seconds)> + '_ {
        Pass::ALL.into_iter().filter_map(|p| self.get(p).map(|t| (p, t)))
    }

    /// The sum of all passes that ran.
    pub fn total(&self) -> Seconds {
        Seconds(self.iter().map(|(_, t)| t.0).sum())
    }
}

/// GPU timings. They arrive late: timestamp queries resolve a frame or two after submission, so these describe the
/// newest *finished* frame, named by `frame`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GpuTimings {
    /// The `FrameInfo::index` these timings belong to (at most the current frame).
    pub frame: u64,
    /// GPU time per pass.
    pub passes: PassTimes,
    /// Of `Pass::PaintTextures`, the painters' own commands (the face rasteriser's raster and resolve); the rest of that
    /// pass is the renderer's own work on what they painted (premultiply and mip rebuild). `None` when nothing was
    /// painted.
    pub painters: Option<Seconds>,
}

/// What one [`Renderer::render`](crate::Renderer::render) call did, for the frame-budget scheduler (spec §8).
///
/// It is `Copy` and holds no heap data, so reading it every frame costs nothing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameReport {
    /// The `FrameInfo::index` of the scene that was rendered.
    pub frame: u64,
    /// The internal resolution used (target size × `render_scale` × any supersampling the renderer applied).
    pub render_size: UVec2,
    /// CPU time spent recording each pass.
    pub cpu: PassTimes,
    /// GPU time per pass, when the backend can measure it ([`Capabilities::gpu_timings`](crate::Capabilities)).
    pub gpu: Option<GpuTimings>,
    /// Draw calls issued.
    pub draw_calls: u32,
    /// Triangles submitted.
    pub triangles: u64,
    /// Particle slots submitted: the sum of the drawn spawn batches' `count` (an upper bound on the particles drawn), so
    /// tests and the scheduler see the load.
    pub particles: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_pass_once_in_index_order() {
        for (i, pass) in Pass::ALL.into_iter().enumerate() {
            assert_eq!(pass.index(), i);
        }
        let mut names: Vec<_> = Pass::ALL.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Pass::COUNT, "names are unique");
    }

    #[test]
    fn the_order_encodes_the_frame_graph_rules() {
        let before = |a: Pass, b: Pass| a.index() < b.index();
        assert!(before(Pass::PaintTextures, Pass::Opaque), "textures are painted before anything samples them");
        assert!(before(Pass::Shadows, Pass::Opaque), "shadow maps exist before lit shading reads them");
        assert!(before(Pass::Opaque, Pass::Outlines), "outlines hide behind the model");
        assert!(before(Pass::Outlines, Pass::Transparent) && before(Pass::Transparent, Pass::Bloom));
        assert!(before(Pass::Opaque, Pass::Particles), "particles depth-test against the characters");
        assert!(before(Pass::Outlines, Pass::Particles), "and against their lines");
        assert!(before(Pass::Particles, Pass::Transparent), "blended surfaces depth-test against particles");
        assert!(before(Pass::Particles, Pass::Bloom) && before(Pass::Particles, Pass::DepthOfField), "glow and blur");
        assert_eq!(Pass::ALL.last(), Some(&Pass::Resolve));
    }

    #[test]
    fn pass_times_tell_skipped_from_free() {
        let mut t = PassTimes::default();
        assert_eq!(t.get(Pass::Opaque), None);
        t.set(Pass::Resolve, Seconds(0.001));
        t.set(Pass::Opaque, Seconds(0.0));
        assert_eq!(t.get(Pass::Opaque), Some(Seconds(0.0)), "ran, instantly");
        assert_eq!(t.iter().map(|(p, _)| p).collect::<Vec<_>>(), vec![Pass::Opaque, Pass::Resolve], "frame order");
        assert_eq!(t.total(), Seconds(0.001));
    }
}
