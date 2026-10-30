//! The 3D eye: a real eyeball with a refractive cornea, the lids and the skin round it, at close-up quality.
//!
//! The 2D painter (`crate::painter`) draws the anime eye as a texture on the head; this builds the eye as geometry and
//! textures for the renderer's `eye` and `skin` shading programs (`omuzaki-render/src/program/eye.rs`, `skin.rs`):
//!
//! - [`textures::atlas`]: the iris (fibres, collarette, crypts, furrows, specks, limbal ring) and the white of the eye
//!   (vessels, the fine network), one atlas for the `eye` program;
//! - [`geometry::meshes`]: the eyeball and cornea, the lids with their margins, crease and pocket, the caruncle, the
//!   occlusion shell and the lashes;
//! - [`skin::texture`]: the skin round the eye (tone, mottling, fine lines, freckles);
//! - [`eye_params`] and [`skin_params`]: the programs' parameters for a design and where the eye is.
//! - [`closeup::Closeup`]: all of it built on a renderer, lit and framed as a macro still (the lab test and
//!   `render.eye` draw through it), from a view or any [`closeup::Shot`];
//! - [`fit::fit`]: the opening, iris, pupil, gaze and camera fitted to landmarks measured on a photograph
//!   (`face.eye3d.fit`); named shapes and iris colours ([`EyeShape`](omuzaki_face_api::eye3d::EyeShape),
//!   [`IrisColour`](omuzaki_face_api::eye3d::IrisColour), `face.eye3d.preset`) are the quick start before it.
//!
//! Marks painted by hand ([`EyeDesign::paint`], `face.eye3d.paint`) go over the generated detail of the skin, the
//! white and the iris, so any freckle, vessel or fleck can be put exactly where it is wanted.
//!
//! Everything is a pure function of an [`EyeDesign`], whose seed drives every random detail.

#![allow(
    clippy::many_single_char_names,
    reason = "geometry, noise and colour maths: x, y, u, v, r, a, b, ... are the field's names"
)]

pub mod canvas;
pub mod closeup;
pub mod design;
pub mod fit;
pub mod geometry;
pub mod noise;
mod paint;
pub mod skin;
pub mod textures;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::id::Slug;
use omuzaki_render_api::{ParamValue, ProgramParams};

pub use design::EyeDesign;
pub use geometry::{EyeMeshes, LIMBUS, meshes};

/// Where the eye is: its centre and gaze in world space, and which way is up.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EyeFrame {
    /// The eyeball's centre.
    pub centre: Vec3,
    /// The gaze.
    pub forward: Vec3,
    /// Up, roughly (it is made square to the gaze).
    pub up: Vec3,
}

fn put(params: &mut ProgramParams, name: &str, value: ParamValue) {
    if let Ok(slug) = Slug::new(name) {
        params.insert(slug, value);
    }
}

/// The `eye` program's parameters for `design`'s eyeball at `frame`.
pub fn eye_params(design: &EyeDesign, frame: EyeFrame) -> ProgramParams {
    let ball = &design.ball;
    let mut p = ProgramParams::new();
    put(&mut p, "centre", ParamValue::Vec3(frame.centre));
    put(&mut p, "forward", ParamValue::Vec3(frame.forward.normalize_or_zero()));
    put(&mut p, "up", ParamValue::Vec3(frame.up.normalize_or_zero()));
    put(&mut p, "radius", ParamValue::Float(ball.radius));
    put(&mut p, "iris-radius", ParamValue::Float(ball.iris_radius));
    put(&mut p, "pupil", ParamValue::Float(ball.pupil));
    put(&mut p, "cornea-rise", ParamValue::Float(ball.cornea_rise));
    put(&mut p, "limbus", ParamValue::Float(LIMBUS));
    // The iris's rim sits just behind the limbus's plane.
    let limbus_z = (1.0 - (ball.iris_radius * LIMBUS / ball.radius).powi(2)).max(0.0).sqrt();
    put(&mut p, "iris-depth", ParamValue::Float(limbus_z - 0.04));
    // Toward anime the eye is drawn, not seen through a lens: the cornea stops bending the view, and the iris's caustic,
    // relief and scattering give way to the painted iris and its painted highlights.
    let real = 1.0 - design.style;
    put(&mut p, "scatter", ParamValue::Float(0.2 * real));
    put(&mut p, "ior", ParamValue::Float(1.0 + 0.376 * real));
    put(&mut p, "caustic", ParamValue::Float(0.6 * real));
    put(&mut p, "iris-relief", ParamValue::Float(0.008 * real));
    put(&mut p, "iris-bulge", ParamValue::Float(0.035 * real));
    put(&mut p, "tear-wobble", ParamValue::Float(0.05 * real));
    put(&mut p, "gloss", ParamValue::Float(real));
    put(&mut p, "drawn", ParamValue::Float(design.style));
    if design.style > 0.0 {
        put(&mut p, "pupil-colour", ParamValue::Colour(design.anime.pupil));
    }
    p
}

/// The `skin` program's parameters for the skin round `design`'s eye. Toward anime the skin reads as a flat painted
/// tone: no pores, little scattering or shine, and light that wraps round softly.
pub fn skin_params(design: &EyeDesign) -> ProgramParams {
    let real = 1.0 - design.style;
    let mut p = ProgramParams::new();
    put(&mut p, "pore-depth", ParamValue::Float(0.02 * real));
    put(&mut p, "roughness", ParamValue::Float(0.52 + 0.4 * design.style));
    put(&mut p, "sheen", ParamValue::Float(0.7 * real));
    put(&mut p, "sss", ParamValue::Float(0.3 + 0.7 * real));
    put(&mut p, "wrap", ParamValue::Float(0.4 + 0.5 * design.style));
    put(&mut p, "specular", ParamValue::Float(0.028 * real));
    // The lid margins' wet pink gives way to the skin's own tone.
    put(&mut p, "wet-tint", ParamValue::Float(0.4 * real));
    put(&mut p, "wet-colour", ParamValue::Colour(Colour::hex(0x00b8_8070)));
    p
}

/// The `skin` program's parameters for the lashes: no pores, a little gloss.
pub fn lash_params() -> ProgramParams {
    let mut p = ProgramParams::new();
    put(&mut p, "pore-depth", ParamValue::Float(0.0));
    put(&mut p, "sss", ParamValue::Float(0.0));
    put(&mut p, "wrap", ParamValue::Float(0.25));
    put(&mut p, "roughness", ParamValue::Float(0.45));
    put(&mut p, "sheen", ParamValue::Float(0.4));
    put(&mut p, "scatter-colour", ParamValue::Colour(Colour::hex(0x0060_3020)));
    p
}
