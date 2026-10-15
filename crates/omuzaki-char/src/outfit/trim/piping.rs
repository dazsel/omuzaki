//! Piping: a raised strip along a rail, with bevelled edges and a bulging top (the robe's `addStrip` cross-section; a
//! rolled edge is a piping bevelled all the way).

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Finish, Rail, RailSpec, TrimError, checked, feature};
use crate::kit::curve::fraction;
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::loft::Surface;

/// How far the top of a strip bulges above its flat top, as a share of its thickness.
pub const BULGE: f64 = 0.25;
/// Points across the bulging top of a strip.
const TOP_POINTS: usize = 6;
/// A strip is sampled about this often along its rail, metres.
pub const STEP: f64 = 0.004;
/// Points closer than this in a cross-section are one point, metres.
const SAME_POINT: f64 = 1e-7;

/// A raised strip along a rail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Piping {
    /// Where it runs.
    pub rail: RailSpec,
    /// Its width across the rail, metres.
    pub width: f64,
    /// Its height off the rail, metres.
    pub thickness: f64,
    /// How much of each top corner is cut away, metres, up to half the smaller of width and thickness (that much
    /// rounds it into a rolled edge).
    #[serde(default)]
    pub bevel: f64,
    /// What it is made of.
    #[serde(default = "cream")]
    pub finish: Finish,
}

fn cream() -> Finish {
    Finish::cloth(ColourRef::rgb(0xf2, 0xea, 0xd8))
}

impl Validate for Piping {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        feature(c, "width", self.width);
        feature(c, "thickness", self.thickness);
        c.range("bevel", self.bevel, 0.0, 0.5 * self.width.min(self.thickness));
        c.nested("finish", &self.finish);
    }
}

/// The cross-section of a strip, `(across, up)` from the rail: a flat base, straight sides, bevelled top corners and a
/// top that bulges by [`BULGE`] of the thickness. Convex, counter-clockwise seen along the rail.
pub fn section(width: f64, thickness: f64, bevel: f64) -> Vec<DVec2> {
    let half = width / 2.0;
    let bevel = bevel.clamp(0.0, 0.5 * width.min(thickness));
    let top = half - bevel;
    let mut points = vec![DVec2::new(-half, 0.0), DVec2::new(half, 0.0), DVec2::new(half, thickness - bevel)];
    for k in 0..=TOP_POINTS {
        let s = top * (1.0 - 2.0 * fraction(k, TOP_POINTS));
        let rise = if top > 0.0 { 1.0 - (s / top).powi(2) } else { 0.0 };
        points.push(DVec2::new(s, thickness + BULGE * thickness * rise));
    }
    points.push(DVec2::new(-half, thickness - bevel));
    points.dedup_by(|a, b| a.distance(*b) < SAME_POINT);
    if points.len() > 1 && points[0].distance(points[points.len() - 1]) < SAME_POINT {
        points.pop();
    }
    points
}

/// Sweeps the closed `section` (`(across, up)` from the rail) along `rail` into a closed shell: the ends of an open rail
/// are capped flat, a closed rail wraps. Each ring is skinned like the surface under its rail point.
///
/// # Errors
/// [`TrimError::Invalid`] for a section of fewer than 3 points; the kit's errors for a degenerate shape.
pub fn sweep_section(rail: &Rail, section: &[DVec2]) -> Result<MeshBuilder, TrimError> {
    if section.len() < 3 {
        return Err(TrimError::invalid("section", format!("{} points: a cross-section needs 3", section.len())));
    }
    let points = rail.points();
    let cols = section.len() + 1;
    let at = |i: usize, j: usize| {
        let p = &points[i];
        let q = section[j % section.len()];
        p.point + p.side * q.x + p.normal * q.y
    };
    let grid = Grid::from_fn(points.len(), cols, at)?;
    let centre = section.iter().copied().sum::<DVec2>() / f64::from(u32::try_from(section.len()).unwrap_or(u32::MAX));
    let inside = |i: usize, _: usize| points[i].point + points[i].side * centre.x + points[i].normal * centre.y;
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let options = GridOptions {
        wrap_i: rail.closed(),
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    if !rail.closed() {
        let last = points.len() - 1;
        for (i, outward) in [(0, -points[0].tangent), (last, points[last].tangent)] {
            let ring: Vec<DVec3> = (0..section.len()).map(|j| at(i, j)).collect();
            cap(&mut builder, ring, outward, &points[i].skin)?;
        }
    }
    Ok(builder)
}

/// Closes a ring with a flat polygon facing `outward`, wound to match.
fn cap(
    builder: &mut MeshBuilder,
    mut ring: Vec<DVec3>,
    outward: DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    // Newell's normal of the ring as given: reverse the ring if it faces the other way.
    let mut newell = DVec3::ZERO;
    for (k, p) in ring.iter().enumerate() {
        let q = ring[(k + 1) % ring.len()];
        newell += p.cross(q);
    }
    if newell.dot(outward) < 0.0 {
        ring.reverse();
    }
    builder.add_polygon(&ring, outward, [1.0; 4], skin)?;
    Ok(())
}

/// The piping `spec` describes on `surface`.
///
/// # Errors
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation.
pub fn build(surface: &Surface, spec: &Piping) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    let rail = Rail::with_step(surface, &spec.rail, STEP.min(spec.width))?;
    sweep_section(&rail, &section(spec.width, spec.thickness, spec.bevel))
}
