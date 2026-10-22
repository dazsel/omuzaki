//! The left sleeve: a tube along the arm, from the shoulder to the cuff (`uniform.js` `armPolyline` and `buildSleeve`,
//! 77-122). The right sleeve is its mirror image.
//!
//! The sleeve's centre line walks the arm's two bones, from the shoulder to the elbow and on to a point just past the
//! wrist, and four passes of a moving average round the elbow's corner. A ring of 24 points sits round every station
//! ([`stations`]), square to the line, with the radius of the arm there (the fit's own, so the sleeve follows a slim or
//! a broad arm) plus the shirt's ease, the sleeve's extra width and, at the elbow, its extra ease. The wrist end is open,
//! hidden in the cuff.
//!
//! **The shoulder.** The prototype closes the sleeve in a dome whose tip is the arm's first point, inside its arm. A body
//! here has an arm there, which a garment must never enter, so the dome is moved back: the eight stations that make it
//! lie *behind* the arm's start ([`SHOULDER_SPAN`] before it), over the shoulder, and the rest follow along the arm.
//! The dome ends inside the shirt's body, where [`union`](super::union) trims it; [`Sleeve::arm`] is the arm with the
//! shirt's ease on it, which the body is trimmed against in turn.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};

use super::union::Tube;
use super::{ARM_BONES, tile_uvs};
use crate::kit::curve::{TAU, fraction, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::skin::{CapsuleParams, SegmentSet};
use crate::outfit::common::GarmentError;

/// Segments round the sleeve.
pub const SEGMENTS: usize = 24;
/// Passes of the moving average that round the elbow's corner.
pub const SMOOTHING_PASSES: usize = 4;
/// How far the sleeve runs past the wrist, metres, so the cuff covers its end.
pub const WRIST_OVERLAP: f64 = 0.014;
/// The first stations, from the dome's tip: eight of them make the dome, in the prototype's easing.
pub const SHOULDER_STATIONS: usize = 8;
/// The span of the dome, metres: the prototype's is 12 mm, in an arm that fills it; this one covers the shoulder the body
/// has behind the arm's start, so it reaches back 28 mm.
pub const SHOULDER_SPAN: f64 = 0.028;
/// Where the regular stations start, from the dome's tip: 16 mm past the arm's start, as the prototype's do, metres.
pub const STATION_START: f64 = SHOULDER_SPAN + 0.016;
/// The spacing of the regular stations, metres.
pub const STATION_STEP: f64 = 0.0105;
/// The regular stations stop this far short of the end, which has a station of its own, metres.
pub const STATION_TAIL: f64 = 0.004;
/// The sleeve darkens this much at the shoulder (vertex colour), over its first `SHADE_SPAN` metres.
pub const SHOULDER_SHADE: f64 = 0.10;
/// See [`SHOULDER_SHADE`], metres.
pub const SHADE_SPAN: f64 = 0.1;
/// How far the arm the shirt's body is trimmed against starts behind the arm's start, metres: the body's vertices on
/// the start's plane are inside it, and the sleeve there is still all but its whole width.
pub const TUBE_LEAD: f64 = 0.004;
/// How far either side of the elbow the extra ease at the elbow reaches, metres.
pub const ELBOW_REACH: f64 = 0.05;

/// What the sleeve is cut from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SleeveSpec {
    /// The shirt's ease over the arm, metres.
    pub ease: f64,
    /// The sleeve's extra width over the arm, metres.
    pub width: f64,
    /// Extra ease at the elbow, metres.
    pub ease_at_elbow: f64,
    /// How much of the arm the sleeve covers, from `0` to `1`; at `1` it runs to the wrist and on into the cuff.
    pub length: f64,
}

impl SleeveSpec {
    /// Whether the sleeve reaches the wrist, where the cuff goes.
    pub fn full(&self) -> bool {
        self.length >= 1.0
    }
}

/// The left sleeve.
#[derive(Debug)]
pub struct Sleeve {
    /// The sleeve's mesh.
    pub mesh: MeshBuilder,
    /// The arm with the shirt's ease on it, from the arm's start along the sleeve: what the shirt's body is trimmed against.
    pub arm: Tube,
    /// The wrist.
    pub wrist: DVec3,
    /// The forearm's direction, unit, from the elbow to the wrist.
    pub forearm: DVec3,
    /// The sleeve's radius at its end, metres: what the cuff closes round.
    pub end_radius: f64,
    /// Its rows of [`SEGMENTS`] + 1 vertices each.
    pub rows: usize,
}

