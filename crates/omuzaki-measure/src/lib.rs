//! Image measures for likeness (VF-01, ADR 0029): the maths that says whether a render's face matches a reference.
//!
//! The measures take an image and its landmarks, never a character, so a reference (a sheet, a drawing) and a render run
//! through the same code (`docs/design/likeness-measures.md`, D1). This first step reads the pixels: it decodes an image
//! from the store or a file, and converts colour to CIELAB for the later steps to compare.
//!
//! # Module map
//!
//! | Module | Holds |
//! |---|---|
//! | [`image`] | `ImageRef` and `load`: a reference or render decoded to straight sRGB RGBA8, under the file fence and the size limits (design 4.1) |
//! | [`colour`] | `lab`: sRGB bytes to CIELAB (D65), the colour space the skin and line tests compare in |
//! | [`messages`] | the refusals of design 7.3 and the three image refusals, declared as localised messages (ADR 0023) |
//! | `header` | what a PNG or JPEG header says before a decode: the JPEG frame and the colour tags, read from the bytes |
//! | `icc` | whether an embedded ICC profile is sRGB, from its primaries, white and curves (not its description) |
//!
//! Later tasks add the modules of design D9 beside these: `face::{frame, segment, snap, outline, spacing, likeness}`,
//! and the ops that call them. No op is registered yet, so the crate has no `define_link!` (ADR 0015).
#![deny(missing_docs)]

pub mod colour;
pub mod image;
pub mod messages;

mod header;
mod icc;
