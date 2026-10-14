//! The GPU rasteriser: draws a `DrawList` straight into the renderer's native texture through `GpuRasterizer`; stub,
//! not implemented yet (fills, then strokes). Which approach it takes (Vello or custom coverage) is still to be decided.

pub mod clip;
pub mod fill;
pub mod gradient;
pub mod stroke;
