//! The iris's structure (#2465): the limbal ring, radial fibres, the collarette, flecks and the glow round the pupil,
//! painted by [`super::iris`] over the iris gradient and under the crescent and the pupil, inside the iris's clip.
//!
//! Every piece is off at strength 0 (the default), so a face that never sets them paints the prototype's iris. Each is
//! one command whatever its count (the fibres two: light and dark), so the eye's command budget does not grow with the
//! detail: a fibre or a fleck is a sub-path of one fill.
//!
//! The pattern is a pure function of the eye's [`seed`](omuzaki_face_api::EyeStyle::seed): each piece draws from its
//! own stream, so turning one up never moves another. The right eye is the left eye mirrored, as a pair of drawn eyes
//! is.

use std::f32::consts::TAU;

use libm::{cosf, sinf};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cmd, DrawList, FaceLayerId, IrisDetail, Paint, Stops, narrow};

use super::iris::{Iris, lighten};
use super::{FALLBACK_COLOUR, Side};
use crate::painter::marks::Rng;
use crate::painter::{PaintCtx, fixed_colour, scaled};

/// Below this a strength counts as off.
const OFF: f32 = 0.005;
/// Each piece's stream, mixed into the eye's seed.
const FIBRE_STREAM: u32 = 0x6a09_e667;
const COLLARETTE_STREAM: u32 = 0xbb67_ae85;
const FLECK_STREAM: u32 = 0x3c6e_f372;
/// Points per collarette tooth.
const TOOTH_POINTS: u32 = 6;
/// The flecks sit between these fractions of the iris radius.
const FLECK_BAND: (f32, f32) = (0.32, 0.9);
/// The pupil glow reaches this far past the pupil, in iris radii.
const GLOW_REACH: f32 = 0.38;
/// How far the glow's colour is lightened from the iris's lit colour.
const GLOW_LIGHTEN: f32 = 0.35;

/// The iris in its own unit circle: `(x, y)` with `x` toward the character's left for the left eye and mirrored for the
/// right, so a pattern drawn here is one design for both eyes.
struct Unit<'a> {
    iris: &'a Iris,
    mirror: f32,
}

impl Unit<'_> {
    fn at(&self, r: f32, angle: f32) -> Vec2 {
        self.iris.at(self.mirror * r * cosf(angle), r * sinf(angle))
    }
}

/// Paints the detail of one iris; the caller has the iris clip pushed. `low` is the iris's lit colour and `pupil` the
/// pupil's centre and radii (for the glow).
pub(super) fn paint(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, low: Colour, pupil: (Vec2, Vec2)) {
    let style = &ctx.profile.eyes;
    let detail = &style.iris.detail;
    let seed = style.seed;
    let unit = Unit { iris, mirror: if side == Side::Left { 1.0 } else { -1.0 } };
    let list = ctx.list(FaceLayerId::Base);
    limbal(list, iris, detail);
    fibres(list, &unit, detail, seed);
    collarette(list, &unit, iris, detail, seed);
    flecks(list, &unit, iris, detail, seed);
    pupil_glow(list, iris, detail, low, pupil);
}

/// The squash that turns a disc of radius `rx` at the iris centre into the iris ellipse.
fn squash(iris: &Iris) -> Affine2 {
    Affine2::from_translation(iris.centre)
        * Affine2::from_scale(Vec2::new(1.0, iris.ry / iris.rx))
        * Affine2::from_translation(-iris.centre)
}

/// The limbal ring: the colour fading in over the outer `width` of the radius, strongest at the rim.
fn limbal(list: &mut DrawList, iris: &Iris, detail: &IrisDetail) {
    let ring = &detail.limbal;
    let strength = narrow(ring.strength).clamp(0.0, 1.0);
    if strength < OFF {
        return;
    }
    let colour = fixed_colour(&ring.colour, FALLBACK_COLOUR);
    let inner = 1.0 - narrow(ring.width).clamp(0.0, 1.0);
    let stops = Stops::new(&[
        (0.0, scaled(colour, 0.0)),
        (inner, scaled(colour, 0.0)),
        (inner + (1.0 - inner) * 0.6, scaled(colour, 0.55)),
        (1.0, colour),
    ]);
    let disc = list.path().ellipse(iris.centre, iris.rx, iris.rx).finish();
    list.push(Cmd::PushTransform(squash(iris)));
    list.push(Cmd::Fill {
        path: disc,
        paint: Paint::Radial { centre: iris.centre, radius: iris.rx, stops },
        opacity: strength,
    });
    list.push(Cmd::PopTransform);
}

