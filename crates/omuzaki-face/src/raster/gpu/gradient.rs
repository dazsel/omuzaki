//! Paints for the GPU encoder: solid colours and linear or radial gradients as Vello brushes.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::Paint;
use vello::kurbo::Point;
use vello::peniko::color::AlphaColor;
use vello::peniko::{Brush, ColorStop, Gradient, InterpolationAlphaSpace};

/// A plane point as a kurbo point.
pub(super) fn point(p: Vec2) -> Point {
    Point::new(f64::from(p.x), f64::from(p.y))
}

/// `colour` with `opacity` folded into its alpha, rounded to a byte exactly as the CPU raster's shader does.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
fn colour(c: Colour, opacity: f32) -> AlphaColor<vello::peniko::color::Srgb> {
    AlphaColor::from_rgba8(c.r, c.g, c.b, (f32::from(c.a) * opacity).round().clamp(0.0, 255.0) as u8)
}

/// The Vello brush for `paint` at `opacity`. `stops` is the encoder's reusable stop buffer.
///
/// Gradients interpolate straight (unpremultiplied) colour, as the canvas and the CPU raster do; peniko's default
/// premultiplied interpolation fails the gradient probe at 33.5 dB where this gives 46.3 dB (research doc, section 2).
pub(super) fn brush(paint: &Paint, opacity: f32, stops: &mut Vec<ColorStop>) -> Brush {
    let (mut gradient, list) = match *paint {
        Paint::Solid(c) => return Brush::Solid(colour(c, opacity)),
        Paint::Linear { from, to, ref stops } => (Gradient::new_linear(point(from), point(to)), stops),
        Paint::Radial { centre, radius, ref stops } => (Gradient::new_radial(point(centre), radius), stops),
    };
    stops.clear();
    stops.extend(
        list.as_slice().iter().map(|s| ColorStop { offset: s.offset, color: colour(s.colour, opacity).into() }),
    );
    gradient = gradient.with_stops(stops.as_slice());
    gradient.interpolation_alpha_space = InterpolationAlphaSpace::Unpremultiplied;
    Brush::Gradient(gradient)
}
