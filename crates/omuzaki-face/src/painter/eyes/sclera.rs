//! The eye white and the pink-lilac shade under the lid (the `eye()` prologue, `face.js` lines 126-132).

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceLayerId, Join, Paint, PathRef, Stops, narrow};

use super::FALLBACK_COLOUR;
use super::aperture::{self, Eye};
use crate::painter::marks::Rng;
use crate::painter::{PaintCtx, fixed_colour, scaled};

/// Fills the aperture `outline` with the three-stop eye white: `sclera.top` under the upper lid, `mid` half way,
/// `bottom` at the lower lid. The gradient runs straight down from the upper lid to the lower lid at the eye's centre
/// (as the prototype's does), so a tilted lid still shades from where the lid is. Draws one command, before the
/// caller's clip to `outline` starts.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, eye: &Eye, outline: PathRef) {
    let style = &ctx.profile.eyes.sclera;
    let stops = Stops::new(&[
        (0.0, fixed_colour(&style.top, FALLBACK_COLOUR)),
        (0.5, fixed_colour(&style.mid, FALLBACK_COLOUR)),
        (1.0, fixed_colour(&style.bottom, FALLBACK_COLOUR)),
    ]);
    let paint = Paint::Linear { from: eye.point(0.0, eye.top(0.0)), to: eye.point(0.0, eye.bottom(0.0)), stops };
    ctx.list(FaceLayerId::Base).push(Cmd::Fill { path: outline, paint, opacity: 1.0 });
}

// ------------------------------------------------------------------------------------------------ detail (#2467)

/// Below this a strength counts as off.
const OFF: f32 = 0.005;
/// The corner shade reaches this far in from each corner, as a fraction of the eye's width.
const CORNER_REACH: f32 = 0.32;
/// The vessels' stream, mixed into the eye's seed; vessels per corner at full strength; points per vessel; their width
/// as a fraction of the eye's half-width; how far in they reach (eye units) and how much they wander.
const VESSEL_STREAM: u32 = 0x510e_527f;
const VESSELS_PER_CORNER: u32 = 3;
const VESSEL_POINTS: usize = 6;
const VESSEL_WIDTH: f32 = 0.009;
const VESSEL_REACH: f32 = 0.45;
const VESSEL_WANDER: f32 = 0.07;
/// The caruncle: its place along the eye (eye units from the centre), its radii (eye units).
const CARUNCLE_AT: f32 = -0.9;
const CARUNCLE_RADII: Vec2 = Vec2::new(0.11, 0.16);
/// The waterline: its rim's height above the lower lid (eye units), the samples along it, its run along the lid, and
/// the bright wet line's width (eye units) and opacity.
const WATERLINE_HEIGHT: f32 = 0.055;
const WATERLINE_POINTS: usize = 24;
const WATERLINE_SPAN: (f32, f32) = (-0.95, 0.97);
const WET_WIDTH: f32 = 0.008;
const WET_OPACITY: f32 = 0.45;

/// The sclera's detail of one open eye, inside the aperture clip and under the iris: the corner shade, the vessels and
/// the caruncle.
pub(super) fn paint_detail(ctx: &mut PaintCtx<'_>, eye: &Eye) {
    let style = &ctx.profile.eyes.sclera;
    let seed = ctx.profile.eyes.seed;
    let (shade, vessels, caruncle) = (narrow(style.corner_shade), narrow(style.vessels), narrow(style.caruncle));
    let (corner, vessel, pink) = (
        fixed_colour(&style.corner_colour, FALLBACK_COLOUR),
        fixed_colour(&style.vessel_colour, FALLBACK_COLOUR),
        fixed_colour(&style.caruncle_colour, FALLBACK_COLOUR),
    );
    let outline_mid = eye.midline(0.0);
    let list = ctx.list(FaceLayerId::Base);
    if shade > OFF {
        let clear = scaled(corner, 0.0);
        let stops = Stops::new(&[(0.0, corner), (CORNER_REACH, clear), (1.0 - CORNER_REACH, clear), (1.0, corner)]);
        let paint = Paint::Linear { from: eye.point(-1.0, outline_mid), to: eye.point(1.0, outline_mid), stops };
        let path = aperture::outline(list, eye);
        list.push(Cmd::Fill { path, paint, opacity: shade.clamp(0.0, 1.0) });
    }
    if vessels > OFF {
        paint_vessels(list, eye, seed, vessel, vessels.clamp(0.0, 1.0));
    }
    if caruncle > OFF {
        let centre = eye.point(CARUNCLE_AT, eye.midline(CARUNCLE_AT));
        let (rx, ry) = (CARUNCLE_RADII.x * eye.width(), CARUNCLE_RADII.y * eye.width());
        let stops = Stops::new(&[(0.0, pink), (0.55, scaled(pink, 0.7)), (1.0, scaled(pink, 0.0))]);
        let squash = Affine2::from_translation(centre)
            * Affine2::from_scale(Vec2::new(1.0, ry / rx))
            * Affine2::from_translation(-centre);
        let disc = list.path().ellipse(centre, rx, rx).finish();
        list.push(Cmd::PushTransform(squash));
        list.push(Cmd::Fill {
            path: disc,
            paint: Paint::Radial { centre, radius: rx, stops },
            opacity: caruncle.clamp(0.0, 1.0),
        });
        list.push(Cmd::PopTransform);
    }
}