/// The stations along the sleeve, as distances from the shoulder: eight easing out of the shoulder, then one every
/// [`STATION_STEP`], then the end. The regular ones accumulate by addition, as the prototype's loop does.
pub fn stations(end: f64) -> Vec<f64> {
    let mut out: Vec<f64> = (0..SHOULDER_STATIONS)
        .map(|k| {
            let u = fraction(k, SHOULDER_STATIONS - 1);
            SHOULDER_SPAN * (1.0 - (u * std::f64::consts::FRAC_PI_2).cos())
        })
        .collect();
    let mut s = STATION_START;
    while s < end - STATION_TAIL {
        out.push(s);
        s += STATION_STEP;
    }
    out.push(end);
    out
}

/// The arm of `fit`: the shoulder, the elbow and the wrist of the left arm.
fn arm(fit: &dyn BodyFit) -> Result<[DVec3; 3], GarmentError> {
    let end = |t: f32| {
        fit.section(BodyRegion::ArmL, t)
            .map(|s| s.centre.as_dvec3())
            .ok_or(GarmentError::NoSection { region: BodyRegion::ArmL, t: f64::from(t) })
    };
    let elbow = fit
        .joint(&JointRef::Humanoid(HumanoidBone::LeftLowerArm))
        .map(Vec3::as_dvec3)
        .ok_or_else(|| GarmentError::invalid("body", "the fit has no left elbow".to_owned()))?;
    Ok([end(0.0)?, elbow, end(1.0)?])
}

/// The point `a` metres along the arm from the shoulder, walking `shoulder`, `elbow`, `end` (`upper` is the length of the
/// first bone). Before the shoulder the line goes on straight back along the upper arm.
fn walk([shoulder, elbow, end]: [DVec3; 3], upper: f64, a: f64) -> DVec3 {
    if a <= upper {
        shoulder.lerp(elbow, a / upper)
    } else {
        elbow.lerp(end, ((a - upper) / elbow.distance(end)).min(1.0))
    }
}

/// How round the sleeve's shoulder dome is at the prototype's distance `s` from its tip: a quarter circle over the first
/// [`SHOULDER_SPAN`], a full ring after.
fn dome(s: f64) -> f64 {
    if s < SHOULDER_SPAN { (1.0 - ((SHOULDER_SPAN - s) / SHOULDER_SPAN).powi(2)).max(0.0).sqrt() } else { 1.0 }
}

