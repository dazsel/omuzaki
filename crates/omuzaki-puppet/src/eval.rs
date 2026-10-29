//! The evaluator: parameter values in, a posed puppet and its [`DrawList2D`] out.
//!
//! Each frame:
//! 1. every binding of every parameter is interpolated at the parameter's value and accumulated on its node
//!    (offsets, translations, rotations and draw-order shifts add; scales, opacities and tints multiply);
//! 2. each node's local transform is rebuilt (scale, then rotation, then translation) and its deformable points are
//!    its rest points plus their offsets;
//! 3. every point of a part or mask is carried to model space: up through plain transforms, and through each lattice
//!    or mesh deformer it hangs under, in that deformer's local space;
//! 4. the drawables are sorted back to front (a larger total `zsort` draws first; ties keep tree order) and written to
//!    the draw list, masks and composites included.
//!
//! All memory is sized in [`Evaluator::new`]; [`Evaluator::evaluate`] allocates nothing (the draw list keeps its
//! capacity), and the same values give the same bytes on every run.
#![allow(clippy::many_single_char_names, reason = "geometry and sampling read best in their formulas' own letters")]

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_puppet_api::{
    BindTarget, BindValues, DrawCmd2D, DrawList2D, DrawState, Interp, Mesh, NodeIx, NodeKind, Param, ParamIx,
    PuppetDef, PuppetError, Vertex2D,
};

/// A per-axis interpolation stencil: up to four key indices and their weights.
#[derive(Copy, Clone, Debug, Default)]
struct Stencil {
    idx: [usize; 4],
    w: [f32; 4],
    n: usize,
}

impl Stencil {
    fn single(i: usize) -> Self {
        Self { idx: [i, 0, 0, 0], w: [1.0, 0.0, 0.0, 0.0], n: 1 }
    }

    /// The stencil of `interp` at `v` over ascending `keys`.
    fn new(keys: &[f32], v: f32, interp: Interp) -> Self {
        let last = keys.len().saturating_sub(1);
        if last == 0 {
            return Self::single(0);
        }
        // The segment [i0, i0 + 1] holding v, clamped to the ends.
        let i0 = keys.partition_point(|&k| k <= v).saturating_sub(1).min(last - 1);
        let (k0, k1) = (keys[i0], keys[i0 + 1]);
        let t = if k1 > k0 { ((v - k0) / (k1 - k0)).clamp(0.0, 1.0) } else { 0.0 };
        match interp {
            Interp::Nearest => Self::single(if t < 0.5 { i0 } else { i0 + 1 }),
            Interp::Linear => Self { idx: [i0, i0 + 1, 0, 0], w: [1.0 - t, t, 0.0, 0.0], n: 2 },
            Interp::Cubic => {
                // Catmull-Rom through the neighbours, the ends repeated.
                let im = i0.saturating_sub(1);
                let ip = (i0 + 2).min(last);
                let (t2, t3) = (t * t, t * t * t);
                let w = [
                    -0.5 * t3 + t2 - 0.5 * t,
                    1.5 * t3 - 2.5 * t2 + 1.0,
                    -1.5 * t3 + 2.0 * t2 + 0.5 * t,
                    0.5 * t3 - 0.5 * t2,
                ];
                Self { idx: [im, i0, i0 + 1, ip], w, n: 4 }
            }
        }
    }
}

/// A uniform grid of a mesh deformer's rest triangles, for finding the triangle under a point.
#[derive(Clone, Debug, Default)]
struct TriBins {
    lo: Vec2,
    cell: Vec2,
    nx: usize,
    ny: usize,
    /// Start of each cell's run in `tris` (`nx * ny + 1` entries).
    start: Vec<u32>,
    tris: Vec<u32>,
}

const BINS: usize = 16;

