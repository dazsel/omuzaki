//! Joining the shirt's body and its sleeves into one surface at the shoulders.
//!
//! A body's arm starts inside its torso, so a shirt's body, lofted round the torso, passes through the place the arm
//! comes out of, and a sleeve, lofted along the arm, passes through the torso. A garment must stay outside both. Neither
//! can be bent clear of the other (a body wall that clears the arm would have to fold over itself), so the shirt takes
//! the surface of their union instead: each piece loses what is inside the other, and keeps a little of its own that
//! reaches inside, pushed back onto the other's surface so the two meet without a gap.
//!
//! [`weld`] does this to a finished mesh. For each vertex of a piece a *depth* says how far inside the other piece it is
//! (negative outside). A triangle whose three vertices all lie inside the other piece, or within [`CUT_BAND`] of
//! it, is dropped. A vertex that remains and is still inside slides along its own surface, towards the nearest vertex of
//! its triangles that is clear of the other piece, until it comes out onto the other's surface. (A push straight out of
//! the other piece would go through the body wherever the two pieces meet at an angle.) The dropped triangles leave a
//! hole that the other piece covers, with its own edge on the same surface. A triangle that has to go to avoid a pinch (a
//! vertex where the hole touches itself, which no surface can have) takes its neighbours at that vertex with it, and
//! vertices no triangle uses are removed.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use omuzaki_core::glam::DVec3;
use omuzaki_core::mesh::MeshData;

use crate::outfit::common::GarmentError;

/// How far outside the other piece a triangle may still lie and be dropped, metres: a hole is a little larger than the
/// piece that fills it, so the pushed edge of what remains lies on the other's surface and not short of it.
pub const CUT_BAND: f64 = 0.003;
/// How far the slid vertices stand over the other piece's surface, metres.
pub const LIFT: f64 = 0.0005;
/// Positions closer than this are one vertex when finding the hole's border, metres (the conformance suite's weld).
const WELD: f64 = 1e-6;
/// The most rounds of removing pinched vertices before the mesh is refused: each round clears every pinch there is, so a
/// handful is plenty.
const MAX_ROUNDS: usize = 8;
/// Bisection steps of a slide: far below a micrometre.
const SLIDE_STEPS: usize = 30;

/// A tube along a centre line with a radius at each point: the arm, with the shirt's ease on it.
#[derive(Clone, Debug)]
pub struct Tube {
    points: Vec<DVec3>,
    radii: Vec<f64>,
}

impl Tube {
    /// The tube through `points` with `radii` at each.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] for fewer than two points or a radius per point missing.
    pub fn new(points: Vec<DVec3>, radii: Vec<f64>) -> Result<Self, GarmentError> {
        if points.len() < 2 || points.len() != radii.len() {
            return Err(GarmentError::invalid(
                "tube",
                format!(
                    "{} points and {} radii: need at least two points and a radius for each",
                    points.len(),
                    radii.len()
                ),
            ));
        }
        Ok(Self { points, radii })
    }

    /// The tube's mirror image, across x = 0.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        Self { points: self.points.iter().map(|p| DVec3::new(-p.x, p.y, p.z)).collect(), radii: self.radii.clone() }
    }

    /// How far inside the tube `p` is (negative outside), with the nearest point on the centre line and the radius there.
    /// A point before the tube's first point, past its last, or off the line's ends counts as outside.
    fn depth(&self, p: DVec3) -> Option<(f64, DVec3, f64)> {
        let mut best: Option<(f64, DVec3, f64)> = None;
        let last = self.points.len() - 2;
        for (k, pair) in self.points.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let along = b - a;
            let reach = (p - a).dot(along) / along.length_squared();
            if (k == 0 && reach < 0.0) || (k == last && reach > 1.0) {
                continue;
            }
            let reach = reach.clamp(0.0, 1.0);
            let on = a + along * reach;
            let radius = self.radii[k] + (self.radii[k + 1] - self.radii[k]) * reach;
            let depth = radius - p.distance(on);
            if best.is_none_or(|(d, ..)| depth > d) {
                best = Some((depth, on, radius));
            }
        }
        best
    }

    /// How far inside the tube `p` is, metres; negative outside it, and minus infinity where the tube does not reach.
    pub fn inside(&self, p: DVec3) -> f64 {
        self.depth(p).map_or(f64::NEG_INFINITY, |(d, ..)| d)
    }
}

/// One piece of the mesh and how it sits in the other: its vertices, and how deep inside the other piece a point is
/// (negative outside, zero on its surface).
pub struct Cut<'a> {
    /// The piece's vertices.
    pub vertices: Range<usize>,
    /// How far inside the other piece a point is.
    pub depth: &'a dyn Fn(DVec3) -> f64,
}

/// What [`weld`] did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Welded {
    /// Triangles dropped.
    pub dropped: usize,
    /// Vertices slid out onto the other piece.
    pub pushed: usize,
    /// Vertices removed because nothing uses them any more.
    pub removed: usize,
}

