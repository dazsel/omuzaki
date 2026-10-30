//! Tube geometry: a clump's tapered lens-shaped tube with the `hair-uv` and `hair-info` attributes the `hair` shading
//! program reads (port of `tubeInto` and `tubeIndex`, `parts/hair.js`).
//!
//! **The tube.** Each rest point carries a ring of [`RING`] vertices on a lens: [`Taper`] wide across the lock, [`Bulge`]
//! thick through it, with a flat front and back and a rounded spine (`yy = sy x th x (0.55 + 0.45 sqrt(1 - cx^2))`). The
//! ring's thin axis is the clump's *outward* direction ([`OutwardFrame`]), so the wide face of a lock lies along the
//! surface it hangs on. Normals come from the ellipse. Rings are joined by quads wound so that the triangles face out of
//! the closed tube: a lock is a closed lens, so the renderer's inverted-hull outline (back faces only) finds a back face
//! from every side, which a flat one-layer ribbon cannot give it.
//!
//! **Two entry points.** [`tube_vertices`] writes positions and normals into caller-owned slices without allocating:
//! the runtime calls it every step on the posed particles. [`TubeMesh`] builds a whole mesh at rest, with the
//! attributes and vertex colours.
//!
//! The vertex data is `f32`; the prototype's is `f64`, and the parity test holds them to rounding noise.

use omuzaki_char_api::{Bulge, ExtraAttribute, HAIR_INFO, HAIR_UV, Quality, Taper};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::{Slug, SlugError};
use omuzaki_core::mesh::MeshData;

use crate::hair::strands::{self, StrandMap};

/// Vertices in one ring of a live tube (the prototype's `K`).
pub const RING: usize = 8;
/// Vertices in one ring of an offline tube: twice the live ring, so a lock's lens reads round in a close-up instead of
/// as an octagon.
pub const RING_OFFLINE: usize = 16;

/// The ring size for `quality`: [`RING`] live, [`RING_OFFLINE`] for offline renders and export.
pub fn ring_for(quality: Quality) -> usize {
    match quality {
        Quality::Live => RING,
        Quality::Offline => RING_OFFLINE,
    }
}

/// The vertices a tube's two caps add after its rings of `ring` vertices: a centre and a rim copy per end (see
/// `TubeMesh::push_cap`).
pub const fn cap_vertices(ring: usize) -> usize {
    2 * (ring + 1)
}

/// The vertices a live tube's two caps add.
pub const CAP_VERTICES: usize = cap_vertices(RING);

/// A smoothstep that is 0 at or below `lo`, 1 at or above `hi` (three.js `MathUtils.smoothstep`).
pub(crate) fn smoothstep(x: f32, lo: f32, hi: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Where "outward" is measured from, so a lock's thin axis points away from what it hangs on: from the head's centre
/// near the head, from the body's vertical axis further down (hair on the back hangs off the back, not off the skull).
///
/// The prototype's numbers are for its head at world height 0.778; here the blend zones are relative to the head centre.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct OutwardFrame {
    /// The head centre.
    pub head: Vec3,
    /// The body's vertical axis, as `(x, z)`.
    pub body_axis: Vec2,
}

/// Rest: the blend runs from this far below the head centre ...
const REST_BLEND_FROM: f32 = -0.178;
/// ... to this far below it.
const REST_BLEND_TO: f32 = -0.078;
/// Posed: the same blend, a little lower, for hair that has swung.
const POSED_BLEND_FROM: f32 = -0.20;
/// ... to this far below the head centre.
const POSED_BLEND_TO: f32 = -0.08;

impl OutwardFrame {
    /// The outward direction at `p` for hair at rest (`outwardRest`): the reference axis slides from the body axis to the
    /// head centre as `p` rises through the blend zone, and never rises above the head centre while below it.
    pub fn rest(&self, p: Vec3) -> Vec3 {
        let blend = smoothstep(p.y - self.head.y, REST_BLEND_FROM, REST_BLEND_TO);
        let mut axis = Vec3::new(
            self.body_axis.x,
            p.y.min(self.head.y),
            self.body_axis.y + (self.head.z - self.body_axis.y) * blend,
        );
        if blend > 0.0 {
            axis = axis.lerp(self.head, blend);
        }
        away_from(p, axis)
    }

