//! The cuff: a rolled band round the wrist, a thin fold seam round it, a pearl button and the four thread holes in the
//! button (`uniform.js` `buildCuff` and `ringTubeAxis`, 123-147).
//!
//! The band is a lathe round the forearm's axis, its profile a table of `(distance along the forearm from the wrist,
//! radius over the sleeve's end)` rows: the outer face, the rolled edge at the far end and the turned-back inner face.
//! The prototype fitted its radii to its own wrist; here each is the sleeve's end radius plus the row's [`CUFF_PROFILE`]
//! offset, so the band closes round whatever sleeve it is given. The profile's lengths past the wrist stretch to the
//! cuff's height.

use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, RegionEnd};
use omuzaki_core::glam::DVec3;

use super::sleeve::Sleeve;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::loft::lathe_axis;
use crate::kit::sweep::place;
use crate::outfit::common::GarmentError;
use crate::template::layout::section_basis;

/// Segments round the band and the seam ring.
pub const SEGMENTS: usize = 40;
/// Sides of the seam ring's tube.
pub const SEAM_SIDES: usize = 6;
/// The seam ring's tube radius, metres.
pub const SEAM_RADIUS: f64 = 0.00058;
/// Where the seam ring sits along the forearm from the wrist, metres, at the prototype's cuff height.
pub const SEAM_AT: f64 = 0.0095;
/// How far the seam ring stands over the band's outer face, metres.
pub const SEAM_LIFT: f64 = 0.00025;
/// Where the button sits along the forearm from the wrist, metres, at the prototype's cuff height.
pub const BUTTON_AT: f64 = 0.0185;
/// The pearl: a disc of this top radius, bottom radius and thickness, and a rim torus of this ring and tube radius.
pub const BUTTON_DISC: (f64, f64, f64) = (0.0036, 0.0039, 0.0018);
/// See [`BUTTON_DISC`].
pub const BUTTON_RIM: (f64, f64) = (0.0030, 0.00055);
/// Segments round the pearl's disc and its rim's ring, and sides of the rim's tube.
pub const BUTTON_SEGMENTS: usize = 16;
/// See [`BUTTON_SEGMENTS`].
pub const BUTTON_RIM_SIDES: usize = 6;
/// The side of the cuff the button is on, before it is made square to the forearm: out and a little forward.
pub const BUTTON_SIDE: DVec3 = DVec3::new(1.0, 0.0, 0.25);
/// The pearl's vertex colours: the disc a little brighter than the cloth, the rim a little darker.
pub const BUTTON_DISC_COLOUR: [f32; 4] = [1.03, 1.03, 1.0, 1.0];
/// See [`BUTTON_DISC_COLOUR`].
pub const BUTTON_RIM_COLOUR: [f32; 4] = [0.92, 0.92, 0.95, 1.0];
/// The four thread holes: their radius, depth and sides; their spacing either side of the button's middle; how far they
/// stand over the pearl, metres.
pub const HOLE: (f64, f64, usize) = (0.00026, 0.0003, 6);
/// See [`HOLE`].
pub const HOLE_SPACING: f64 = 0.0011;
/// See [`HOLE`].
pub const HOLE_LIFT: f64 = 0.0012;
/// How far the rim torus stands over the disc's middle, metres.
pub const RIM_LIFT: f64 = 0.0009;
/// Rows of the profile that are the cuff's outer face; the rows after are its turned-back inner face, a little darker.
pub const OUTER_ROWS: usize = 10;
/// The darkening of the inner face (vertex colour).
pub const INNER_SHADE: f32 = 0.86;
/// How far past the wrist the prototype's cuff reaches, metres: the profile's longest row.
pub const REFERENCE_HEIGHT: f64 = 0.0388;

/// The cuff band's profile rows: `(distance along the forearm from the wrist, radius over the sleeve's end)`, metres.
/// The prototype's fitted radii (`0.0215` to `0.0267` outside, `0.0186` inside) less its sleeve's end radius of `0.0182`.
pub const CUFF_PROFILE: [(f64, f64); 16] = [
    (-0.0030, 0.0033),
    (-0.0018, 0.0068),
    (0.0000, 0.0081),
    (0.0030, 0.0085),
    (0.0120, 0.0085),
    (0.0260, 0.0085),
    (0.0320, 0.0084),
    (0.0360, 0.0076),
    (0.0382, 0.0056),
    (0.0388, 0.0034),
    (0.0382, 0.0015),
    (0.0365, 0.0005),
    (0.0340, 0.0004),
    (0.0200, 0.0004),
    (0.0000, 0.0004),
    (-0.0030, 0.0004),
];
/// The profile row the seam ring and the button sit on (the outer face): its radius over the sleeve's end.
pub const OUTER_OFFSET: f64 = 0.0085;

/// What the cuff is cut to.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CuffSpec {
    /// How far past the wrist the cuff reaches, metres.
    pub height: f64,
    /// Whether there is a pearl button.
    pub button: bool,
}

