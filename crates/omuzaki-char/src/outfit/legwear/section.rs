//! The legwear's rings: the prototype's rows of `buildLeg` over any leg of any fit, and where the garment ends.
//!
//! The prototype lays its rows out in heights of one body (the leg runs from `y = 0.42` at the hip to `0.04` at the ankle).
//! They are kept as fractions of that leg, `u = (0.42 - y) / 0.38`, so they land on the same places of any template's
//! leg. A ring is the fit's section at `u`, `offset` outside it ([`ring_at`]). The tube runs from the top edge down to
//! where the foot begins: the prototype closes it under the ankle in a cap its boot hides, but the cap would be inside the
//! foot, so the garment ends in an open cuff just above the foot instead.

use std::sync::Arc;

use omuzaki_char_api::{BodyFit, BodyRegion, RegionEnd};
use omuzaki_core::glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction, rows_of};
use crate::kit::grid::Grid;
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{MIN_EXPONENT, RegionWeights, Ring, Surface, ring_at};

/// Columns of a ring: 44 segments once round the leg, and the closing column that repeats the first (`SEG = 44`).
pub const COLUMNS: usize = 45;
/// The prototype's leg, hip to ankle, metres: the frame its row heights are in.
pub const PROTOTYPE_LEG_LENGTH: f64 = 0.38;
/// The height of the prototype's hip, the top of its leg curves.
const PROTOTYPE_HIP_Y: f64 = 0.42;
/// The prototype's body rows, as `(from, to, segments)` heights: eight segments over the lowest 8 cm, 28 over the rest.
const PROTOTYPE_ROWS: [(f64, f64, usize); 2] = [(0.04, 0.12, 8), (0.12, 0.40, 28)];
/// How far from the midplane a tube stops, metres: the two tubes stay a hair apart, so they don't weld into one surface.
pub const MIDPLANE_GAP: f64 = 1e-4;
/// How far below the top edge the spans are cut finer, as a fraction of the leg, and into how many pieces.
const HIP_ZONE: f64 = 0.2;
const HIP_SPLIT: usize = 4;
/// How far a ring must lie outside the foot for the tube to clear it, metres.
const FOOT_MARGIN: f64 = 0.002;
/// The ring points tested against the foot, and the bisections that find where it starts (far below a micrometre).
const FOOT_SAMPLES: usize = 24;
const FOOT_BISECTIONS: usize = 24;
/// How far a ring must lie below the torso's lower edge for the tube to clear it, as a fraction of the leg's length (about
/// a centimetre on the soft-sd body): the leg swells and turns into the hip there, and a tube close to the edge would
/// cut through the swelling between its rings.
const WAIST_MARGIN: f64 = 0.03;
/// The step the torso's lower cap is walked down in, metres, and the most steps.
const CAP_STEP: f64 = 0.001;
const CAP_STEPS: usize = 200;
/// Bisections that find that ring: far below a micrometre.
const WAIST_BISECTIONS: usize = 40;
/// How far from the waist edge to the knee a thigh-high ends, as a fraction.
const THIGH_FRACTION: f64 = 0.35;
/// How far above the knee joint a knee-high ends, as a fraction of the leg.
const KNEE_ABOVE: f64 = 0.05;

/// Where the legwear ends at the top.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LegTop {
    /// Up to the torso's lower edge, where the garments above take over. The prototype's tights run on into the hips
    /// and close under the skirt; that part is inside the torso, which a garment can't be.
    Waist,
    /// A third of the way from there down to the knee.
    Thigh,
    /// Just above the knee.
    Knee,
}

/// The prototype's rows as fractions of the leg from the hip (`u`), ascending in `u`: the body rows, hip to ankle.
pub fn rows_of_leg() -> Vec<f64> {
    let mut u: Vec<f64> =
        rows_of(&PROTOTYPE_ROWS).into_iter().map(|y| (PROTOTYPE_HIP_Y - y) / PROTOTYPE_LEG_LENGTH).collect();
    u.sort_by(f64::total_cmp);
    u
}

