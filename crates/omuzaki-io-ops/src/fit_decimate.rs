//! Cutting a skinned mesh to a triangle budget (`fit.decimate`, EX-03).
//!
//! A quadric edge-collapse simplifier (Garland and Heckbert) that knows what a character mesh carries. Every vertex
//! has a quadric of its triangles' planes; a collapse is priced by that quadric at the new position, plus how far the
//! two ends' morph target deltas and UVs are apart, so a collapse that would tear a blink or a mouth shape open costs
//! what it would visibly do. A collapse keeps one end's slot, so skin weights, morph deltas, UVs, colours and extra
//! attributes stay consistent with the vertex they belong to, and indices and every array shrink together.
//!
//! The candidates for an edge are each end and its midpoint (no optimal-position solve), so a vertex keeps a position
//! the artist put there when it can. The queue is ordered by (cost, vertex ids), so the output depends on the input
//! alone: same mesh and options, same bytes, on every platform.
//!
//! Vertices that share a position but differ in other attributes (UV seams, hard normals) are separate vertices; this
//! module never moves one of them, because moving one half of a seam would open a crack. With
//! [`DecimateOptions::lock_uv_seams`] they also never merge; without it a seam vertex may still absorb a neighbour.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use omuzaki_core::glam::{DVec3, Vec2, Vec3};
use omuzaki_io_api::{IoError, IoScene, Primitive};

/// How many influences a vertex may keep: the width of `JOINTS_0`/`WEIGHTS_0`.
const INFLUENCES: usize = 4;
/// The smallest share `decimate_scene` gives a primitive.
const MIN_SHARE: usize = 100;
/// Weight of the plane that guards an open edge, relative to a triangle plane.
const BOUNDARY_WEIGHT: f64 = 4.0;
/// Edge length (squared) added to every cost, so flat regions collapse short edges first instead of in id order.
const LENGTH_BIAS: f64 = 1e-6;
/// The collapse positions tried for an edge, as the fraction of the way from the lower-id vertex to the higher.
const CANDIDATES: [f32; 3] = [0.0, 1.0, 0.5];
/// How much a whole texture's worth of UV change counts, as a fraction of the diagonal. UVs are not lengths and a
/// planar unwrap interpolates exactly, so a neighbour's UV differing by 0.1 must not cost a tenth of the model.
const UV_ERROR_SCALE: f64 = 0.01;
/// A collapse may not turn a triangle's normal by more than this angle's cosine allows: just under 90 degrees.
const MIN_NORMAL_COS: f64 = 1e-4;

/// What to keep and what to give up.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DecimateOptions {
    /// Stop at this many triangles or fewer.
    pub target_triangles: usize,
    /// Stop early when the best collapse would exceed this, as a fraction of the primitive's bounding-box diagonal.
    pub max_error: f32,
    /// Vertices on open edges never move or merge.
    pub lock_boundary: bool,
    /// Vertices where UVs split never move or merge.
    pub lock_uv_seams: bool,
    /// How strongly morph target deltas count in the error (1.0: as much as the base shape).
    pub morph_weight: f32,
}

impl DecimateOptions {
    /// Options for `target_triangles` that guard the shape: an error cap of 1% of the diagonal, boundary and seams
    /// locked, morph targets counting as much as the base shape.
    pub fn new(target_triangles: usize) -> Self {
        Self { target_triangles, max_error: 0.01, lock_boundary: true, lock_uv_seams: true, morph_weight: 1.0 }
    }
}

/// What [`decimate`] did.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DecimateReport {
    /// Triangles before and after.
    pub triangles: (usize, usize),
    /// Vertices before and after.
    pub vertices: (usize, usize),
    /// The largest error of any collapse made, as a fraction of the bounding-box diagonal (an upper bound: it also
    /// counts the morph and UV terms).
    pub max_error: f32,
    /// True if the result has at most `target_triangles`.
    pub reached_target: bool,
}

