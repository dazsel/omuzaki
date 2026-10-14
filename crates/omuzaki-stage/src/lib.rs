//! The character stage (spec v3 §8, §18, §19): the wiring that puts a built character on a renderer and runs it.
//!
//! It uploads a [`BuiltCharacter`](omuzaki_char_api::BuiltCharacter)'s meshes, materials and textures, then each frame
//! solves the pose through the rig, steps the part runtimes, updates the face and fills the renderer's `Scene`. It
//! also holds the scene recipes (camera, lights, ground), the character sheets and the `render.*` and `sheet.*` ops.
//!
//! This is an **App** crate (`crates/README.md`): the UI, the CLI, the web player and the golden runner all link the
//! same wiring, so it may depend on implementation crates, which the API+implementation pairs may not. Everything is
//! written against `dyn Renderer` from `omuzaki-render-api`; the wgpu renderer plugs in behind the `wgpu` feature and
//! the render mock stands in for it in tests.
//!
//! # Module map
//!
//! The whole tree exists from the start, so parallel changes never edit this file or `Cargo.toml`: each stub file
//! starts with a `//!` line naming its purpose, and a `mod.rs` lists the
//! submodules that exist. A directory with a single owner (`ops/render/`, `ops/sheet/`) starts with a `mod.rs` that
//! its submodules extend.
//!
//! | Module | Holds |
//! |---|---|
//! | [`error`] | [`StageError`](error::StageError) |
//! | [`upload`] | [`StageCharacter`](upload::StageCharacter): meshes, textures and materials on a renderer, the face atlas resolution knob |
//! | `release` (private) | releasing in dependency order, and the leak guard on drop |
//! | [`frame`] | the frame loop: [`pose`](frame::pose) (bind-based pose assembly), [`rig`](frame::rig), [`runtimes`](frame::runtimes), [`scene`](frame::scene), then [`face`](frame::face) |
//! | [`scene`] | the scene: [`landmarks`](scene::landmarks), [`frame`](scene::frame) (framing presets), [`resolve`](scene::resolve), [`apply`](scene::apply), [`default`](scene::default) |
//! | [`ops::scene`] | the `stage.*` scene ops |
//! | [`recipe`] | the recipe type with its post settings, [`lights`](recipe::lights), [`camera`](recipe::camera), [`ground`](recipe::ground) |
//! | [`ops::render`] | `render.preview`, `render.turnaround`, `render.contact_sheet`, `render.compare` |
//! | [`sheet`] | [`layout`](sheet::layout), [`turnaround`](sheet::turnaround), [`expressions`](sheet::expressions), [`export`](sheet::export) |
//! | [`ops::sheet`] | the `sheet.*` ops |
//!
//! Benchmarks sit in `benches/` (`frame`) and each change adds its own files under `tests/`.
//!
//! # Linked for their registrations
//!
//! A binary keeps a crate's self-registrations (part kinds, templates, grammar rules, ops) only if it links the crate,
//! and on wasm only if it calls into it. [`link`] calls the `link()` of every registering crate the stage builds on,
//! and every App that uses the stage calls [`link`] at startup (ADR 0015; `cargo xtask lint-links` checks the chain,
//! `tests/registrations.rs` the result). `omuzaki-rig` is linked for the rig the frame loop calls. The `omuzaki-render`
//! wgpu backend (R1) is not linked here: only the `wgpu` feature pulls it in.
#![deny(missing_docs)]

pub mod error;
pub mod frame;
pub mod ops;
pub mod recipe;
mod release;
pub mod scene;
pub mod sheet;
pub mod upload;

pub use error::StageError;
pub use upload::{StageCharacter, UploadOptions};

use omuzaki_rig as _;

// The stage's `link()` forwards to every registering crate it builds on; Apps call it once at startup (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_anim::link();
    omuzaki_char::link();
    omuzaki_face::link();
    #[cfg(feature = "wgpu")]
    omuzaki_render::link();
}
