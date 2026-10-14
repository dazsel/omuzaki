//! CPU half of the custom rasteriser: walks a `DrawList` and turns every fill and stroke into an *edge soup* in pixel space
//! (one line segment per edge) plus one `GpuCmd` per drawing command. Curves are flattened here; a stroke becomes a union
//! of convex pieces (segment quads, join and cap discs, miter wedges), each wound positively, so the GPU can treat it as a
//! non-zero fill and the stroke's own overlaps never double-blend.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceLayerSpec, Join, Paint, PathEl, PathRef};

/// Most nested clips one command can sit under.
pub const MAX_CLIPS: usize = 4;
/// Flattening tolerance in pixels.
const TOL_PX: f32 = 0.05;
/// Canvas 2D's miter limit (the CPU rasteriser follows it).
const MITER_LIMIT: f32 = 10.0;

/// One drawing command as the shader reads it. Layout mirrors `Cmd` in `fill.wgsl` (all members 16-byte aligned).
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct GpuCmd {
    pub bbox: [f32; 4],
    /// unused x3, paint kind (0 solid, 1 linear, 2 radial).
    pub ranges: [u32; 4],
    /// opacity, stop count, radius, unused.
    pub params: [f32; 4],
    pub offsets: [f32; 4],
    pub col: [[f32; 4]; 4],
    /// Linear: from.xy, to.xy. Radial: centre.xy.
    pub grad: [f32; 4],
    /// Pixel to paint-space affine, two rows of (a, b, c, unused).
    pub inv0: [f32; 4],
    pub inv1: [f32; 4],
}

/// Height of a band in pixels: the unit the edge lists are bucketed by, so a fragment loops over only the edges that cross its
/// band (not every edge of the path).
pub const BAND: f32 = 16.0;

/// One instance to draw: a command restricted to one band of rows, with the edge lists that matter there.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct GpuBand {
    /// command index, geometry list start, geometry list count, clip count.
    pub head: [u32; 4],
    /// top and bottom row of the band's quad.
    pub y: [f32; 4],
    /// clip 0 start, count, clip 1 start, count.
    pub c01: [u32; 4],
    pub c23: [u32; 4],
}

/// Per-band index lists of one path: band `first + i` has `ranges[i]` (start, count) in `Prepared::indices`.
#[derive(Clone)]
struct BandList {
    first: i32,
    ranges: Vec<(u32, u32)>,
}

impl BandList {
    fn get(&self, band: i32) -> (u32, u32) {
        let i = band - self.first;
        if i < 0 { return (0, 0) }
        self.ranges.get(i as usize).copied().unwrap_or((0, 0))
    }
}

/// The prepared frame: edges, band index lists and commands, reusable between frames to keep their capacity.
#[derive(Default)]
pub struct Prepared {
    pub edges: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub bands: Vec<GpuBand>,
    pub cmds: Vec<GpuCmd>,
    scratch: Vec<Vec<Vec2>>,
    pieces: Vec<Vec2>,
    counts: Vec<u32>,
    pts: Vec<Vec2>,
}

#[derive(Clone)]
struct ClipRef {
    lists: BandList,
    bbox: [f32; 4],
}