/// Builds the left sleeve over the arm of `fit`.
///
/// The single letters are the geometry's own: the stations `s` along the sleeve from its tip, `a` along the arm, the frame's
/// `t`, `n` and `b`.
///
/// # Errors
/// [`GarmentError`] if the fit has no left arm or elbow, or a station's frame is degenerate.
#[allow(clippy::many_single_char_names, clippy::cast_possible_truncation)] // see above; shades are in 0..=1
pub fn build(fit: &dyn BodyFit, spec: &SleeveSpec, tile: Option<f64>) -> Result<Sleeve, GarmentError> {
    let [shoulder, elbow, wrist] = arm(fit)?;
    let forearm = (wrist - elbow)
        .try_normalize()
        .ok_or_else(|| GarmentError::invalid("body", "the left arm has no forearm".to_owned()))?;
    let past_wrist = wrist + forearm * WRIST_OVERLAP;
    let upper = shoulder.distance(elbow);
    let arm_length = upper + elbow.distance(wrist);
    let end = if spec.full() { upper + elbow.distance(past_wrist) } else { spec.length * arm_length };
    // The prototype's stations, measured from the dome's tip; the arm's start is `SHOULDER_SPAN` along.
    let s = stations(end + SHOULDER_SPAN);
    let a: Vec<f64> = s.iter().map(|s| s - SHOULDER_SPAN).collect();
    let back = (shoulder - elbow) / upper;
    let mut points: Vec<DVec3> = a
        .iter()
        .map(|&a| if a < 0.0 { shoulder + back * -a } else { walk([shoulder, elbow, past_wrist], upper, a) })
        .collect();
    for _ in 0..SMOOTHING_PASSES {
        let before = points.clone();
        for i in 1..before.len() - 1 {
            points[i] = (before[i - 1] + before[i] * 2.0 + before[i + 1]) * 0.25;
        }
    }
    // The arm's widest half-extent at distance `a` from its start (the end holds past the wrist).
    let arm_radius = |a: f64| -> Result<f64, GarmentError> {
        let t = (a.max(0.0) / arm_length).min(1.0);
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let section =
            fit.section(BodyRegion::ArmL, t as f32).ok_or(GarmentError::NoSection { region: BodyRegion::ArmL, t })?;
        Ok(f64::from(section.radii.max_element()))
    };
    let arm_radii = a.iter().map(|&a| arm_radius(a)).collect::<Result<Vec<_>, _>>()?;
    let radii: Vec<f64> = a
        .iter()
        .zip(&s)
        .zip(&arm_radii)
        .map(|((&a, &s), &arm)| {
            let bump = spec.ease_at_elbow * (1.0 - sstep(0.0, ELBOW_REACH, (a - upper).abs()));
            (arm + spec.ease + spec.width + bump) * dome(s)
        })
        .collect();
    let rows = points.len();
    let mut rings: Vec<Vec<DVec3>> = Vec::with_capacity(rows);
    for i in 0..rows {
        let t = match i {
            0 => points[1] - points[0],
            _ if i == rows - 1 => points[i] - points[i - 1],
            _ => points[i + 1] - points[i - 1],
        }
        .try_normalize()
        .ok_or_else(|| GarmentError::invalid("sleeve", format!("station {i} has no direction")))?;
        // The frame's first axis is the world's +z with the tangent cut out, as the prototype's is.
        let n = (DVec3::Z - t * DVec3::Z.dot(t))
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("sleeve", "the arm runs straight along z".to_owned()))?;
        let b = t.cross(n).normalize();
        rings.push(
            (0..=SEGMENTS)
                .map(|j| {
                    let phi = TAU * fraction(j, SEGMENTS);
                    points[i] + (n * phi.cos() + b * phi.sin()) * radii[i]
                })
                .collect(),
        );
    }
    let grid = Grid::from_rows(&rings)?;
    let uvs = tile_uvs(&grid, tile, true);
    let uv = |i: usize, j: usize| uvs.as_ref().map_or(DVec2::ZERO, |u| u[i * grid.cols() + j]);
    let inside = |i: usize, _: usize| points[i];
    // Skinned as the body's arm is, by the capsules of its bones, so a raised arm carries the sleeve with it as it carries
    // its own skin (the shoulder bone included, which keeps the root of the sleeve from running into the arm).
    let segments = SegmentSet::from_fit(fit, &ARM_BONES)?;
    let skin = |p: DVec3, _: usize, _: usize| segments.weights_at(p, &ARM_BONES, CapsuleParams::PROTOTYPE);
    let colour = |i: usize, _: usize, _: DVec3| {
        let k = (1.0 - SHOULDER_SHADE * sstep(SHADE_SPAN, 0.0, s[i])) as f32;
        [k, k, k, 1.0]
    };
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        uv: uvs.is_some().then_some(&uv as &dyn Fn(usize, usize) -> DVec2),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut mesh = MeshBuilder::with_colours();
    mesh.add_grid(&grid, &options)?;
    // The arm the body is trimmed against: from just behind the arm's exact start (the smoothing moved the stations
    // along the line), with the shirt's ease on the arm's own radius.
    let first = a.iter().position(|&a| a > 0.0).unwrap_or(rows - 1);
    let mut tube_points = vec![shoulder + back * TUBE_LEAD, shoulder];
    tube_points.extend_from_slice(&points[first..]);
    let mut tube_radii = vec![arm_radius(0.0)? + spec.ease; 2];
    tube_radii.extend(arm_radii[first..].iter().map(|r| r + spec.ease));
    let arm = Tube::new(tube_points, tube_radii)?;
    let end_radius = radii.last().copied().unwrap_or_default();
    Ok(Sleeve { mesh, arm, wrist, forearm, end_radius, rows })
}
