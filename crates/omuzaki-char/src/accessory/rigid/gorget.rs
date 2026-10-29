//! The gorget (#2450): the filigree collar grown into a plate of metal that rises up the neck and spreads down onto the
//! collarbones, the surface its scrollwork and its centre stone sit on.
//!
//! **The plate** is one closed shell round the neck: for each direction `theta` round the neck (`0` at the front) a
//! profile runs from its lower edge up through the band to its upper edge, its inside [`Plate::ease`] off the body and
//! its outside a thickness further out, the two joined over a rounded lip at each edge. Up the neck the profile follows
//! the neck's sections. Down the chest it is marched over the body a fraction of a millimetre at a time: straight down
//! while the body falls away under it, pushed out to the body where the body comes out to meet it (the torso's own
//! sections, and the arms' roots and caps beside it), and never back in, so it lies on the collarbones and the top of
//! the chest the way a stiff plate would. Its lower edge is a crescent: deepest at the front, rising to the band at
//! `spread` either side, so the back of the neck carries only the band and the neck plate.
//!
//! The plate is built in the body's bind space from the published fit and then taken into the accessory's own frame,
//! so a user's `attach` moves it as one piece.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use omuzaki_char_api::{BodyFit, BodyRegion, RegionEnd, Section};
use omuzaki_core::glam::{DMat4, DVec3};

use crate::kit::curve::fraction;

/// How far down the profile is marched at a time, metres.
const STEP: f64 = 0.000_25;
/// How far down an arm its sections are taken, as a fraction of the arm, and how many.
const ARM_REACH: f64 = 0.2;
const ARM_SAMPLES: usize = 16;
/// The spacing of the sections up an arm root's cap, metres.
const CAP_STEP: f64 = 0.0025;
/// The most sections up a cap: 40 cm.
const CAP_SAMPLES: u32 = 160;
/// How far above the point where an arm's root meets it the plate's lower edge stops, beyond its thickness, metres.
const ARM_CLEAR: f64 = 0.002;
/// How near the plate's inside may come to an arm's root as the fit gives it, metres: the arm's own mesh lies inside
/// the fit, and the fit's sections are joined here by straight capsules.
const ARM_GAP: f64 = 0.001;
/// How far inside the neck or the torso a point of an arm is hidden in the body, metres.
const HIDDEN: f64 = 0.001;
/// The spacing of the body's sections kept for the hidden test, metres.
const TABLE_STEP: f64 = 0.001;
/// How far past its fit the body's own mesh may stand between the fit's sections, metres: the plate keeps this much
/// off the fit besides its ease.
const SLACK: f64 = 0.0015;
/// The step out over an arm, metres.
const OVER_STEP: f64 = 0.0005;
/// The farthest the plate is carried out over an arm, metres.
const OVER_REACH: f64 = 0.2;
/// How many rows either side a row carried out over an arm carries out with it.
const OVER: isize = 4;
/// How far what is worn may stand off the body and still be taken as lying on it, metres.
const WORN: f64 = 0.012;
/// Points round each rounded lip.
const LIP: usize = 3;
/// How far below the band the plate's ride over an arm eases back to the body.
const EASE_OFF: f64 = 0.004;
/// How the crescent's depth falls toward its ends: `cos^CRESCENT` of the angle from the front, scaled to `spread`.
const CRESCENT: f64 = 0.7;

/// What the plate's shape is made of, in metres and radians.
#[derive(Copy, Clone, Debug)]
pub(super) struct Shape {
    /// How far the plate rises above the band's centre.
    pub(super) height: f64,
    /// How far the plate runs down the chest at the front, along its surface below the band.
    pub(super) chest: f64,
    /// How far round the neck the chest piece reaches, either side of the front.
    pub(super) spread: f64,
    /// Half the band's own height: the plate reaches this far past the neck plate's top and the chest piece's foot.
    pub(super) band: f64,
    /// The gap between the body and the plate's inside.
    pub(super) ease: f64,
    /// The plate's thickness.
    pub(super) thickness: f64,
}

impl Shape {
    /// How far down the chest the plate runs at `theta`, below the band.
    pub(super) fn depth(&self, theta: f64) -> f64 {
        let off = wrap(theta).abs();
        if self.spread <= 0.0 || off >= self.spread {
            return 0.0;
        }
        self.chest * (FRAC_PI_2 * off / self.spread).cos().powf(CRESCENT)
    }

