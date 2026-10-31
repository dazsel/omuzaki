//! The head mesh (port of `buildHeadGeo` and `mergeSeam` in the prototype's `head.js`, lines 48 to 80 and 145 to 157, with
//! the lower face redesigned).
//!
//! A sphere grid of directions (`rings + 1` rows of theta from the top pole to the bottom pole, `segments + 1` columns of
//! phi from the front once round, the last column repeating the first) is pushed out to the cranium's radius, then the
//! lower face is reshaped to the plausible outline: the chin stretched onto its point and every height scaled to the
//! outline's width ([`super::outline`]). Normals are the area-weighted
//! average of the faces around each vertex (what three.js's `computeVertexNormals` does); the copies of a vertex on the
//! phi seam share one normal, and each pole has the normal straight up or down, so the lighting is seamless.
//!
//! The mesh is in bind space (the head-local points plus `cranium.centre()`), unskinned and without UVs: the head part
//! weights it to the head and neck bones, and the painted face is projected from `HeadParams::face_projection`.
//!
//! [`build_head`] also returns the surface of the head as built, a [`HeadSurface`] that what sits on the head (hair
//! roots, ears, the eyes) samples, so it lands on the mesh rather than on the unreshaped cranium.

use std::f64::consts::PI;
use std::sync::Arc;

use omuzaki_char_api::{HeadParams, HeadSurface, Quality};
use omuzaki_core::glam::{DMat2, DVec2, DVec3, Vec3};
use omuzaki_core::mesh::{MeshData, MeshError};
use omuzaki_face_api::{ParamErrors, Validate};

use super::cranium::{Cranial, Trace, TraceSample, radius_traced, unit};
use super::outline::{LowerFace, plausible, reshape};
use super::sculpt::{Landmarks, Sculptor, warp};
use crate::kit::curve::{TAU, fraction};

/// Why a head mesh could not be built.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum HeadMeshError {
    /// The parameters are out of range.
    #[error("invalid head parameters: {0}")]
    Params(#[from] ParamErrors),
    /// The grid is too coarse to close a head, or too fine to index.
    #[error("a head grid of {segments} x {rings} is outside the limits of `Resolution`")]
    Resolution {
        /// Columns of phi.
        segments: u16,
        /// Rows of theta.
        rings: u16,
    },
    /// The traced map produced a radius that is not finite, so the mesh would be broken.
    #[error(transparent)]
    Mesh(#[from] MeshError),
}

/// How finely the head is tessellated.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// Columns of phi once round the head, from the front.
    pub segments: u16,
    /// Rows of theta from the top pole to the bottom pole.
    pub rings: u16,
}

impl Resolution {
    /// The prototype's grid: 128 x 96, 12 513 vertices and 24 576 triangles.
    pub const LIVE: Self = Self { segments: 128, rings: 96 };
    /// Twice as fine in each direction, for offline renders and export.
    pub const OFFLINE: Self = Self { segments: 256, rings: 192 };
    /// The fewest segments that close a head.
    pub const MIN_SEGMENTS: u16 = 3;
    /// The fewest rings that make a head with a body between its poles.
    pub const MIN_RINGS: u16 = 2;
    /// The most segments or rings: far finer than any use, and keeps the vertex count inside a `u32` index.
    pub const MAX_SIDE: u16 = 1024;

    /// The grid for a build quality.
    pub const fn of(quality: Quality) -> Self {
        match quality {
            Quality::Live => Self::LIVE,
            Quality::Offline => Self::OFFLINE,
        }
    }

    fn is_valid(self) -> bool {
        (Self::MIN_SEGMENTS..=Self::MAX_SIDE).contains(&self.segments)
            && (Self::MIN_RINGS..=Self::MAX_SIDE).contains(&self.rings)
    }
}

