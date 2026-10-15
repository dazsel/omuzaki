//! Draped chains: links hanging between two points on a garment, as the robe's chain between its clasps (#189's
//! reference, C).
//!
//! The chain's centre line is the prototype's parabola, not a catenary (the Director's ruling on #189): the straight
//! line between its ends, dropped by `sag` and bowed out by `bow`, both weighted by `1 - (2t - 1)^2`. It is sampled at
//! [`CURVE_SAMPLES`] + 1 points, and the links sit at even distances along it, every other one a quarter turn round the
//! chain, as real links interlock. Where a link would sink into the garment it rests on it instead, pushed out along
//! its section's radius. A chain can carry the small pearled cross at its middle ([`super::clasp::pendant_cross`]).

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::clasp::pendant_cross;
use super::{Basis, Finish, TrimError, checked, feature, position, torus};
use crate::kit::curve::fraction;
use crate::kit::grid::MeshBuilder;
use crate::kit::skin::{MAX_INFLUENCES, limit_influences, normalise};
use crate::outfit::loft::Surface;

/// The prototype's sag and bow, metres.
pub const SAG: (f64, f64) = (0.026, 0.004);
/// The most a chain may sag, metres.
pub const MAX_SAG: f64 = 0.5;
/// The prototype's link: ring radius and wire radius, metres.
pub const LINK: (f64, f64) = (0.0024, 0.00105);
/// The link's segments round its wire and round its ring.
pub const LINK_SEGMENTS: (usize, usize) = (4, 10);
/// How much longer than wide a link is, along the chain.
pub const LINK_STRETCH: f64 = 1.45;
/// From one link's centre to the next, metres, for the prototype's link.
pub const PITCH: f64 = 0.0052;
/// The fewest links a chain has unless told otherwise.
pub const MIN_LINKS: u32 = 8;
/// The most links a chain may have.
pub const MAX_LINKS: u32 = 400;
/// Pieces the centre line is measured in.
pub const CURVE_SAMPLES: usize = 200;
/// How far either side of a link its direction is taken, metres, for the prototype's link.
const TANGENT_REACH: f64 = 0.002;

/// A chain hanging between two points on the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    /// Where it starts, `[t, phi]` on the surface.
    pub from: [f64; 2],
    /// Where it ends.
    pub to: [f64; 2],
    /// How far its middle drops below the straight line between its ends, metres, 0 to [`MAX_SAG`].
    #[serde(default = "sag")]
    pub sag: f64,
    /// How far its middle bows out from the garment, metres.
    #[serde(default = "bow")]
    pub bow: f64,
    /// A link's ring radius, metres (the prototype's [`LINK`]); the wire, the pitch and the pendant are in proportion.
    #[serde(default = "link")]
    pub link_size: f64,
    /// How many links; `None` fits one every pitch, at least [`MIN_LINKS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_count: Option<u32>,
    /// Hang the pearled cross at its middle.
    #[serde(default)]
    pub pendant: bool,
    /// What it is made of (gold unless set).
    #[serde(default)]
    pub finish: Finish,
}

fn sag() -> f64 {
    SAG.0
}

fn bow() -> f64 {
    SAG.1
}

fn link() -> f64 {
    LINK.0
}

impl Validate for Chain {
    fn check(&self, c: &mut Check) {
        position(c, "from", self.from);
        position(c, "to", self.to);
        c.range("sag", self.sag, 0.0, MAX_SAG);
        c.range("bow", self.bow, -super::MAX_FEATURE, super::MAX_FEATURE);
        feature(c, "link_size", self.link_size);
        if let Some(count) = self.link_count
            && !(1..=MAX_LINKS).contains(&count)
        {
            c.fail("link_count", format!("{count}: a chain has 1 to {MAX_LINKS} links"));
        }
        c.nested("finish", &self.finish);
    }
}

/// The shape a chain hangs in: its ends, which way is up and which way is out from the garment, and how far it drops
/// and bows.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hang {
    /// The start.
    pub from: DVec3,
    /// The end.
    pub to: DVec3,
    /// Up (unit): the chain sags against it.
    pub up: DVec3,
    /// Out from the garment (unit): the chain bows along it.
    pub out: DVec3,
    /// The drop at the middle, metres.
    pub sag: f64,
    /// The bow at the middle, metres.
    pub bow: f64,
}

impl Hang {
    /// The centre line at `t` from 0 to 1 (the prototype's curve).
    pub fn at(&self, t: f64) -> DVec3 {
        let w = 1.0 - (2.0 * t - 1.0).powi(2);
        self.from.lerp(self.to, t) - self.up * (self.sag * w) + self.out * (self.bow * w)
    }
}

/// One link of a chain: its centre and the chain's direction there.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Link {
    /// Where its centre is.
    pub centre: DVec3,
    /// The unit direction along the chain.
    pub along: DVec3,
}