    /// The outward direction at `p` for hair in motion (`outwardWorld`): `head` and `body_axis` are the posed head centre
    /// and chest axis. The axis keeps `p`'s height and slides from the chest axis to the head centre.
    pub fn posed(&self, p: Vec3) -> Vec3 {
        let blend = smoothstep(p.y - self.head.y, POSED_BLEND_FROM, POSED_BLEND_TO);
        let axis = Vec3::new(self.body_axis.x, p.y, self.body_axis.y).lerp(self.head, blend);
        away_from(p, axis)
    }
}

/// The unit direction from `axis` to `p`, or straight back (`-Z`) when they coincide.
fn away_from(p: Vec3, axis: Vec3) -> Vec3 {
    (p - axis).try_normalize().unwrap_or(Vec3::NEG_Z)
}

/// Why a tube could not be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TubeError {
    /// A tube needs two points to have a direction.
    #[error("a tube needs at least 2 points, got {0}")]
    TooFewPoints(usize),
    /// The output slices are not a ring of vertices per point, or the ring has fewer than three.
    #[error("{points} points need {expected} output vertices, got {got}")]
    WrongOutputLength {
        /// Points in the polyline.
        points: usize,
        /// `points * ring`.
        expected: usize,
        /// What the caller gave.
        got: usize,
    },
    /// More vertices than a mesh can index.
    #[error("a mesh can hold at most 2^32 vertices")]
    TooMany,
    /// A contract attribute name stopped being a valid slug.
    #[error("attribute name: {0}")]
    Name(#[from] SlugError),
}

/// Writes the ring vertices and normals of the tube along `points` into `positions` and `normals`, `ring` per point
/// ([`RING`] live), ring after ring. Allocates nothing. `outward` gives the direction the lens's thin axis faces at each
/// point.
///
/// # Errors
/// [`TubeError::TooFewPoints`] for fewer than two points; [`TubeError::WrongOutputLength`] if either output slice is not
/// `points.len() * ring` long, or `ring` is below 3.
pub fn tube_vertices(
    points: &[Vec3],
    ring: usize,
    width: &Taper,
    thickness: &Bulge,
    outward: impl Fn(Vec3) -> Vec3,
    positions: &mut [Vec3],
    normals: &mut [Vec3],
) -> Result<(), TubeError> {
    let n = points.len();
    if n < 2 {
        return Err(TubeError::TooFewPoints(n));
    }
    let expected = n * ring;
    if ring < 3 || positions.len() != expected || normals.len() != expected {
        return Err(TubeError::WrongOutputLength { points: n, expected, got: positions.len().min(normals.len()) });
    }
    let last = n - 1;
    for (i, ((ring_p, ring_n), p)) in
        positions.chunks_exact_mut(ring).zip(normals.chunks_exact_mut(ring)).zip(points).enumerate()
    {
        let (a, b) = (points[i.saturating_sub(1)], points[(i + 1).min(last)]);
        let tangent = (b - a).normalize_or_zero();
        let mut across = tangent.cross(outward(*p));
        if across.length_squared() < 1e-8 {
            across = Vec3::X;
        }
        let across = across.normalize_or_zero();
        let thin = across.cross(tangent).normalize_or_zero();
        let t = along(i, last);
        let (half_w, half_t) = (width.at(t) * 0.5, thickness.at(t) * 0.5);
        for (k, (vp, vn)) in ring_p.iter_mut().zip(ring_n.iter_mut()).enumerate() {
            let (sy, cx) = ring_angle(k, ring).sin_cos();
            // A lens: flat-ish front and back with a soft rounded spine.
            let yy = sy * half_t * (0.55 + 0.45 * (1.0 - cx * cx).max(0.0).sqrt());
            *vp = *p + across * (cx * half_w) + thin * yy;
            let (nx, ny) = (cx / half_w.max(1e-4), sy / half_t.max(1e-4));
            *vn = (across * nx + thin * (ny * 0.6)).normalize_or_zero();
        }
    }
    Ok(())
}

