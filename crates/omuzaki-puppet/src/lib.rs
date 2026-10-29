//! The flat 2D puppet runtime (spec v3 EX-08; contract: `omuzaki-puppet-api`; design: `docs/design/puppet2d.md`).
//!
//! - [`eval`]: the [`Evaluator`], parameter values in, a posed puppet and its draw list out (no allocation per frame)
//! - [`physics`]: the pendulums that swing hair and accessories and write their parameters
//! - [`blend`] and [`raster`]: every blend mode as a pure function, and the CPU renderer built on them
//! - [`texture`]: premultiplied textures with mips
//! - [`builder`]: puppets assembled in code (the auto-rig, fixtures and tests)
#![deny(missing_docs)]

pub mod blend;
pub mod builder;
pub mod eval;
pub mod physics;
pub mod raster;
pub mod texture;

pub use builder::{PuppetBuilder, grid_mesh};
pub use eval::Evaluator;
pub use physics::Physics;
pub use raster::{Canvas, CpuRenderer, fit_view};
pub use texture::Texture;