/// Fine wandering lines running in from both corners, thinning toward the iris; one stroke for all of them.
fn paint_vessels(list: &mut DrawList, eye: &Eye, seed: u32, colour: Colour, strength: f32) {
    let mut rng = Rng::new(seed ^ VESSEL_STREAM);
    let mut path = list.path();
    for corner in [-1.0_f32, 1.0] {
        for _ in 0..VESSELS_PER_CORNER {
            let (start, reach, drift) = (rng.unit(), rng.unit(), rng.unit() - 0.5);
            let u0 = corner * (0.97 - 0.05 * start);
            let length = VESSEL_REACH * (0.5 + 0.5 * reach);
            let v0 = eye.midline(u0) + (start - 0.5) * 0.25;
            for i in 0..VESSEL_POINTS {
                // At most 6, exact in f32.
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / (VESSEL_POINTS - 1) as f32;
                let u = u0 - corner * length * t;
                let v = v0 + drift * 0.3 * t + VESSEL_WANDER * (rng.unit() - 0.5);
                let p = eye.point(u, v);
                path = if i == 0 { path.move_to(p) } else { path.line_to(p) };
            }
        }
    }
    let path = path.finish();
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(colour),
        width: VESSEL_WIDTH * eye.width(),
        cap: Cap::Round,
        join: Join::Round,
        opacity: 0.6 * strength,
    });
}

/// The wet waterline of one open eye, inside the aperture clip and over the iris: a soft rim of the lid's colour along
/// the lower lid with a thin bright line on its upper edge.
pub(super) fn paint_waterline(ctx: &mut PaintCtx<'_>, eye: &Eye) {
    let style = &ctx.profile.eyes.sclera;
    let strength = narrow(style.waterline).clamp(0.0, 1.0);
    if strength < OFF {
        return;
    }
    let rim = fixed_colour(&style.waterline_colour, FALLBACK_COLOUR);
    let at = |i: usize| {
        // At most 24, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / (WATERLINE_POINTS - 1) as f32;
        let u = WATERLINE_SPAN.0 + (WATERLINE_SPAN.1 - WATERLINE_SPAN.0) * t;
        // The rim thins toward both corners.
        let height = WATERLINE_HEIGHT * (1.0 - u * u * 0.6);
        (u, eye.bottom(u), height)
    };
    let list = ctx.list(FaceLayerId::Base);
    let mut band = list.path();
    for i in 0..WATERLINE_POINTS {
        let (u, v, _) = at(i);
        let p = eye.point(u, v - 0.02);
        band = if i == 0 { band.move_to(p) } else { band.line_to(p) };
    }
    for i in (0..WATERLINE_POINTS).rev() {
        let (u, v, h) = at(i);
        band = band.line_to(eye.point(u, v + h));
    }
    let band = band.close().finish();
    let top = eye.point(0.0, eye.bottom(0.0) + WATERLINE_HEIGHT);
    let stops = Stops::new(&[(0.0, scaled(rim, 0.35)), (1.0, rim)]);
    let paint = Paint::Linear { from: top, to: eye.point(0.0, eye.bottom(0.0)), stops };
    list.push(Cmd::Fill { path: band, paint, opacity: strength });
    let mut wet = list.path();
    for i in 1..WATERLINE_POINTS - 1 {
        let (u, v, h) = at(i);
        let p = eye.point(u, v + h);
        wet = if i == 1 { wet.move_to(p) } else { wet.line_to(p) };
    }
    let wet = wet.finish();
    list.push(Cmd::Stroke {
        path: wet,
        paint: Paint::Solid(Colour::rgb(255, 255, 255)),
        width: WET_WIDTH * eye.width(),
        cap: Cap::Round,
        join: Join::Round,
        opacity: WET_OPACITY * strength,
    });
}
