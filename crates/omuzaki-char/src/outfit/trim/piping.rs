//! Piping: the robe's thick-edged strip (`addStrip`: a band on the cloth with a rolled edge and a lip tucked under) and
//! the vest's round cord (`ringTube` round a closed rail, `tube` along an open one).
//!
//! **The strip** runs along its rail, which is its free edge; the band lies `width` across the surface to one side of
//! it, the inner border. Each sample of the rail is a rung of 11 vertices (9 without a lip), in this order: the lip
//! tucked under the cloth (2), the rolled edge (3, a quarter-turn arc of a half-ellipse), then the face across the band
//! at the fractions [`FRACS`] (6), its last stretch bevelled down to the cloth. The constants are the prototype's.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Finish, Rail, RailPoint, RailSpec, TrimError, checked, feature, param_offset};
use crate::kit::curve::{TAU, fraction, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;
use crate::outfit::loft::Surface;

/// Where the face's rungs sit across the band, from the free edge (0) to the inner border (1).
pub const FRACS: [f64; 6] = [0.0, 0.22, 0.5, 0.78, 0.935, 1.0];
/// Where the bevel starts and is full, across the band.
pub const BEVEL_FROM: (f64, f64) = (0.935, 1.0);
/// The face's margin over its lift, so it never z-fights the cloth, metres.
pub const FACE_MARGIN: f64 = 0.0003;
/// The rolled edge's bulge at full bulge, metres, and the least it bulges.
pub const BULGE: (f64, f64) = (0.0016, 0.0004);
/// Where the direction into the band is taken from, across it.
const INWARD_AT: f64 = 0.2;
/// The prototype's robe strip: thickness under the cloth, its lip's height and width, and its band's width (metres).
pub const ROBE: (f64, f64, f64, f64) = (0.0065, 0.0022, 0.0065, 0.034);
/// The texture's run along the strip, metres a repeat.
pub const TILE: f64 = 0.1;
/// The texture's `v` at the lip and the rolled edge; the face runs from there over [`V_FACE`].
const V_EDGE: f64 = 0.02;
/// The share of the texture's `v` the face spans.
const V_FACE: f64 = 0.96;

/// The lip of a strip: tucked under the cloth at its free edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lip {
    /// How much deeper than the strip's thickness it reaches under the cloth, metres.
    pub height: f64,
    /// How far into the band it runs, metres.
    pub width: f64,
}

impl Default for Lip {
    fn default() -> Self {
        Self { height: ROBE.1, width: ROBE.2 }
    }
}

/// A thick-edged strip along a rail (the robe's front, hem and cuffs; the collar's gold piping).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Piping {
    /// The free edge it runs along; its lift is the strip's.
    pub rail: RailSpec,
    /// The band's width across the surface, metres, to the rail's side (`tangent x normal`), or the other side when
    /// `flip`.
    pub width: f64,
    /// How far the rolled edge reaches under the cloth, metres. Keep it within the garment's ease.
    pub thickness: f64,
    /// How much of the bevel at the inner border there is, 0 (none) to 1.
    #[serde(default = "one")]
    pub bevel: f64,
    /// The lip tucked under the cloth (the robe's strip has [`Lip::default`]); `None` leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lip: Option<Lip>,
    /// How much the rolled edge bulges out, 0 to 1.
    #[serde(default = "one")]
    pub bulge: f64,
    /// Over how many rungs the bulge grows from nothing at each end (the collar's 5); 0 bulges everywhere.
    #[serde(default)]
    pub ramp: u32,
    /// Whether the ends are closed with flat caps.
    #[serde(default)]
    pub caps: bool,
    /// Lay the band to the other side of the rail.
    #[serde(default)]
    pub flip: bool,
    /// The texture's run along the strip, metres a repeat.
    #[serde(default = "tile")]
    pub tile: f64,
    /// What it is made of.
    #[serde(default = "cream")]
    pub finish: Finish,
}

fn one() -> f64 {
    1.0
}

fn tile() -> f64 {
    TILE
}

fn cream() -> Finish {
    Finish::cloth(ColourRef::rgb(0xf2, 0xea, 0xd8))
}

impl Validate for Piping {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        feature(c, "width", self.width);
        feature(c, "thickness", self.thickness);
        c.range("bevel", self.bevel, 0.0, 1.0);
        c.range("bulge", self.bulge, 0.0, 1.0);
        if let Some(lip) = &self.lip {
            c.range("lip/height", lip.height, 0.0, super::MAX_FEATURE);
            c.range("lip/width", lip.width, 0.0, self.width);
        }
        feature(c, "tile", self.tile);
        c.nested("finish", &self.finish);
    }
}