/// Decimates `primitive` towards `options.target_triangles`. Joints and weights (blended and renormalised to four
/// influences when a collapse merges different sets), morph target deltas, UVs, colours and extra attributes follow
/// the surviving vertices; normals are recomputed. A primitive at or under the target is left as it is.
///
/// A collapse never turns a neighbouring triangle's normal by 90 degrees or more and never makes an edge shared by
/// three triangles. If the budget cannot be met without breaking those rules or `max_error`, the result is as far as
/// it got and `reached_target` is false. An error leaves the primitive untouched.
///
/// # Errors
/// [`IoError::Malformed`] if the options are not finite and non-negative, or the primitive is inconsistent (the
/// mesh fails [`MeshData::validate`](omuzaki_core::mesh::MeshData::validate), or a morph target's arrays are not one
/// entry per vertex).
pub fn decimate(primitive: &mut Primitive, options: &DecimateOptions) -> Result<DecimateReport, IoError> {
    if !(options.max_error.is_finite()
        && options.max_error >= 0.0
        && options.morph_weight.is_finite()
        && options.morph_weight >= 0.0)
    {
        return Err(IoError::malformed("decimate options: max_error and morph_weight must be finite and non-negative"));
    }
    primitive.mesh.validate().map_err(|e| IoError::malformed(e.to_string()))?;
    let n = primitive.mesh.positions.len();
    let triangles = primitive.mesh.triangle_count();
    for (k, t) in primitive.morph_targets.iter().enumerate() {
        let ok = t.positions.len() == n && (t.normals.is_empty() || t.normals.len() == n);
        if !ok || t.positions.iter().chain(&t.normals).any(|v| !v.is_finite()) {
            return Err(IoError::malformed(format!("morph target {k} is not one finite entry per vertex")));
        }
    }
    if triangles <= options.target_triangles {
        return Ok(DecimateReport {
            triangles: (triangles, triangles),
            vertices: (n, n),
            max_error: 0.0,
            reached_target: true,
        });
    }
    let mut work = Work::new(primitive, options);
    let max_error = work.run(options);
    let after = work.alive_tris;
    let vertices = work.write_back(primitive);
    Ok(DecimateReport {
        triangles: (triangles, after),
        vertices: (n, vertices),
        max_error,
        reached_target: after <= options.target_triangles,
    })
}

/// Splits `total` across the scene's primitives in proportion to their surface area (at least 100 each, or the
/// primitive's own count when that is smaller, and at most its own count: what a small primitive cannot use goes to the
/// others), then decimates each with `options` (its `target_triangles` is
/// replaced by the share). Reports come in mesh order, then primitive order.
///
/// The scene is changed only if every primitive succeeds and the result passes [`IoScene::check`].
///
/// # Errors
/// The first primitive's error from [`decimate`], or [`IoError::Rejected`] with the check's report if the decimated
/// scene fails [`IoScene::check`]. The scene is unchanged in both cases.
pub fn decimate_scene(
    scene: &mut IoScene,
    total: usize,
    options: &DecimateOptions,
) -> Result<Vec<DecimateReport>, IoError> {
    let slots: Vec<(usize, usize)> =
        scene.meshes.iter().enumerate().flat_map(|(m, mesh)| (0..mesh.primitives.len()).map(move |p| (m, p))).collect();
    let areas: Vec<f64> = slots.iter().map(|&(m, p)| surface_area(&scene.meshes[m].primitives[p])).collect();
    let counts: Vec<usize> = slots.iter().map(|&(m, p)| scene.meshes[m].primitives[p].mesh.triangle_count()).collect();
    let shares = split_budget(&areas, &counts, total);
    let mut results = Vec::with_capacity(slots.len());
    for (&(m, p), &share) in slots.iter().zip(&shares) {
        let mut copy = scene.meshes[m].primitives[p].clone();
        let report = decimate(&mut copy, &DecimateOptions { target_triangles: share, ..*options })?;
        results.push((copy, report));
    }
    let mut originals = Vec::with_capacity(results.len());
    let mut reports = Vec::with_capacity(results.len());
    for (&(m, p), (copy, report)) in slots.iter().zip(results) {
        originals.push(std::mem::replace(&mut scene.meshes[m].primitives[p], copy));
        reports.push(report);
    }
    let check = scene.check();
    if check.has_errors() {
        for (&(m, p), original) in slots.iter().zip(originals) {
            scene.meshes[m].primitives[p] = original;
        }
        return Err(IoError::Rejected(check));
    }
    Ok(reports)
}