    /// The profile's `s` (up from the band's centre) at the plate's lower edge at `theta`.
    pub(super) fn foot(&self, theta: f64) -> f64 {
        -(self.depth(theta) + self.band)
    }

    /// The profile's `s` at the plate's upper edge.
    pub(super) fn top(&self) -> f64 {
        self.height + self.band
    }
}

/// `theta` wrapped into `-PI..=PI`.
fn wrap(theta: f64) -> f64 {
    (theta + PI).rem_euclid(TAU) - PI
}

/// A stretch of an arm between two of its sections, as a capsule whose radius runs from one end's to the other's.
#[derive(Copy, Clone, Debug)]
struct Capsule {
    a: DVec3,
    b: DVec3,
    radii: (f64, f64),
}

impl Capsule {
    /// How far `p` lies outside the capsule (negative inside), and the point of its surface nearest `p`.
    fn nearest(&self, p: DVec3) -> (f64, DVec3) {
        let ab = self.b - self.a;
        let length = ab.length_squared();
        let t = if length > 0.0 { ((p - self.a).dot(ab) / length).clamp(0.0, 1.0) } else { 0.0 };
        let axis = self.a + ab * t;
        let radius = self.radii.0 + (self.radii.1 - self.radii.0) * t;
        let out = (p - axis).try_normalize().unwrap_or(DVec3::Y);
        (p.distance(axis) - radius, axis + out * radius)
    }

    /// The lowest and highest the capsule reaches.
    fn heights(&self) -> (f64, f64) {
        let r = self.radii.0.max(self.radii.1);
        (self.a.y.min(self.b.y) - r, self.a.y.max(self.b.y) + r)
    }
}

/// The body under the plate, in bind space: the neck and the torso as upright ellipses by height, and the arms' roots
/// as capsules from a fifth of the way down each arm, through its root and up its cap.
struct Body {
    neck: (Section, Section),
    torso: (Section, Section),
    fit: std::sync::Arc<dyn BodyFit>,
    /// The envelopes of what is worn over the body, innermost first.
    worn: Vec<std::sync::Arc<dyn BodyFit>>,
    /// The body's own sections every [`TABLE_STEP`] up from the lowest height the plate can reach, for the arms'
    /// hidden test.
    table: (f64, Vec<Section>),
    arms: Vec<Capsule>,
}

impl Body {
    fn new(fits: &[std::sync::Arc<dyn BodyFit>]) -> Option<Self> {
        let (fit, worn) = fits.split_first()?;
        let (fit, worn) = (fit.clone(), worn.to_vec());
        let ends = |region| Some((fit.section(region, 0.0)?, fit.section(region, 1.0)?));
        let (neck, torso) = (ends(BodyRegion::Neck)?, ends(BodyRegion::Torso)?);
        let mut arms = Vec::new();
        for arm in [BodyRegion::ArmL, BodyRegion::ArmR] {
            let mut sections = Vec::new();
            for k in (0..=ARM_SAMPLES).rev() {
                #[allow(clippy::cast_possible_truncation)] // a fraction
                sections.extend(fit.section(arm, (ARM_REACH * fraction(k, ARM_SAMPLES)) as f32));
            }
            // The root's cap, as far as it runs: on a short-necked body it curls in against the neck.
            for k in 1..CAP_SAMPLES {
                #[allow(clippy::cast_possible_truncation)] // a few centimetres
                let Some(cap) = fit.cap(arm, RegionEnd::Start, (CAP_STEP * f64::from(k)) as f32) else { break };
                sections.push(cap);
            }
            let end = |s: &Section| (DVec3::from(s.centre), f64::from(s.radii.x.max(s.radii.y)));
            arms.extend(sections.windows(2).map(|pair| {
                let ((a, ra), (b, rb)) = (end(&pair[0]), end(&pair[1]));
                Capsule { a, b, radii: (ra, rb) }
            }));
        }
        let mut body = Self { neck, torso, fit, worn, arms, table: (0.0, Vec::new()) };
        let (low, high) = (f64::from(body.torso.0.centre.y), f64::from(body.neck.1.centre.y));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few hundred steps
        let steps = ((high - low) / TABLE_STEP).ceil() as usize + 1;
        #[allow(clippy::cast_precision_loss)] // a few hundred steps
        let table = (0..steps).filter_map(|k| body.section(body.fit.as_ref(), low + TABLE_STEP * k as f64)).collect();
        body.table = (low, table);
        Some(body)
    }

