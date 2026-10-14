//! Marks: freckles, beauty marks, face paint and scars (CB-23), drawn on the Base layer in profile order.
//!
//! The prototype has none, so these are designed here from the contract ([`FaceMark`]) with three promises: a mark is a
//! pure function of its fields (freckles scatter from `seed` with a small integer generator, never a clock or a global
//! RNG); a mark never leaves the face rect (its centre is held far enough inside for its widest extent); and nothing
//! allocates once the draw list is warm.

use std::f32::consts::{PI, TAU};

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, Colour, ColourRef, FaceLayerId, FaceMark, FaceRect, Join, MarkKind, Paint, narrow};

use crate::painter::PaintCtx;

/// A freckle's radius as a share of the patch radius (`size`).
const FRECKLE_RADIUS: f32 = 0.12;
/// The farthest any mark reaches from its centre, in units of `size`, strokes and dot radii included: the freckle dot
/// beyond the patch edge (1.12), the corner of an outer stripe (1.22), the end of a scar's tick with its cap (0.4). The centre is held this far inside the face rect.
const REACH: f32 = 1.3;
/// Stripe paint: bars, their length (in `size`), width, spacing across, and the direction they run.
const STRIPES: u8 = 3;
const STRIPE_HALF_LENGTH: f32 = 1.0;
const STRIPE_WIDTH: f32 = 0.28;
const STRIPE_SPACING: f32 = 0.55;
/// Star paint: points, and the inner radius as a share of the outer.
const STAR_POINTS: usize = 5;
const STAR_INNER: f32 = 0.45;
/// The direction paint and scars run, in radians from the +x axis (a cheek stripe leans toward the nose).
const SLANT: f32 = 1.05;
/// Scar: line width and cross-tick width and length as shares of the scar's length (`size`), and the ticks' stations.
const SCAR_WIDTH: f32 = 0.08;
const TICK_WIDTH: f32 = 0.05;
const TICK_HALF_LENGTH: f32 = 0.16;
const TICK_STATIONS: [f32; 4] = [-0.3, -0.1, 0.1, 0.3];
/// Colour used when a palette slot reached the painter unresolved (it should not: the host resolves them).
const FALLBACK_COLOUR: Colour = Colour::rgb(0x7a, 0x4a, 0x40);

/// The shapes face paint can take, picked by the mark's `seed` modulo their count so a character's paint is stable.
#[derive(Copy, Clone)]
enum PaintShape {
    Stripes,
    Star,
}

const PAINT_SHAPES: [PaintShape; 2] = [PaintShape::Stripes, PaintShape::Star];

/// Paints every mark of the profile on the Base layer.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let rect = ctx.specs[FaceLayerId::Base as usize].rect;
    let profile = ctx.profile;
    for mark in &profile.marks {
        let colour = match &mark.colour {
            ColourRef::Fixed(c) => *c,
            ColourRef::Slot(_) => FALLBACK_COLOUR,
        };
        // A mark that cannot fit the rect at its size is scaled down until its full reach does, so it never leaves it.
        let size = narrow(mark.size).min(rect.width().min(rect.height()) / (2.0 * REACH));
        let centre = keep_inside(Vec2::new(narrow(mark.at[0]), narrow(mark.at[1])), &rect, REACH * size);
        let opacity = narrow(mark.opacity).clamp(0.0, 1.0);
        if opacity <= 0.0 || size <= 0.0 {
            continue;
        }
        let list = ctx.list(FaceLayerId::Base);
        let ink = Ink { paint: Paint::Solid(colour), opacity };
        match mark.kind {
            MarkKind::BeautyMark => disc(list, &ink, centre, size),
            MarkKind::Freckles => freckles(list, &ink, mark, centre, size),
            MarkKind::Paint => face_paint(list, &ink, mark.seed, centre, size),
            MarkKind::Scar => scar(list, &ink, centre, size),
        }
    }
}

/// How a mark's shapes are coloured.
struct Ink {
    paint: Paint,
    opacity: f32,
}

type List = omuzaki_face_api::DrawList;

/// `p` held at least `margin` inside `rect`. Written with `max`/`min` rather than `clamp`, which panics on a rect
/// narrower than twice the margin; the caller has already made the margin fit.
fn keep_inside(p: Vec2, rect: &FaceRect, margin: f32) -> Vec2 {
    Vec2::new(p.x.max(rect.x0 + margin).min(rect.x1 - margin), p.y.max(rect.y0 + margin).min(rect.y1 - margin))
}

