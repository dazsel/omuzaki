//! The skirt's default profile: an envelope derived from the fit it is worn over.
//!
//! The prototype's skirt envelope is a table fitted to one body. The default here is derived from whatever the skirt is
//! worn over instead: at each of a few heights between the hem and the waist, the footprint of everything the skirt
//! must clear (the torso where it reaches, and both legs), the ellipse that encloses it, and room for the ease, the
//! cloth, the deepest fold and the flare.

use omuzaki_char_api::{BodyFit, BodyRegion};
use omuzaki_core::glam::DVec2;

use super::pleats::{THICKNESS, amplitude_at};
use crate::kit::curve::{TAU, fraction, lerp};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{MIN_EXPONENT, Ring, ring_at};

/// How many knots the derived profile has, from the hem to the waist.
pub const KNOTS: usize = 9;
/// How many points of each section the footprint is sampled at.
const SAMPLES: usize = 48;
/// Bisections that find the section at a height: far below a micrometre.
const BISECTIONS: usize = 40;
/// The envelope at its three curves: half widths across and through the body, and the centre's offset to the front.
pub type Profile = [Vec<[f64; 2]>; 3];

/// The section of `region` of `fit` whose centre is at height `y`, if it reaches that height.
fn at_height(fit: &dyn BodyFit, region: BodyRegion, y: f64) -> Result<Option<Ring>, GarmentError> {
    let section = |t: f64| ring_at(fit, region, t, MIN_EXPONENT, 0.0);
    let (a, b) = (section(0.0)?, section(1.0)?);
    let (lo, hi) = (a.centre.y.min(b.centre.y), a.centre.y.max(b.centre.y));
    if !(lo..=hi).contains(&y) {
        return Ok(None);
    }
    // The centre's height runs monotonically along a region, up or down.
    let rising = b.centre.y > a.centre.y;
    let (mut from, mut to) = (0.0, 1.0);
    for _ in 0..BISECTIONS {
        let mid = f64::midpoint(from, to);
        if (section(mid)?.centre.y < y) == rising {
            from = mid;
        } else {
            to = mid;
        }
    }
    section(f64::midpoint(from, to)).map(Some)
}

/// The points `(x, z)` of the footprint of `fit` at height `y`: the torso and both legs where they reach it.
fn footprint(fit: &dyn BodyFit, y: f64) -> Result<Vec<DVec2>, GarmentError> {
    let mut points = Vec::new();
    for region in [BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR] {
        // Below the torso's lower edge its last section stands for the pelvis, which the skirt must still clear.
        let ring = match at_height(fit, region, y)? {
            Some(ring) => Some(ring),
            None if region == BodyRegion::Torso && y < ring_at(fit, region, 0.0, MIN_EXPONENT, 0.0)?.centre.y => {
                Some(ring_at(fit, region, 0.0, MIN_EXPONENT, 0.0)?)
            }
            None => None,
        };
        if let Some(ring) = ring {
            points.extend((0..SAMPLES).map(|k| {
                let p = ring.point(TAU * fraction(k, SAMPLES));
                DVec2::new(p.x, p.z)
            }));
        }
    }
    if points.is_empty() {
        return Err(GarmentError::invalid(
            "skirt",
            format!("the fit has no body at height {y} m for the skirt to clear"),
        ));
    }
    Ok(points)
}

/// The profile for a skirt between `hem` and `top` metres that clears `fit` by `ease`, with folds `depth` deep and
/// `flare` metres wider at the hem.
///
/// At each knot the enclosing ellipse is the box of the footprint (centred between its front and back, as wide as its
/// sides) grown just enough to take in every point of it, then widened by the room the skirt needs.
///
/// # Errors
/// [`GarmentError`] for a height where the fit has no body, or a fit whose sections the loft refuses.
pub fn derive(
    fit: &dyn BodyFit,
    hem: f64,
    top: f64,
    ease: f64,
    depth: f64,
    flare: f64,
) -> Result<Profile, GarmentError> {
    let mut profile: Profile = [Vec::new(), Vec::new(), Vec::new()];
    for k in 0..KNOTS {
        let s = fraction(k, KNOTS - 1);
        let points = footprint(fit, lerp(hem, top, s))?;
        let (mut x_max, mut z_min, mut z_max) = (0.0_f64, f64::INFINITY, f64::NEG_INFINITY);
        for p in &points {
            x_max = x_max.max(p.x.abs());
            z_min = z_min.min(p.y);
            z_max = z_max.max(p.y);
        }
        let (centre, half_z) = (f64::midpoint(z_min, z_max), (z_max - z_min) / 2.0);
        let (a, b) = (x_max.max(f64::EPSILON), half_z.max(f64::EPSILON));
        let grow = points.iter().map(|p| (p.x / a).hypot((p.y - centre) / b)).fold(1.0, f64::max);
        let room = ease + THICKNESS + amplitude_at(depth, s) + flare * (1.0 - s).powi(2);
        profile[0].push([s, grow * a + room]);
        profile[1].push([s, grow * b + room]);
        profile[2].push([s, centre]);
    }
    Ok(profile)
}
