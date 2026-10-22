//! Contract for the renderer (spec v3 §9 `omuzaki-render`, §10 budgets, §21 look, §26 renders): what the rest of Omuzaki
//! says to a renderer and what it hears back, without a single GPU type.
//!
//! **The shape.** Heavy data is uploaded once and referenced by handle ([`MeshId`], [`TextureId`], [`MaterialId`],
//! [`TargetId`]); each frame the stage fills a plain [`Scene`] value and calls [`Renderer::render`], which runs a
//! fixed frame graph ([`Pass::ALL`]) and returns a [`FrameReport`] of per-pass timings for the frame-budget
//! scheduler. Headless rendering to RGBA ([`RendererExt::render_image`]) is the same call with a throwaway target, so
//! tests, goldens, thumbnails and the live viewport share one code path.
//!
//! **What is where.**
//! - [`Scene`], [`Camera`], [`Light`], [`Fog`], [`Instance`], [`Post`]: one frame, rebuilt every frame (scene.rs).
//! - [`EnvironmentDesc`], [`EnvironmentUse`]: image-based light, a resource plus how a frame uses it (environment.rs).
//! - [`Material`], [`Toon`], [`Outline`], [`Overlay`], [`Mapping`]: how surfaces look, including the overlays the face
//!   painter composites through (material.rs).
//! - [`TextureDesc`], [`Image`]: data that lives on the GPU for many frames (resource.rs); [`MeshData`] and [`Colour`]
//!   are defined in `omuzaki-core` (shared with the character kit and I/O) and re-exported here.
//! - [`Quality`]: how much work a frame may spend; live versus frame-exact offline (quality.rs).
//! - [`Pass`], [`FrameReport`]: the frame graph and its timings (pass.rs).
//! - [`Renderer`], [`Painter`], [`PaintTarget`]: the entry points (renderer.rs).
//!
//! **Why the types are split into "parameters" and "runtime".** Parameter types (colours, toon numbers, lights, post,
//! quality) are `serde` + JSON Schema, so they go into the project document and ops (LK-01..LK-05). Types that hold
//! handles ([`Material`], [`Instance`], [`Scene`]) are run-time values and don't serialise: handles only mean something
//! to the renderer that issued them.
//!
//! The rationale, the frame graph table and the "interfaces with" notes for the other contracts are in
//! `docs/design/render-api.md`. A mock lives in `omuzaki-render-mock`, together with the conformance checks that every
//! real renderer also passes.
#![deny(missing_docs)]

mod environment;
mod error;
mod handle;
mod material;
pub mod memory;
mod morph;
mod mtoon;
mod pass;
mod quality;
mod renderer;
mod resource;
mod scene;

pub use environment::{EnvironmentDesc, EnvironmentUse, sky_radiance};
pub use error::RenderError;
pub use handle::{EnvironmentId, MaterialId, MeshId, ResourceRef, TargetId, TextureId};
pub use material::{
    AlphaMode, Mapping, Material, Outline, OutlineStyle, OutlineWidthMode, Overlay, ParamValue, ProgramParams, Rim,
    Shading, Skin, Toon, UvTransform,
};
pub use morph::{MeshOptions, MeshUsage, MorphSet, SparseMorph};
pub use mtoon::Mtoon;
pub use omuzaki_core::colour::{Colour, ColourParseError, srgb_decode, srgb_encode, to_byte};
pub use omuzaki_core::mesh::{ExtraAttribute, MeshData, MeshError};
pub use pass::{FrameReport, GpuTimings, Pass, PassTimes};
pub use quality::{Quality, QualityMode, ShadowQuality, budget};
pub use renderer::{Capabilities, MtoonSupport, PaintTarget, Painter, Renderer, RendererExt};
pub use resource::{
    AlphaEncoding, ColourSpace, Filter, Image, PixelRect, ResourceStats, Sampler, TargetDesc, TextureDesc,
    TextureUsage, Wrap, limits, rgba_len,
};
pub use scene::{
    Background, Bloom, Camera, ColourGrade, DepthOfField, FilmGrain, Fog, FrameInfo, Instance, JointRange, Light,
    MorphRange, Post, Projection, Scene, Tonemap,
};