/// The rings of one leg's tube, bottom to top.
#[derive(Clone, Debug)]
pub struct LegSections {
    rings: Vec<Ring>,
    /// The leg's region: its side of the body's midplane is the side its tube stays on.
    region: BodyRegion,
    /// How far each ring is below the top one along the leg, metres.
    from_top: Vec<f64>,
}

impl LegSections {
    /// The tube over `region` of `fit`, `offset` metres out, from just above the foot up to `top`; or, with a
    /// `bottom`, ending that fraction of the way from the waist edge to the knee (shorts, the bottom of a leotard), or
    /// past the knee toward the ankle above 1.
    ///
    /// # Errors
    /// [`GarmentError`] for a fit with no such sections, a leg without a knee, a leg whose lower end is not below the
    /// torso's lower edge, a top that is not above the foot, or a bottom that is not below the top.
    pub fn new(
        fit: &Arc<dyn BodyFit>,
        region: BodyRegion,
        offset: f64,
        top: LegTop,
        bottom: Option<f64>,
    ) -> Result<Self, GarmentError> {
        let section = |t: f64| ring_at(fit.as_ref(), region, t, MIN_EXPONENT, offset);
        let length = {
            let (a, b) = (section(0.0)?, section(1.0)?);
            a.centre.distance(b.centre)
        };
        let knee = RegionWeights::of(fit.as_ref(), region)?
            .chain()
            .get(1)
            .map(|&(_, at)| at)
            .ok_or_else(|| GarmentError::invalid("region", format!("{region:?} has no knee joint")))?;
        let waist = waist_t(fit.as_ref(), region, offset, length, &section)?;
        let top_t = match top {
            LegTop::Waist => waist,
            LegTop::Thigh => waist + THIGH_FRACTION * (knee - waist),
            LegTop::Knee => (knee - KNEE_ABOVE).max(waist),
        };
        let ankle_t = ankle_t(fit, region, knee, &section)?;
        let ankle_t = match bottom {
            Some(b) => {
                let end = (waist + b * (knee - waist)).min(ankle_t);
                if end <= top_t + 1e-6 {
                    return Err(GarmentError::invalid(
                        "bottom",
                        format!("{region:?}: the bottom ({end:.3}) isn't below the top ({top_t:.3}): end it lower"),
                    ));
                }
                end
            }
            None => ankle_t,
        };
        if ankle_t <= top_t {
            return Err(GarmentError::invalid(
                "region",
                format!("{region:?}: the top ({top_t}) isn't above the foot ({ankle_t})"),
            ));
        }
        // Bottom to top: the cuff, the prototype's body rows between, the top edge.
        let mut stops = vec![ankle_t];
        stops.extend(rows_of_leg().into_iter().rev().filter(|&u| u < ankle_t - 1e-9 && u > top_t + 1e-9));
        stops.push(top_t);
        // The leg swells and turns into the hip, so the spans near the top edge are cut finer than the prototype's rows: a
        // flat band across a curve that tight would cut into the leg.
        let mut ts = vec![ankle_t];
        for pair in stops.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let pieces = if to < top_t + HIP_ZONE { HIP_SPLIT } else { 1 };
            ts.extend((1..=pieces).map(|k| from + (to - from) * fraction(k, pieces)));
        }
        let rings = ts.iter().map(|&t| section(t)).collect::<Result<Vec<_>, GarmentError>>()?;
        let from_top = ts.iter().map(|&t| (t - top_t) * length).collect();
        Ok(Self { rings, region, from_top })
    }

    /// The rings' grid, bottom to top. A tube never crosses the body's midplane (`x = 0`): where the legs are nearer
    /// each other than twice the ease it is flattened against it, so the two tubes meet there, a hair ([`MIDPLANE_GAP`]) apart.
    ///
    /// # Errors
    /// [`KitError`] for a grid the kit refuses.
    pub fn grid(&self) -> Result<Grid, KitError> {
        Grid::from_fn(self.rings.len(), COLUMNS, |i, j| {
            // The closing column repeats the first exactly, so the seam welds.
            let phi = if j == COLUMNS - 1 { 0.0 } else { TAU * fraction(j, COLUMNS - 1) };
            let p = self.rings[i].point(phi);
            // Where the legs are nearer each other than the ease, the tubes meet on the midplane instead of crossing it.
            match self.region {
                BodyRegion::LegL => DVec3::new(p.x.max(MIDPLANE_GAP), p.y, p.z),
                _ => DVec3::new(p.x.min(-MIDPLANE_GAP), p.y, p.z),
            }
        })
    }

    /// The centre of ring `i`: the inside the normals point away from.
    pub fn centre(&self, i: usize) -> DVec3 {
        self.rings[i].centre
    }

    /// How far ring `i` is below the top edge, along the leg, metres.
    pub fn depth_below_top(&self, i: usize) -> f64 {
        self.from_top[i]
    }
}