/// The surface point and normal at `at`.
fn on(surface: &Surface, at: [f64; 2]) -> Result<(DVec3, DVec3), TrimError> {
    let frame = surface.at(at[0].clamp(0.0, 1.0), at[1])?;
    Ok((frame.point, frame.normal))
}

/// `[t, phi]` a fraction `f` of the way from `a` to `b`.
fn across(a: [f64; 2], b: [f64; 2], f: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
}

/// One rung of the strip from its free edge `a` to its inner border `b`: its vertices, lip first, each with its `v`
/// across the texture.
fn rung(
    surface: &Surface,
    spec: &Piping,
    (a, b): ([f64; 2], [f64; 2]),
    (bulge, lift): (f64, f64),
) -> Result<Vec<(DVec3, f64)>, TrimError> {
    let (p_a, n_a) = on(surface, a)?;
    let toward = on(surface, across(a, b, INWARD_AT))?.0 - p_a;
    let inward = (toward - n_a * toward.dot(n_a))
        .try_normalize()
        .ok_or_else(|| TrimError::invalid("piping", "the band has no width on the surface"))?;
    let outward = -inward;
    let thick = spec.thickness;
    let lip = spec.lip.as_ref().map_or(0.0, |l| l.height);
    let (half, middle) = ((lift + thick + lip) / 2.0, (lift - thick - lip) / 2.0);
    let mut out = Vec::with_capacity(11);
    if let Some(l) = &spec.lip {
        let (p, n) = on(surface, across(a, b, l.width / spec.width))?;
        out.push((p + n * (-thick - FACE_MARGIN), V_EDGE));
        out.push((p_a + n_a * (-thick - lip), V_EDGE));
    }
    let reach = (BULGE.0 * bulge).max(BULGE.1);
    for j in [3.0_f64, 2.0, 1.0] {
        let theta = std::f64::consts::FRAC_PI_2 - std::f64::consts::PI * j / 4.0;
        out.push((p_a + outward * (reach * theta.cos()) + n_a * (middle + half * theta.sin()), V_EDGE));
    }
    for f in FRACS {
        let bevel = sstep(BEVEL_FROM.0, BEVEL_FROM.1, f) * spec.bevel;
        let (p, n) = on(surface, across(a, b, f))?;
        out.push((p + n * (lift * (1.0 - bevel) + FACE_MARGIN), V_EDGE + V_FACE * f));
    }
    Ok(out)
}

/// The strip `spec` describes on `surface`: one rung per rail sample, an open sheet (closed at its ends with flat caps
/// when `caps`). Row `i` of the mesh is rung `i`, so vertex `i * rung + k` is vertex `k` of rung `i`.
///
/// # Errors
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation or
/// a band with no width on the surface.
pub fn build(surface: &Surface, spec: &Piping) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, spec.rail.step)?;
    let points = rail.points();
    let last = points.len() - 1;
    let sign = if spec.flip { -1.0 } else { 1.0 };
    let ramp = f64::from(spec.ramp);
    let mut rungs = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let a = [p.t, p.phi];
        let b = param_offset(surface, a, p.side * sign, spec.width)?;
        #[allow(clippy::cast_precision_loss)] // rung indices are small
        let end = i.min(last - i) as f64;
        let bulge = if ramp > 0.0 { (end / ramp).clamp(0.0, 1.0) * spec.bulge } else { spec.bulge };
        rungs.push(rung(surface, spec, (a, b), (bulge, spec.rail.lift))?);
    }
    let cols = rungs[0].len();
    let grid = Grid::from_fn(rungs.len(), cols, |i, j| rungs[i][j].0)?;
    // Below the cloth, under the band: the sheet faces away from it.
    let inside = |i: usize, _: usize| points[i].on_surface - points[i].normal * (spec.thickness * 4.0);
    let uv = |i: usize, j: usize| DVec2::new(points[i].distance / spec.tile, rungs[i][j].1);
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let options = GridOptions {
        wrap_i: rail.closed(),
        inside: Some(&inside),
        uv: Some(&uv),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    if spec.caps && !rail.closed() {
        for (i, outward) in [(0, -points[0].tangent), (last, points[last].tangent)] {
            let ring: Vec<DVec3> = rungs[i].iter().map(|(p, _)| *p).collect();
            fan(&mut builder, &ring, outward, &points[i].skin)?;
        }
    }
    Ok(builder)
}