/// The cuff's pieces, for the left sleeve.
#[derive(Debug)]
pub struct Cuff {
    /// The band and the pearl, in the shirt's cloth.
    pub cloth: MeshBuilder,
    /// The fold seam, drawn unlit.
    pub lines: MeshBuilder,
    /// The thread holes, drawn unlit.
    pub holes: MeshBuilder,
}

/// The clearance a cuff keeps over the hand inside it, metres.
pub const HAND_CLEARANCE: f64 = 0.0005;
/// Steps along the hand a cuff's height is checked at, metres.
const HAND_STEP: f64 = 0.0005;
/// Points taken round each slice of the hand.
const HAND_SAMPLES: usize = 24;

/// The height of a cuff on `sleeve`, at most `height`, that stops short of where the hand inside it (the cap past the
/// end of `fit`'s left arm, [`BodyFit::cap_slice`]) grows wider than the cuff's inner face: a hand that spreads to its
/// palm and thumb close past the wrist takes a shorter cuff. `height` itself over a fit without the hand.
pub fn clear_of_hand(fit: &dyn BodyFit, sleeve: &Sleeve, height: f64) -> f64 {
    let Some(end) = fit.section(BodyRegion::ArmL, 1.0) else { return height };
    let (wrist, axis) = (sleeve.wrist, sleeve.forearm);
    let inner = sleeve.end_radius + CUFF_PROFILE.iter().map(|&(_, offset)| offset).fold(f64::INFINITY, f64::min);
    // Where along the cuff (stretched to `h`) its inner face is, at the hand's slice `a` past the wrist: the turned-back
    // face runs to the cuff's far edge, so the hand must fit inside it all the way there.
    let wider = |a: f64| -> bool {
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let Some(s) = fit.cap_slice(BodyRegion::ArmL, RegionEnd::End, a as f32) else { return false };
        let (across, through) = section_basis(BodyRegion::ArmL, s.axis);
        let (centre, across, through) = (s.centre.as_dvec3(), across.as_dvec3(), through.as_dvec3());
        let radii = s.radii.as_dvec2();
        (0..HAND_SAMPLES).any(|k| {
            let (sin, cos) = (TAU * fraction(k, HAND_SAMPLES)).sin_cos();
            let p = centre + across * (radii.x * cos) + through * (radii.y * sin) - wrist;
            (p - axis * p.dot(axis)).length() + HAND_CLEARANCE > inner
        })
    };
    // The slices are measured from the arm's end, along its axis; the cuff's lengths from the wrist, along the forearm.
    let lead = (wrist - end.centre.as_dvec3()).dot(end.axis.as_dvec3());
    let mut a = 0.0;
    while a < height {
        if wider(a + lead) {
            return a.max(HAND_STEP);
        }
        a += HAND_STEP;
    }
    height
}

/// Builds the cuff on the end of `sleeve`.
///
/// # Errors
/// [`GarmentError`] if the cuff's height is not a length, or the kit refuses a shape.
pub fn build(sleeve: &Sleeve, spec: &CuffSpec) -> Result<Cuff, GarmentError> {
    if !(spec.height.is_finite() && spec.height > 0.0) {
        return Err(GarmentError::invalid("cuff", format!("a height of {} m", spec.height)));
    }
    let stretch = spec.height / REFERENCE_HEIGHT;
    // Only the part past the wrist stretches: the band's back edge stays where it is.
    let along = |u: f64| if u > 0.0 { u * stretch } else { u };
    let (wrist, axis) = (sleeve.wrist, sleeve.forearm);
    let skin = [(JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftLowerArm), 1.0)];
    let profile: Vec<(f64, f64)> =
        CUFF_PROFILE.iter().map(|&(u, offset)| (along(u), sleeve.end_radius + offset)).collect();

    // The band.
    let band = lathe_axis(wrist, axis, &profile, SEGMENTS)?;
    let inside = |i: usize, _: usize| wrist + axis * profile[i].0;
    let shade = |i: usize, _: usize, _: DVec3| {
        let k = if i >= OUTER_ROWS { INNER_SHADE } else { 1.0 };
        [k, k, k, 1.0]
    };
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&shade),
        skin: Skin::Blend(skin.to_vec()),
        ..GridOptions::default()
    };
    let mut cloth = MeshBuilder::with_colours();
    cloth.add_grid(&band, &options)?;

    // The fold seam: a thin tube round the band's outer face.
    let seam_at = along(SEAM_AT);
    let seam_radius = sleeve.end_radius + OUTER_OFFSET + SEAM_LIFT;
    let seam_centre = wrist + axis * seam_at;
    // A lathe needs two rows; the ring is the first.
    let ring = lathe_axis(wrist, axis, &[(seam_at, seam_radius), (seam_at + SEAM_RADIUS, seam_radius)], SEGMENTS)?;
    let ring: Vec<DVec3> = (0..=SEGMENTS).map(|j| ring.at(0, j)).collect();
    let (tube, centres) = ring_tube(&ring, seam_centre, axis, SEAM_RADIUS)?;
    let mut lines = MeshBuilder::new();
    let inside = |i: usize, _: usize| centres[i];
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Blend(skin.to_vec()),
        ..GridOptions::default()
    };
    lines.add_grid(&tube, &options)?;

    // The pearl and its holes.
    let mut holes = MeshBuilder::new();
    if spec.button {
        let out = (BUTTON_SIDE - axis * BUTTON_SIDE.dot(axis))
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("cuff", "the forearm runs along the button's side".to_owned()))?;
        let at = wrist + axis * along(BUTTON_AT) + out * (sleeve.end_radius + OUTER_OFFSET);
        disc(&mut cloth, at, out, BUTTON_DISC, BUTTON_SEGMENTS, BUTTON_DISC_COLOUR, &skin)?;
        torus(&mut cloth, at + out * RIM_LIFT, out, BUTTON_RIM, &skin)?;
        let across = out.cross(axis).normalize();
        for dx in [-HOLE_SPACING, HOLE_SPACING] {
            for dy in [-HOLE_SPACING, HOLE_SPACING] {
                let centre = at + out * HOLE_LIFT + across * dx + axis * dy;
                disc(&mut holes, centre, out, (HOLE.0, HOLE.0, HOLE.1), HOLE.2, [1.0; 4], &skin)?;
            }
        }
    }
    Ok(Cuff { cloth, lines, holes })
}