fn disc(list: &mut List, ink: &Ink, centre: Vec2, radius: f32) {
    let path = list.path().ellipse(centre, radius, radius).finish();
    list.push(Cmd::Fill { path, paint: ink.paint, opacity: ink.opacity });
}

fn stroke(list: &mut List, ink: &Ink, from: Vec2, to: Vec2, width: f32) {
    let path = list.path().move_to(from).line_to(to).finish();
    let (cap, join) = (Cap::Round, Join::Round);
    list.push(Cmd::Stroke { path, paint: ink.paint, width, cap, join, opacity: ink.opacity });
}

/// `count` dots scattered uniformly over a disc of radius `size` from the mark's seed.
fn freckles(list: &mut List, ink: &Ink, mark: &FaceMark, centre: Vec2, size: f32) {
    let mut rng = Rng::new(mark.seed);
    for _ in 0..mark.count {
        // The square root makes the scatter uniform over the disc's area, not crowded at its centre.
        let reach = size * rng.unit().sqrt();
        let (sin, cos) = libm::sincosf(TAU * rng.unit());
        disc(list, ink, centre + Vec2::new(cos, sin) * reach, size * FRECKLE_RADIUS);
    }
}

/// Face paint: three diagonal stripes or a star, by the seed.
fn face_paint(list: &mut List, ink: &Ink, seed: u32, centre: Vec2, size: f32) {
    // The length is a handful of shapes, so the cast and the modulo cannot lose anything.
    #[allow(clippy::cast_possible_truncation)]
    let shape = PAINT_SHAPES[seed as usize % PAINT_SHAPES.len()];
    let (sin, cos) = libm::sincosf(SLANT);
    let (along, across) = (Vec2::new(cos, sin), Vec2::new(-sin, cos));
    match shape {
        PaintShape::Stripes => {
            for i in 0..STRIPES {
                let offset = (f32::from(i) - f32::from(STRIPES - 1) / 2.0) * STRIPE_SPACING * size;
                let middle = centre + across * offset;
                let half = along * (STRIPE_HALF_LENGTH * size);
                let bar = across * (STRIPE_WIDTH * size / 2.0);
                let path = list
                    .path()
                    .polyline(&[middle - half - bar, middle + half - bar, middle + half + bar, middle - half + bar])
                    .close()
                    .finish();
                list.push(Cmd::Fill { path, paint: ink.paint, opacity: ink.opacity });
            }
        }
        PaintShape::Star => {
            let mut points = [Vec2::ZERO; 2 * STAR_POINTS];
            for (i, point) in points.iter_mut().enumerate() {
                let radius = if i % 2 == 0 { size } else { size * STAR_INNER };
                // Ten vertices: the index is exact in f32.
                #[allow(clippy::cast_precision_loss)]
                let angle = PI / 2.0 + PI * i as f32 / STAR_POINTS as f32;
                let (sin, cos) = libm::sincosf(angle);
                *point = centre + Vec2::new(cos, sin) * radius;
            }
            let path = list.path().polyline(&points).close().finish();
            list.push(Cmd::Fill { path, paint: ink.paint, opacity: ink.opacity });
        }
    }
}

/// A scar: a line `size` long with short ticks across it, like stitches.
fn scar(list: &mut List, ink: &Ink, centre: Vec2, size: f32) {
    let (sin, cos) = libm::sincosf(SLANT);
    let (along, across) = (Vec2::new(cos, sin), Vec2::new(-sin, cos));
    stroke(list, ink, centre - along * (size / 2.0), centre + along * (size / 2.0), size * SCAR_WIDTH);
    for station in TICK_STATIONS {
        let at = centre + along * (station * size);
        let half = across * (TICK_HALF_LENGTH * size);
        stroke(list, ink, at - half, at + half, size * TICK_WIDTH);
    }
}

/// A 32-bit xorshift generator: tiny, portable and identical on every platform, which is all a freckle scatter needs.
struct Rng(u32);

impl Rng {
    /// Seeded generator; xorshift never leaves zero, so a zero seed is replaced.
    fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9e37_79b9 } else { seed })
    }

    /// The next value in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        // The top 24 bits are exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let bits = (self.0 >> 8) as f32;
        bits / 16_777_216.0
    }
}
