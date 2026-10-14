//! The `head` part kind: cranium, face-shape outline, neck, ears, traced heads and shape presets.
//!
//! The cranium and the face shape (CB-10) are here: [`cranium`] is the analytic head surface, [`outline`] reshapes the
//! lower face (the face-shape superellipse, clamped to a plausible space, and the chin stretch), and [`mesh`] builds
//! the head mesh from both, with the surface of the head as built that hair, hats and ears sit on
//! ([`mesh::build_head`]). The cranium and the mesh are ports of the prototype's
//! `head.js`, checked against its dumps by `crates/omuzaki-test/tests/char_head_analytic_shape_parity.rs`; the lower face
//! was redesigned so every face-shape slider position gives a smooth head (`crates/omuzaki-char/tests/head_quality`).

pub mod cranium;
pub mod ears;
pub mod material;
pub mod mesh;
pub mod neck;
pub mod outline;
pub mod part;
pub mod presets;
pub mod trace;

pub use cranium::{TraceSample, surface};