/// Builds the head mesh for `params` at `resolution`, with `trace` (the traced radius map, mixed in as
/// `params.traced.mix` says) blended into the cranium where it has data.
///
/// Vertex `(ring i, segment j)` is index `i * (segments + 1) + j`; each cell is two triangles `(a, b, a+1)` and
/// `(b, b+1, a+1)` with `b` one ring below `a`, counter-clockwise from outside.
///
/// # Errors
/// [`HeadMeshError::Params`] for out-of-range parameters, [`HeadMeshError::Resolution`] for a grid outside
/// [`Resolution`]'s limits, [`HeadMeshError::Mesh`] if the result is not a valid mesh (a trace that returns NaN).
pub fn build(
    params: &HeadParams,
    resolution: Resolution,
    trace: Option<&dyn TraceSample>,
) -> Result<MeshData, HeadMeshError> {
    build_parts(params, None, resolution, trace).map(|(mesh, _)| mesh)
}

/// A head as built: its mesh and the surface of that mesh.
#[derive(Debug)]
pub struct BuiltHead {
    /// The head mesh, bind space, as [`build`] makes it.
    pub mesh: MeshData,
    /// The head's outer surface as built: the cranium (with its trace) after the lower face is reshaped. A point placed
    /// on it lies on [`BuiltHead::mesh`] to within the mesh's faceting (hundredths of a millimetre on the Live grid).
    /// The head part publishes it, so hair roots, ears and the eyes sit on the head as it is drawn.
    pub surface: Arc<dyn HeadSurface>,
}

/// [`build`], with the built head's surface. The map is taken by `Arc` because the surface outlives the build (hair
/// and accessories keep it).
///
/// # Errors
/// As [`build`].
pub fn build_head(
    params: &HeadParams,
    resolution: Resolution,
    trace: Option<Arc<dyn TraceSample>>,
) -> Result<BuiltHead, HeadMeshError> {
    build_sculpted(params, None, resolution, trace)
}

/// [`build_head`] with the face's forms sculpted in ([`HeadParams::sculpt`]) where `landmarks` place them: the head
/// part passes the painted face's. Without landmarks, or with a flat sculpt, it is exactly [`build_head`].
///
/// # Errors
/// As [`build`].
pub fn build_sculpted(
    params: &HeadParams,
    landmarks: Option<Landmarks>,
    resolution: Resolution,
    trace: Option<Arc<dyn TraceSample>>,
) -> Result<BuiltHead, HeadMeshError> {
    let sculptor = landmarks.and_then(|at| Sculptor::new(&params.sculpt, at));
    let (mesh, face) = build_parts(params, sculptor.as_ref(), resolution, trace.as_deref())?;
    let surface = BuiltSurface { cranium: Cranial::new(params, trace), face, sculptor };
    Ok(BuiltHead { mesh, surface: Arc::new(surface) })
}

/// The mesh and the lower face's reshaping it was built with (`None` when the outline leaves the cranium alone), with
/// the face's forms sculpted in after the reshaping when there is a `sculptor`.
fn build_parts(
    params: &HeadParams,
    sculptor: Option<&Sculptor>,
    resolution: Resolution,
    trace: Option<&dyn TraceSample>,
) -> Result<(MeshData, Option<LowerFace>), HeadMeshError> {
    params.validate()?;
    if !resolution.is_valid() {
        return Err(HeadMeshError::Resolution { segments: resolution.segments, rings: resolution.rings });
    }
    let columns = usize::from(resolution.segments) + 1;
    let trace = trace.map(|map| Trace::of(map, params));
    let mut points = sphere_points(params, resolution, trace, sculptor.is_some());
    let radius = |dir: DVec3| radius_traced(&params.cranium, dir, trace);
    let face = reshape(&mut points, &radius, &plausible(&params.outline, &params.cranium));
    if let Some(sculptor) = sculptor {
        for p in &mut points {
            *p = sculptor.apply(*p);
        }
    }

    let centre = DVec3::from_array(params.cranium.centre());
    // Rounded to f32 once, here, like the prototype's Float32BufferAttribute: the normals come from what the mesh holds.
    let positions: Vec<Vec3> = points.iter().map(|p| (*p + centre).as_vec3()).collect();
    let indices = grid_indices(resolution);
    let mut normals = vertex_normals(&positions, &indices);
    merge_seam_and_poles(&mut normals, columns, usize::from(resolution.rings));
    let mesh = MeshData { positions, normals, indices, ..MeshData::default() };
    mesh.validate()?;
    Ok((mesh, face))
}

