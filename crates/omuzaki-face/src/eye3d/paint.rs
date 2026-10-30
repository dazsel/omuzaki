//! The eye's paint layer: marks a person or an agent places by hand on the skin, the white or the iris
//! ([`EyeDesign::paint`]), drawn over each texture after its generated detail, so a freckle, a vessel, a crypt or a
//! fleck can sit exactly where it is wanted. A mark of one point is a dot (soft-edged as its `softness` asks); more
//! points are a stroke through them, narrowing by its `taper`.

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::eye3d::{EyeMark, EyeSurface};

use super::canvas::Canvas;
use super::design::EyeDesign;
use super::geometry::{SKIN_HALF, SKIN_TOP};
use super::textures::{DISC, SCLERA_REACH};

/// Paints `design`'s marks on `surface` onto `canvas`, whose pixels map to the surface through `to_px` (metres in the
/// mark's frame to pixels) at `px_per_m` pixels a metre.
fn paint(canvas: &mut Canvas, design: &EyeDesign, surface: EyeSurface, px_per_m: f32, to_px: impl Fn(Vec2) -> Vec2) {
    for mark in design.paint.iter().filter(|m| m.on == surface) {
        draw(canvas, mark, px_per_m, &to_px);
    }
}

fn draw(canvas: &mut Canvas, mark: &EyeMark, px_per_m: f32, to_px: &impl Fn(Vec2) -> Vec2) {
    let colour = mark.colour.to_linear();
    let width = (mark.size * px_per_m).max(0.5);
    let points: Vec<Vec2> = mark.points.iter().map(|&p| to_px(Vec2::from(p))).collect();
    match points.as_slice() {
        [] => {}
        [at] => canvas.disc(*at, Vec2::splat(0.5 * width), colour, mark.opacity, 1.0 - mark.softness),
        _ => canvas.stroke(&points, (width, width * (1.0 - mark.taper)), colour, (mark.opacity, mark.opacity)),
    }
}

/// The skin's marks: points are metres in the eye's frame (`+x` toward the nose, `+y` up), as the skin texture lies.
pub(super) fn skin(canvas: &mut Canvas, design: &EyeDesign) {
    #[allow(clippy::cast_precision_loss, reason = "a texture's width")]
    let px_per_m = canvas.width as f32 / (2.0 * SKIN_HALF.x);
    paint(canvas, design, EyeSurface::Skin, px_per_m, |p| {
        Vec2::new((p.x + SKIN_HALF.x) * px_per_m, (SKIN_TOP - p.y) * px_per_m)
    });
}

/// The iris's marks: points are metres from the iris's centre in its own plane, the eye looking straight ahead.
pub(super) fn iris(canvas: &mut Canvas, design: &EyeDesign) {
    #[allow(clippy::cast_precision_loss, reason = "a texture's width")]
    let half = 0.5 * DISC * canvas.width as f32;
    let px_per_m = half / design.ball.iris_radius.max(1e-4);
    #[allow(clippy::cast_precision_loss, reason = "a texture's width")]
    let centre = 0.5 * canvas.width as f32;
    paint(canvas, design, EyeSurface::Iris, px_per_m, |p| Vec2::new(centre + p.x * px_per_m, centre - p.y * px_per_m));
}

/// The white's marks: points are metres across the eyeball seen from straight in front (its centre at the origin),
/// the eye looking straight ahead; the white wraps them round the ball.
pub(super) fn sclera(canvas: &mut Canvas, design: &EyeDesign) {
    let radius = design.ball.radius.max(1e-4);
    #[allow(clippy::cast_precision_loss, reason = "a texture's width")]
    let half = 0.5 * DISC * canvas.width as f32;
    // Along the surface near the front, a metre is this many pixels of the polar map.
    let px_per_m = half / (SCLERA_REACH * radius);
    #[allow(clippy::cast_precision_loss, reason = "a texture's width")]
    let centre = 0.5 * canvas.width as f32;
    paint(canvas, design, EyeSurface::Sclera, px_per_m, |p| {
        let off = (p.length() / radius).min(1.0).asin() / SCLERA_REACH;
        let dir = p.normalize_or_zero();
        Vec2::new(centre + dir.x * off * half, centre - dir.y * off * half)
    });
}