/// The angle of vertex `k` of a ring of `ring`.
#[allow(clippy::cast_precision_loss)] // k < ring, a few dozen
fn ring_angle(k: usize, ring: usize) -> f32 {
    k as f32 / ring as f32 * std::f32::consts::TAU
}

/// `points` with `between` more points in each segment, on the Catmull-Rom spline through them (centripetal ends
/// mirrored): offline locks are drawn on these, so a curl reads as a curve, not as straight runs. The originals are kept,
/// in place.
pub fn refine(points: &[Vec3], between: usize) -> Vec<Vec3> {
    let n = points.len();
    if n < 2 || between == 0 {
        return points.to_vec();
    }
    let at = |i: isize| -> Vec3 {
        match usize::try_from(i) {
            Ok(i) if i < n => points[i],
            Ok(_) => 2.0 * points[n - 1] - points[n - 2],
            Err(_) => 2.0 * points[0] - points[1],
        }
    };
    let mut out = Vec::with_capacity((n - 1) * (between + 1) + 1);
    #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)] // a clump has a few hundred points at most
    for i in 0..n - 1 {
        let k = i as isize;
        let (p0, p1, p2, p3) = (at(k - 1), at(k), at(k + 1), at(k + 2));
        out.push(p1);
        for j in 1..=between {
            let t = j as f32 / (between + 1) as f32;
            let (t2, t3) = (t * t, t * t * t);
            out.push(
                0.5 * ((2.0 * p1)
                    + (p2 - p0) * t
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
                    + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3),
            );
        }
    }
    out.push(points[n - 1]);
    out
}

/// `i / last` as the position along a clump, `0..=1`; exactly 1 at the last point.
#[allow(clippy::cast_precision_loss)] // clumps have a few dozen points
pub(crate) fn along(i: usize, last: usize) -> f32 {
    i as f32 / last as f32
}

/// The triangles that join the rings (of `ring` vertices) of a tube of `points` points whose first vertex is `base`,
/// wound so they face out of the closed tube (`tubeIndex`).
pub fn tube_indices(base: u32, points: usize, ring: usize) -> impl Iterator<Item = u32> {
    let segments = u32::try_from(points.saturating_sub(1)).unwrap_or(0);
    let ring = u32::try_from(ring).unwrap_or(0);
    (0..segments).flat_map(move |i| {
        (0..ring).flat_map(move |k| {
            let a = base + i * ring + k;
            let b = base + i * ring + (k + 1) % ring;
            let (c, d) = (a + ring, b + ring);
            [a, c, b, b, c, d]
        })
    })
}

/// The polar angle from the top of the head to each point, as `hair-info` carries it: `acos` of the unit direction's
/// height, so 0 at the crown and pi/2 at the equator.
pub fn polar_angles(points: &[Vec3], head_centre: Vec3) -> Vec<f32> {
    points.iter().map(|p| (*p - head_centre).normalize_or_zero().y.clamp(-1.0, 1.0).acos()).collect()
}

/// Checks that a tube of `rings` rings of `ring` vertices, with its two caps, appended at vertex `first`, still gets
/// `u32` indices. Every vertex of the tube is counted before anything is written, so an over-full mesh is refused, not
/// saturated.
fn vertices_fit(first: usize, rings: usize, ring: usize) -> Result<(), TubeError> {
    let vertices =
        rings.checked_mul(ring).and_then(|n| n.checked_add(cap_vertices(ring))).and_then(|n| n.checked_add(first));
    match vertices.map(u32::try_from) {
        Some(Ok(_)) => Ok(()),
        _ => Err(TubeError::TooMany),
    }
}