/// Newton steps [`BuiltSurface`] may take to meet a ray: it needs three or four on the reshaped lower face.
const SURFACE_STEPS: u32 = 24;
/// How far off the ray (radians, seen from the centre) an image may land and count as on it: far below an `f32`.
const SURFACE_TOLERANCE: f64 = 1e-10;
/// The angle (radians) the slope of the image is measured over.
const SURFACE_DELTA: f64 = 1e-6;
/// The largest Newton step (radians), so a step never leaves the part of the head it started on.
const SURFACE_MAX_STEP: f64 = 0.2;
/// How many times a Newton step that misses the ray by more than it started may be halved.
const SURFACE_HALVINGS: i32 = 8;

/// The surface of a built head. The mesh is the cranium's sphere grid moved by the lower face's reshaping, so a ray
/// from the centre meets it where the image of some cranium direction lies on the ray: [`BuiltSurface::radius_along`]
/// solves for that direction.
#[derive(Debug)]
struct BuiltSurface {
    cranium: Cranial,
    face: Option<LowerFace>,
    sculptor: Option<Sculptor>,
}

impl BuiltSurface {
    /// Where the cranium's unit direction `u` lands on the built head, head-local.
    fn image(&self, u: DVec3) -> DVec3 {
        let p = u * self.cranium.radius_at(u);
        let p = self.face.as_ref().map_or(p, |face| face.apply(p));
        self.sculptor.as_ref().map_or(p, |sculptor| sculptor.apply(p))
    }

    /// The distance from the centre to the built head along the unit direction `dir`.
    ///
    /// The reshaping moves a point only a little across the sphere of directions, so Newton's method in two angles
    /// about `dir`, starting from `dir` itself, finds the cranium direction whose image is on the ray. Above the
    /// reshaped lower face the image is the cranium itself and the first evaluation is already there. Should the steps
    /// stall (no head the tests try does), the image closest to the ray answers.
    fn radius_along(&self, dir: DVec3) -> f64 {
        let (across, up) = dir.any_orthonormal_pair();
        // The image of the direction `x` radians off `dir`, and where it lands seen from the centre, in those radians.
        let at = |x: DVec2| {
            let q = self.image((dir + across * x.x + up * x.y).normalize());
            let seen = q.normalize_or_zero();
            (q, DVec2::new(seen.dot(across), seen.dot(up)))
        };
        let mut x = DVec2::ZERO;
        let (mut image, mut miss) = at(x);
        for _ in 0..SURFACE_STEPS {
            if miss.length() < SURFACE_TOLERANCE {
                break;
            }
            let slope =
                |axis: DVec2| (at(x + axis * SURFACE_DELTA).1 - at(x - axis * SURFACE_DELTA).1) / (2.0 * SURFACE_DELTA);
            let jacobian = DMat2::from_cols(slope(DVec2::X), slope(DVec2::Y));
            if jacobian.determinant().abs() < f64::EPSILON {
                break;
            }
            let full = (jacobian.inverse() * miss).clamp_length_max(SURFACE_MAX_STEP);
            // Halve a step that lands farther off the ray than it started (a fold of the image near the chin's tip).
            let better = (0..SURFACE_HALVINGS)
                .map(|k| x - full * 0.5_f64.powi(k))
                .map(|next| (next, at(next)))
                .find(|(_, (_, next_miss))| next_miss.length() < miss.length());
            let Some((next, (next_image, next_miss))) = better else { break };
            (x, image, miss) = (next, next_image, next_miss);
        }
        image.dot(dir)
    }
}

impl HeadSurface for BuiltSurface {
    fn centre(&self) -> Vec3 {
        self.cranium.centre()
    }

    #[allow(clippy::cast_possible_truncation)] // the trait speaks f32; the radius is a fraction of a metre
    fn radius(&self, dir: Vec3) -> f32 {
        self.radius_along(unit(dir)) as f32
    }
}