/// The triangle count each primitive gets out of `total`: proportional to area, but never under [`MIN_SHARE`] (or the
/// primitive's own count, when that is smaller) and never over the primitive's own count, which it cannot be given more
/// than it has. A share that binds at either end is paid for, or taken up, by the rest, which are shared out again until
/// none is out of bounds. Remainders go to the largest fractions, ties to the lowest index, so the shares add up to
/// `total` exactly, unless the floors exceed it or the counts together fall short of it.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // counts are far below 2^52; ideals are >= 0
fn split_budget(areas: &[f64], counts: &[usize], total: usize) -> Vec<usize> {
    let floor_of = |i: usize| MIN_SHARE.min(counts[i]);
    // `fixed[i]` is the share a primitive is held at once it binds, if it does.
    let mut fixed: Vec<Option<usize>> = vec![None; areas.len()];
    loop {
        let held: usize = fixed.iter().flatten().sum();
        let free: Vec<usize> = (0..areas.len()).filter(|&i| fixed[i].is_none()).collect();
        let free_area: f64 = free.iter().map(|&i| areas[i]).sum();
        let free_total = total.saturating_sub(held) as f64;
        let ideal = |i: usize| {
            if free_area > 0.0 { free_total * areas[i] / free_area } else { free_total / free.len() as f64 }
        };
        let clamped = |i: usize| ideal(i).clamp(floor_of(i) as f64, counts[i] as f64);
        let excess = free.iter().map(|&i| clamped(i)).sum::<f64>() - free_total;
        // Over-allocated: the primitives raised to their floor are the ones to hold. Under-allocated: the ones cut to
        // their count. Balanced: every one out of bounds is held, and the rest are as they are.
        let is_low = |i: usize| ideal(i) < floor_of(i) as f64;
        let is_high = |i: usize| ideal(i) > counts[i] as f64;
        let (any_low, any_high) = (free.iter().any(|&i| is_low(i)), free.iter().any(|&i| is_high(i)));
        let tolerance = f64::EPSILON * free_total.max(1.0);
        let bound: Vec<(usize, usize)> = free
            .iter()
            .filter_map(|&i| {
                let hold_low = is_low(i) && (excess >= -tolerance || !any_high);
                let hold_high = is_high(i) && (excess <= tolerance || !any_low);
                if hold_low { Some((i, floor_of(i))) } else { hold_high.then_some((i, counts[i])) }
            })
            .collect();
        if bound.is_empty() {
            let mut shares: Vec<usize> =
                (0..areas.len()).map(|i| fixed[i].unwrap_or_else(|| ideal(i).floor() as usize)).collect();
            let mut left = total.saturating_sub(shares.iter().sum());
            let mut order = free.clone();
            order.sort_by(|&x, &y| ideal(y).fract().total_cmp(&ideal(x).fract()).then(x.cmp(&y)));
            for i in order {
                if left == 0 {
                    break;
                }
                if shares[i] < counts[i] {
                    shares[i] += 1;
                    left -= 1;
                }
            }
            return shares;
        }
        for (i, share) in bound {
            fixed[i] = Some(share);
        }
    }
}

/// A vertex or triangle index as the `u32` the mesh uses. The mesh's own indices are `u32`, so a count that does not
/// fit cannot be indexed anyway; the saturation documents that and never fires.
fn id(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn surface_area(p: &Primitive) -> f64 {
    let pos = |i: u32| p.mesh.positions.get(i as usize).map_or(DVec3::ZERO, |v| v.as_dvec3());
    p.mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| 0.5 * (pos(t[1]) - pos(t[0])).cross(pos(t[2]) - pos(t[0])).length())
        .sum()
}