    /// How far `p` lies outside the arms' roots (negative inside one), looking no farther than `within`. A capsule whose
    /// surface nearest `p` lies inside the neck or the torso is hidden in the body there and doesn't count.
    fn arm_gap(&self, p: DVec3, within: f64) -> f64 {
        self.arms
            .iter()
            .filter(|c| {
                let (low, high) = c.heights();
                p.y >= low - within && p.y <= high + within
            })
            .filter_map(|c| {
                let (gap, nearest) = c.nearest(p);
                (!self.hidden(nearest)).then_some(gap)
            })
            .fold(within, f64::min)
    }

    /// Whether `p` lies inside the body's neck or torso, by more than [`HIDDEN`].
    fn hidden(&self, p: DVec3) -> bool {
        let (low, table) = &self.table;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped into the table
        let at = (((p.y - low) / TABLE_STEP).round().max(0.0) as usize).min(table.len().saturating_sub(1));
        table.get(at).is_some_and(|section| {
            let across = f64::from(section.radii.x) - HIDDEN;
            let through = f64::from(section.radii.y) - HIDDEN;
            let off = (p.x - f64::from(section.centre.x), p.z - f64::from(section.centre.z));
            across > 0.0 && through > 0.0 && (off.0 / across).powi(2) + (off.1 / through).powi(2) < 1.0
        })
    }

    /// Where the plate lying `r` out from `origin` along `d` at height `y` comes out past an arm's root it would pass
    /// within `gap` of: the distance out at which it clears the arm by `gap`.
    fn over_arm(&self, origin: DVec3, d: DVec3, y: f64, r: f64, gap: f64) -> Option<f64> {
        let at = |r: f64| DVec3::new(origin.x, y, origin.z) + d * r;
        if self.arm_gap(at(r), gap) >= gap {
            return None;
        }
        let mut out = r;
        while out < r + OVER_REACH && self.arm_gap(at(out), gap) < gap {
            out += OVER_STEP;
        }
        Some(out)
    }

    /// The section of the neck or the torso at height `y`.
    fn section(&self, fit: &dyn BodyFit, y: f64) -> Option<Section> {
        let (region, (bottom, top)) = if y >= f64::from(self.torso.1.centre.y) {
            (BodyRegion::Neck, self.neck)
        } else {
            (BodyRegion::Torso, self.torso)
        };
        let (y0, y1) = (f64::from(bottom.centre.y), f64::from(top.centre.y));
        let t = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.0 };
        #[allow(clippy::cast_possible_truncation)] // a fraction
        fit.section(region, t as f32)
    }

    /// How far out from `origin` (on the neck's axis), along the level direction `d`, the neck or the torso reaches at
    /// height `y`: the body's own section, or what is worn over it where that lies on the body (within [`WORN`] of
    /// it). An envelope standing farther off (round a strap, over a shoulder) is air there, not cloth.
    fn reach(&self, origin: DVec3, d: DVec3, y: f64) -> f64 {
        let body = self.section(self.fit.as_ref(), y).map_or(0.0, |s| ray(&s, origin, d));
        self.worn
            .iter()
            .filter_map(|fit| self.section(fit.as_ref(), y).map(|s| ray(&s, origin, d)))
            .filter(|&r| r <= body + WORN)
            .fold(body, f64::max)
    }
}

/// How far from `origin` along the level direction `d` the ellipse of `section` lies.
fn ray(section: &Section, origin: DVec3, d: DVec3) -> f64 {
    let (a, b) = (f64::from(section.radii.x).max(1e-4), f64::from(section.radii.y).max(1e-4));
    let (ox, oz) = (origin.x - f64::from(section.centre.x), origin.z - f64::from(section.centre.z));
    let qa = (d.x / a).powi(2) + (d.z / b).powi(2);
    let qb = 2.0 * (ox * d.x / (a * a) + oz * d.z / (b * b));
    let qc = (ox / a).powi(2) + (oz / b).powi(2) - 1.0;
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 { 0.0 } else { ((-qb + disc.sqrt()) / (2.0 * qa)).max(0.0) }
}

