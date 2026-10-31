//! The illustrator's engine (spec v3 §19 and §20's painted looks): a [`Painting`] is layers of [`Mark`]s, and
//! [`render`] turns it into pixels on the CPU, the same on every machine.
//!
//! Marks are the unit an agent or a person works in, so a part of a character can be redrawn mark by mark and each mark
//! edited or undone on its own:
//! - [`Stroke`]: a brush stroke or ink line through points with pen pressure, smoothed by a spline, with the
//!   [`brush::presets`] (ink, G-pen, maru pen, marker, soft round, airbrush, pencil, chalk, flat brush, watercolour,
//!   spray, blender, erasers) and any setting overridden ([`BrushSettings`]);
//! - [`ShapeMark`]: a filled region ([`Geometry`]: SVG path data, polygons, smooth splines, ellipses, rounded
//!   rectangles) with a colour or a [`Gradient`], feathering and an outline;
//! - [`Bucket`]: a flood fill bounded by line art on other layers, closing gaps and growing under the line;
//! - [`Hatch`]: parallel tapered lines across a region (blush, shading).
//!
//! Layers have the 18 [`BlendMode`]s, opacity, clipping to the layer under them, masks made of marks, and
//! groups (isolated or pass-through); any mark can lock alpha (paint only where its layer has paint). The engine blends in sRGB-encoded values, as painting tools do by default.
//!
//! This is a library crate (ADR 0011): no registrations and no global state. The `paint.*` ops (`omuzaki-paint-ops`)
//! keep paintings in the document, and other painters (the eye's paint layer, the puppet's layered art) draw single
//! marks onto their own canvases with [`stroke_mask`] and [`shape_mask`].
#![deny(missing_docs)]
// Raster maths reads best in its formulas' own notation (x, y, r, a and b; dx and dy; t0 and t1).
#![allow(clippy::many_single_char_names, clippy::similar_names)]

pub mod blend;
pub mod brush;
pub mod bucket;
pub mod canvas;
pub mod colour;
mod error;
pub mod geom;
pub mod hatch;
pub mod model;
mod noise;
pub mod raster;
pub mod render;

pub use blend::BlendMode;
pub use brush::{Brush, BrushSettings, Preset, StrokeInput, preset, presets, stroke_mask};
pub use canvas::{Canvas, Mask, Rect};
pub use colour::{Colour, Px};
pub use error::PaintError;
pub use geom::{ColourPoint, Fill, Freeform, Geometry, Gradient, GradientKind, Stop};
pub use model::{Bucket, Hatch, Layer, LayerKind, Mark, Outline, Painting, ShapeMark, Stroke, check_id};
pub use raster::FillRule;
pub use render::{draw_mark, layer_pixels, render};

/// The coverage of `geometry` filled with `rule` on a `width` by `height` canvas, feathered by `feather` pixels: a
/// shape for another painter to colour.
///
/// # Errors
/// [`PaintError::Invalid`] when the geometry can't be drawn.
pub fn shape_mask(
    geometry: &Geometry,
    rule: FillRule,
    feather: f64,
    width: u32,
    height: u32,
) -> Result<Mask, PaintError> {
    if width == 0 || height == 0 || width > Canvas::MAX_SIDE || height > Canvas::MAX_SIDE {
        return Err(PaintError::invalid(
            "size",
            format!("{width}x{height} is outside 1 to {} px a side", Canvas::MAX_SIDE),
        ));
    }
    #[allow(clippy::cast_possible_wrap)] // at most MAX_SIDE
    let clip = Rect { x0: 0, y0: 0, x1: width as i32, y1: height as i32 };
    let mask = raster::fill_path(&geometry.to_path("geometry")?, rule, clip);
    Ok(if feather > 0.0 { raster::feather(&mask, feather, clip) } else { mask })
}