/// The sum of squared plane distances, as ten numbers of a symmetric 4x4 matrix.
#[derive(Copy, Clone, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    fn plane(n: DVec3, d: f64, weight: f64) -> Self {
        Self(
            [n.x * n.x, n.x * n.y, n.x * n.z, n.x * d, n.y * n.y, n.y * n.z, n.y * d, n.z * n.z, n.z * d, d * d]
                .map(|v| v * weight),
        )
    }

    fn sum(self, other: Self) -> Self {
        let mut q = self.0;
        for (x, y) in q.iter_mut().zip(other.0) {
            *x += y;
        }
        Self(q)
    }

    fn eval(&self, p: DVec3) -> f64 {
        let q = &self.0;
        (q[0] * p.x * p.x + 2.0 * q[1] * p.x * p.y + 2.0 * q[2] * p.x * p.z + 2.0 * q[3] * p.x)
            + (q[4] * p.y * p.y + 2.0 * q[5] * p.y * p.z + 2.0 * q[6] * p.y)
            + (q[7] * p.z * p.z + 2.0 * q[8] * p.z)
            + q[9]
    }
}

/// A queue entry: cost bits (costs are non-negative, so bits order like numbers), the edge, the candidate, and the
/// versions of both ends it was priced at.
type Entry = Reverse<(u64, u32, u32, usize, u32, u32)>;

/// The mesh while it is being collapsed. Per-vertex data that a collapse interpolates lives in one `attrs` array of
/// `stride` floats per vertex: morph position deltas first, then UVs, colours, extra attributes and morph normal
/// deltas, so one lerp covers them all.
struct Work {
    pos: Vec<Vec3>,
    attrs: Vec<f32>,
    stride: usize,
    morph_len: usize,
    uv_at: Option<usize>,
    joints: Vec<[u16; INFLUENCES]>,
    weights: Vec<[f32; INFLUENCES]>,
    old_normals: Vec<Vec3>,
    quadric: Vec<Quadric>,
    tris: Vec<[u32; 3]>,
    tri_alive: Vec<bool>,
    alive_tris: usize,
    vertex_tris: Vec<Vec<u32>>,
    boundary: Vec<bool>,
    seam: Vec<bool>,
    pinned: Vec<bool>,
    removed: Vec<bool>,
    version: Vec<u32>,
    diag: f64,
    lock_boundary: bool,
    lock_seams: bool,
    morph_weight: f64,
    layout: Layout,
}

/// Which optional arrays the primitive had, so `write_back` can unpack `attrs` the way it was packed.
struct Layout {
    colours: bool,
    extra_components: Vec<usize>,
    morph_normals: Vec<bool>,
}

impl Work {
    fn new(primitive: &Primitive, options: &DecimateOptions) -> Self {
        let m = &primitive.mesh;
        let n = m.positions.len();
        let k = primitive.morph_targets.len();
        let layout = Layout {
            colours: !m.colours.is_empty(),
            extra_components: m.extra.iter().map(|e| usize::from(e.components)).collect(),
            morph_normals: primitive.morph_targets.iter().map(|t| !t.normals.is_empty()).collect(),
        };
        let morph_len = 3 * k;
        let uv_at = (!m.uvs.is_empty()).then_some(morph_len);
        let stride = morph_len
            + 2 * usize::from(uv_at.is_some())
            + 4 * usize::from(layout.colours)
            + layout.extra_components.iter().sum::<usize>()
            + 3 * layout.morph_normals.iter().filter(|b| **b).count();
        let mut attrs = Vec::with_capacity(n * stride);
        for v in 0..n {
            for t in &primitive.morph_targets {
                attrs.extend(t.positions[v].to_array());
            }
            if uv_at.is_some() {
                attrs.extend(m.uvs[v].to_array());
            }
            if layout.colours {
                attrs.extend(m.colours[v]);
            }
            for e in &m.extra {
                let c = usize::from(e.components);
                attrs.extend_from_slice(&e.data[v * c..(v + 1) * c]);
            }
            for t in primitive.morph_targets.iter().filter(|t| !t.normals.is_empty()) {
                attrs.extend(t.normals[v].to_array());
            }
        }
        let (lo, hi) = m
            .positions
            .iter()
            .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let tris: Vec<[u32; 3]> = m.indices.as_chunks::<3>().0.to_vec();
        let mut work = Self {
            pos: m.positions.clone(),
            attrs,
            stride,
            morph_len,
            uv_at,
            joints: m.joints.clone(),
            weights: m.weights.clone(),
            old_normals: m.normals.clone(),
            quadric: vec![Quadric::default(); n],
            tri_alive: vec![true; tris.len()],
            alive_tris: tris.len(),
            vertex_tris: vec![Vec::new(); n],
            tris,
            boundary: vec![false; n],
            seam: vec![false; n],
            pinned: vec![false; n],
            removed: vec![false; n],
            version: vec![0; n],
            diag: f64::from((hi - lo).length()),
            lock_boundary: options.lock_boundary,
            lock_seams: options.lock_uv_seams,
            morph_weight: f64::from(options.morph_weight),
            layout,
        };
        work.index_topology();
        work
    }

