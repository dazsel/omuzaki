//! Rasterisers: they turn a `DrawList` into pixels (`FaceRasterizer` on the CPU, `gpu` on the renderer's textures);
//! [`cpu`] is the production CPU one, `gpu` is not implemented yet.
//!
//! The CPU rasteriser is a port of how Skia's software pipeline draws a path, so that its pixels are the 2D canvas's:
//! [`scan`] turns edges into anti-aliased coverage, [`clip`] keeps the clip masks, [`paint`] colours and blends,
//! [`stroke`] makes a stroke's outline, [`path`] is the outline buffer they share and [`geom`] the curve geometry under them.

mod clip;
pub mod cpu;
mod geom;
pub mod gpu;
mod hairline;
mod paint;
mod path;
mod scan;
mod stroke;

pub use cpu::SkiaRasterizer;