/// The plate's inside surface over a regular grid of `theta` round the neck and `s` along each profile, in the
/// accessory's frame, with its outward normals.
pub(super) struct Plate {
    shape: Shape,
    /// Directions round the neck, `0..rows` (the grid wraps).
    rows: usize,
    /// Samples along each profile.
    cols: usize,
    /// The `s` of the first sample, and the spacing.
    s0: f64,
    ds: f64,
    inside: Vec<DVec3>,
    normals: Vec<DVec3>,
    /// The `s` of the lower edge in each direction: the crescent's, or higher where an arm's root would meet it.
    feet: Vec<f64>,
}

impl Plate {
    /// The plate of `shape` round the neck section `neck` of `fit` (its centre at the band, `+Y` up the neck, `+Z` to
    /// the front, `to_local` taking bind space into the accessory's frame); `rows` directions round it. `None` when the
    /// fit has no neck or torso.
    #[allow(clippy::cast_precision_loss)] // a few hundred samples
    pub(super) fn new(
        fits: &[std::sync::Arc<dyn BodyFit>],
        neck: &DMat4,
        to_local: &DMat4,
        shape: Shape,
        rows: usize,
    ) -> Option<Self> {
        let body = Body::new(fits)?;
        let origin = neck.transform_point3(DVec3::ZERO);
        let (across, front) = (neck.transform_vector3(DVec3::X), neck.transform_vector3(DVec3::Z));
        let (s0, top) = (-(shape.chest + shape.band), shape.top());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few hundred samples
        let cols = (((top - s0) / (4.0 * STEP)).ceil() as usize).max(8) + 1;
        let ds = (top - s0) / (cols - 1) as f64;
        // Each row's profile on the shared grid of `s`: its direction, and `(r, y)` at each sample.
        let mut dirs = Vec::with_capacity(rows);
        let mut grid: Vec<Vec<(f64, f64)>> = Vec::with_capacity(rows);
        let mut feet = Vec::with_capacity(rows);
        for i in 0..rows {
            let theta = TAU * fraction(i, rows);
            let d = DVec3::new(
                across.x * theta.sin() + front.x * theta.cos(),
                0.0,
                across.z * theta.sin() + front.z * theta.cos(),
            )
            .normalize_or_zero();
            let (profile, stop) = profile(&body, origin, d, shape, s0, top);
            let foot = stop.map_or(f64::MIN, |stop| stop + shape.thickness + ARM_CLEAR);
            feet.push(shape.foot(theta).max(foot).min(-shape.band));
            dirs.push(d);
            grid.push((0..cols).map(|k| sample(&profile, s0 + ds * k as f64)).collect());
        }
        // Up the neck and through the band, a row carried out over an arm carries its neighbours out with it and eases
        // back to the neck, so no strip between a row over the arm and one beside it cuts through the arm.
        // Below the band it eases off over [`EASE_OFF`], so the plate never steps in where it would turn its outside down.
        let carried = |s: f64| {
            let t = ((s + shape.band + shape.thickness + EASE_OFF) / EASE_OFF).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        for k in (0..cols).filter(|&k| carried(s0 + ds * k as f64) > 0.0) {
            let weight = carried(s0 + ds * k as f64);
            let r: Vec<f64> = grid.iter().map(|row| row[k].0).collect();
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)] // a few hundred rows
            let at = |values: &[f64], i: usize, j: isize| values[(i as isize + j).rem_euclid(rows as isize) as usize];
            let wide: Vec<f64> =
                (0..rows).map(|i| (-OVER..=OVER).map(|j| at(&r, i, j)).fold(f64::MIN, f64::max)).collect();
            for (i, row) in grid.iter_mut().enumerate() {
                // A tent kernel, so the plate eases out and back in.
                let (sum, total) = (-OVER..=OVER).fold((0.0, 0.0), |(sum, total), j| {
                    #[allow(clippy::cast_precision_loss)] // a few rows
                    let w = (OVER + 1 - j.abs()) as f64;
                    (sum + w * at(&wide, i, j), total + w)
                });
                row[k].0 = r[i].max(r[i] + (sum / total - r[i]) * weight);
            }
        }
        let mut inside = Vec::with_capacity(rows * cols);
        let mut normals = Vec::with_capacity(rows * cols);
        for (d, row) in dirs.iter().zip(&grid) {
            for k in 0..cols {
                let (r, y) = row[k];
                let (r_lo, y_lo) = row[k.saturating_sub(1)];
                let (r_hi, y_hi) = row[(k + 1).min(cols - 1)];
                let tangent = (*d * (r_hi - r_lo) + DVec3::Y * (y_hi - y_lo)).normalize_or_zero();
                let normal = (*d * tangent.y - DVec3::Y * tangent.dot(*d)).normalize_or_zero();
                let point = DVec3::new(origin.x, y, origin.z) + *d * r;
                inside.push(to_local.transform_point3(point));
                normals.push(to_local.transform_vector3(normal).normalize_or_zero());
            }
        }
        // A row cut short by an arm cuts its neighbours as short, so no strip between them reaches over the arm.
        let feet = (0..rows).map(|i| feet[(i + rows - 1) % rows].max(feet[i]).max(feet[(i + 1) % rows])).collect();
        Some(Self { shape, rows, cols, s0, ds, inside, normals, feet })
    }

    /// The plate's shape.
    pub(super) fn shape(&self) -> &Shape {
        &self.shape
    }

    /// The point of the plate's inside at `theta` round the neck and `s` along its profile, and the outward normal there.
    #[allow(clippy::cast_precision_loss)] // a few hundred samples
    pub(super) fn at(&self, theta: f64, s: f64) -> (DVec3, DVec3) {
        let fi = theta.rem_euclid(TAU) / TAU * self.rows as f64;
        let fk = ((s - self.s0) / self.ds).clamp(0.0, (self.cols - 1) as f64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in range
        let (i0, k0) = (fi.floor() as usize % self.rows, (fk.floor() as usize).min(self.cols - 2));
        let (i1, k1) = ((i0 + 1) % self.rows, k0 + 1);
        let (wi, wk) = (fi - fi.floor(), fk - k0 as f64);
        let mix = |v: &[DVec3]| {
            let a = v[i0 * self.cols + k0].lerp(v[i0 * self.cols + k1], wk);
            let b = v[i1 * self.cols + k0].lerp(v[i1 * self.cols + k1], wk);
            a.lerp(b, wi)
        };
        (mix(&self.inside), mix(&self.normals).normalize_or_zero())
    }

    /// The `s` of the plate's lower edge at `theta`.
    #[allow(clippy::cast_precision_loss)] // a few hundred rows
    pub(super) fn foot(&self, theta: f64) -> f64 {
        let fi = theta.rem_euclid(TAU) / TAU * self.rows as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in range
        let i0 = fi.floor() as usize % self.rows;
        let w = fi - fi.floor();
        self.feet[i0] * (1.0 - w) + self.feet[(i0 + 1) % self.rows] * w
    }

    /// How far the plate runs down the chest at `theta`, below the band.
    pub(super) fn depth(&self, theta: f64) -> f64 {
        -self.foot(theta) - self.shape.band
    }

    /// The point on the plate's outside at `theta` and `s`, `lift` further out, and the outward normal there.
    pub(super) fn outside(&self, theta: f64, s: f64, lift: f64) -> (DVec3, DVec3) {
        let (p, n) = self.at(theta, s);
        (p + n * (self.shape.thickness + lift), n)
    }

    /// The closed shell of the plate as a grid: rows round the neck (`segments` of them, the last repeating the first),
    /// each a loop up its outside from the foot to the top, over the upper lip, down its inside and back under the lower
    /// lip; and, for each point, a point inside the shell there.
    pub(super) fn shell(&self, segments: usize, along: usize) -> (Vec<Vec<DVec3>>, Vec<Vec<DVec3>>) {
        let thick = self.shape.thickness;
        let mut points = Vec::with_capacity(segments + 1);
        let mut middles = Vec::with_capacity(segments + 1);
        for i in 0..=segments {
            let theta = TAU * fraction(i % segments, segments);
            let (foot, top) = (self.foot(theta), self.shape.top());
            let at = |k: usize| self.at(theta, foot + (top - foot) * fraction(k, along));
            let mut row = Vec::new();
            let mut middle = Vec::new();
            let mut push = |p: DVec3, m: DVec3| {
                row.push(p);
                middle.push(m);
            };
            for k in 0..=along {
                let (p, n) = at(k);
                push(p + n * thick, p + n * (0.5 * thick));
            }
            let lip = |k: usize, up: bool| {
                let (p, n) = at(k);
                let (q, _) = at(if up { k - 1 } else { k + 1 });
                let tangent = (p - q).normalize_or_zero();
                let centre = p + n * (0.5 * thick);
                (0..LIP)
                    .map(|m| {
                        let a = PI * fraction(m + 1, LIP + 1);
                        (centre + (n * a.cos() + tangent * a.sin()) * (0.5 * thick), centre)
                    })
                    .collect::<Vec<_>>()
            };
            for (p, m) in lip(along, true) {
                push(p, m);
            }
            for k in (0..=along).rev() {
                let (p, n) = at(k);
                push(p, p + n * (0.5 * thick));
            }
            for (p, m) in lip(0, false).into_iter().rev() {
                push(p, m);
            }
            let (first, first_middle) = (row[0], middle[0]);
            row.push(first);
            middle.push(first_middle);
            points.push(row);
            middles.push(middle);
        }
        (points, middles)
    }
}