impl TriBins {
    fn new(mesh: &Mesh) -> Self {
        let rest: Vec<Vec2> = mesh.vertices.iter().map(|v| Vec2::from(*v)).collect();
        let Some(first) = rest.first() else { return Self::default() };
        let (lo, hi) = rest.iter().fold((*first, *first), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let span = (hi - lo).max(Vec2::splat(1e-3));
        let (nx, ny) = (BINS, BINS);
        #[allow(clippy::cast_precision_loss, reason = "16 bins")]
        let cell = span / Vec2::new(nx as f32, ny as f32);
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); nx * ny];
        for (t, tri) in mesh.triangles.iter().enumerate() {
            let [a, b, c] = tri.map(|i| rest[i as usize]);
            let (tlo, thi) = (a.min(b).min(c), a.max(b).max(c));
            let (x0, y0) = Self::cell_of(lo, cell, nx, ny, tlo);
            let (x1, y1) = Self::cell_of(lo, cell, nx, ny, thi);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    lists[y * nx + x].push(u32::try_from(t).unwrap_or(u32::MAX));
                }
            }
        }
        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut tris = Vec::new();
        for list in &lists {
            start.push(u32::try_from(tris.len()).unwrap_or(u32::MAX));
            tris.extend_from_slice(list);
        }
        start.push(u32::try_from(tris.len()).unwrap_or(u32::MAX));
        Self { lo, cell, nx, ny, start, tris }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the grid")]
    fn cell_of(lo: Vec2, cell: Vec2, nx: usize, ny: usize, p: Vec2) -> (usize, usize) {
        let f = (p - lo) / cell;
        ((f.x.max(0.0) as usize).min(nx - 1), (f.y.max(0.0) as usize).min(ny - 1))
    }

    fn candidates(&self, p: Vec2) -> &[u32] {
        if self.start.is_empty() {
            return &[];
        }
        let (x, y) = Self::cell_of(self.lo, self.cell, self.nx, self.ny, p);
        let c = y * self.nx + x;
        &self.tris[self.start[c] as usize..self.start[c + 1] as usize]
    }
}

/// Barycentric coordinates of `p` in triangle `abc` (zero area gives the first vertex).
fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> [f32; 3] {
    let (v0, v1, v2) = (b - a, c - a, p - a);
    let den = v0.perp_dot(v1);
    if den.abs() < 1e-12 {
        return [1.0, 0.0, 0.0];
    }
    let v = v2.perp_dot(v1) / den;
    let w = v0.perp_dot(v2) / den;
    [1.0 - v - w, v, w]
}

/// The runtime state of one node.
#[derive(Clone, Debug)]
struct NodeRt {
    // What the bindings accumulated this frame.
    t: Vec2,
    r: f32,
    rx: f32,
    ry: f32,
    s: Vec2,
    z: f32,
    opacity: f32,
    tint: [f32; 3],
    offsets: Vec<Vec2>,
    // What the frame derived.
    rest: Vec<Vec2>,
    points: Vec<Vec2>,
    model: Vec<Vec2>,
    local: Affine2,
    up: Affine2,
    deformer: Option<usize>,
    total_z: f32,
    visible: bool,
    bins: Option<TriBins>,
}

/// A puppet ready to evaluate.
#[derive(Clone, Debug)]
pub struct Evaluator {
    def: PuppetDef,
    values: Vec<[f32; 2]>,
    /// Every binding's values with the unset key points filled, per parameter, per binding.
    filled: Vec<Vec<BindValues>>,
    nodes: Vec<NodeRt>,
    /// The drawables of each container (index 0: the puppet; then each composite by node index), in tree order.
    members: Vec<Vec<u32>>,
    /// The container slot of each composite node.
    slot: Vec<usize>,
    list: DrawList2D,
}

