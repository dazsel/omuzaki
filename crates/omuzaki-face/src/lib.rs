//! The procedural face (spec v3 §19, CB-20..25): turns the channel bus into painted face layers, on the contracts of
//! `omuzaki-face-api` (what the face does) and `omuzaki-render-api` (where the pixels go).
//!
//! This is an implementation crate: it depends on API crates only (`crates/README.md`), and leaves parity and golden
//! tests against the JavaScript prototype to `crates/omuzaki-test-face/tests/` (an implementation crate may not depend on an App crate such as `omuzaki-test-face`).
//!
//! # The frame
//!
//! [`FaceSystem::update`] is the whole per-frame path: [`FaceMapping::apply`](omuzaki_face_api::FaceMapping::apply) turns
//! the channels into paint parameters once, a [`FacePainter`](omuzaki_face_api::FacePainter) (the [`RealPainter`] in
//! production) fills one vector `DrawList` per layer, and only the layers whose fingerprint changed are rasterised
//! (the prototype redrew only on change; an unchanged face costs next to nothing). [`FaceUpdate`] says which layers
//! changed, and [`FaceSystem::layer_painter`] wraps one in the renderer's `Painter`, which draws it on the GPU when a
//! [`GpuRasterizer`] is registered and otherwise writes the CPU raster, so the stage calls
//! `renderer.paint_texture(texture, &mut painter)` for exactly the changed layers. Layers keep premultiplied sRGB.
//!
//! # Module map
//!
//! The whole tree exists from the start, so parallel changes never edit this file or `Cargo.toml`: each module file
//! starts with a `//!` line naming its purpose, and a `mod.rs` lists the submodules
//! that exist.
//!
//! | Module | Holds |
//! |---|---|
//! | [`system`] | [`FaceSystem`]: mapping, painter, fingerprints, rasterisation of changed layers |
//! | [`layer`] | [`FaceLayerPainter`], the renderer `Painter` for one layer, and the [`GpuRasterizer`] seam |
//! | [`painter`] | [`RealPainter`] and its sections in draw order: cheeks, marks, brows, eyes (`eyes/`), nose, mouth (`mouth/`), overlay |
//! | [`raster`] | the CPU rasteriser (`cpu`) and the GPU one (`gpu/`) |
//! | [`preview`] | pictures of a face for the tools (sweep, range test): paint, compare, contact sheets |
//! | [`underlay`] | tracing a sheet face under the painted one |
//! | [`ops`] | the registered face ops: `style`, `mapping`, `rest`, `expression`, `exaggeration`, `sweep`, `range_test` |
//! | [`player`] | the expression player: fades, auto-stop, groups, layers, cuts, the default; a short's cue track |
//! | [`exaggeration`] | the exaggeration dial: presets from realistic through anime to chibi extreme |
//! | [`bake`] | the expression bakers (`morph`, `flipbook`) |
//! | [`atlas`] | the atlas-scale controller: the layers' density stepped with the face's GPU budget pressure |
//!
//! Benchmarks sit in `benches/` (one pre-declared target per hot path) and each change adds its own files under `tests/`.
#![deny(missing_docs)]

pub mod atlas;
pub mod bake;
pub mod exaggeration;
pub mod eye_presets;
pub mod layer;
pub mod ops;
/// The helpers the face ops share. They sit beside `ops/`, not in it, because the generated family list in `ops/` makes
/// every folder there `pub`, and these are crate-private.
mod ops_shared;
pub mod painter;
pub mod player;
pub mod preview;
pub mod raster;
pub mod system;
pub mod underlay;

pub use layer::{FaceLayerPainter, GpuRasterizer};
pub use painter::{LayerSections, MouthPiece, OpenMouth, PaintCtx, RealPainter, Section, SectionRanges};
pub use system::{FaceSystem, FaceTimings, FaceUpdate};

omuzaki_core::define_link! {
    omuzaki_char_api::link();
}
