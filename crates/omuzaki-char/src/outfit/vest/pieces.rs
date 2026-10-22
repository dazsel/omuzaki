//! The vest's pieces, added to one coloured mesh in the prototype's order: the outer shell, the lining, the rim and hem
//! piping, then the seams (with their end caps last, so the vertex order up to the caps is the prototype's).

use std::f64::consts::PI;

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};

use super::profile::{Profile, along};
use crate::kit::curve::{TAU, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;
use crate::outfit::common::GarmentError;
use crate::outfit::loft::Surface;
use crate::outfit::trim::TrimError;
use crate::outfit::trim::buttons::{ButtonStyle, button_at};
use crate::outfit::trim::piping::{CORD_SEGMENTS, VEST_PIPING};

/// Intervals down the shell, hem to rim (the prototype's `NS`: 35 rows).
pub const SHELL_INTERVALS: usize = 34;
/// Intervals round the shell (the prototype's `NJ`: 85 columns, the last the first again).
pub const SHELL_SEGMENTS: usize = 84;
/// The shell is darkest below this height, as a fraction of the torso (the prototype's `0.34` m): it reads as a fitted
/// garment.
const SHADE_FROM: f64 = along(0.34);
/// The shell is lightest above this height (the prototype's `0.44` m).
const SHADE_TO: f64 = along(0.44);
/// The vertex colour of the outer shell, before the shading: a little blue.
const OUTER: [f32; 3] = [1.0, 1.0, 1.02];
/// The vertex colour of the lining.
const LINING: [f32; 4] = [0.8, 0.8, 0.82, 1.0];
/// The vertex colour of the piping. Above 1 on purpose: the cords read lighter than the cloth they sit on, so vertex
/// colours must stay unclamped.
const PIPING: [f32; 4] = [2.6, 2.5, 2.8, 1.0];
/// The vertex colour of the seams (above 1 for the same reason).
const SEAMS: [f32; 4] = [2.4, 2.3, 2.6, 1.0];
/// The radius of a seam cord, metres.
const SEAM_RADIUS: f64 = 0.0007;
/// Segments round a seam cord.
const SEAM_SEGMENTS: usize = 5;
/// How far the seams stand off the shell's outside, metres.
pub const SEAM_LIFT: f64 = 0.0003;

/// The skin of a point at height `t`.
type Skinning = Vec<(JointRef, f32)>;

/// One seam: a straight line in `(angle, height)` from `from` to `to`, each end `(phi, f)` with `f` the fraction of
/// the way from the hem (0) to the rim (1) there, sampled in `segments` steps. Placeholders fitted to the soft-sd body:
/// they follow the outline when it changes.
#[derive(Clone, Copy)]
struct Seam {
    from: (f64, f64),
    to: (f64, f64),
    segments: usize,
}

/// The centre front seam: the V tip to the hem tip.
const CENTRE_FRONT: Seam = Seam { from: (0.0, 1.0), to: (0.0, 0.0), segments: 12 };
/// A front dart (princess seam): from under the V's shoulder to just above the hem; mirrored for the other side.
const DART: Seam = Seam { from: (0.80, 0.761_680_965_828_592_8), to: (0.60, 0.016_664_088_591_570_227), segments: 14 };
/// A side seam; mirrored.
const SIDE: Seam = Seam { from: (1.50, 0.656_534_954_483_522_8), to: (1.50, 0.012_158_054_711_246_213), segments: 10 };
/// The centre back seam.
const BACK: Seam = Seam { from: (PI, 0.943_037_974_683_544_6), to: (PI, 0.006_329_113_924_050_637), segments: 12 };

/// The surfaces a vest is cut from, all of the torso at different offsets from the fit under it.
pub struct Surfaces {
    /// The outside of the shell.
    pub outer: Surface,
    /// The lining, on the inside.
    pub inner: Surface,
    /// Half way between: where the piping's centres run.
    pub mid: Surface,
    /// Just outside the shell, for the seams.
    pub seam: Surface,
}

impl Surfaces {
    /// How far the shell's outside stands over the lining: the cloth's thickness, metres.
    pub fn thickness(&self) -> f64 {
        self.outer.offset() - self.inner.offset()
    }
}

/// The points of a shell grid for `surface`: row `i` of the `SHELL_INTERVALS + 1` runs from the hem to the rim of each
/// column, `t[i][j]` heights along the torso.
fn shell_points(profile: &Profile, surface: &Surface, phis: &[f64]) -> Result<(Vec<f64>, Vec<DVec3>), GarmentError> {
    let cols = phis.len();
    let mut heights = Vec::with_capacity((SHELL_INTERVALS + 1) * cols);
    let mut points = Vec::with_capacity((SHELL_INTERVALS + 1) * cols);
    for i in 0..=SHELL_INTERVALS {
        let s = fraction(i, SHELL_INTERVALS);
        for &phi in phis {
            let t = profile.between(phi, s);
            heights.push(t);
            points.push(surface.point(t, phi)?);
        }
    }
    Ok((heights, points))
}

/// UVs that count fabric tiles of `tile` metres along the shell's rows and columns, each row rounded to whole tiles so
/// the seam at the back matches.
fn tile_uvs(grid: &Grid, tile: f64) -> Vec<DVec2> {
    let (rows, cols) = (grid.rows(), grid.cols());
    let mut uvs = vec![DVec2::ZERO; rows * cols];
    let mut v = vec![0.0; cols];
    for i in 0..rows {
        let mut along_row = vec![0.0; cols];
        for j in 1..cols {
            along_row[j] = along_row[j - 1] + grid.at(i, j).distance(grid.at(i, j - 1));
        }
        let width = along_row[cols - 1];
        let scale = if width > 0.0 { (width / tile).round().max(1.0) / width } else { 0.0 };
        for j in 0..cols {
            if i > 0 {
                v[j] += grid.at(i, j).distance(grid.at(i - 1, j)) / tile;
            }
            uvs[i * cols + j] = DVec2::new(along_row[j] * scale, v[j]);
        }
    }
    uvs
}

/// Adds the outer shell and then the lining (the same grid pushed in, wound inward).
///
/// # Errors
/// The surfaces' errors; the kit's.
#[allow(clippy::cast_possible_truncation)] // vertex colours are f32
pub fn add_shell(
    builder: &mut MeshBuilder,
    profile: &Profile,
    surfaces: &Surfaces,
    tile: Option<f64>,
) -> Result<(), GarmentError> {
    let phis = profile.columns(SHELL_SEGMENTS);
    let cols = phis.len();
    let surface = &surfaces.outer;
    let (heights, outer) = shell_points(profile, surface, &phis)?;
    let rings = heights.iter().map(|&t| surface.ring(t)).collect::<Result<Vec<_>, _>>()?;
    let centres: Vec<DVec3> = rings.iter().map(|r| r.centre).collect();
    let skins: Vec<Skinning> = heights.iter().map(|&t| surface.weights().at(t)).collect();
    // The lining is the shell pushed in by the cloth's thickness, along the ray from its section's centre in the
    // section's plane: the two lie the thickness apart, and the lining is never nearer the layer below than its ease.
    let thickness = surfaces.thickness();
    let lining_points: Vec<DVec3> = outer
        .iter()
        .zip(&rings)
        .map(|(p, ring)| {
            let d = *p - ring.centre;
            let d = d - ring.axis * d.dot(ring.axis);
            *p - d.normalize_or_zero() * thickness
        })
        .collect();
    for (points, lining) in [(outer, false), (lining_points, true)] {
        let grid = Grid::new(SHELL_INTERVALS + 1, cols, points)?;
        let uvs = tile.filter(|t| t.is_finite() && *t > 0.0).map(|t| tile_uvs(&grid, t));
        let inside = |i: usize, j: usize| centres[i * cols + j];
        let uv = |i: usize, j: usize| uvs.as_ref().map_or(DVec2::ZERO, |u| u[i * cols + j]);
        let uv_fn: Option<&dyn Fn(usize, usize) -> DVec2> = if uvs.is_some() { Some(&uv) } else { None };
        let skin = |_: DVec3, i: usize, j: usize| skins[i * cols + j].clone();
        let colour = |i: usize, j: usize, _: DVec3| {
            if lining {
                LINING
            } else {
                let k = (0.9 + 0.1 * sstep(SHADE_FROM, SHADE_TO, heights[i * cols + j])) as f32;
                [k * OUTER[0], k * OUTER[1], k * OUTER[2], 1.0]
            }
        };
        let options = GridOptions {
            wrap_j: true,
            invert: lining,
            inside: Some(&inside),
            uv: uv_fn,
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        builder.add_grid(&grid, &options)?;
    }
    Ok(())
}

/// A closed round tube through `centres` (the loop is closed by repeating the first), each ring square to the path
/// with its first point turned away from the vertical axis through the origin (the prototype's `ringTube`, which this
/// repeats because the library's takes no vertex colours).
fn loop_tube(builder: &mut MeshBuilder, centres: &[DVec3], skins: &[Skinning]) -> Result<(), GarmentError> {
    let n = centres.len();
    let frame = |k: usize| -> Result<(DVec3, DVec3), GarmentError> {
        let (p, a, b) = (centres[k % n], centres[(k + n - 1) % n], centres[(k + 1) % n]);
        let tangent = (b - a).try_normalize().ok_or_else(|| GarmentError::invalid("piping", "two points coincide"))?;
        let radial = DVec3::new(p.x, 0.0, p.z).try_normalize().unwrap_or(DVec3::Z);
        let normal = (radial - tangent * radial.dot(tangent)).try_normalize().unwrap_or(DVec3::Y);
        Ok((normal, tangent.cross(normal).normalize_or_zero()))
    };
    let frames = (0..=n).map(frame).collect::<Result<Vec<_>, _>>()?;
    let segments = CORD_SEGMENTS as usize;
    let grid = Grid::from_fn(n + 1, segments + 1, |i, j| {
        let a = TAU * fraction(j % segments, segments);
        let (normal, binormal) = frames[i];
        centres[i % n] + normal * (VEST_PIPING * a.cos()) + binormal * (VEST_PIPING * a.sin())
    })?;
    let inside = |i: usize, _: usize| centres[i % n];
    let skin = |_: DVec3, i: usize, _: usize| skins[i % n].clone();
    let colour = |_: usize, _: usize, _: DVec3| PIPING;
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// Adds the piping round the rim and then round the hem.
///
/// # Errors
/// The surfaces' errors; [`GarmentError::Invalid`] for a loop whose points coincide; the kit's.
pub fn add_piping(builder: &mut MeshBuilder, profile: &Profile, mid: &Surface) -> Result<(), GarmentError> {
    let mut phis = profile.columns(SHELL_SEGMENTS);
    phis.pop();
    for edge in [Profile::top, Profile::hem] {
        let heights: Vec<f64> = phis.iter().map(|&phi| edge(profile, phi)).collect();
        let centres = heights.iter().zip(&phis).map(|(&t, &phi)| mid.point(t, phi)).collect::<Result<Vec<_>, _>>()?;
        let skins: Vec<Skinning> = heights.iter().map(|&t| mid.weights().at(t)).collect();
        loop_tube(builder, &centres, &skins)?;
    }
    Ok(())
}

/// The seams in the prototype's order: the centre front, then for the left and right a dart and a side seam, then the
/// centre back. Their tips are tapered to 1% of the cord, which leaves an open ring; the rings are closed with caps
/// added last.
///
/// # Errors
/// The surfaces' errors; the kit's.
pub fn add_seams(builder: &mut MeshBuilder, profile: &Profile, seam: &Surface) -> Result<(), GarmentError> {
    let mut caps = Vec::new();
    let mirrored = |s: &Seam, side: f64| Seam { from: (side * s.from.0, s.from.1), to: (side * s.to.0, s.to.1), ..*s };
    let mut seams = vec![CENTRE_FRONT];
    for side in [-1.0, 1.0] {
        seams.push(mirrored(&DART, side));
        seams.push(mirrored(&SIDE, side));
    }
    seams.push(BACK);
    for s in &seams {
        let steps: Vec<(f64, f64)> = (0..=s.segments)
            .map(|k| {
                let u = fraction(k, s.segments);
                let phi = lerp(s.from.0, s.to.0, u);
                let t = lerp(profile.between(s.from.0, s.from.1), profile.between(s.to.0, s.to.1), u);
                (t, phi)
            })
            .collect();
        let points = steps.iter().map(|&(t, phi)| seam.point(t, phi)).collect::<Result<Vec<_>, _>>()?;
        let skins: Vec<Skinning> = steps.iter().map(|&(t, _)| seam.weights().at(t)).collect();
        let swept = tube(&points, |_| SEAM_RADIUS, SEAM_SEGMENTS)?;
        let (centres, skin_rows) = (&swept.centres, &skins);
        let inside = |i: usize, _: usize| centres[i];
        let skin = |_: DVec3, i: usize, _: usize| skin_rows[i].clone();
        let colour = |_: usize, _: usize, _: DVec3| SEAMS;
        let options = GridOptions {
            wrap_j: true,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        builder.add_grid(&swept.grid, &options)?;
        let last = points.len() - 1;
        for (row, outward) in [(0, points[0] - points[1]), (last, points[last] - points[last - 1])] {
            let ring: Vec<DVec3> = (0..SEAM_SEGMENTS).map(|j| swept.grid.at(row, j)).collect();
            caps.push((ring, outward, skins[row].clone()));
        }
    }
    for (ring, outward, skin) in caps {
        add_cap(builder, &ring, outward, &skin)?;
    }
    Ok(())
}

/// Closes a small ring facing `outward` with a fan, wound to face that way.
fn add_cap(
    builder: &mut MeshBuilder,
    ring: &[DVec3],
    outward: DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), GarmentError> {
    let facing = ring.iter().enumerate().map(|(k, p)| p.cross(ring[(k + 1) % ring.len()])).sum::<DVec3>();
    let mut ordered = ring.to_vec();
    if facing.dot(outward) < 0.0 {
        ordered.reverse();
    }
    builder.add_polygon(&ordered, outward.try_normalize().unwrap_or(DVec3::Y), SEAMS, skin)?;
    Ok(())
}

/// The gold buttons: `count` of them down the centre front, `spacing` metres apart along the torso, the first at
/// fraction `first` of the way from the hem tip up to the V tip.
///
/// # Errors
/// [`TrimError::Invalid`] if the row doesn't fit between the hem and the V tip; the surface's and the kit's errors.
pub fn buttons(
    surface: &Surface,
    profile: &Profile,
    torso_length: f64,
    count: u32,
    spacing: f64,
    first: f64,
    style: &ButtonStyle,
) -> Result<MeshBuilder, TrimError> {
    let top = profile.between(0.0, first);
    let step = spacing / torso_length;
    let last = top - step * f64::from(count.saturating_sub(1));
    let clear = style.radius / torso_length;
    if count > 0 && last - clear < profile.hem(0.0) {
        return Err(TrimError::invalid(
            "buttons",
            format!(
                "{count} buttons {spacing} m apart from fraction {first} of the front panel run below the hem: use \
                 fewer, closer together, or start them higher"
            ),
        ));
    }
    let mut out = MeshBuilder::new();
    for k in 0..count {
        out.merge(&button_at(surface, [top - step * f64::from(k), 0.0], style)?)?;
    }
    Ok(out)
}