fn fill_unset(param: &Param, set: &[bool], values: &BindValues, target: BindTarget) -> BindValues {
    let (nx, ny) = (param.keys[0].len(), param.keys[1].len());
    let is_set = |i: usize| set.is_empty() || set.get(i).copied().unwrap_or(false);
    let mut known: Vec<bool> = (0..nx * ny).map(is_set).collect();
    let mut out = values.clone();
    // Along x within each row of y, then along y within each column, by linear interpolation in key position between
    // the nearest set neighbours (or a copy of the only one).
    let pass = |along_x: bool, out: &mut BindValues, known: &mut Vec<bool>| {
        let (outer, inner) = if along_x { (ny, nx) } else { (nx, ny) };
        let keys = if along_x { &param.keys[0] } else { &param.keys[1] };
        let idx = |o: usize, i: usize| if along_x { param.key_index(i, o) } else { param.key_index(o, i) };
        let snapshot = known.clone();
        for o in 0..outer {
            for i in 0..inner {
                if snapshot[idx(o, i)] {
                    continue;
                }
                let left = (0..i).rev().find(|&j| snapshot[idx(o, j)]);
                let right = (i + 1..inner).find(|&j| snapshot[idx(o, j)]);
                let (a, b, t) = match (left, right) {
                    (Some(l), Some(r)) => {
                        let span = keys[r] - keys[l];
                        (l, r, if span > 0.0 { (keys[i] - keys[l]) / span } else { 0.0 })
                    }
                    (Some(l), None) => (l, l, 0.0),
                    (None, Some(r)) => (r, r, 0.0),
                    (None, None) => continue,
                };
                let (ia, ib, it) = (idx(o, a), idx(o, b), idx(o, i));
                match out {
                    BindValues::Scalar(v) => v[it] = v[ia] + (v[ib] - v[ia]) * t,
                    BindValues::Offsets(v) => {
                        let row: Vec<[f32; 2]> = v[ia]
                            .iter()
                            .zip(&v[ib])
                            .map(|(p, q)| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t])
                            .collect();
                        v[it] = row;
                    }
                }
                known[it] = true;
            }
        }
    };
    pass(true, &mut out, &mut known);
    pass(false, &mut out, &mut known);
    // Nothing set anywhere on a line: the identity.
    for (i, k) in known.iter().enumerate() {
        if !k {
            match &mut out {
                BindValues::Scalar(v) => v[i] = target.identity(),
                BindValues::Offsets(v) => v[i].iter_mut().for_each(|p| *p = [0.0, 0.0]),
            }
        }
    }
    out
}

fn rest_points(kind: &NodeKind) -> Vec<Vec2> {
    match kind {
        NodeKind::Part(part) => part.mesh.vertices.iter().map(|v| Vec2::from(*v)).collect(),
        NodeKind::Mask(mesh) | NodeKind::MeshDeformer(mesh) => mesh.vertices.iter().map(|v| Vec2::from(*v)).collect(),
        NodeKind::Lattice(l) => {
            let mut pts = Vec::with_capacity(l.points());
            for row in 0..l.rows {
                for col in 0..l.cols {
                    pts.push(Vec2::from(l.rest(col, row)));
                }
            }
            pts
        }
        _ => Vec::new(),
    }
}

impl Evaluator {
    /// Prepares `def` (checked first) at its parameters' defaults.
    ///
    /// # Errors
    /// The first rule `def` breaks.
    pub fn new(def: PuppetDef) -> Result<Self, PuppetError> {
        def.check()?;
        let filled = def
            .params
            .iter()
            .map(|p| p.bindings.iter().map(|b| fill_unset(p, &b.set, &b.values, b.target)).collect())
            .collect();
        let nodes = def
            .nodes
            .iter()
            .map(|n| {
                let rest = rest_points(&n.kind);
                let count = rest.len();
                let drawn = matches!(n.kind, NodeKind::Part(_) | NodeKind::Mask(_));
                NodeRt {
                    t: Vec2::ZERO,
                    r: 0.0,
                    rx: 0.0,
                    ry: 0.0,
                    s: Vec2::ONE,
                    z: 0.0,
                    opacity: 1.0,
                    tint: [1.0; 3],
                    offsets: vec![Vec2::ZERO; count],
                    points: rest.clone(),
                    model: if drawn { rest.clone() } else { Vec::new() },
                    rest,
                    local: Affine2::IDENTITY,
                    up: Affine2::IDENTITY,
                    deformer: None,
                    total_z: 0.0,
                    visible: true,
                    bins: match &n.kind {
                        NodeKind::MeshDeformer(mesh) => Some(TriBins::new(mesh)),
                        _ => None,
                    },
                }
            })
            .collect();
        // Containers: the puppet, then each composite.
        let mut slot = vec![0usize; def.nodes.len()];
        let mut containers = 1;
        for (i, n) in def.nodes.iter().enumerate() {
            if matches!(n.kind, NodeKind::Composite(_)) {
                slot[i] = containers;
                containers += 1;
            }
        }
        let mut members = vec![Vec::new(); containers];
        for (i, n) in def.nodes.iter().enumerate() {
            if !matches!(n.kind, NodeKind::Part(_) | NodeKind::Composite(_)) {
                continue;
            }
            // The nearest composite ancestor owns it.
            let mut owner = 0;
            let mut p = n.parent;
            while let Some(ix) = p {
                if matches!(def.nodes[ix.get()].kind, NodeKind::Composite(_)) {
                    owner = slot[ix.get()];
                    break;
                }
                p = def.nodes[ix.get()].parent;
            }
            members[owner].push(u32::try_from(i).unwrap_or(u32::MAX));
        }
        let values = def.params.iter().map(|p| p.default).collect();
        let mut eval = Self { def, values, filled, nodes, members, slot, list: DrawList2D::default() };
        eval.evaluate();
        Ok(eval)
    }

