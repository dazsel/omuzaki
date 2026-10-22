//! How a coat's skirt hangs: a table of superellipse rings from the waist seam down past the lowest hem.
//!
//! At each knot the skirt must enclose the slice of the body [`Cloud`] there, with the ease. Cloth falls: below the
//! widest point of what it hangs over it does not come back in, so the front, the back and the sides only ever move
//! out going down ([`Fall::Drape`]), unless the cut is a pencil, which follows the body ([`Fall::Pencil`]). The flare
//! widens the hem. Where the arms hang beside the body the skirt is kept clear of them and the hands (and of a
//! sleeve over them), unless the garment encloses the arms ([`Arms::Enclose`], a cape). Knot 0 is the waist seam's own
//! ring, so the skirt meets the bodice above it exactly.

use omuzaki_core::glam::{DVec2, DVec3};

use super::cloud::Cloud;
use crate::kit::curve::{fraction, lerp, sstep};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::Ring;

/// Knots from the waist seam to the lowest hem.
pub const KNOTS: usize = 40;
/// The least half height of the slice a knot reads, metres.
const MIN_BAND: f64 = 0.006;
/// What the skirt keeps beyond the ease, metres: a ring's radii grown by the ease clear the body by a little less than
/// it where the ring is not round.
const SAFETY: f64 = 0.0006;
/// Smoothing passes over the knots below the waist.
const SMOOTHING: usize = 3;
/// How far the flare's growth is from linear: it opens toward the hem.
const FLARE_POWER: f64 = 1.5;
/// How much a knot's ring may be grown to take in the last points of its slice; a factor above this is a body the
/// table can't enclose.
const MAX_GROW: f64 = 4.0;

/// How the skirt falls below the waist.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fall {
    /// It hangs from the widest point above: never narrower going down.
    Drape,
    /// It follows the body (a pencil skirt), the ease and the flare added.
    Pencil,
}

/// What the skirt does about the arms.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Arms {
    /// It keeps clear of them and the hands: `sleeve(t)` is how far a sleeve stands over the arm at `t` along it (0 the
    /// shoulder, 1 the wrist), and `margin` the gap left beyond.
    Clear {
        /// The sleeve's extra radius at the shoulder and at the wrist (and the hand), metres.
        sleeve: (f64, f64),
        /// The gap, metres.
        margin: f64,
    },
    /// It encloses them (a cape).
    Enclose,
}

/// What the table is cut from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HangSpec {
    /// The waist seam's ring, ease included: knot 0.
    pub waist: Ring,
    /// The lowest the skirt reaches anywhere, metres.
    pub bottom: f64,
    /// The ease over the body, metres.
    pub ease: f64,
    /// How far the outside of the cloth stands over the table's rings (the thickness and the folds), metres: what the
    /// arms must clear.
    pub outside: f64,
    /// How it falls.
    pub fall: Fall,
    /// How much wider it is at `bottom`, metres.
    pub flare: f64,
    /// The arms.
    pub arms: Arms,
}

/// One knot: a superellipse round `(centre.x, y, z)` from `back` to `front` through the body and `half` across.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Knot {
    y: f64,
    half: f64,
    front: f64,
    back: f64,
}

/// The rings of a coat's skirt by height.
#[derive(Clone, Debug)]
pub struct Table {
    waist: Ring,
    knots: Vec<Knot>,
    /// The arc length of the skirt's outline down to each knot, for spacing the rows.
    length: Vec<f64>,
}

/// The superellipse "norm" of `q` from `centre` with half-extents `radii` and `exponent`: 1 on the curve.
pub fn norm(q: DVec2, centre: DVec2, radii: DVec2, exponent: f64) -> f64 {
    let d = (q - centre).abs() / radii;
    (d.x.powf(exponent) + d.y.powf(exponent)).powf(1.0 / exponent)
}

impl Knot {
    fn centre(&self, x: f64) -> DVec2 {
        DVec2::new(x, f64::midpoint(self.front, self.back))
    }

    fn radii(&self) -> DVec2 {
        DVec2::new(self.half, (self.front - self.back) / 2.0)
    }

    /// The knot of the ring at `y` with centre `c` and `radii`.
    fn of(y: f64, c: DVec2, radii: DVec2) -> Self {
        Self { y, half: radii.x, front: c.y + radii.y, back: c.y - radii.y }
    }