    /// Fills the per-vertex triangle lists, quadrics and the boundary, seam and non-manifold flags.
    fn index_topology(&mut self) {
        for (t, tri) in self.tris.iter().enumerate() {
            for &v in tri {
                self.vertex_tris[v as usize].push(id(t));
            }
        }
        let mut edges: Vec<(u32, u32, u32)> = Vec::with_capacity(self.tris.len() * 3);
        for (t, tri) in self.tris.iter().enumerate() {
            let plane = self.plane_of(*tri);
            for &v in tri {
                if let Some((n, d)) = plane {
                    self.quadric[v as usize] = self.quadric[v as usize].sum(Quadric::plane(n, d, 1.0));
                }
            }
            for e in 0..3 {
                let (a, b) = (tri[e], tri[(e + 1) % 3]);
                edges.push((a.min(b), a.max(b), id(t)));
            }
        }
        edges.sort_unstable();
        for run in edges.chunk_by(|x, y| (x.0, x.1) == (y.0, y.1)) {
            let (a, b, t) = run[0];
            match run.len() {
                1 => self.guard_open_edge(a, b, t),
                2 => {}
                _ => {
                    self.pinned[a as usize] = true;
                    self.pinned[b as usize] = true;
                }
            }
        }
        let mut by_position: Vec<(u32, u32, u32, u32)> =
            self.pos.iter().enumerate().map(|(i, p)| (p.x.to_bits(), p.y.to_bits(), p.z.to_bits(), id(i))).collect();
        by_position.sort_unstable();
        for run in by_position.chunk_by(|x, y| (x.0, x.1, x.2) == (y.0, y.1, y.2)).filter(|r| r.len() > 1) {
            for &(_, _, _, v) in run {
                self.seam[v as usize] = true;
            }
        }
    }

    /// Marks an open edge's ends and adds the plane through the edge, square to its triangle, to their quadrics.
    fn guard_open_edge(&mut self, a: u32, b: u32, t: u32) {
        self.boundary[a as usize] = true;
        self.boundary[b as usize] = true;
        let Some((normal, _)) = self.plane_of(self.tris[t as usize]) else { return };
        let (pa, pb) = (self.pos[a as usize].as_dvec3(), self.pos[b as usize].as_dvec3());
        let across = (pb - pa).cross(normal).normalize_or_zero();
        if across != DVec3::ZERO {
            let q = Quadric::plane(across, -across.dot(pa), BOUNDARY_WEIGHT);
            self.quadric[a as usize] = self.quadric[a as usize].sum(q);
            self.quadric[b as usize] = self.quadric[b as usize].sum(q);
        }
    }

    /// The unit normal and offset of a triangle's plane, or `None` for a degenerate triangle.
    fn plane_of(&self, tri: [u32; 3]) -> Option<(DVec3, f64)> {
        let p = |i: usize| self.pos[tri[i] as usize].as_dvec3();
        let n = (p(1) - p(0)).cross(p(2) - p(0)).try_normalize()?;
        Some((n, -n.dot(p(0))))
    }