/// The radial fibres: lens-shaped wedges from `inner` to `outer`, alternately light and dark, each bent and shortened a
/// little by the seed. One fill per colour.
fn fibres(list: &mut DrawList, unit: &Unit<'_>, detail: &IrisDetail, seed: u32) {
    let style = &detail.fibres;
    let strength = narrow(style.strength).clamp(0.0, 1.0);
    if strength < OFF || style.count == 0 {
        return;
    }
    let count = style.count.max(2);
    // At most 256, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let step = TAU / count as f32;
    let (inner, outer) = (narrow(style.inner), narrow(style.outer));
    let half = 0.5 * narrow(style.width) * step;
    let wave = narrow(style.waviness).clamp(0.0, 1.0);
    for (shade, colour) in [(0, &style.light), (1, &style.dark)] {
        let mut rng = Rng::new(seed ^ FIBRE_STREAM);
        let mut path = list.path();
        for i in 0..count {
            // Every draw happens for every fibre, light or dark, so the two fills share one pattern.
            let (jitter, length, bend, swell) = (rng.unit(), rng.unit(), rng.unit(), rng.unit());
            if i % 2 != shade {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // at most 256
            let angle = (i as f32 + (jitter - 0.5) * wave) * step;
            let start = inner * (1.0 + 0.25 * wave * (swell - 0.5));
            let end = start + (outer - start) * (1.0 - 0.45 * wave * length);
            let mid = f32::midpoint(start, end);
            let lean = (bend - 0.5) * wave * step * 1.5;
            let width = half * (0.6 + 0.8 * swell);
            path = path
                .move_to(unit.at(start, angle))
                .quad_to(unit.at(mid, angle + lean + width), unit.at(end, angle + 2.0 * lean))
                .quad_to(unit.at(mid, angle + lean - width), unit.at(start, angle))
                .close();
        }
        let path = path.finish();
        let paint = Paint::Solid(fixed_colour(colour, FALLBACK_COLOUR));
        list.push(Cmd::Fill { path, paint, opacity: strength });
    }
}

/// The collarette: a closed toothed line round the iris at `radius`, each tooth its own height.
fn collarette(list: &mut DrawList, unit: &Unit<'_>, iris: &Iris, detail: &IrisDetail, seed: u32) {
    let style = &detail.collarette;
    let strength = narrow(style.strength).clamp(0.0, 1.0);
    if strength < OFF || style.teeth == 0 {
        return;
    }
    let (radius, depth, rough) =
        (narrow(style.radius), narrow(style.depth), narrow(style.irregularity).clamp(0.0, 1.0));
    let mut rng = Rng::new(seed ^ COLLARETTE_STREAM);
    let points = style.teeth * TOOTH_POINTS;
    let mut path = list.path();
    let mut height = 1.0;
    for i in 0..=points {
        if i % TOOTH_POINTS == 0 && i < points {
            height = 1.0 - rough * rng.unit();
        }
        // At most 64 * 6 points, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let t = (i % points) as f32 / points as f32;
        #[allow(clippy::cast_precision_loss)]
        let phase = (i % TOOTH_POINTS) as f32 / TOOTH_POINTS as f32;
        // A rounded tooth: out at its middle, in at its ends.
        let tooth = sinf(phase * TAU * 0.5);
        let r = radius + depth * (tooth * height - 0.5);
        let p = unit.at(r, t * TAU);
        path = if i == 0 { path.move_to(p) } else { path.line_to(p) };
    }
    let path = path.close().finish();
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(colour),
        width: narrow(style.width) * iris.rx,
        cap: omuzaki_face_api::Cap::Round,
        join: omuzaki_face_api::Join::Round,
        opacity: strength,
    });
}

/// The flecks: small ellipses scattered over the band between the collarette and the rim.
fn flecks(list: &mut DrawList, unit: &Unit<'_>, iris: &Iris, detail: &IrisDetail, seed: u32) {
    let style = &detail.flecks;
    let strength = narrow(style.strength).clamp(0.0, 1.0);
    if strength < OFF || style.count == 0 {
        return;
    }
    let mut rng = Rng::new(seed ^ FLECK_STREAM);
    let size = narrow(style.size);
    let mut path = list.path();
    for _ in 0..style.count {
        let (angle, band, grow, squash) = (rng.unit() * TAU, rng.unit(), rng.unit(), rng.unit());
        let r = FLECK_BAND.0 + (FLECK_BAND.1 - FLECK_BAND.0) * band;
        let radius = size * (0.5 + grow) * iris.rx;
        path = path.ellipse(unit.at(r, angle), radius, radius * (0.6 + 0.4 * squash));
    }
    let path = path.finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(fixed_colour(&style.colour, FALLBACK_COLOUR)), opacity: strength });
}

/// A soft lit ring round the pupil, in a lighter shade of the iris's lit colour, fading out [`GLOW_REACH`] past it.
fn pupil_glow(list: &mut DrawList, iris: &Iris, detail: &IrisDetail, low: Colour, (centre, radii): (Vec2, Vec2)) {
    let strength = narrow(detail.pupil_glow).clamp(0.0, 1.0);
    if strength < OFF || radii.x <= 0.0 {
        return;
    }
    let colour = lighten(low, GLOW_LIGHTEN);
    let reach = radii.x + GLOW_REACH * iris.rx;
    let at_pupil = (radii.x / reach).clamp(0.0, 0.95);
    let stops = Stops::new(&[(0.0, colour), (at_pupil, colour), (1.0, scaled(colour, 0.0))]);
    let squash = Affine2::from_translation(centre)
        * Affine2::from_scale(Vec2::new(1.0, radii.y / radii.x))
        * Affine2::from_translation(-centre);
    let disc = list.path().ellipse(centre, reach, reach).finish();
    list.push(Cmd::PushTransform(squash));
    list.push(Cmd::Fill { path: disc, paint: Paint::Radial { centre, radius: reach, stops }, opacity: strength });
    list.push(Cmd::PopTransform);
}