/// The profile at level direction `d` from `origin`: `(s, r, y)` from `s0` up to `top`, `r` out from the neck's axis.
/// Up the neck and through the band it follows the neck and rides out over an arm's root where the shoulder rises into
/// the neck; below the band it follows the torso, never back in, and stops short of an arm: the `s` where an arm first
/// comes within the plate's thickness and [`ARM_GAP`] of it, if one does, is returned with it.
fn profile(
    body: &Body,
    origin: DVec3,
    d: DVec3,
    shape: Shape,
    s0: f64,
    top: f64,
) -> (Vec<(f64, f64, f64)>, Option<f64>) {
    let gap = shape.thickness + ARM_GAP;
    let over = |r: f64, y: f64| body.over_arm(origin, d, y, r, gap).map_or(r, |out| r.max(out));
    // Where the plate rode out over a shoulder to clear it by `gap`, it carries on down past it: only an arm that comes
    // closer than that stops it.
    let meets = shape.thickness + 0.5 * ARM_GAP;
    let mut up = Vec::new();
    let mut s = 0.0;
    while s <= top + STEP {
        let y = origin.y + s;
        up.push((s, over(body.reach(origin, d, y) + shape.ease + SLACK, y), y));
        s += STEP;
    }
    let mut down = Vec::new();
    let (mut s, mut r, mut y) = (0.0, up[0].1, origin.y);
    let mut stop = None;
    while s > s0 - STEP {
        y -= STEP;
        let mut next = r.max(body.reach(origin, d, y) + shape.ease + SLACK);
        if s > -(shape.band + shape.thickness) {
            next = over(next, y);
        } else if stop.is_none() && body.arm_gap(DVec3::new(origin.x, y, origin.z) + d * next, gap) < meets {
            stop = Some(s);
        }
        s -= STEP.hypot(next - r);
        r = next;
        down.push((s, r, y));
    }
    down.reverse();
    down.extend(up);
    (down, stop)
}