/// The grid of directions pushed out to the cranium, head-local, row by row; `crowded` (a sculpted head) crowds the
/// rows and columns toward the face ([`warp`]).
fn sphere_points(params: &HeadParams, resolution: Resolution, trace: Option<Trace<'_>>, crowded: bool) -> Vec<DVec3> {
    let (segments, rings) = (usize::from(resolution.segments), usize::from(resolution.rings));
    let theta = |v: f64| if crowded { warp(0.0, v).1 } else { v };
    let phi = |u: f64| if crowded { warp(u, 0.0).0 } else { u };
    // sin and cos per ring and per column rather than per vertex: the same values, a tenth of the trigonometry.
    let rows: Vec<(f64, f64)> = (0..=rings).map(|i| theta(fraction(i, rings) * PI).sin_cos()).collect();
    let mut cols: Vec<(f64, f64)> = (0..=segments).map(|j| phi(fraction(j, segments) * TAU).sin_cos()).collect();
    // A crowded grid's seam columns are made the same direction to the bit, so the seam stays closed after the warp.
    if crowded && let Some(&first) = cols.first() {
        cols[segments] = first;
    }
    let mut points = Vec::with_capacity((rings + 1) * (segments + 1));
    for &(sin_theta, cos_theta) in &rows {
        for &(sin_phi, cos_phi) in &cols {
            // Theta runs from the top pole; phi from the front (+Z) toward +X.
            let dir = DVec3::new(sin_theta * sin_phi, cos_theta, sin_theta * cos_phi);
            points.push(dir * radius_traced(&params.cranium, dir, trace));
        }
    }
    points
}

/// The triangle list of the grid, in the prototype's order.
fn grid_indices(resolution: Resolution) -> Vec<u32> {
    // `Resolution::MAX_SIDE` keeps every index below 1025 * 1025, far inside a u32.
    let columns = u32::from(resolution.segments) + 1;
    let mut indices = Vec::with_capacity(usize::from(resolution.rings) * usize::from(resolution.segments) * 6);
    for i in 0..u32::from(resolution.rings) {
        for j in 0..columns - 1 {
            let a = i * columns + j;
            let b = a + columns;
            indices.extend([a, b, a + 1, b, b + 1, a + 1]);
        }
    }
    indices
}

/// Smooth vertex normals: each triangle adds its (unnormalised, so area-weighted) face normal to its three corners.
pub(crate) fn vertex_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut sums = vec![DVec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| positions[i as usize].as_dvec3());
        let face = (c - b).cross(a - b);
        for &i in tri {
            sums[i as usize] += face;
        }
    }
    sums.into_iter().map(|n| n.normalize_or_zero().as_vec3()).collect()
}