/// Why a list could not be prepared.
#[derive(Debug)]
pub struct Unsupported(pub &'static str);

impl Prepared {
    /// Prepares `list` for a target of `spec` at `px_per_m`.
    pub fn build(&mut self, list: &DrawList, spec: &FaceLayerSpec, px_per_m: f32) -> Result<(), Unsupported> {
        self.edges.clear();
        self.cmds.clear();
        self.indices.clear();
        self.bands.clear();
        let (w, h) = spec.rect.size_px(px_per_m);
        #[allow(clippy::cast_precision_loss)]
        let (sx, sy) = (w as f32 / spec.rect.width(), h as f32 / spec.rect.height());
        let to_px = Affine2::from_cols(
            Vec2::new(sx, 0.0),
            Vec2::new(0.0, -sy),
            Vec2::new(-spec.rect.x0 * sx, spec.rect.y1 * sy),
        );
        let target = [0.0, 0.0, w as f32, h as f32];
        let mut xforms = vec![Affine2::IDENTITY];
        let mut clips: Vec<ClipRef> = Vec::new();
        for cmd in list.cmds() {
            match *cmd {
                Cmd::PushTransform(t) => {
                    let top = *xforms.last().unwrap_or(&Affine2::IDENTITY);
                    xforms.push(top * t);
                }
                Cmd::PopTransform => {
                    if xforms.len() > 1 {
                        xforms.pop();
                    }
                }
                Cmd::PushClip(p) => {
                    let m = to_px * *xforms.last().unwrap_or(&Affine2::IDENTITY);
                    let start = self.edges.len();
                    let bbox = self.emit_fill(list, p, m);
                    let lists = self.bucket(start);
                    clips.push(ClipRef { lists, bbox });
                }
                Cmd::PopClip => {
                    clips.pop();
                }
                Cmd::Fill { path, paint, opacity } => {
                    let m = to_px * *xforms.last().unwrap_or(&Affine2::IDENTITY);
                    let start = self.edges.len();
                    let bbox = self.emit_fill(list, path, m);
                    self.finish_cmd(start, bbox, &clips, &paint, opacity, m, target)?;
                }
                Cmd::Stroke { path, paint, width, cap, join, opacity } => {
                    let m = to_px * *xforms.last().unwrap_or(&Affine2::IDENTITY);
                    let start = self.edges.len();
                    let bbox = self.emit_stroke(list, path, m, width, cap, join);
                    self.finish_cmd(start, bbox, &clips, &paint, opacity, m, target)?;
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_cmd(
        &mut self,
        start: usize,
        mut bbox: [f32; 4],
        clips: &[ClipRef],
        paint: &Paint,
        opacity: f32,
        m: Affine2,
        target: [f32; 4],
    ) -> Result<(), Unsupported> {
        if clips.len() > MAX_CLIPS {
            return Err(Unsupported("clip stack deeper than 4"));
        }
        if self.edges.len() == start {
            return Ok(());
        }
        // One pixel of margin for the anti-aliased rim, then the intersection with the target and every clip.
        bbox = [bbox[0].floor() - 1.0, bbox[1].floor() - 1.0, bbox[2].ceil() + 1.0, bbox[3].ceil() + 1.0];
        for b in clips.iter().map(|c| c.bbox).chain([target]) {
            bbox = [bbox[0].max(b[0].floor()), bbox[1].max(b[1].floor()), bbox[2].min(b[2].ceil()), bbox[3].min(b[3].ceil())];
        }
        if bbox[2] <= bbox[0] || bbox[3] <= bbox[1] {
            self.edges.truncate(start);
            return Ok(());
        }
        let mut c = GpuCmd::zeroed();
        c.bbox = bbox;
        c.params[0] = opacity;
        // Paint space = the command's local space: invert the whole local-to-pixel matrix.
        let inv = m.inverse();
        c.inv0 = [inv.matrix2.x_axis.x, inv.matrix2.y_axis.x, inv.translation.x, 0.0];
        c.inv1 = [inv.matrix2.x_axis.y, inv.matrix2.y_axis.y, inv.translation.y, 0.0];
        match paint {
            Paint::Solid(col) => {
                c.col[0] = srgb(col.r, col.g, col.b, col.a);
                c.params[1] = 1.0;
            }
            Paint::Linear { from, to, stops } => {
                c.ranges[3] = 1;
                c.grad = [from.x, from.y, to.x, to.y];
                fill_stops(&mut c, stops.as_slice());
            }
            Paint::Radial { centre, radius, stops } => {
                c.ranges[3] = 2;
                c.grad = [centre.x, centre.y, 0.0, 0.0];
                c.params[2] = *radius;
                fill_stops(&mut c, stops.as_slice());
            }
        }
        let cmd_index = self.cmds.len() as u32;
        self.cmds.push(c);
        let geom = self.bucket(start);
        let (b0, b1) = ((bbox[1] / BAND).floor() as i32, ((bbox[3] / BAND).ceil() as i32));
        for band in b0..b1 {
            let (gs, gc) = geom.get(band);
            if gc == 0 {
                continue;
            }
            let mut clip_ranges = [0u32; 8];
            let mut empty = false;
            for (i, clip) in clips.iter().enumerate() {
                let (cs, cc) = clip.lists.get(band);
                empty |= cc == 0;
                clip_ranges[i * 2] = cs;
                clip_ranges[i * 2 + 1] = cc;
            }
            if empty {
                continue;
            }
            self.bands.push(GpuBand {
                head: [cmd_index, gs, gc, clips.len() as u32],
                y: [(band as f32 * BAND).max(bbox[1]), ((band + 1) as f32 * BAND).min(bbox[3]), 0.0, 0.0],
                c01: [clip_ranges[0], clip_ranges[1], clip_ranges[2], clip_ranges[3]],
                c23: [clip_ranges[4], clip_ranges[5], clip_ranges[6], clip_ranges[7]],
            });
        }
        Ok(())
    }

    /// Buckets the edges from `start` to the end of the edge list by band; their indices go to `self.indices`.
    fn bucket(&mut self, start: usize) -> BandList {
        let edges = &self.edges[start..];
        if edges.is_empty() {
            return BandList { first: 0, ranges: Vec::new() };
        }
        let band_of = |y: f32| (y / BAND).floor() as i32;
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for e in edges {
            lo = lo.min(band_of(e[1].min(e[3])));
            hi = hi.max(band_of(e[1].max(e[3])));
        }
        // Counting sort: sizes first, then one pass writing each edge index into every band it spans.
        let n_bands = (hi - lo + 1) as usize;
        self.counts.clear();
        self.counts.resize(n_bands, 0);
        for e in edges {
            for b in band_of(e[1].min(e[3]))..=band_of(e[1].max(e[3])) {
                self.counts[(b - lo) as usize] += 1;
            }
        }
        let base = self.indices.len() as u32;
        let mut ranges = Vec::with_capacity(n_bands);
        let mut at = base;
        for &c in &self.counts {
            ranges.push((at, c));
            at += c;
        }
        self.indices.resize(at as usize, 0);
        self.counts.fill(0);
        for (i, e) in edges.iter().enumerate() {
            for b in band_of(e[1].min(e[3]))..=band_of(e[1].max(e[3])) {
                let slot = (b - lo) as usize;
                self.indices[(ranges[slot].0 + self.counts[slot]) as usize] = (start + i) as u32;
                self.counts[slot] += 1;
            }
        }
        BandList { first: lo, ranges }
    }

    // ------------------------------------------------------------------------------------------ fills

    fn emit_fill(&mut self, list: &DrawList, path: PathRef, m: Affine2) -> [f32; 4] {
        let mut subs = std::mem::take(&mut self.scratch);
        flatten(list.elements(path), tol_local(m), &mut subs);
        let mut bb = BBox::new();
        for sub in subs.iter().filter(|s| s.len() >= 2) {
            // Filled sub-paths are implicitly closed.
            let n = sub.len();
            for i in 0..n {
                let (a, b) = (m.transform_point2(sub[i]), m.transform_point2(sub[(i + 1) % n]));
                self.push_edge(a, b, &mut bb);
            }
        }
        self.scratch = subs;
        bb.0
    }

    fn push_edge(&mut self, a: Vec2, b: Vec2, bb: &mut BBox) {
        bb.add(a);
        if a.y != b.y {
            self.edges.push([a.x, a.y, b.x, b.y]);
        }
    }

    // ------------------------------------------------------------------------------------------ strokes

    fn emit_stroke(&mut self, list: &DrawList, path: PathRef, m: Affine2, width: f32, cap: Cap, join: Join) -> [f32; 4] {
        let mut subs = std::mem::take(&mut self.scratch);
        flatten(list.elements(path), tol_local(m), &mut subs);
        let mut bb = BBox::new();
        let half = width * 0.5;
        let scale = (m.matrix2.x_axis.length() + m.matrix2.y_axis.length()) * 0.5;
        for sub in &subs {
            let closed = sub.len() > 2 && sub.first() == sub.last();
            let mut pts: Vec<Vec2> = Vec::with_capacity(sub.len());
            for &p in sub {
                if pts.last().is_none_or(|&q| (q - p).length_squared() > 1e-14) {
                    pts.push(p);
                }
            }
            if closed && pts.len() > 1 {
                pts.pop();
            }
            if pts.len() == 1 {
                if cap == Cap::Round {
                    self.disc(pts[0], half, m, scale, &mut bb);
                }
                continue;
            }
            if pts.len() < 2 {
                continue;
            }
            let n = pts.len();
            let segs = if closed { n } else { n - 1 };
            for i in 0..segs {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let d = (b - a).normalize() * half;
                let nrm = Vec2::new(-d.y, d.x);
                self.piece(&[a + nrm, b + nrm, b - nrm, a - nrm], m, &mut bb);
            }
            let join_range = if closed { 0..n } else { 1..n - 1 };
            for i in join_range {
                let (p, a, b) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
                self.join(p, a, b, half, join, m, scale, &mut bb);
            }
            if !closed && cap == Cap::Round {
                self.disc(pts[0], half, m, scale, &mut bb);
                self.disc(pts[n - 1], half, m, scale, &mut bb);
            }
        }
        self.scratch = subs;
        bb.0
    }

    #[allow(clippy::too_many_arguments)]
    fn join(&mut self, p: Vec2, a: Vec2, b: Vec2, half: f32, join: Join, m: Affine2, scale: f32, bb: &mut BBox) {
        let d0 = (a - p).normalize();
        let d1 = (b - a).normalize();
        let cross = d0.x * d1.y - d0.y * d1.x;
        if cross.abs() < 1e-6 && d0.dot(d1) > 0.0 {
            return;
        }
        if join == Join::Round {
            // A pie wedge on the outer side only, sized by the turn: flattened curves turn by tiny angles at every vertex,
            // and a full disc per vertex would multiply the edge count by the disc's segment count.
            let side = if cross > 0.0 { -1.0 } else { 1.0 };
            let n0 = Vec2::new(-d0.y, d0.x) * half * side;
            let a0 = n0.y.atan2(n0.x);
            let turn = cross.atan2(d0.dot(d1));
            let rp = (half * scale).max(0.01);
            let step = 2.0 * (1.0 - (TOL_PX / rp).min(0.99)).acos().max(0.05);
            let steps = ((turn.abs() / step).ceil() as usize).max(1);
            let mut pts = std::mem::take(&mut self.pts);
            pts.clear();
            pts.push(a);
            for i in 0..=steps {
                let t = a0 + turn * i as f32 / steps as f32;
                pts.push(a + Vec2::new(t.cos(), t.sin()) * half);
            }
            self.piece(&pts, m, bb);
            self.pts = pts;
            return;
        }
        // The outer side is opposite the turn.
        let side = if cross > 0.0 { -1.0 } else { 1.0 };
        let n0 = Vec2::new(-d0.y, d0.x) * half * side;
        let n1 = Vec2::new(-d1.y, d1.x) * half * side;
        let cos_turn = d0.dot(d1);
        // Miter length ratio = 1 / sin(theta/2) where theta is the interior angle; theta = pi - turn.
        let cos_half = ((1.0 + cos_turn) * 0.5).sqrt();
        let ratio = 1.0 / cos_half.max(1e-6);
        if ratio <= MITER_LIMIT && cos_half > 1e-3 {
            let tip = a + (n0 + n1) / (1.0 + cos_turn);
            self.piece(&[a, a + n0, tip, a + n1], m, bb);
        } else {
            self.piece(&[a, a + n0, a + n1], m, bb);
        }
    }

    fn disc(&mut self, c: Vec2, r: f32, m: Affine2, scale: f32, bb: &mut BBox) {
        let rp = (r * scale).max(0.01);
        let n = ((std::f32::consts::PI / (1.0 - (TOL_PX / rp).min(0.99)).acos()).ceil() as usize).clamp(8, 96);
        let mut pts = std::mem::take(&mut self.pts);
        pts.clear();
        pts.extend((0..n).map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            c + Vec2::new(t.cos(), t.sin()) * r
        }));
        self.piece(&pts, m, bb);
        self.pts = pts;
    }

    /// Adds a convex piece (local space), positively wound in pixel space.
    fn piece(&mut self, pts: &[Vec2], m: Affine2, bb: &mut BBox) {
        self.pieces.clear();
        self.pieces.extend(pts.iter().map(|&p| m.transform_point2(p)));
        let mut area = 0.0;
        for i in 0..self.pieces.len() {
            let (a, b) = (self.pieces[i], self.pieces[(i + 1) % self.pieces.len()]);
            area += a.x * b.y - b.x * a.y;
        }
        if area < 0.0 {
            self.pieces.reverse();
        }
        let n = self.pieces.len();
        for i in 0..n {
            let (a, b) = (self.pieces[i], self.pieces[(i + 1) % n]);
            self.push_edge(a, b, bb);
        }
    }
}

struct BBox([f32; 4]);

impl BBox {
    fn new() -> Self {
        Self([f32::MAX, f32::MAX, f32::MIN, f32::MIN])
    }
    fn add(&mut self, p: Vec2) {
        self.0 = [self.0[0].min(p.x), self.0[1].min(p.y), self.0[2].max(p.x), self.0[3].max(p.y)];
    }
}

fn srgb(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [r, g, b, a].map(|c| f32::from(c) / 255.0)
}

fn fill_stops(c: &mut GpuCmd, stops: &[omuzaki_face_api::GradientStop]) {
    c.params[1] = stops.len() as f32;
    for (i, s) in stops.iter().enumerate().take(4) {
        c.offsets[i] = s.offset;
        c.col[i] = srgb(s.colour.r, s.colour.g, s.colour.b, s.colour.a);
    }
}

/// Flattening tolerance in local units for a local-to-pixel matrix.
fn tol_local(m: Affine2) -> f32 {
    let s = m.matrix2.x_axis.length().max(m.matrix2.y_axis.length()).max(1e-9);
    TOL_PX / s
}

/// Flattens path elements into polylines (closed sub-paths repeat their first point at the end).
pub fn flatten(els: &[PathEl], tol: f32, out: &mut Vec<Vec<Vec2>>) {
    out.clear();
    let mut cur: Vec<Vec2> = Vec::new();
    let mut start = Vec2::ZERO;
    let mut last = Vec2::ZERO;
    for el in els {
        match *el {
            PathEl::Move(p) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(p);
                start = p;
                last = p;
            }
            PathEl::Line(p) => {
                cur.push(p);
                last = p;
            }
            PathEl::Quad(c, p) => {
                let dd = (last - 2.0 * c + p).length();
                let n = ((dd / (4.0 * tol)).sqrt().ceil() as usize).clamp(1, 200);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    cur.push(last * (u * u) + c * (2.0 * u * t) + p * (t * t));
                }
                last = p;
            }
            PathEl::Cubic(c1, c2, p) => {
                let dd = (last - 2.0 * c1 + c2).length().max((c1 - 2.0 * c2 + p).length());
                let n = ((0.75 * dd / tol).sqrt().ceil() as usize).clamp(1, 200);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    cur.push(last * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + p * (t * t * t));
                }
                last = p;
            }
            PathEl::Close => {
                if !cur.is_empty() {
                    cur.push(start);
                    out.push(std::mem::take(&mut cur));
                    cur.push(start);
                    last = start;
                }
            }
        }
    }
    if cur.len() > 1 || (cur.len() == 1 && out.is_empty()) {
        out.push(cur);
    }
}