    /// This knot grown about its centre (x at `x`) until every one of `points` is inside it with `ease` to spare.
    fn enclosing(self, x: f64, points: &[DVec2], ease: f64, exponent: f64) -> Result<Self, GarmentError> {
        let (c, inner) = (self.centre(x), (self.radii() - DVec2::splat(ease)).max(DVec2::splat(1e-4)));
        let grow = points.iter().map(|&q| norm(q, c, inner, exponent)).fold(1.0, f64::max);
        if grow > MAX_GROW {
            return Err(GarmentError::invalid(
                "skirt",
                format!("the body at height {:.3} m reaches {grow:.1} times past the skirt's ring", self.y),
            ));
        }
        Ok(if grow > 1.0 { Self::of(self.y, c, inner * grow + DVec2::splat(ease)) } else { self })
    }
}

/// The box round `points`, centred at `x` across: `(half, front, back)`.
fn bounds(points: &[DVec2], x: f64) -> Option<(f64, f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let half = points.iter().map(|q| (q.x - x).abs()).fold(0.0, f64::max);
    let front = points.iter().map(|q| q.y).fold(f64::NEG_INFINITY, f64::max);
    let back = points.iter().map(|q| q.y).fold(f64::INFINITY, f64::min);
    Some((half, front, back))
}

impl Table {
    /// The table for `spec` over `cloud`.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] if the bottom is not below the waist or the body can't be enclosed.
    #[allow(clippy::many_single_char_names, clippy::cast_precision_loss)] // the outline's textbook names; a small count
    pub fn new(cloud: &Cloud, spec: &HangSpec) -> Result<Self, GarmentError> {
        let w = spec.waist;
        let (x, n) = (w.centre.x, w.exponent);
        let top = w.centre.y;
        if spec.bottom.partial_cmp(&(top - 0.01)) != Some(std::cmp::Ordering::Less) {
            return Err(GarmentError::invalid(
                "hem",
                format!("the hem at {:.3} m must be below the waist seam at {top:.3} m", spec.bottom),
            ));
        }
        let first = Knot::of(top, DVec2::new(x, w.centre.z), w.radii);
        let spec = &HangSpec { ease: spec.ease + SAFETY, ..*spec };
        let band = ((top - spec.bottom) / (KNOTS - 1) as f64 / 2.0).max(MIN_BAND);
        // What each knot must enclose, and the body's own enclosing ring there.
        let mut needs: Vec<(Knot, Vec<DVec2>)> = Vec::with_capacity(KNOTS);
        for k in 0..KNOTS {
            let y = lerp(top, spec.bottom, fraction(k, KNOTS - 1));
            let mut points = cloud.body_at(y, band);
            if spec.arms == Arms::Enclose {
                points.extend(cloud.arms_at(y, band).map(|a| DVec2::new(a.p.x, a.p.z)));
            }
            let need = match bounds(&points, x) {
                Some((half, front, back)) if k > 0 => {
                    let knot = Knot { y, half: half + spec.ease, front: front + spec.ease, back: back - spec.ease };
                    knot.enclosing(x, &points, spec.ease, n)?
                }
                _ => Knot { y, ..first },
            };
            needs.push((need, points));
        }
        let mut knots: Vec<Knot> = Vec::with_capacity(KNOTS);
        for (k, (need, _)) in needs.iter().enumerate() {
            let knot = match (k, spec.fall) {
                (0, _) => first,
                (_, Fall::Pencil) => *need,
                (_, Fall::Drape) => {
                    let above = knots[k - 1];
                    Knot {
                        y: need.y,
                        half: need.half.max(above.half),
                        front: need.front.max(above.front),
                        back: need.back.min(above.back),
                    }
                }
            };
            knots.push(knot);
        }
        // Flare toward the hem; then the arms; then smooth and enclose again.
        for (k, knot) in knots.iter_mut().enumerate().skip(1) {
            let f = spec.flare * fraction(k, KNOTS - 1).powf(FLARE_POWER);
            knot.half += f;
            knot.front += f;
            knot.back -= f;
        }
        for _ in 0..SMOOTHING {
            let before = knots.clone();
            for k in 1..KNOTS - 1 {
                let mix =
                    |f: fn(&Knot) -> f64| 0.25 * f(&before[k - 1]) + 0.5 * f(&before[k]) + 0.25 * f(&before[k + 1]);
                knots[k] =
                    Knot { y: before[k].y, half: mix(|k| k.half), front: mix(|k| k.front), back: mix(|k| k.back) };
            }
        }
        for k in 1..KNOTS {
            let (need, points) = &needs[k];
            let mut knot = knots[k];
            knot.half = knot.half.max(need.half);
            knot.front = knot.front.max(need.front);
            knot.back = knot.back.min(need.back);
            if let Arms::Clear { sleeve, margin } = spec.arms {
                knot.half = clear_arms(cloud, &knot, x, n, band, sleeve, margin + spec.outside).max(need.half);
            }
            knots[k] = knot.enclosing(x, points, spec.ease, n)?;
        }
        let mut length = vec![0.0];
        for k in 1..KNOTS {
            let (a, b) = (knots[k - 1], knots[k]);
            let widening = (b.half - a.half).abs().max((b.front - a.front).abs()).max((b.back - a.back).abs());
            length.push(length[k - 1] + (a.y - b.y).hypot(widening));
        }
        Ok(Self { waist: w, knots, length })
    }