/// The profile's `(r, y)` at `s`, by linear interpolation (held at its ends).
fn sample(profile: &[(f64, f64, f64)], s: f64) -> (f64, f64) {
    let next = profile.partition_point(|sample| sample.0 < s);
    if next == 0 {
        return (profile[0].1, profile[0].2);
    }
    let Some(&above) = profile.get(next) else {
        let last = profile[profile.len() - 1];
        return (last.1, last.2);
    };
    let below = profile[next - 1];
    let w = if above.0 > below.0 { (s - below.0) / (above.0 - below.0) } else { 0.0 };
    (below.1 + (above.1 - below.1) * w, below.2 + (above.2 - below.2) * w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crescent_is_deepest_at_the_front_and_meets_the_band_at_its_ends() {
        let shape = Shape { height: 0.02, chest: 0.04, spread: 1.0, band: 0.001, ease: 0.002, thickness: 0.001 };
        assert!((shape.depth(0.0) - 0.04).abs() < 1e-12);
        assert!(shape.depth(0.5) < 0.04 && shape.depth(0.5) > 0.0);
        assert_eq!(shape.depth(1.0), 0.0);
        assert_eq!(shape.depth(PI), 0.0, "the back carries no chest piece");
        assert!((shape.depth(-0.5) - shape.depth(0.5)).abs() < 1e-12, "symmetric");
        assert!((shape.foot(PI) + 0.001).abs() < 1e-12);
    }

    #[test]
    fn a_profile_is_sampled_by_its_length() {
        let profile = vec![(-1.0, 2.0, 0.0), (0.0, 1.0, 1.0), (1.0, 1.0, 2.0)];
        assert_eq!(sample(&profile, -0.5), (1.5, 0.5));
        assert_eq!(sample(&profile, 0.5), (1.0, 1.5));
        assert_eq!(sample(&profile, -5.0), (2.0, 0.0));
        assert_eq!(sample(&profile, 5.0), (1.0, 2.0));
    }
}
