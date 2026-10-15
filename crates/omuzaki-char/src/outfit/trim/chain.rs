//! Draped chains: links hanging as a catenary between two points on a garment (the robe's drooping chain).
//!
//! The chain hangs under gravity (-y in bind space) in the vertical plane through its two anchors, as the catenary of
//! its length: the chord times `1 + slack`. Where that curve would pass into the garment it rests on it instead, pushed
//! out along the section's radius. Each link is a torus stretched along the chain, every other one turned a quarter
//! turn about the chain, as real links interlock.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Finish, TrimError, checked, feature, position};
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::skin::{MAX_INFLUENCES, limit_influences, normalise};
use crate::outfit::loft::Surface;

/// The most slack a chain may have: its length is at most this many times the chord, plus one.
pub const MAX_SLACK: f64 = 2.0;
/// The most links a chain may have.
pub const MAX_LINKS: u32 = 400;
/// How far apart neighbouring links sit, as a share of a link's length: they overlap to interlock.
pub const PITCH: f64 = 0.78;
/// The wire of a link, as a share of its length, unless set.
pub const WIRE: f64 = 0.16;
/// A link's width, as a share of its length.
const LINK_WIDTH: f64 = 0.62;
/// Segments round a link.
const AROUND: usize = 12;
/// Segments round its wire.
const WIRE_ROUND: usize = 6;
/// Bisections that solve the catenary's parameter.
const SOLVE_STEPS: usize = 200;

/// A chain hanging between two points on the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    /// Where it starts, `[t, phi]` on the surface.
    pub from: [f64; 2],
    /// Where it ends.
    pub to: [f64; 2],
    /// How much longer than the straight line between its ends it is, as a share of that line (0 hangs it taut), up
    /// to [`MAX_SLACK`].
    pub slack: f64,
    /// The length of one link, metres.
    pub link_size: f64,
    /// How many links; `None` fits as many as its length takes at [`PITCH`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_count: Option<u32>,
    /// The thickness of a link's wire, metres; `None` is [`WIRE`] of the link size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<f64>,
    /// What it is made of (gold unless set).
    #[serde(default)]
    pub finish: Finish,
}

impl Validate for Chain {
    fn check(&self, c: &mut Check) {
        position(c, "from", self.from);
        position(c, "to", self.to);
        c.range("slack", self.slack, 0.0, MAX_SLACK);
        feature(c, "link_size", self.link_size);
        if let Some(count) = self.link_count
            && !(1..=MAX_LINKS).contains(&count)
        {
            c.fail("link_count", format!("{count}: a chain has 1 to {MAX_LINKS} links"));
        }
        if let Some(wire) = self.wire {
            c.range("wire", wire, super::MIN_FEATURE, self.link_size * 0.4);
        }
        c.nested("finish", &self.finish);
    }
}