    /// The puppet.
    #[must_use]
    pub fn def(&self) -> &PuppetDef {
        &self.def
    }

    /// Sets parameter `param` to `value`, clamped to its range (a one-axis parameter ignores `y`).
    pub fn set(&mut self, param: ParamIx, value: [f32; 2]) {
        if let (Some(p), Some(v)) = (self.def.params.get(param.get()), self.values.get_mut(param.get())) {
            *v = [value[0].clamp(p.min[0], p.max[0]), value[1].clamp(p.min[1], p.max[1])];
        }
    }

    /// The value of `param`.
    #[must_use]
    pub fn value(&self, param: ParamIx) -> [f32; 2] {
        self.values.get(param.get()).copied().unwrap_or_default()
    }

    /// Every parameter back to its default.
    pub fn reset(&mut self) {
        for (v, p) in self.values.iter_mut().zip(&self.def.params) {
            *v = p.default;
        }
    }

    /// Where `node`'s local point `p` is in model space, this frame.
    #[must_use]
    pub fn to_model(&self, node: NodeIx, p: Vec2) -> Vec2 {
        if node.get() >= self.nodes.len() {
            return p;
        }
        self.to_model_from(node.get(), p)
    }

    /// The transform of `node` up to the nearest deformer it hangs under (all the way up when there is none).
    #[must_use]
    pub fn frame(&self, node: NodeIx) -> Affine2 {
        self.nodes.get(node.get()).map_or(Affine2::IDENTITY, |n| n.up)
    }

    /// The model-space points of a part or mask node, this frame (empty for any other node).
    #[must_use]
    pub fn points(&self, node: NodeIx) -> &[Vec2] {
        self.nodes.get(node.get()).map_or(&[], |n| n.model.as_slice())
    }

    /// The frame's draw list, from the last [`Evaluator::evaluate`].
    #[must_use]
    pub fn draw_list(&self) -> &DrawList2D {
        &self.list
    }