/// One end of a tube to close.
struct Cap {
    /// The index of the end ring's first vertex.
    ring: u32,
    /// The end point of the polyline.
    centre: Vec3,
    /// The outward unit direction along the tube axis, the cap's normal.
    axis: Vec3,
    /// `hair-uv.y` there.
    t: f32,
    /// `hair-info`'s polar angle there, the same as the end ring's.
    polar: f32,
    /// Whether this is the root end, whose fan is wound to face back along the axis.
    flip_winding: bool,
}

/// The per-clump data a tube mesh carries beside its geometry.
#[derive(Clone, Copy, Debug)]
pub struct TubeLook<'a> {
    /// The strand seed (`hair-info.x`): a random number per clump, so neighbouring locks shade differently.
    pub seed: f32,
    /// The shade multiplier (`hair-info.y`).
    pub shade: f32,
    /// The polar angle at each point (`hair-info.z`), see [`polar_angles`]; one per point.
    pub polar: &'a [f32],
    /// The vertex colour: the compiled streak tint, or `None` for white (the hair's own colour).
    pub tint: Option<Colour>,
    /// `1` or `-1`: which way `hair-uv.x` runs across the lock, for a mirrored lock whose shading must not mirror.
    pub flip: f32,
}

/// A mesh of tubes at rest, with the two attributes the hair program reads and a vertex colour per vertex.
#[derive(Clone, Debug)]
pub struct TubeMesh {
    mesh: MeshData,
    uv: Vec<f32>,
    info: Vec<f32>,
    /// Vertices per ring.
    ring: usize,
    /// How each lock is laid on the strand texture.
    map: StrandMap,
}

impl Default for TubeMesh {
    fn default() -> Self {
        Self::with_ring(RING)
    }
}

impl TubeMesh {
    /// An empty mesh of live tubes ([`RING`] vertices a ring).
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty mesh whose tubes have `ring` vertices a ring (at least 3; [`ring_for`] picks it by quality).
    pub fn with_ring(ring: usize) -> Self {
        Self {
            mesh: MeshData::default(),
            uv: Vec::new(),
            info: Vec::new(),
            ring: ring.max(3),
            map: StrandMap::default(),
        }
    }

    /// The same mesh, its locks laid on the strand texture by `map`.
    #[must_use]
    pub fn with_map(mut self, map: StrandMap) -> Self {
        self.map = map;
        self
    }

    /// Vertices so far.
    pub fn vertex_count(&self) -> usize {
        self.mesh.positions.len()
    }

    /// Whether nothing has been added.
    pub fn is_empty(&self) -> bool {
        self.mesh.positions.is_empty()
    }