    /// Collapses the cheapest edges until the target is met, nothing allowed is left or the error cap is hit.
    /// Returns the largest error of a collapse made, as a fraction of the diagonal.
    #[allow(clippy::cast_possible_truncation)] // a fraction of the diagonal fits f32
    fn run(&mut self, options: &DecimateOptions) -> f32 {
        let cap = (f64::from(options.max_error) * self.diag).powi(2);
        let mut queue = BinaryHeap::new();
        let mut edges: Vec<(u32, u32)> = self
            .tris
            .iter()
            .flat_map(|t| (0..3).map(|e| (t[e].min(t[(e + 1) % 3]), t[e].max(t[(e + 1) % 3]))))
            .collect();
        edges.sort_unstable();
        edges.dedup();
        for (a, b) in edges {
            self.price(&mut queue, a, b);
        }
        let mut worst = 0.0_f64;
        while self.alive_tris > options.target_triangles {
            let Some(Reverse((bits, a, b, candidate, va, vb))) = queue.pop() else { break };
            let cost = f64::from_bits(bits);
            if cost > cap {
                break;
            }
            let (ai, bi) = (a as usize, b as usize);
            if self.removed[ai] || self.removed[bi] || self.version[ai] != va || self.version[bi] != vb {
                continue;
            }
            let t = CANDIDATES[candidate];
            let p = self.pos[ai].lerp(self.pos[bi], t);
            if !self.link_ok(a, b) || self.flips(a, b, p) {
                continue;
            }
            worst = worst.max(cost);
            self.collapse(a, b, t, p);
            for n in self.neighbours(a) {
                self.price(&mut queue, a.min(n), a.max(n));
            }
        }
        if self.diag > 0.0 { (worst.sqrt() / self.diag) as f32 } else { 0.0 }
    }

    /// Queues the cheapest allowed collapse of edge `(a, b)`, if there is one.
    fn price(&self, queue: &mut BinaryHeap<Entry>, a: u32, b: u32) {
        let (ai, bi) = (a as usize, b as usize);
        if self.pinned[ai] || self.pinned[bi] {
            return;
        }
        let frozen = |v: usize| (self.lock_boundary && self.boundary[v]) || (self.lock_seams && self.seam[v]);
        if frozen(ai) || frozen(bi) {
            return;
        }
        // An end that is on the boundary or a seam may only be the one that stays, unless the edge runs along the boundary.
        let along_boundary = self.shared_triangles(a, b) == 1;
        let fixed = |v: usize| self.seam[v] || (self.boundary[v] && !along_boundary);
        let quadric = self.quadric[ai].sum(self.quadric[bi]);
        let (pa, pb) = (self.pos[ai], self.pos[bi]);
        let spread = self.attribute_spread(ai, bi);
        let best = CANDIDATES
            .iter()
            .enumerate()
            .filter(|&(c, _)| match c {
                0 => !fixed(bi),
                1 => !fixed(ai),
                _ => !(fixed(ai) || fixed(bi)),
            })
            .map(|(c, &t)| {
                let p = pa.lerp(pb, t);
                let ends = f64::from(t * t + (1.0 - t) * (1.0 - t));
                let cost = quadric.eval(p.as_dvec3()).max(0.0)
                    + ends * spread
                    + LENGTH_BIAS * f64::from((pa - pb).length_squared());
                (cost, c)
            })
            .min_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        if let Some((cost, c)) = best {
            queue.push(Reverse((cost.to_bits(), a, b, c, self.version[ai], self.version[bi])));
        }
    }

    /// How far apart the two ends' morph deltas and UVs are, as a squared length: the price of merging them.
    fn attribute_spread(&self, a: usize, b: usize) -> f64 {
        let row = |v: usize| &self.attrs[v * self.stride..(v + 1) * self.stride];
        let squared = |from: usize, to: usize| -> f64 {
            row(a)[from..to].iter().zip(&row(b)[from..to]).map(|(x, y)| f64::from(x - y).powi(2)).sum()
        };
        let uv = self.uv_at.map_or(0.0, |at| squared(at, at + 2) * (UV_ERROR_SCALE * self.diag).powi(2));
        self.morph_weight * squared(0, self.morph_len) + uv
    }

    fn shared_triangles(&self, a: u32, b: u32) -> usize {
        self.vertex_tris[a as usize].iter().filter(|&&t| self.tris[t as usize].contains(&b)).count()
    }