    /// Maps `q`, in deformer `d`'s rest space, through its current shape.
    fn warp(&self, d: usize, q: Vec2) -> Vec2 {
        let n = &self.nodes[d];
        match &self.def.nodes[d].kind {
            NodeKind::Lattice(l) => {
                let (cols, rows) = (l.cols as usize, l.rows as usize);
                #[allow(clippy::cast_precision_loss, reason = "at most 64 points")]
                let u = (q.x - l.origin[0]) / l.size[0] * (cols - 1) as f32;
                #[allow(clippy::cast_precision_loss, reason = "at most 64 points")]
                let v = (q.y - l.origin[1]) / l.size[1] * (rows - 1) as f32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the grid")]
                let c0 = (u.floor().max(0.0) as usize).min(cols - 2);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the grid")]
                let r0 = (v.floor().max(0.0) as usize).min(rows - 2);
                // Outside the grid the edge cells extend linearly.
                #[allow(clippy::cast_precision_loss, reason = "at most 64 points")]
                let (fu, fv) = (u - c0 as f32, v - r0 as f32);
                let p = &n.points;
                let at = |c: usize, r: usize| p[r * cols + c];
                let top = at(c0, r0).lerp(at(c0 + 1, r0), fu);
                let bottom = at(c0, r0 + 1).lerp(at(c0 + 1, r0 + 1), fu);
                top.lerp(bottom, fv)
            }
            NodeKind::MeshDeformer(mesh) => {
                let tri_at = |t: u32| mesh.triangles.get(t as usize).map(|tri| tri.map(|i| i as usize));
                let bary = |tri: [usize; 3]| barycentric(q, n.rest[tri[0]], n.rest[tri[1]], n.rest[tri[2]]);
                let inside = n.bins.as_ref().and_then(|bins| {
                    bins.candidates(q).iter().find_map(|&t| {
                        let tri = tri_at(t)?;
                        let b = bary(tri);
                        (b.iter().all(|&x| x >= -1e-5)).then_some((tri, b))
                    })
                });
                // Outside every triangle: the least-outside one, extrapolated.
                let best = inside.or_else(|| {
                    mesh.triangles.iter().map(|tri| tri.map(|i| i as usize)).map(|tri| (tri, bary(tri))).max_by(
                        |a, b| {
                            let ma = a.1.iter().fold(f32::INFINITY, |m, &x| m.min(x));
                            let mb = b.1.iter().fold(f32::INFINITY, |m, &x| m.min(x));
                            ma.total_cmp(&mb)
                        },
                    )
                });
                match best {
                    Some((tri, b)) => n.points[tri[0]] * b[0] + n.points[tri[1]] * b[1] + n.points[tri[2]] * b[2],
                    None => q,
                }
            }
            _ => q,
        }
    }

    /// Poses the puppet at the current values and rebuilds the draw list. Allocates nothing.
    pub fn evaluate(&mut self) {
        for n in &mut self.nodes {
            n.t = Vec2::ZERO;
            n.r = 0.0;
            n.rx = 0.0;
            n.ry = 0.0;
            n.s = Vec2::ONE;
            n.z = 0.0;
            n.opacity = 1.0;
            n.tint = [1.0; 3];
            n.offsets.iter_mut().for_each(|o| *o = Vec2::ZERO);
        }
        self.apply_bindings();
        self.pose();
        self.build_list();
    }