/// The links of a chain hanging as `hang`: `count` of them when given, else one per `pitch` and at least
/// [`MIN_LINKS`]; link `k` centred `(k + 0.5) / n` of the way along the sampled centre line, its direction the chord
/// `reach` either side of it.
///
/// # Errors
/// [`TrimError::Invalid`] for a centre line of no length.
pub fn links(hang: &Hang, pitch: f64, count: Option<u32>, reach: f64) -> Result<Vec<Link>, TrimError> {
    let samples: Vec<DVec3> = (0..=CURVE_SAMPLES).map(|i| hang.at(fraction(i, CURVE_SAMPLES))).collect();
    let mut lengths = Vec::with_capacity(samples.len());
    let mut total = 0.0;
    lengths.push(0.0);
    for pair in samples.windows(2) {
        total += pair[0].distance(pair[1]);
        lengths.push(total);
    }
    if !(total.is_finite() && total > 0.0) {
        return Err(TrimError::invalid("chain", "its ends coincide and it doesn't sag: it has no length"));
    }
    let along = |distance: f64| {
        let distance = distance.clamp(0.0, total);
        let k = lengths.partition_point(|l| *l < distance).clamp(1, samples.len() - 1);
        let (before, after) = (lengths[k - 1], lengths[k]);
        let share = if after > before { (distance - before) / (after - before) } else { 0.0 };
        samples[k - 1].lerp(samples[k], share)
    };
    let n = match count {
        Some(n) => n,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a positive ratio, clamped
        None => ((total / pitch).round().clamp(f64::from(MIN_LINKS), f64::from(MAX_LINKS))) as u32,
    };
    (0..n)
        .map(|k| {
            let d = (f64::from(k) + 0.5) * total / f64::from(n);
            let direction = (along(d + reach) - along(d - reach))
                .try_normalize()
                .ok_or_else(|| TrimError::invalid("chain", "a link has no direction"))?;
            Ok(Link { centre: along(d), along: direction })
        })
        .collect()
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

/// Adds the links of a chain to `builder`: each the prototype's stretched torus, facing out from the garment (`out`)
/// and every other one turned a quarter turn round the chain, at `scale` of the prototype's link; `skin(k)` skins link
/// `k`.
///
/// # Errors
/// The kit's errors for a degenerate link.
pub fn add_links(
    builder: &mut MeshBuilder,
    links: &[Link],
    out: DVec3,
    scale: f64,
    skin: &dyn Fn(usize) -> Vec<(JointRef, f32)>,
) -> Result<(), TrimError> {
    let stretch = DVec3::new(LINK_STRETCH, 1.0, 1.0);
    for (k, link) in links.iter().enumerate() {
        let x = link.along;
        let facing = out.cross(x).try_normalize().unwrap_or_else(|| x.any_orthonormal_vector());
        let y = if k % 2 == 0 { facing } else { out };
        let basis = Basis { origin: link.centre, x, y, z: x.cross(y).normalize_or_zero() };
        torus(builder, &basis, (LINK.0 * scale, LINK.1 * scale), LINK_SEGMENTS, stretch, &skin(k))?;
    }
    Ok(())
}

/// How the chain `spec` hangs on `surface`: from its anchors lifted clear of the garment by a link, up the world's up,
/// out along the anchors' normals.
///
/// # Errors
/// The surface's errors; [`TrimError::Invalid`] for a spec that fails validation.
pub fn hang(surface: &Surface, spec: &Chain) -> Result<Hang, TrimError> {
    checked(spec)?;
    let clearance = clearance(spec);
    let (a, b) = (surface.at(spec.from[0], spec.from[1])?, surface.at(spec.to[0], spec.to[1])?);
    let up = DVec3::Y;
    let normal = a.normal + b.normal;
    let out = (normal - up * normal.dot(up)).try_normalize().unwrap_or(DVec3::Z);
    Ok(Hang {
        from: a.point + a.normal * clearance,
        to: b.point + b.normal * clearance,
        up,
        out,
        sag: spec.sag,
        bow: spec.bow,
    })
}

/// How far a link's centre keeps off the garment: its stretched half-length and its wire.
fn clearance(spec: &Chain) -> f64 {
    (LINK.0 * LINK_STRETCH + LINK.1) * spec.link_size / LINK.0
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

/// The chain `spec` describes on `surface`: its links, skinned from the start's surface to the end's, and the pendant
/// cross at its middle when it has one.
///
/// # Errors
/// As [`hang`] and [`links`]; the kit's errors for a degenerate link.
pub fn build(surface: &Surface, spec: &Chain) -> Result<MeshBuilder, TrimError> {
    let hang = hang(surface, spec)?;
    let scale = spec.link_size / LINK.0;
    let clearance = clearance(spec);
    let mut chain = links(&hang, PITCH * scale, spec.link_count, TANGENT_REACH * scale)?;
    for link in &mut chain {
        rest_on(surface, &mut link.centre, clearance)?;
    }
    let (from, to) = (surface.weights().at(spec.from[0]), surface.weights().at(spec.to[0]));
    let last = chain.len().max(2) - 1;
    let mut builder = MeshBuilder::new();
    add_links(&mut builder, &chain, hang.out, scale, &|k| mix(&from, &to, fraction(k.min(last), last)))?;
    if spec.pendant {
        let mut middle = hang.at(0.5);
        rest_on(surface, &mut middle, clearance)?;
        let basis = Basis { origin: middle, x: hang.up.cross(hang.out), y: hang.up, z: hang.out };
        pendant_cross(&mut builder, &basis, scale, &mix(&from, &to, 0.5))?;
    }
    Ok(builder)
}