/// Welds the pieces of `geometry` into one surface: see the [module](self).
///
/// # Errors
/// [`GarmentError::Invalid`] if the geometry has extra attributes (which would have to be thinned too), a cut's vertices
/// are not all in the mesh, or pinched vertices survive [`MAX_ROUNDS`] rounds of removal.
pub fn weld(geometry: &mut MeshData, cuts: &[Cut<'_>]) -> Result<Welded, GarmentError> {
    let n = geometry.positions.len();
    if !geometry.extra.is_empty() {
        return Err(GarmentError::invalid("shirt", "a shirt's mesh has no extra attributes to thin".to_owned()));
    }
    let mut depth = vec![f64::NEG_INFINITY; n];
    let mut owner: Vec<Option<usize>> = vec![None; n];
    for (k, cut) in cuts.iter().enumerate() {
        if cut.vertices.end > n {
            return Err(GarmentError::invalid("shirt", format!("a cut reaches vertex {} of {n}", cut.vertices.end)));
        }
        for v in cut.vertices.clone() {
            depth[v] = (cut.depth)(geometry.positions[v].as_dvec3());
            owner[v] = Some(k);
        }
    }
    let triangles: Vec<[u32; 3]> = geometry.indices.as_chunks::<3>().0.to_vec();
    let mut kept: Vec<[u32; 3]> =
        triangles.iter().copied().filter(|t| t.iter().any(|&v| depth[v as usize] <= -CUT_BAND)).collect();
    let ids = weld_ids(geometry);
    let mut rounds = 0;
    loop {
        let pinched = pinched_vertices(&kept, &ids);
        if pinched.is_empty() {
            break;
        }
        rounds += 1;
        if rounds > MAX_ROUNDS {
            return Err(GarmentError::invalid(
                "shirt",
                format!("{} pinched vertices survive {MAX_ROUNDS} rounds", pinched.len()),
            ));
        }
        kept.retain(|t| !t.iter().any(|&v| pinched.contains(&ids[v as usize])));
    }
    let mut report = Welded { dropped: triangles.len() - kept.len(), ..Welded::default() };
    let mut used = vec![false; n];
    for &v in kept.iter().flatten() {
        used[v as usize] = true;
    }
    // The nearest vertex clear of the other piece, among those that share a triangle with each vertex.
    let mut anchor: Vec<Option<usize>> = vec![None; n];
    for t in &kept {
        for &v in t {
            for &w in t {
                let (v, w) = (v as usize, w as usize);
                if v != w && depth[v] > 0.0 && depth[w] <= -CUT_BAND {
                    let nearer = |a: usize| geometry.positions[v].distance(geometry.positions[a]);
                    if anchor[v].is_none_or(|a| nearer(w) < nearer(a)) {
                        anchor[v] = Some(w);
                    }
                }
            }
        }
    }
    for v in 0..n {
        if !(used[v] && depth[v] > 0.0) {
            continue;
        }
        let (Some(k), Some(w)) = (owner[v], anchor[v]) else {
            return Err(GarmentError::invalid(
                "shirt",
                format!("vertex {v} is inside the other piece with no way out"),
            ));
        };
        let (clear, inside) = (geometry.positions[w].as_dvec3(), geometry.positions[v].as_dvec3());
        // Along the way from the clear vertex in: the last point that is still LIFT outside.
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..SLIDE_STEPS {
            let mid = f64::midpoint(lo, hi);
            if (cuts[k].depth)(clear.lerp(inside, mid)) <= -LIFT {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        geometry.positions[v] = clear.lerp(inside, lo).as_vec3();
        report.pushed += 1;
    }
    report.removed = used.iter().filter(|u| !**u).count();
    compact(geometry, &kept, &used);
    Ok(report)
}

/// The welded identity of every vertex: positions within [`WELD`] share one.
fn weld_ids(geometry: &MeshData) -> Vec<usize> {
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    geometry
        .positions
        .iter()
        .map(|p| {
            #[allow(clippy::cast_possible_truncation)] // a position over WELD is far below the i64 range
            let key = [p.x, p.y, p.z].map(|c| (f64::from(c) / WELD).round() as i64);
            let next = ids.len();
            *ids.entry(key).or_insert(next)
        })
        .collect()
}

/// The welded vertices that two or more open edges leave: where the border of `triangles` touches itself.
fn pinched_vertices(triangles: &[[u32; 3]], ids: &[usize]) -> HashSet<usize> {
    let mut edges: HashSet<(usize, usize)> = HashSet::new();
    for t in triangles {
        let [a, b, c] = t.map(|v| ids[v as usize]);
        if a == b || b == c || c == a {
            continue;
        }
        edges.extend([(a, b), (b, c), (c, a)]);
    }
    let mut leaving: HashMap<usize, usize> = HashMap::new();
    for &(a, b) in &edges {
        if !edges.contains(&(b, a)) {
            *leaving.entry(a).or_default() += 1;
        }
    }
    leaving.into_iter().filter(|&(_, count)| count > 1).map(|(v, _)| v).collect()
}

/// `values` thinned to the entries whose vertex is `used`; nothing for an attribute the mesh does not have.
fn thin<T: Copy>(values: &mut Vec<T>, used: &[bool]) {
    if values.is_empty() {
        return;
    }
    let mut k = 0;
    values.retain(|_| {
        k += 1;
        used[k - 1]
    });
}

/// Keeps the vertices `used` and the `kept` triangles, renumbering both.
fn compact(geometry: &mut MeshData, kept: &[[u32; 3]], used: &[bool]) {
    let mut renumber = vec![0_u32; used.len()];
    let mut next = 0_u32;
    for (v, &u) in used.iter().enumerate() {
        if u {
            renumber[v] = next;
            next += 1;
        }
    }
    thin(&mut geometry.positions, used);
    thin(&mut geometry.normals, used);
    thin(&mut geometry.uvs, used);
    thin(&mut geometry.colours, used);
    thin(&mut geometry.joints, used);
    thin(&mut geometry.weights, used);
    geometry.indices = kept.iter().flat_map(|t| t.map(|v| renumber[v as usize])).collect();
}