    fn apply_bindings(&mut self) {
        for (pi, param) in self.def.params.iter().enumerate() {
            let value = self.values[pi];
            for (binding, values) in param.bindings.iter().zip(&self.filled[pi]) {
                let sx = Stencil::new(&param.keys[0], value[0], binding.interp);
                let sy = Stencil::new(&param.keys[1], value[1], binding.interp);
                let n = &mut self.nodes[binding.node.get()];
                match values {
                    BindValues::Scalar(v) => {
                        let mut acc = 0.0;
                        for a in 0..sx.n {
                            for b in 0..sy.n {
                                acc += sx.w[a] * sy.w[b] * v[param.key_index(sx.idx[a], sy.idx[b])];
                            }
                        }
                        match binding.target {
                            BindTarget::TranslateX => n.t.x += acc,
                            BindTarget::TranslateY => n.t.y += acc,
                            BindTarget::Rotate => n.r += acc,
                            BindTarget::RotateX => n.rx += acc,
                            BindTarget::RotateY => n.ry += acc,
                            BindTarget::ScaleX => n.s.x *= acc,
                            BindTarget::ScaleY => n.s.y *= acc,
                            BindTarget::ZSort => n.z += acc,
                            BindTarget::Opacity => n.opacity *= acc,
                            BindTarget::TintR => n.tint[0] *= acc,
                            BindTarget::TintG => n.tint[1] *= acc,
                            BindTarget::TintB => n.tint[2] *= acc,
                            // `check` refuses a deform binding with numbers.
                            _ => {}
                        }
                    }
                    BindValues::Offsets(sets) => {
                        for a in 0..sx.n {
                            for b in 0..sy.n {
                                let w = sx.w[a] * sy.w[b];
                                if w == 0.0 {
                                    continue;
                                }
                                let set = &sets[param.key_index(sx.idx[a], sy.idx[b])];
                                for (o, d) in n.offsets.iter_mut().zip(set) {
                                    *o += Vec2::from(*d) * w;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn pose(&mut self) {
        for i in 0..self.nodes.len() {
            let node = &self.def.nodes[i];
            let tr = &node.transform;
            let rt = &self.nodes[i];
            let scale = Vec2::from(tr.scale) * rt.s * Vec2::new(rt.ry.cos(), rt.rx.cos());
            let local =
                Affine2::from_scale_angle_translation(scale, tr.rotation + rt.r, Vec2::from(tr.translation) + rt.t);
            let (up, deformer, parent_z, parent_visible) = match node.parent {
                None => (local, None, 0.0, true),
                Some(p) => {
                    let pr = &self.nodes[p.get()];
                    let parent_is_deformer =
                        matches!(self.def.nodes[p.get()].kind, NodeKind::Lattice(_) | NodeKind::MeshDeformer(_));
                    if parent_is_deformer {
                        (local, Some(p.get()), pr.total_z, pr.visible)
                    } else {
                        (pr.up * local, pr.deformer, pr.total_z, pr.visible)
                    }
                }
            };
            let rt = &mut self.nodes[i];
            rt.local = local;
            rt.up = up;
            rt.deformer = deformer;
            rt.total_z = parent_z + node.zsort + rt.z;
            rt.visible = parent_visible && node.enabled;
            for ((p, r), o) in rt.points.iter_mut().zip(&rt.rest).zip(&rt.offsets) {
                *p = *r + *o;
            }
        }
        // Parents are posed before children, so deformers are ready when their children map through them.
        for i in 0..self.nodes.len() {
            if self.nodes[i].model.is_empty() {
                continue;
            }
            let mut model = std::mem::take(&mut self.nodes[i].model);
            for (m, p) in model.iter_mut().zip(&self.nodes[i].points) {
                *m = self.to_model_from(i, *p);
            }
            self.nodes[i].model = model;
        }
    }

    fn to_model_from(&self, i: usize, p: Vec2) -> Vec2 {
        let n = &self.nodes[i];
        let mut q = n.up.transform_point2(p);
        let mut d = n.deformer;
        while let Some(di) = d {
            q = self.warp(di, q);
            q = self.nodes[di].up.transform_point2(q);
            d = self.nodes[di].deformer;
        }
        q
    }

    fn build_list(&mut self) {
        let mut list = std::mem::take(&mut self.list);
        list.clear();
        let mut members = std::mem::take(&mut self.members);
        for m in &mut members {
            let nodes = &self.nodes;
            // Back to front: the larger total zsort first; ties keep tree order. Keys are unique, so the unstable
            // (allocation-free) sort is deterministic.
            m.sort_unstable_by(|&a, &b| {
                let (za, zb) = (nodes[a as usize].total_z, nodes[b as usize].total_z);
                zb.total_cmp(&za).then(a.cmp(&b))
            });
        }
        self.emit(&members, 0, &mut list);
        self.members = members;
        self.list = list;
    }

    fn emit(&self, members: &[Vec<u32>], container: usize, list: &mut DrawList2D) {
        for &m in &members[container] {
            let i = m as usize;
            if !self.nodes[i].visible {
                continue;
            }
            match &self.def.nodes[i].kind {
                NodeKind::Part(part) => {
                    let masked = self.emit_masks(&part.draw.masks, part.draw.mask_threshold, list);
                    let (first, count) = self.push_mesh(i, &part.mesh, list);
                    let state = self.state(i, &part.draw, masked);
                    list.cmds.push(DrawCmd2D::Mesh { texture: part.texture, first, count, state });
                }
                NodeKind::Composite(c) => {
                    list.cmds.push(DrawCmd2D::CompositeBegin);
                    self.emit(members, self.slot[i], list);
                    let masked = self.emit_masks(&c.draw.masks, c.draw.mask_threshold, list);
                    list.cmds.push(DrawCmd2D::CompositeEnd { state: self.state(i, &c.draw, masked) });
                }
                _ => {}
            }
        }
    }

    fn state(&self, i: usize, draw: &omuzaki_puppet_api::Drawable, masked: bool) -> DrawState {
        let rt = &self.nodes[i];
        DrawState {
            blend: draw.blend,
            opacity: (draw.opacity * rt.opacity).clamp(0.0, 1.0),
            tint: [draw.tint[0] * rt.tint[0], draw.tint[1] * rt.tint[1], draw.tint[2] * rt.tint[2]],
            screen_tint: draw.screen_tint,
            masked,
        }
    }

    fn emit_masks(&self, masks: &[omuzaki_puppet_api::MaskRef], threshold: f32, list: &mut DrawList2D) -> bool {
        if masks.is_empty() {
            return false;
        }
        list.cmds.push(DrawCmd2D::MaskBegin);
        for mask in masks {
            let s = mask.source.get();
            let (mesh, texture) = match &self.def.nodes[s].kind {
                NodeKind::Part(part) => (&part.mesh, Some(part.texture)),
                NodeKind::Mask(mesh) => (mesh, None),
                _ => continue,
            };
            let (first, count) = self.push_mesh(s, mesh, list);
            list.cmds.push(DrawCmd2D::MaskShape { texture, first, count, mode: mask.mode, threshold });
        }
        true
    }

    fn push_mesh(&self, i: usize, mesh: &Mesh, list: &mut DrawList2D) -> (u32, u32) {
        let base = u32::try_from(list.vertices.len()).unwrap_or(u32::MAX);
        let first = u32::try_from(list.indices.len()).unwrap_or(u32::MAX);
        let model = &self.nodes[i].model;
        for (k, p) in model.iter().enumerate() {
            let uv = mesh.uvs.get(k).copied().unwrap_or([0.0, 0.0]);
            list.vertices.push(Vertex2D { pos: p.to_array(), uv });
        }
        for tri in &mesh.triangles {
            list.indices.extend(tri.iter().map(|&k| base + k));
        }
        let count = u32::try_from(mesh.triangles.len() * 3).unwrap_or(u32::MAX);
        (first, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stencils_weigh_the_keys_around_the_value() {
        let keys = [-1.0, 0.0, 1.0];
        let s = Stencil::new(&keys, 0.5, Interp::Linear);
        assert_eq!((s.idx[0], s.idx[1], s.n), (1, 2, 2));
        assert!((s.w[0] - 0.5).abs() < 1e-6 && (s.w[1] - 0.5).abs() < 1e-6);
        let s = Stencil::new(&keys, 9.0, Interp::Linear);
        assert!((s.w[1] - 1.0).abs() < 1e-6, "clamped past the last key");
        let s = Stencil::new(&keys, 0.4, Interp::Nearest);
        assert_eq!((s.idx[0], s.n), (1, 1));
        let s = Stencil::new(&keys, 0.3, Interp::Cubic);
        let sum: f32 = s.w.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "Catmull-Rom weights sum to one");
        let s = Stencil::new(&keys, 1.0, Interp::Cubic);
        let at: f32 = (0..s.n).filter(|&k| s.idx[k] == 2).map(|k| s.w[k]).sum();
        assert!((at - 1.0).abs() < 1e-5, "Cubic passes through a key");
    }

    #[test]
    fn barycentric_coordinates_rebuild_the_point() {
        let (a, b, c) = (Vec2::new(0.0, 0.0), Vec2::new(4.0, 0.0), Vec2::new(0.0, 4.0));
        let p = Vec2::new(1.0, 2.0);
        let w = barycentric(p, a, b, c);
        let back = a * w[0] + b * w[1] + c * w[2];
        assert!((back - p).length() < 1e-6);
    }
}