    /// The waist seam's height.
    pub fn top(&self) -> f64 {
        self.waist.centre.y
    }

    /// The lowest knot's height.
    pub fn bottom(&self) -> f64 {
        self.knots[KNOTS - 1].y
    }

    /// The knot interval holding `y` and how far down it `y` is.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small index
    fn interval(&self, y: f64) -> (usize, f64) {
        let s = ((self.top() - y) / (self.top() - self.bottom())).clamp(0.0, 1.0) * (KNOTS - 1) as f64;
        let k = (s.floor() as usize).min(KNOTS - 2);
        (k, s - k as f64)
    }

    /// The ring at height `y`, ease included: the waist seam's own ring at or above it.
    #[allow(clippy::many_single_char_names)]
    pub fn ring(&self, y: f64) -> Ring {
        if y >= self.top() {
            return self.waist;
        }
        let (k, f) = self.interval(y);
        let (a, b) = (self.knots[k], self.knots[k + 1]);
        let knot =
            Knot { y, half: lerp(a.half, b.half, f), front: lerp(a.front, b.front, f), back: lerp(a.back, b.back, f) };
        let c = knot.centre(self.waist.centre.x);
        Ring { centre: DVec3::new(c.x, y, c.y), axis: DVec3::Y, radii: knot.radii(), t: 0.0, ..self.waist }
    }

    /// The arc length of the outline from the waist down to `y`.
    pub fn length_at(&self, y: f64) -> f64 {
        let (k, f) = self.interval(y);
        lerp(self.length[k], self.length[k + 1], f)
    }

    /// The height at arc length `l` from the waist.
    pub fn height_at(&self, l: f64) -> f64 {
        let k = self.length.partition_point(|&x| x <= l).clamp(1, KNOTS - 1) - 1;
        let span = self.length[k + 1] - self.length[k];
        let f = if span > 0.0 { ((l - self.length[k]) / span).clamp(0.0, 1.0) } else { 0.0 };
        lerp(self.knots[k].y, self.knots[k + 1].y, f)
    }
}

/// The widest `knot` may be across and still keep its outside, `margin` further out and the sleeve's width more, clear
/// of every arm and hand point in its slice.
fn clear_arms(cloud: &Cloud, knot: &Knot, x: f64, n: f64, band: f64, sleeve: (f64, f64), margin: f64) -> f64 {
    let c = knot.centre(x);
    let mut half = knot.half;
    for a in cloud.arms_at(knot.y, band) {
        let out = margin + lerp(sleeve.0, sleeve.1, sstep(0.3, 1.0, a.t));
        let through = knot.radii().y + out;
        let q = DVec2::new(a.p.x, a.p.z) - c;
        let share = (q.y.abs() / through).powf(n);
        if share >= 1.0 {
            continue;
        }
        // The half width at which the ring, `out` further out, passes through the point.
        half = half.min(q.x.abs() / (1.0 - share).powf(1.0 / n) - out);
    }
    half
}