/// The catenary of `length` hanging from `a` to `b` under gravity (-y), as `n + 1` points evenly spaced along it.
/// A length no longer than the chord gives the straight line.
///
/// # Errors
/// [`TrimError::Invalid`] for ends that are one above the other (a hanging chain then folds on itself), non-finite
/// input, or `n` of 0.
#[allow(clippy::many_single_char_names)] // the catenary's textbook names: ends a and b, parameter c, spans h and v
pub fn catenary(a: DVec3, b: DVec3, length: f64, n: usize) -> Result<Vec<DVec3>, TrimError> {
    if !(a.is_finite() && b.is_finite() && length.is_finite()) || n == 0 {
        return Err(TrimError::invalid("chain", "the ends, the length and the link count must be finite and positive"));
    }
    let across = DVec3::new(b.x - a.x, 0.0, b.z - a.z);
    let h = across.length();
    let v = b.y - a.y;
    let chord = a.distance(b);
    if length <= chord * (1.0 + 1e-9) {
        return Ok((0..=n).map(|k| a.lerp(b, fraction(k, n))).collect());
    }
    if h < super::MIN_FEATURE {
        return Err(TrimError::invalid("chain", "its ends are one above the other: a hanging chain needs them apart"));
    }
    let dir = across / h;
    // 2 c sinh(h / 2c) = sqrt(L^2 - v^2): the left side falls as c grows, so bisect c on a log scale.
    let target = (length * length - v * v).sqrt();
    let (mut lo, mut hi) = ((h / 1400.0).ln(), (h * 1e6).ln());
    for _ in 0..SOLVE_STEPS {
        let mid = f64::midpoint(lo, hi);
        let c = mid.exp();
        if 2.0 * c * (h / (2.0 * c)).sinh() > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let c = f64::midpoint(lo, hi).exp();
    // The lowest point sits at x0 along the ground line from a; y(x) = c cosh((x - x0) / c) + k.
    let x0 = h / 2.0 - c * (v / length).atanh();
    let s0 = (-x0 / c).sinh();
    let y = |x: f64| c * ((x - x0) / c).cosh() - c * (x0 / c).cosh();
    let total = c * ((h - x0) / c).sinh() - c * s0;
    Ok((0..=n)
        .map(|k| {
            let s = total * fraction(k, n);
            let x = x0 + c * (s / c + s0).asinh();
            a + dir * x + DVec3::Y * y(x)
        })
        .collect())
}

/// How far the lowest point of `points` sags below the straight line between its ends, measured straight down.
pub fn sag(points: &[DVec3]) -> f64 {
    let (Some(&a), Some(&b)) = (points.first(), points.last()) else {
        return 0.0;
    };
    let ground = DVec3::new(b.x - a.x, 0.0, b.z - a.z);
    let h = ground.length_squared();
    points
        .iter()
        .map(|p| {
            let s = if h > 0.0 { (DVec3::new(p.x - a.x, 0.0, p.z - a.z).dot(ground) / h).clamp(0.0, 1.0) } else { 0.0 };
            a.y + (b.y - a.y) * s - p.y
        })
        .fold(0.0, f64::max)
}

/// The link centres of the chain `spec` describes on `surface`: the catenary between its anchors, rested on the
/// surface where it would pass into it. Returns the points (one more than the links) and the clearance kept.
///
/// # Errors
/// As [`catenary`]; the surface's errors.
pub fn drape(surface: &Surface, spec: &Chain) -> Result<Vec<DVec3>, TrimError> {
    checked(spec)?;
    let clearance = clearance(spec);
    let end = |at: [f64; 2]| -> Result<DVec3, TrimError> {
        let frame = surface.at(at[0], at[1])?;
        Ok(frame.point + frame.normal * clearance)
    };
    let (a, b) = (end(spec.from)?, end(spec.to)?);
    let length = a.distance(b) * (1.0 + spec.slack);
    let count = links(spec, length)?;
    let mut points = catenary(a, b, length, count)?;
    for p in &mut points {
        rest_on(surface, p, clearance)?;
    }
    Ok(points)
}

/// How far a link's centre keeps off the surface: half its width and its wire.
fn clearance(spec: &Chain) -> f64 {
    0.5 * LINK_WIDTH * spec.link_size + wire(spec)
}

fn wire(spec: &Chain) -> f64 {
    spec.wire.unwrap_or(WIRE * spec.link_size)
}

/// The number of links of a chain of `length`.
fn links(spec: &Chain, length: f64) -> Result<usize, TrimError> {
    let count = match spec.link_count {
        Some(count) => count,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a positive ratio, clamped
        None => (length / (PITCH * spec.link_size)).round().clamp(1.0, f64::from(MAX_LINKS)) as u32,
    };
    usize::try_from(count).map_err(|_| TrimError::invalid("link_count", "too many links"))
}

/// Pushes `p` out along its section's radius until it is `clearance` outside the surface's plain ring.
fn rest_on(surface: &Surface, p: &mut DVec3, clearance: f64) -> Result<(), TrimError> {
    let Some(at) = surface.project(*p)? else {
        return Ok(());
    };
    if at.outside >= clearance {
        return Ok(());
    }
    let ring = surface.ring(at.t)?;
    let d = *p - ring.centre;
    if let Some(radial) = (d - ring.axis * d.dot(ring.axis)).try_normalize() {
        *p += radial * (clearance - at.outside);
    }
    Ok(())
}

/// The skin `share` of the way from `a` to `b`.
fn mix(a: &[(JointRef, f32)], b: &[(JointRef, f32)], share: f64) -> Vec<(JointRef, f32)> {
    #[allow(clippy::cast_possible_truncation)] // a share in 0..=1
    let s = share as f32;
    let mut blend: Vec<(JointRef, f32)> =
        a.iter().map(|(j, w)| (j.clone(), w * (1.0 - s))).chain(b.iter().map(|(j, w)| (j.clone(), w * s))).collect();
    limit_influences(&mut blend, MAX_INFLUENCES);
    normalise(&mut blend);
    blend
}

/// The chain `spec` describes on `surface`: one closed torus per link, skinned from the start's surface to the end's.
///
/// # Errors
/// As [`drape`]; the kit's errors for a degenerate link.
pub fn build(surface: &Surface, spec: &Chain) -> Result<MeshBuilder, TrimError> {
    let points = drape(surface, spec)?;
    let (from, to) = (surface.weights().at(spec.from[0]), surface.weights().at(spec.to[0]));
    let last = points.len() - 1;
    let r = wire(spec) / 2.0;
    let mut builder = MeshBuilder::new();
    for k in 0..last {
        let (a, b) = (points[k], points[k + 1]);
        let centre = a.lerp(b, 0.5);
        let along = (b - a).try_normalize().ok_or_else(|| TrimError::invalid("chain", "two links coincide"))?;
        // Every other link a quarter turn round the chain.
        let first = DVec3::Y.cross(along).try_normalize().unwrap_or_else(|| along.any_orthonormal_vector());
        let across = if k % 2 == 0 { first } else { along.cross(first) };
        let length = (a.distance(b) / PITCH).max(spec.link_size * 0.5);
        let radii = (length / 2.0 - r, 0.5 * LINK_WIDTH * spec.link_size - r);
        let skin = mix(&from, &to, f64::midpoint(fraction(k, last), fraction(k + 1, last)));
        torus(&mut builder, centre, (along, across), radii, r, skin)?;
    }
    Ok(builder)
}

/// Adds a closed torus to `builder`: its centre line the ellipse of semi-axes `radii` along the unit vectors `axes`
/// round `centre`, its tube of radius `r`, skinned with `skin`. A link, a bail, a clasp's ring.
///
/// # Errors
/// The kit's errors for a degenerate shape.
#[allow(clippy::many_single_char_names)] // axes a and b, radii, tube radius r
pub fn torus(
    builder: &mut MeshBuilder,
    centre: DVec3,
    (a, b): (DVec3, DVec3),
    (ra, rb): (f64, f64),
    r: f64,
    skin: Vec<(JointRef, f32)>,
) -> Result<(), TrimError> {
    let normal = a.cross(b).normalize_or_zero();
    let ring = |i: usize| {
        let u = TAU * fraction(i % AROUND, AROUND);
        // The point on the centre line, and the ellipse's outward normal there (its gradient).
        let out = (a * (u.cos() * rb) + b * (u.sin() * ra)).normalize_or_zero();
        (centre + a * (ra * u.cos()) + b * (rb * u.sin()), out)
    };
    let grid = Grid::from_fn(AROUND + 1, WIRE_ROUND + 1, |i, j| {
        let (p, out) = ring(i);
        let w = TAU * fraction(j % WIRE_ROUND, WIRE_ROUND);
        p + (out * w.cos() + normal * w.sin()) * r
    })?;
    let inside = |i: usize, _: usize| ring(i).0;
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Blend(skin),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}