    /// The vertices that share a triangle with `v`, ascending.
    fn neighbours(&self, v: u32) -> Vec<u32> {
        let mut out: Vec<u32> =
            self.vertex_tris[v as usize].iter().flat_map(|&t| self.tris[t as usize]).filter(|&w| w != v).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The link condition: `a` and `b` share exactly the vertices opposite the edge, so no new edge is shared by three
    /// triangles and no fold is made.
    fn link_ok(&self, a: u32, b: u32) -> bool {
        let mut apex: Vec<u32> = self.vertex_tris[a as usize]
            .iter()
            .map(|&t| self.tris[t as usize])
            .filter(|t| t.contains(&b))
            .flat_map(|t| t.into_iter().filter(|&w| w != a && w != b))
            .collect();
        apex.sort_unstable();
        apex.dedup();
        let nb = self.neighbours(b);
        let common: Vec<u32> = self.neighbours(a).into_iter().filter(|w| nb.binary_search(w).is_ok()).collect();
        !apex.is_empty() && common == apex
    }

    /// True if moving `a` and `b` to `p` turns a surviving triangle's normal by about 90 degrees or more.
    fn flips(&self, a: u32, b: u32, p: Vec3) -> bool {
        [(a, b), (b, a)].into_iter().any(|(moved, other)| {
            self.vertex_tris[moved as usize].iter().map(|&t| self.tris[t as usize]).filter(|t| !t.contains(&other)).any(
                |tri| {
                    let at = |moving: bool| {
                        tri.map(|v| if moving && v == moved { p.as_dvec3() } else { self.pos[v as usize].as_dvec3() })
                    };
                    let normal = |c: [DVec3; 3]| (c[1] - c[0]).cross(c[2] - c[0]);
                    let (before, after) = (normal(at(false)), normal(at(true)));
                    let (lb, la) = (before.length(), after.length());
                    lb > 0.0 && (la == 0.0 || before.dot(after) <= MIN_NORMAL_COS * lb * la)
                },
            )
        })
    }

    /// Merges `b` into `a`'s slot at `p`, `t` of the way from `a` to `b`.
    fn collapse(&mut self, a: u32, b: u32, along: f32, to: Vec3) {
        let (ai, bi) = (a as usize, b as usize);
        self.pos[ai] = to;
        for i in 0..self.stride {
            let (from, other) = (self.attrs[ai * self.stride + i], self.attrs[bi * self.stride + i]);
            self.attrs[ai * self.stride + i] = from * (1.0 - along) + other * along;
        }
        if !self.joints.is_empty() {
            let (joints, weights) =
                blend_influences((self.joints[ai], self.weights[ai]), (self.joints[bi], self.weights[bi]), along);
            self.joints[ai] = joints;
            self.weights[ai] = weights;
        }
        self.quadric[ai] = self.quadric[ai].sum(self.quadric[bi]);
        self.boundary[ai] |= self.boundary[bi];
        self.seam[ai] |= self.seam[bi];
        self.removed[bi] = true;
        self.version[ai] += 1;
        for tri in std::mem::take(&mut self.vertex_tris[bi]) {
            if self.tris[tri as usize].contains(&a) {
                self.kill(tri);
            } else {
                for v in &mut self.tris[tri as usize] {
                    if *v == b {
                        *v = a;
                    }
                }
                self.vertex_tris[ai].push(tri);
            }
        }
    }

    fn kill(&mut self, t: u32) {
        self.tri_alive[t as usize] = false;
        self.alive_tris -= 1;
        for v in self.tris[t as usize] {
            self.vertex_tris[v as usize].retain(|&x| x != t);
        }
    }

    /// Compacts the surviving vertices (in their old order) into `primitive`, with recomputed normals. Returns the
    /// vertex count.
    fn write_back(&self, primitive: &mut Primitive) -> usize {
        let n = self.pos.len();
        let mut used = vec![false; n];
        for (tri, _) in self.tris.iter().zip(&self.tri_alive).filter(|(_, alive)| **alive) {
            for &v in tri {
                used[v as usize] = true;
            }
        }
        let mut new_index = vec![u32::MAX; n];
        let kept: Vec<usize> = (0..n).filter(|&v| used[v]).collect();
        for (i, &v) in kept.iter().enumerate() {
            new_index[v] = id(i);
        }
        let indices: Vec<u32> = self
            .tris
            .iter()
            .zip(&self.tri_alive)
            .filter(|(_, alive)| **alive)
            .flat_map(|(t, _)| t.map(|v| new_index[v as usize]))
            .collect();
        let positions: Vec<Vec3> = kept.iter().map(|&v| self.pos[v]).collect();
        let mut sums = vec![Vec3::ZERO; kept.len()];
        for t in indices.as_chunks::<3>().0 {
            let p = |i: usize| positions[t[i] as usize];
            let n = (p(1) - p(0)).cross(p(2) - p(0));
            for &v in t {
                sums[v as usize] += n;
            }
        }
        let m = &mut primitive.mesh;
        m.normals = kept.iter().zip(&sums).map(|(&v, s)| s.try_normalize().unwrap_or(self.old_normals[v])).collect();
        m.positions = positions;
        m.indices = indices;
        if !m.joints.is_empty() {
            m.joints = kept.iter().map(|&v| self.joints[v]).collect();
            m.weights = kept.iter().map(|&v| self.weights[v]).collect();
        }
        self.unpack(&kept, primitive);
        kept.len()
    }

    /// Reads the packed attributes of the `kept` vertices back into their arrays, in the order they were packed.
    fn unpack(&self, kept: &[usize], primitive: &mut Primitive) {
        let rows = |from: usize, len: usize| {
            kept.iter().map(move |&v| &self.attrs[v * self.stride + from..v * self.stride + from + len])
        };
        let vec3 = |r: &[f32]| Vec3::new(r[0], r[1], r[2]);
        let mut at = 0;
        for (k, target) in primitive.morph_targets.iter_mut().enumerate() {
            target.positions = rows(3 * k, 3).map(vec3).collect();
        }
        at += self.morph_len;
        if self.uv_at.is_some() {
            primitive.mesh.uvs = rows(at, 2).map(|r| Vec2::new(r[0], r[1])).collect();
            at += 2;
        }
        if self.layout.colours {
            primitive.mesh.colours = rows(at, 4).map(|r| [r[0], r[1], r[2], r[3]]).collect();
            at += 4;
        }
        for (e, &c) in primitive.mesh.extra.iter_mut().zip(&self.layout.extra_components) {
            e.data = rows(at, c).flatten().copied().collect();
            at += c;
        }
        for (target, _) in primitive.morph_targets.iter_mut().zip(&self.layout.morph_normals).filter(|(_, has)| **has) {
            target.normals = rows(at, 3).map(vec3).collect();
            at += 3;
        }
    }
}

/// The influences of a vertex `t` of the way from `a` to `b`: the nearer end's own set when `t` is 0 or 1, otherwise
/// both ends' weights scaled by `1 - t` and `t`, joints merged, the four largest kept (ties to the lower joint) and
/// renormalised to sum to 1.
fn blend_influences(
    a: ([u16; INFLUENCES], [f32; INFLUENCES]),
    b: ([u16; INFLUENCES], [f32; INFLUENCES]),
    t: f32,
) -> ([u16; INFLUENCES], [f32; INFLUENCES]) {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let mut merged: Vec<(u16, f32)> = Vec::with_capacity(2 * INFLUENCES);
    for (joint, weight) in
        a.0.into_iter()
            .zip(a.1)
            .map(|(j, w)| (j, w * (1.0 - t)))
            .chain(b.0.into_iter().zip(b.1).map(|(j, w)| (j, w * t)))
    {
        match merged.iter_mut().find(|(j, _)| *j == joint) {
            Some(entry) => entry.1 += weight,
            None => merged.push((joint, weight)),
        }
    }
    merged.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    merged.truncate(INFLUENCES);
    let total: f32 = merged.iter().map(|e| e.1).sum();
    if total <= 0.0 {
        return a;
    }
    let (mut joints, mut weights) = ([0; INFLUENCES], [0.0; INFLUENCES]);
    for (slot, (joint, weight)) in merged.into_iter().enumerate() {
        joints[slot] = joint;
        weights[slot] = weight / total;
    }
    (joints, weights)
}