    /// Adds the tube along `points`. `outward` is the thin-axis direction ([`OutwardFrame::rest`] at rest).
    ///
    /// # Errors
    /// [`TubeError::TooFewPoints`] for fewer than two points, or [`TubeError::WrongOutputLength`] if `look.polar` is not
    /// one angle per point.
    pub fn push(
        &mut self,
        points: &[Vec3],
        width: &Taper,
        thickness: &Bulge,
        outward: impl Fn(Vec3) -> Vec3,
        look: &TubeLook<'_>,
    ) -> Result<(), TubeError> {
        if look.polar.len() != points.len() {
            return Err(TubeError::WrongOutputLength {
                points: points.len(),
                expected: points.len(),
                got: look.polar.len(),
            });
        }
        let ring = self.ring;
        let count = points.len() * ring;
        let first = self.mesh.positions.len();
        // The caps come after the rings, so the caps' vertices count too, before anything is written.
        vertices_fit(first, points.len(), ring)?;
        let (Some(&root_polar), Some(&tip_polar)) = (look.polar.first(), look.polar.last()) else {
            return Err(TubeError::WrongOutputLength {
                points: points.len(),
                expected: points.len(),
                got: look.polar.len(),
            });
        };
        let base = u32::try_from(first).map_err(|_| TubeError::TooMany)?;
        self.mesh.positions.resize(first + count, Vec3::ZERO);
        self.mesh.normals.resize(first + count, Vec3::ZERO);
        let written = tube_vertices(
            points,
            ring,
            width,
            thickness,
            outward,
            &mut self.mesh.positions[first..],
            &mut self.mesh.normals[first..],
        );
        if let Err(e) = written {
            self.mesh.positions.truncate(first);
            self.mesh.normals.truncate(first);
            return Err(e);
        }
        let last = points.len() - 1;
        // The strand texture's `v` runs by length, so every lock's tip frays over the same few centimetres; its `u` is
        // shifted by the seed, so neighbouring locks show different strands.
        let lengths: Vec<f32> = std::iter::once(0.0)
            .chain(points.windows(2).scan(0.0, |run, pair| {
                *run += pair[0].distance(pair[1]);
                Some(*run)
            }))
            .collect();
        let length = lengths[last];
        let shift = strands::offset(look.seed);
        // The vertex colour's alpha feathers the lock's sides: the fainter strands drop out toward them.
        let colour = vertex_colour(look.tint);
        for (i, polar) in look.polar.iter().enumerate() {
            let t = along(i, last);
            let strand_v = self.map.v(lengths[i], length);
            for k in 0..ring {
                let across = ring_angle(k, ring).cos() * look.flip;
                self.uv.extend([across, t]);
                self.info.extend([look.seed, look.shade, *polar]);
                self.mesh.uvs.push(Vec2::new(shift + 0.5 + 0.5 * across, strand_v));
                let [red, green, blue, _] = colour;
                self.mesh.colours.push([red, green, blue, self.map.coverage(across)]);
            }
        }
        self.mesh.indices.extend(tube_indices(base, points.len(), ring));
        // The caps come after every ring vertex, so the ring vertices keep their indices.
        let ring_u32 = u32::try_from(ring).map_err(|_| TubeError::TooMany)?;
        let tip_ring = base + ring_u32 * u32::try_from(last).map_err(|_| TubeError::TooMany)?;
        let root_axis = (points[0] - points[1]).normalize_or_zero();
        let tip_axis = (points[last] - points[last - 1]).normalize_or_zero();
        let root =
            Cap { ring: base, centre: points[0], axis: root_axis, t: 0.0, polar: root_polar, flip_winding: true };
        self.push_cap(&root, look)?;
        let tip =
            Cap { ring: tip_ring, centre: points[last], axis: tip_axis, t: 1.0, polar: tip_polar, flip_winding: false };
        self.push_cap(&tip, look)
    }

    /// Closes the ring at `cap.ring` with a flat fan: a centre vertex and a copy of the rim, all with the axis as normal
    /// (a flat cap needs its own normals, so it cannot share the ring's vertices; the copies sit at the same positions,
    /// so the mesh is closed once vertices at one position are welded, as an exporter does).
    fn push_cap(&mut self, cap: &Cap, look: &TubeLook<'_>) -> Result<(), TubeError> {
        // Fails before anything is written, so a refused cap leaves no half-built fan behind.
        let centre = u32::try_from(self.mesh.positions.len()).map_err(|_| TubeError::TooMany)?;
        let [r, g, b, _] = vertex_colour(look.tint);
        let rim = cap.ring as usize;
        let add = |m: &mut Self, p: Vec3, across: f32| {
            m.mesh.positions.push(p);
            m.mesh.normals.push(cap.axis);
            m.uv.extend([across * look.flip, cap.t]);
            m.info.extend([look.seed, look.shade, cap.polar]);
            m.mesh
                .uvs
                .push(Vec2::new(strands::offset(look.seed) + 0.5 + 0.5 * across * look.flip, cap.t * strands::V_END));
            m.mesh.colours.push([r, g, b, m.map.coverage(across)]);
        };
        let ring = self.ring;
        let ring_u32 = u32::try_from(ring).map_err(|_| TubeError::TooMany)?;
        add(self, cap.centre, 0.0);
        for k in 0..ring {
            let p = self.mesh.positions[rim + k];
            add(self, p, ring_angle(k, ring).cos());
        }
        for k in 0..ring_u32 {
            let (a, b) = (centre + 1 + k, centre + 1 + (k + 1) % ring_u32);
            if cap.flip_winding {
                self.mesh.indices.extend([centre, a, b]);
            } else {
                self.mesh.indices.extend([centre, b, a]);
            }
        }
        Ok(())
    }

