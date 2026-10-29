//! The procedural character (spec v3 §18): the geometry kit, body templates, head, hair, outfit, accessories, cloth,
//! palette and the character ops, built on the contracts in `omuzaki-char-api`.
//!
//! This is an implementation crate: it depends on API crates only (`crates/README.md`), takes everything it needs from
//! other systems through API types injected into `BuildCtx` and `OpCtx`, and leaves parity and golden tests against the
//! JavaScript prototype to `crates/omuzaki-test/tests/` (an impl crate may not depend on `omuzaki-test`).
//!
//! # Module map
//!
//! The whole tree exists from the start, so adding a module does not mean editing this file or `Cargo.toml`: each
//! module file starts with a `//!` line naming its purpose (a stub says so), and a `mod.rs` lists the submodules that
//! exist. Only the garment directories under `outfit/` hold an empty `mod.rs`: a garment's first change owns its
//! directory.
//!
//! | Module | Holds |
//! |---|---|
//! | [`kit`] | curves, grids, the mesh builder, lofts; sweeps, Catmull-Rom points and resampling; auto-weights |
//! | [`template`] | the body template registry; soft-sd, mini, standard and tall; skeleton assembly; body-shape space |
//! | [`body`] | the `body` part kind and its hands (mitten, simple, five-finger) |
//! | [`head`] | cranium and outline, the `head` part kind, ears, traced heads, shape presets |
//! | [`hair`] | the `hair` part kind, generators, physics, colour, bake |
//! | [`outfit`] | the garment framework, trims, neckwear and one directory per garment and footwear |
//! | [`fit`] | silhouette fit solver, heatmap and the fit ops |
//! | [`accessory`] | attachment, rigid jewellery, hats, glasses, wings, tails |
//! | [`cloth`] | the XPBD sheet solver, contacts, runtime, quality modes, bake (garment presets live in `outfit/*/cloth.rs`) |
//! | [`palette`] | palette ops, colourways, contrast checks, skin tones |
//! | [`look`] | the look presets of `library/looks/` (LK-06), registered from data |
//! | [`ops`] | the registered character, part, variant, body, face, layout, weights, hair and look ops |
//! | [`builder`] | the registry and caching character builders |
//!
//! Benchmarks sit in `benches/` (one pre-declared target per hot path) and each change adds its own files under `tests/`.
#![deny(missing_docs)]

pub mod accessory;
pub mod body;
pub mod builder;
pub mod cloth;
pub mod fit;
pub mod hair;
pub mod head;
pub mod kit;
pub mod library_items;
pub mod look;
pub mod ops;
pub mod outfit;
pub mod palette;
pub mod style;
pub mod template;

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_library_api::link();
}