/// The last `t` along the leg whose ring lies clear of the foot, `FOOT_MARGIN` outside it: where the cuff goes.
fn ankle_t(
    fit: &Arc<dyn BodyFit>,
    region: BodyRegion,
    knee: f64,
    section: &impl Fn(f64) -> Result<Ring, GarmentError>,
) -> Result<f64, GarmentError> {
    let foot_region = match region {
        BodyRegion::LegL => BodyRegion::FootL,
        BodyRegion::LegR => BodyRegion::FootR,
        other => return Err(GarmentError::invalid("region", format!("{other:?} is not a leg"))),
    };
    let foot = Surface::new(Arc::clone(fit), foot_region, MIN_EXPONENT, 0.0)?;
    let clear = |t: f64| -> Result<bool, GarmentError> {
        let ring = section(t)?;
        for k in 0..FOOT_SAMPLES {
            let found = foot.project(ring.point(TAU * fraction(k, FOOT_SAMPLES)))?;
            if found.is_some_and(|p| p.outside < FOOT_MARGIN) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if clear(1.0)? {
        return Ok(1.0);
    }
    if !clear(knee)? {
        return Err(GarmentError::invalid("region", format!("{region:?} is inside the foot at the knee")));
    }
    let (mut lo, mut hi) = (knee, 1.0);
    for _ in 0..FOOT_BISECTIONS {
        let mid = f64::midpoint(lo, hi);
        if clear(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// The first `t` along the leg whose ring lies wholly below the torso, [`WAIST_MARGIN`] clear of it: below its lower
/// edge, and below the lowest point of the cap the torso closes in under that edge (the crotch, between the thighs,
/// where the two tubes meet on the midplane and could not pass either side of it).
fn waist_t(
    fit: &dyn BodyFit,
    region: BodyRegion,
    offset: f64,
    length: f64,
    section: &impl Fn(f64) -> Result<Ring, GarmentError>,
) -> Result<f64, GarmentError> {
    let margin = WAIST_MARGIN * length;
    let edge =
        fit.section(BodyRegion::Torso, 0.0).ok_or(GarmentError::NoSection { region: BodyRegion::Torso, t: 0.0 })?;
    let up = edge.axis.as_dvec3().normalize_or_zero();
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // a fit is sampled in f32
    let depth = (1..=CAP_STEPS)
        .map(|k| k as f64 * CAP_STEP)
        .take_while(|&h| fit.cap(BodyRegion::Torso, RegionEnd::Start, h as f32).is_some())
        .last()
        .unwrap_or(0.0);
    let origin = edge.centre.as_dvec3() - up * depth;
    // How far above the torso's lower edge the highest point of the ring at `t` is (negative below it).
    let height = |t: f64| -> Result<f64, GarmentError> {
        let ring = section(t)?;
        Ok((0..COLUMNS - 1)
            .map(|j| (ring.point(TAU * fraction(j, COLUMNS - 1)) - origin).dot(up))
            .fold(f64::NEG_INFINITY, f64::max))
    };
    if height(0.0)? <= -margin {
        return Ok(0.0);
    }
    if height(1.0)? > -margin {
        return Err(GarmentError::invalid(
            "region",
            format!("{region:?} with offset {offset} never clears the torso's lower edge"),
        ));
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..WAIST_BISECTIONS {
        let mid = f64::midpoint(lo, hi);
        if height(mid)? <= -margin {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}