    /// Appends a finished mesh that already carries `hair-uv` and `hair-info` (a generator's shell, or the curtain's
    /// strips), keeping its vertex colours if it has one per vertex and giving it white ones otherwise. Returns `false`, appending nothing, if `other` lacks either attribute or they disagree with its vertex
    /// count.
    pub fn append(&mut self, other: &MeshData) -> bool {
        let (Some(uv), Some(info)) = (other.extra_attribute(HAIR_UV), other.extra_attribute(HAIR_INFO)) else {
            return false;
        };
        let n = other.positions.len();
        if other.normals.len() != n || uv.data.len() != 2 * n || info.data.len() != 3 * n {
            return false;
        }
        let Ok(base) = u32::try_from(self.mesh.positions.len()) else { return false };
        self.uv.extend_from_slice(&uv.data);
        self.info.extend_from_slice(&info.data);
        self.mesh.positions.extend_from_slice(&other.positions);
        self.mesh.normals.extend_from_slice(&other.normals);
        self.mesh.uvs.extend((0..n).map(|i| other.uvs.get(i).copied().unwrap_or(Vec2::ZERO)));
        if other.colours.len() == n {
            self.mesh.colours.extend_from_slice(&other.colours);
        } else {
            self.mesh.colours.extend(std::iter::repeat_n(vertex_colour(None), n));
        }
        self.mesh.indices.extend(other.indices.iter().map(|i| base + i));
        true
    }

    /// The finished mesh, with `hair-uv` then `hair-info` as its extra attributes.
    ///
    /// # Errors
    /// [`TubeError::Name`] if a contract attribute name is not a valid slug (a test catches that).
    pub fn finish(self) -> Result<MeshData, TubeError> {
        let Self { mut mesh, uv, info, .. } = self;
        mesh.extra = vec![
            ExtraAttribute { name: Slug::new(HAIR_UV)?, components: 2, data: uv },
            ExtraAttribute { name: Slug::new(HAIR_INFO)?, components: 3, data: info },
        ];
        Ok(mesh)
    }
}

/// The vertex colour of a clump: its linear tint, or white where it is untinted, so untinted hair renders as the hair's
/// own colour and the material's `vertex_colours` flag changes nothing for it.
pub fn vertex_colour(tint: Option<Colour>) -> [f32; 4] {
    tint.map_or([1.0; 4], |c| {
        let linear = c.to_linear();
        [linear.x, linear.y, linear.z, 1.0]
    })
}

#[cfg(test)]
#[allow(clippy::chunks_exact_to_as_chunks, clippy::float_cmp)] // plain assertions over small known meshes
mod tests {
    use super::*;

    fn line(n: usize) -> Vec<Vec3> {
        (0..n).map(|i| Vec3::new(0.0, -0.05 * along(i, n - 1) * 4.0, 0.12)).collect()
    }

    fn frame() -> OutwardFrame {
        OutwardFrame { head: Vec3::new(0.0, 0.0, 0.0), body_axis: Vec2::new(0.0, -0.03) }
    }

    fn mesh_of(points: &[Vec3]) -> MeshData {
        let polar = polar_angles(points, Vec3::ZERO);
        let look = TubeLook { seed: 0.3, shade: 0.9, polar: &polar, tint: None, flip: 1.0 };
        let mut m = TubeMesh::new();
        m.push(points, &Taper::default(), &Bulge::default(), |p| frame().rest(p), &look).unwrap();
        m.finish().unwrap()
    }

