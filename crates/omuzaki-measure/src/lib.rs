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
//! | [`image`] | `ImageRef` and `load`: a reference or render decoded to straight sRGB RGBA8 (a tagged colour space converted by `moxcms`), under the file fence and the decoder limits (design 4.1) |
//! | [`colour`] | `lab`: sRGB bytes to CIELAB (D65), the colour space the skin and line tests compare in |
//! | [`messages`] | the refusals of design 7.3 and the three image refusals, declared as localised messages (ADR 0023) |
//!
//! | [`face`] | the face measures of design 4: frame, segmentation, snap, outline, spacing and likeness |
//! | [`readability`] | the readability measures of VF-06: a silhouette at 64 px against others, and expressions at emote sizes |
//! | [`target`] | the likeness target asset (design 6.1): its TOML, its cache and its staleness |
//! | [`ops`] | `measure.face_outline`, `measure.feature_spacing`, `measure.likeness`, `measure.face_overlay`, the likeness targets, `measure.silhouette` and `measure.readability` |
//!
//! The message table and the ops are registrations (the link gate counts them), so the crate defines its `link()`
//! (ADR 0015).
#![deny(missing_docs)]

omuzaki_core::define_link!();

pub mod colour;
pub mod face;
pub mod image;
pub mod messages;
pub mod ops;
pub mod readability;
pub mod target;
