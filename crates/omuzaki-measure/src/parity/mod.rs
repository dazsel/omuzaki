//! Pixel parity: the measures that say, pixel for pixel and frame for frame, how far a render is from its reference,
//! where, and what to change, until the exact difference map is 0.
//!
//! The engine works on decoded pixels and plain numbers, never on the document, so the same code scores a still, a
//! video frame by frame, or a signal sample by sample. The `parity.*` ops ([`crate::ops`]) read the images, run the
//! provenance check, and return the shared result shape ([`score::Metric`], [`score::Worst`]).
//!
//! | Module | Holds |
//! |---|---|
//! | [`raster`] | CIELAB planes with alpha, blur, bilinear sampling, the similarity warp |
//! | [`metrics`] | the exact byte map, CIEDE2000, SSIM, gradient similarity, edges and their distance, silhouettes, statistics |
//! | [`regions`] | given regions (rectangles, ellipses, polygons, masks) and found ones (silhouette, background, outline, eyes, mouth, face, hair, body) |
//! | [`align`] | the camera: the transform from points, silhouettes and a pixel search, and the camera change it asks for |
//! | [`timing`] | time: the best offset and the warp between two sequences |
//! | [`score`] | the score of a pair: done only at a 0 map; regions, worst places, colour gaps and hints |
//! | [`overlay`] | onion, exact, difference, delta E, signed, wipe, checker, edges, silhouette, regions, sheet |
//! | [`series`] | signal parity for tracking, rig and motion curves |
//! | [`provenance`] | recreate, never copy: refuses a score built from reference pixels |

// Pixel arithmetic throughout: image sides, pixel counts and frame counts stay far below 2^23, so their casts to floats
// are exact and their casts to `isize` cannot wrap; the published formulas keep their one-letter names.
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod align;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod metrics;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod overlay;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod provenance;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod raster;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod regions;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod score;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod series;
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub mod timing;