    #[test]
    fn a_tube_has_a_ring_per_point_and_validates() {
        let m = mesh_of(&line(6));
        assert_eq!(m.positions.len(), 6 * RING + 2 * (RING + 1), "ring vertices, then two caps");
        assert_eq!(m.triangle_count(), 5 * RING * 2 + 2 * RING);
        assert_eq!(m.extra_attribute(HAIR_UV).unwrap().data.len(), 2 * m.positions.len());
        assert_eq!(m.extra_attribute(HAIR_INFO).unwrap().data.len(), 3 * m.positions.len());
        assert_eq!(m.colours.len(), m.positions.len());
        m.validate().unwrap();
    }

    #[test]
    fn every_triangle_faces_out_of_the_closed_tube() {
        let points = line(9);
        let m = mesh_of(&points);
        for tri in m.indices.chunks_exact(3).take(8 * RING * 2) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| m.positions[i as usize]);
            let normal = (b - a).cross(c - a);
            let ring = tri[0] as usize / RING;
            let spine = points[ring.min(points.len() - 1)];
            let out = (a + b + c) / 3.0 - spine;
            assert!(normal.dot(out) > 0.0, "triangle {tri:?} faces into the tube");
        }
    }

    #[test]
    fn the_caps_are_flat_fans_facing_along_the_axis_after_the_rings() {
        let points = line(4);
        let m = mesh_of(&points);
        let rings = 4 * RING;
        assert_eq!(m.positions.len(), rings + 2 * (RING + 1));
        let axis = (points[3] - points[0]).normalize();
        let info = &m.extra_attribute(HAIR_INFO).unwrap().data;
        for (cap, sign) in [(0, -1.0), (1, 1.0)] {
            let first = rings + cap * (RING + 1);
            // Each cap's centre carries its end ring's polar angle: the root's the first ring's, the tip's the last.
            let end_ring = if cap == 0 { 0 } else { 3 * RING };
            assert_eq!(info[3 * first + 2], info[3 * end_ring + 2], "cap {cap}: polar angle");
            assert!(m.normals[first..=first + RING].iter().all(|n| n.dot(axis) * sign > 0.99));
            for tri in m.indices.chunks_exact(3).skip(3 * RING * 2 + cap * RING).take(RING) {
                assert!(tri.iter().all(|i| (first..=first + RING).contains(&(*i as usize))));
                let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| m.positions[i as usize]);
                assert!((b - a).cross(c - a).dot(axis) * sign > 0.0, "cap {cap} faces into the tube");
            }
        }
    }

    #[test]
    fn the_caps_count_against_the_index_limit_before_anything_is_written() {
        let rings = 2;
        let last_fit = u32::MAX as usize - rings * RING - CAP_VERTICES;
        assert!(vertices_fit(last_fit, rings, RING).is_ok(), "the rings and both caps end at the last index");
        let ring_prefix = last_fit + 1;
        assert!(
            matches!(vertices_fit(ring_prefix, rings, RING), Err(TubeError::TooMany)),
            "the rings fit but the caps don't"
        );
    }

    #[test]
    fn vertex_normals_point_away_from_the_spine() {
        let points = line(5);
        let m = mesh_of(&points);
        for (v, n) in m.positions.iter().zip(&m.normals).take(5 * RING) {
            let ring = points.iter().min_by(|a, b| a.distance(*v).total_cmp(&b.distance(*v))).unwrap();
            assert!((n.length() - 1.0).abs() < 1e-4);
            assert!(n.dot(*v - *ring) > 0.0);
        }
    }

    #[test]
    fn uv_runs_across_and_along_and_info_carries_the_strand() {
        let m = mesh_of(&line(4));
        let uv = &m.extra_attribute(HAIR_UV).unwrap().data;
        let info = &m.extra_attribute(HAIR_INFO).unwrap().data;
        assert!(uv.chunks_exact(2).all(|c| (-1.0..=1.0).contains(&c[0]) && (0.0..=1.0).contains(&c[1])));
        assert_eq!((uv[1], uv[2 * RING + 1], uv[2 * 3 * RING + 1]), (0.0, 1.0 / 3.0, 1.0));
        assert!(
            info.chunks_exact(3).all(|c| c[0] == 0.3 && c[1] == 0.9 && (0.0..=std::f32::consts::PI).contains(&c[2]))
        );
    }

    #[test]
    fn the_lens_is_wide_across_and_thin_through() {
        let points = line(3);
        let m = mesh_of(&points);
        let mid = &m.positions[RING..2 * RING];
        let spread = |f: &dyn Fn(Vec3) -> f32| {
            let v: Vec<f32> = mid.iter().map(|p| f(*p)).collect();
            v.iter().copied().fold(f32::MIN, f32::max) - v.iter().copied().fold(f32::MAX, f32::min)
        };
        // The lock hangs at +Z from the head, so outward is +Z: the thin axis, with width along X.
        let (across, through) = (spread(&|p| p.x), spread(&|p| p.z));
        assert!((across - Taper::default().at(0.5)).abs() < 1e-5, "{across}");
        assert!(through < across * 0.5, "{through} vs {across}");
    }

    #[test]
    fn wrong_inputs_are_errors_not_panics() {
        let mut p = vec![Vec3::ZERO; RING];
        let mut n = p.clone();
        let one = [Vec3::Z];
        assert_eq!(
            tube_vertices(&one, RING, &Taper::default(), &Bulge::default(), |_| Vec3::Z, &mut p, &mut n),
            Err(TubeError::TooFewPoints(1))
        );
        let two = [Vec3::Z, Vec3::Z * 2.0];
        assert!(matches!(
            tube_vertices(&two, RING, &Taper::default(), &Bulge::default(), |_| Vec3::Z, &mut p, &mut n),
            Err(TubeError::WrongOutputLength { expected: 16, .. })
        ));
    }

    #[test]
    fn a_degenerate_polyline_still_gives_finite_vertices() {
        let points = [Vec3::new(0.0, 1.0, 0.0); 3];
        let m = mesh_of(&points);
        assert!(m.positions.iter().chain(&m.normals).all(|v| v.is_finite()));
    }

    #[test]
    fn the_rest_frame_points_from_the_head_centre_up_high_and_from_the_body_axis_low() {
        let f = frame();
        let high = f.rest(Vec3::new(0.1, 0.05, 0.0));
        assert!(high.x > 0.85 && high.y > 0.0, "{high}");
        let low = f.rest(Vec3::new(0.0, -0.4, -0.1));
        assert!(low.z < -0.9 && low.x.abs() < 1e-6, "{low}");
        assert_eq!(f.rest(Vec3::ZERO), Vec3::NEG_Z, "on the axis itself, straight back");
    }

    #[test]
    fn the_posed_frame_blends_to_the_head_higher_up_only() {
        let f = OutwardFrame { head: Vec3::new(0.0, 0.0, 0.0), body_axis: Vec2::new(0.0, -0.05) };
        let low = f.posed(Vec3::new(0.0, -0.3, -0.12));
        assert!(low.z < -0.9);
        let high = f.posed(Vec3::new(0.0, -0.02, 0.1));
        assert!(high.z > 0.9);
    }

    #[test]
    fn tint_becomes_the_vertex_colour_and_untinted_is_white() {
        assert_eq!(vertex_colour(None), [1.0; 4]);
        let c = vertex_colour(Some(Colour::rgb(255, 0, 0)));
        assert_eq!((c[0], c[1], c[2], c[3]), (1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn append_refuses_a_mesh_without_the_hair_attributes() {
        let mut m = TubeMesh::new();
        assert!(!m.append(&MeshData::default()));
        let shell = mesh_of(&line(3));
        assert!(m.append(&shell));
        assert_eq!(m.vertex_count(), shell.positions.len());
        m.finish().unwrap().validate().unwrap();
    }
}