/// Closes a flat ring with a fan from its centroid facing `outward` (the prototype's end caps).
fn fan(builder: &mut MeshBuilder, ring: &[DVec3], outward: DVec3, skin: &[(JointRef, f32)]) -> Result<(), TrimError> {
    let centre = ring.iter().copied().sum::<DVec3>() / f64::from(u32::try_from(ring.len()).unwrap_or(u32::MAX));
    let mut newell = DVec3::ZERO;
    for (k, p) in ring.iter().enumerate() {
        newell += p.cross(ring[(k + 1) % ring.len()]);
    }
    let mut ordered = ring.to_vec();
    if newell.dot(outward) < 0.0 {
        ordered.reverse();
    }
    // The centre first, then the ring round and back to its start: a fan from the centre.
    let mut points = vec![centre];
    points.extend(ordered.iter().copied());
    points.push(ordered[0]);
    builder.add_polygon(&points, outward, [1.0; 4], skin)?;
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------ cords

/// The vest piping's radius, metres.
pub const VEST_PIPING: f64 = 0.0021;
/// Segments round a cord, as the vest's piping has.
pub const CORD_SEGMENTS: u32 = 6;
/// The most segments round a cord.
pub const MAX_CORD_SEGMENTS: u32 = 32;

/// A round cord along a rail: piping round a closed rail (a vest's rim and hem), a raised seam along an open one (its
/// ends taper to points).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cord {
    /// Where its centre runs.
    pub rail: RailSpec,
    /// Its radius, metres.
    pub radius: f64,
    /// Segments round it, 3 to [`MAX_CORD_SEGMENTS`].
    #[serde(default = "cord_segments")]
    pub segments: u32,
    /// What it is made of.
    #[serde(default = "cream")]
    pub finish: Finish,
}

fn cord_segments() -> u32 {
    CORD_SEGMENTS
}

impl Validate for Cord {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        feature(c, "radius", self.radius);
        if !(3..=MAX_CORD_SEGMENTS).contains(&self.segments) {
            c.fail("segments", format!("{}: a cord has 3 to {MAX_CORD_SEGMENTS} segments round", self.segments));
        }
        c.nested("finish", &self.finish);
    }
}

/// A closed round tube through the centres `points` (the last repeating the first), each ring square to the path, its
/// first point toward the point's `normal` with the path's direction taken out (the prototype's `ringTube`).
///
/// # Errors
/// [`TrimError::Invalid`] for fewer than 4 points or two that coincide; the kit's errors.
pub fn ring_tube(points: &[RailPoint], radius: f64, segments: usize) -> Result<MeshBuilder, TrimError> {
    let n = points.len();
    if n < 4 {
        return Err(TrimError::invalid("cord", format!("{n} points: a closed cord needs 4")));
    }
    let last = n - 1;
    let mut frames = Vec::with_capacity(n);
    for (k, p) in points.iter().enumerate() {
        let (before, after) = if k == 0 || k == last { (last - 1, 1) } else { (k - 1, k + 1) };
        let tangent = (points[after].point - points[before].point)
            .try_normalize()
            .ok_or_else(|| TrimError::invalid("cord", "two points of the loop coincide"))?;
        let normal = (p.normal - tangent * p.normal.dot(tangent)).try_normalize().unwrap_or(DVec3::Y);
        frames.push((normal, tangent.cross(normal)));
    }
    let grid = Grid::from_fn(n, segments + 1, |i, j| {
        let a = TAU * fraction(j % segments, segments);
        let (normal, binormal) = frames[i];
        points[i].point + normal * (radius * a.cos()) + binormal * (radius * a.sin())
    })?;
    let inside = |i: usize, _: usize| points[i].point;
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}

/// The cord `spec` describes on `surface`: a [`ring_tube`] round a closed rail, the kit's tapering `tube` along an
/// open one (its tips capped, so it is closed).
///
/// # Errors
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation.
pub fn build_cord(surface: &Surface, spec: &Cord) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, spec.rail.step)?;
    let segments = usize::try_from(spec.segments).map_err(|_| TrimError::invalid("segments", "too many"))?;
    if rail.closed() {
        return ring_tube(rail.points(), spec.radius, segments);
    }
    let points = rail.points();
    let centres: Vec<DVec3> = points.iter().map(|p| p.point).collect();
    let swept = tube(&centres, |_| spec.radius, segments)?;
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let inside = |i: usize, _: usize| swept.centres[i];
    let options = GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&swept.grid, &options)?;
    let last = points.len() - 1;
    for (i, outward) in [(0, -points[0].tangent), (last, points[last].tangent)] {
        let ring: Vec<DVec3> = (0..segments).map(|j| swept.grid.at(i, j)).collect();
        fan(&mut builder, &ring, outward, &points[i].skin)?;
    }
    Ok(builder)
}