/// Averages the normals of the two copies of each vertex on the phi seam, and points every vertex of the poles straight
/// up (top) or down (bottom), so lighting has no seam and no pinch.
fn merge_seam_and_poles(normals: &mut [Vec3], columns: usize, rings: usize) {
    for row in normals.chunks_exact_mut(columns) {
        let (first, last) = (0, columns - 1);
        let shared = ((row[first].as_dvec3() + row[last].as_dvec3()) / 2.0).normalize_or_zero().as_vec3();
        row[first] = shared;
        row[last] = shared;
    }
    for (ring, up) in [(0, Vec3::Y), (rings, Vec3::NEG_Y)] {
        normals[ring * columns..(ring + 1) * columns].fill(up);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_grid_is_the_prototypes() {
        let mesh = build(&HeadParams::default(), Resolution::LIVE, None).unwrap();
        assert_eq!((mesh.positions.len(), mesh.triangle_count()), (129 * 97, 128 * 96 * 2));
        assert_eq!(Resolution::of(Quality::Live), Resolution::LIVE);
        assert_eq!(Resolution::of(Quality::Offline), Resolution::OFFLINE);
    }

    #[test]
    fn the_poles_are_on_the_axis_and_point_straight_out() {
        let p = HeadParams::default();
        let mesh = build(&p, Resolution::LIVE, None).unwrap();
        let columns = 129;
        let top = mesh.positions[0];
        assert!((top.y - (0.78 + 0.186)).abs() < 1e-6, "{top:?}");
        assert!(mesh.normals[..columns].iter().all(|n| *n == Vec3::Y));
        assert!(mesh.normals[mesh.normals.len() - columns..].iter().all(|n| *n == Vec3::NEG_Y));
        // The seam is welded: both copies share position (to rounding) and normal.
        for row in 1..96 {
            let (a, b) = (row * columns, row * columns + 128);
            assert!((mesh.positions[a] - mesh.positions[b]).length() < 1e-6, "ring {row}");
            assert_eq!(mesh.normals[a], mesh.normals[b], "ring {row}");
        }
    }

    #[test]
    fn a_bad_grid_or_bad_parameters_are_errors_not_panics() {
        let p = HeadParams::default();
        for (segments, rings) in [(2, 8), (8, 1), (0, 0), (4096, 8), (8, 4096), (u16::MAX, u16::MAX)] {
            let r = build(&p, Resolution { segments, rings }, None);
            assert!(matches!(r, Err(HeadMeshError::Resolution { .. })), "{segments} x {rings}");
        }
        let mut bad = HeadParams::default();
        bad.cranium.half_width = 0.0;
        assert!(matches!(build(&bad, Resolution::LIVE, None), Err(HeadMeshError::Params(_))));
        assert!(build(&p, Resolution { segments: 3, rings: 2 }, None).is_ok(), "the smallest grid still builds");
    }

    #[test]
    fn the_built_surface_passes_through_every_vertex_of_the_mesh() {
        use omuzaki_char_api::{Chin, HeadOutline};
        let outlines = [
            HeadOutline::default(),
            HeadOutline { chin: Some(Chin::default()), cheek_puff: 0.01, ..HeadOutline::default() },
            // A long stretched chin, a narrow pointed face and a wide square one.
            HeadOutline { chin: Some(Chin { y: -0.40, stretch_top: -0.06, min_ratio: 0.5 }), ..HeadOutline::default() },
            HeadOutline { half_width: 0.08, exponent: 1.0, height: 0.2, ..HeadOutline::default() },
            HeadOutline { half_width: 0.25, exponent: 4.0, top: -0.12, ..HeadOutline::default() },
        ];
        for outline in outlines {
            let params = HeadParams { outline, ..HeadParams::default() };
            let built = build_head(&params, Resolution::LIVE, None).unwrap();
            assert_eq!(built.mesh, build(&params, Resolution::LIVE, None).unwrap(), "the same mesh either way");
            let centre = built.surface.centre();
            let worst = built
                .mesh
                .positions
                .iter()
                .map(|p| (built.surface.radius(*p - centre) - (*p - centre).length()).abs())
                .fold(0.0, f32::max);
            // The vertices are the images of the grid; f32 rounding of a 1 m position is about 1e-7 m.
            assert!(worst < 1e-5, "{outline:?}: a vertex is {worst:e} m off the built surface");
        }
    }

    #[test]
    fn the_built_surface_is_the_cranium_above_the_reshaped_lower_face() {
        let params = HeadParams::default();
        let built = build_head(&params, Resolution::LIVE, None).unwrap();
        let cranium = super::super::cranium::surface(&params, None);
        for dir in [Vec3::Y, Vec3::new(0.3, 0.8, 0.5).normalize(), Vec3::new(-0.9, 0.2, -0.2).normalize()] {
            assert_eq!(built.surface.radius(dir), cranium.radius(dir), "{dir:?}");
        }
        // At the side, level with the ears, the reshaped face is inside the cranium.
        let side = Vec3::new(0.95, -0.066 / 0.19, -0.06).normalize();
        assert!(built.surface.radius(side) < cranium.radius(side) - 0.004);
    }

    #[derive(Debug)]
    struct Broken;

    impl TraceSample for Broken {
        fn sample(&self, _: Vec3) -> (f32, f32) {
            (f32::NAN, 1.0)
        }
    }

    #[test]
    fn a_trace_that_returns_nan_is_an_error() {
        let r = build(&HeadParams::default(), Resolution::LIVE, Some(&Broken));
        assert!(matches!(r, Err(HeadMeshError::Mesh(_))), "{r:?}");
    }
}
