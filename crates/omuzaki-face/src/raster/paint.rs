//! Colour for a pixel: solid colours and gradients as the 2D canvas computes them, and blending onto the raster.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Colour, GradientStop, Paint, Stops};

/// A paint ready to be sampled: coordinates already carried into pixel space, opacity folded into the colours.
#[derive(Copy, Clone, Debug)]
pub(super) enum Shader {
    /// Premultiplied colour, components 0..=1.
    Solid([f32; 4]),
    Linear {
        /// Pixel space to gradient space, then to the position along the gradient: `t = dot(inv * p - from, axis)`.
        inv: Affine2,
        from: Vec2,
        /// `(to - from) / |to - from|^2`, so a dot product is the position.
        axis: Vec2,
        stops: Stops,
        opacity: f32,
    },
    Radial {
        inv: Affine2,
        centre: Vec2,
        inv_radius: f32,
        stops: Stops,
        opacity: f32,
    },
}

impl Shader {
    /// The shader for `paint` at `opacity`, drawn under `to_px` (plane to pixels). `None` draws nothing: a degenerate
    /// gradient (no span, no radius, no stops) paints transparent on the canvas.
    pub fn new(paint: &Paint, opacity: f32, to_px: Affine2) -> Option<Self> {
        let opacity = opacity.clamp(0.0, 1.0);
        match *paint {
            Paint::Solid(c) => Some(Self::Solid(premultiply(c, opacity))),
            Paint::Linear { from, to, stops } => {
                let d = to - from;
                let len2 = d.length_squared();
                if stops.as_slice().is_empty() || len2 <= 0.0 || !len2.is_finite() || to_px.matrix2.determinant() == 0.0
                {
                    return None;
                }
                let inv = to_px.inverse();
                (inv.is_finite() && from.is_finite()).then_some(Self::Linear {
                    inv,
                    from,
                    axis: d / len2,
                    stops,
                    opacity,
                })
            }
            Paint::Radial { centre, radius, stops } => {
                if stops.as_slice().is_empty()
                    || radius <= 0.0
                    || !radius.is_finite()
                    || to_px.matrix2.determinant() == 0.0
                {
                    return None;
                }
                let inv = to_px.inverse();
                (inv.is_finite() && centre.is_finite()).then_some(Self::Radial {
                    inv,
                    centre,
                    inv_radius: radius.recip(),
                    stops,
                    opacity,
                })
            }
        }
    }

    /// The premultiplied colour at the centre of pixel (`x`, `y`).
    pub fn colour_at(&self, x: usize, y: usize) -> [f32; 4] {
        #[allow(clippy::cast_precision_loss)] // pixel coordinates are far below 2^24
        let pixel = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
        match self {
            Self::Solid(c) => *c,
            Self::Linear { inv, from, axis, stops, opacity } => {
                let t = (inv.transform_point2(pixel) - *from).dot(*axis);
                ramp(stops, t, *opacity)
            }
            Self::Radial { inv, centre, inv_radius, stops, opacity } => {
                let t = (inv.transform_point2(pixel) - *centre).length() * *inv_radius;
                ramp(stops, t, *opacity)
            }
        }
    }

    /// The pixel bytes a fully covered pixel gets, if that is a fixed opaque colour (so it can be written without blending).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
    pub fn opaque_bytes(&self) -> Option<[u8; 4]> {
        match self {
            Self::Solid(c) if c[3] >= 1.0 => Some(c.map(|v| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8)),
            _ => None,
        }
    }
}

fn components(c: Colour) -> [f32; 4] {
    [c.r, c.g, c.b, c.a].map(|v| f32::from(v) / 255.0)
}

fn premultiply(colour: Colour, opacity: f32) -> [f32; 4] {
    let [red, green, blue, alpha] = components(colour);
    let alpha = alpha * opacity;
    [red * alpha, green * alpha, blue * alpha, alpha]
}

/// The colour at position `t` of a gradient: the first stop's colour before it and the last one's after it, between
/// stops a straight blend of the stored colours (the canvas interpolates before it premultiplies, so a stop that fades
/// to transparent keeps its hue), then premultiplied and scaled by the opacity.
fn ramp(stops: &Stops, t: f32, opacity: f32) -> [f32; 4] {
    let stops: &[GradientStop] = stops.as_slice();
    let straight = match (stops.first(), stops.last()) {
        (Some(first), _) if t <= first.offset => components(first.colour),
        (_, Some(last)) if t >= last.offset => components(last.colour),
        _ => {
            let mut colour = [0.0; 4];
            for pair in stops.windows(2) {
                if t <= pair[1].offset {
                    let span = (pair[1].offset - pair[0].offset).max(f32::EPSILON);
                    let along = (t - pair[0].offset) / span;
                    let (from, to) = (components(pair[0].colour), components(pair[1].colour));
                    colour = [0, 1, 2, 3].map(|k| from[k] + (to[k] - from[k]) * along);
                    break;
                }
            }
            colour
        }
    };
    let alpha = straight[3] * opacity;
    [straight[0] * alpha, straight[1] * alpha, straight[2] * alpha, alpha]
}

/// Source-over of premultiplied `src` (0..=1) with `coverage` (0..=255) onto one premultiplied RGBA8 pixel.
pub(super) fn blend(dst: &mut [u8], src: [f32; 4], coverage: u8) {
    let c = f32::from(coverage) / 255.0;
    let keep = 1.0 - src[3] * c;
    for (d, s) in dst.iter_mut().zip(src) {
        let v = s * 255.0 * c + f32::from(*d) * keep;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
        {
            *d = (v + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
}