/// A tube round the closed loop `ring` (its last point repeats its first) about `axis` through `centre`: at each point
/// the tangent is the step between its neighbours, the first frame axis points away from the axis, and the ring of
/// [`SEAM_SIDES`] points is `radius` round the point. Returns the grid and the centre line of each row.
fn ring_tube(ring: &[DVec3], centre: DVec3, axis: DVec3, radius: f64) -> Result<(Grid, Vec<DVec3>), GarmentError> {
    let n = ring.len() - 1;
    let mut rows = Vec::with_capacity(n + 1);
    let mut centres = Vec::with_capacity(n + 1);
    for k in 0..=n {
        let p = ring[k % n];
        let tangent = (ring[(k + 1) % n] - ring[(k + n - 1) % n])
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("cuff", "the seam ring has a repeated point".to_owned()))?;
        let r = p - centre;
        let r = (r - axis * r.dot(axis))
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("cuff", "the seam ring is on the axis".to_owned()))?;
        let b = tangent.cross(r).normalize();
        rows.push(
            (0..=SEAM_SIDES)
                .map(|j| {
                    let phi = TAU * fraction(j, SEAM_SIDES);
                    p + (r * phi.cos() + b * phi.sin()) * radius
                })
                .collect::<Vec<_>>(),
        );
        centres.push(p);
    }
    Ok((Grid::from_rows(&rows)?, centres))
}

/// A closed disc of `(top radius, bottom radius, thickness)` standing along `dir` on `at` (three.js's
/// `CylinderGeometry`, here as one lathe through the caps' middles).
fn disc(
    builder: &mut MeshBuilder,
    at: DVec3,
    dir: DVec3,
    (top, bottom, thickness): (f64, f64, f64),
    sides: usize,
    colour: [f32; 4],
    skin: &[(JointRef, f32)],
) -> Result<(), GarmentError> {
    let half = thickness / 2.0;
    let profile = [(-half, 0.0), (-half, bottom), (half, top), (half, 0.0)];
    let grid = lathe_axis(at, dir, &profile, sides)?;
    let inside = |_: usize, _: usize| at;
    let colour = |_: usize, _: usize, _: DVec3| colour;
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Blend(skin.to_vec()),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// The pearl's rim: a torus of `(ring radius, tube radius)` round `dir` at `at`.
fn torus(
    builder: &mut MeshBuilder,
    at: DVec3,
    dir: DVec3,
    (ring, tube): (f64, f64),
    skin: &[(JointRef, f32)],
) -> Result<(), GarmentError> {
    let frame = place(at, dir, 0.0, 1.0)?;
    let (x, z) = (frame.x_axis.truncate(), frame.z_axis.truncate());
    let point = |i: usize, j: usize| {
        let u = TAU * fraction(i % BUTTON_SEGMENTS, BUTTON_SEGMENTS);
        let v = TAU * fraction(j % BUTTON_RIM_SIDES, BUTTON_RIM_SIDES);
        let radial = x * u.cos() + z * u.sin();
        at + radial * (ring + tube * v.cos()) + dir * (tube * v.sin())
    };
    let grid = Grid::from_fn(BUTTON_SEGMENTS + 1, BUTTON_RIM_SIDES + 1, point)?;
    let inside = |i: usize, _: usize| {
        let u = TAU * fraction(i % BUTTON_SEGMENTS, BUTTON_SEGMENTS);
        at + (x * u.cos() + z * u.sin()) * ring
    };
    let colour = |_: usize, _: usize, _: DVec3| BUTTON_RIM_COLOUR;
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Blend(skin.to_vec()),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}
