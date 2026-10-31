//! Garments skinned from the body they cover (#2604).
//!
//! A garment's generator weights its meshes to the bones with its own rule (capsules, a skirt's hem blend), and the
//! body's weights its skin with another. In the bind pose the two agree on where everything is, so the garment clears
//! the body; at a bend they follow different blends, and the body passes through the cloth over the stomach or the
//! two part at the waist. Real cloth over a body moves with the skin under it.
//!
//! So the body publishes its skinned surface ([`SkinFact`]), and every garment built over it takes, for each vertex
//! close to what lies under it, the weights at the nearest point of that surface (interpolated across the triangle
//! there, as Blender's and Maya's weight transfer do). Close means within [`NEAR`]; from there to [`FAR`] the garment's
//! own weights take over smoothly, so a loose hem or a sleeve's cuff keeps the swing its generator gave it. Each garment
//! then publishes its own skin in turn, so the next layer follows the layer under it: a skirt's waistband over tights
//! moves with the tights, and the tights can't come through it. The builder does this for every part of the garment
//! category right after it builds ([`crate::build_part`]); the character's weight brushes are applied after it, so a
//! person's correction still wins.

use std::collections::HashMap;
use std::sync::Arc;

use omuzaki_core::glam::Vec3;

use crate::geometry::PartMesh;
use crate::ids::JointRef;
use crate::kind::PartOutput;

/// Within this distance of the body a garment vertex moves exactly as the skin under it, metres: a fitted garment's
/// ease and a sweater's drape over the stomach.
pub const NEAR: f32 = 0.025;
/// From this distance on a garment vertex keeps its own weights, metres: a hem or a cuff that stands off the body.
pub const FAR: f32 = 0.05;
/// The side of a cell of the surface's triangle grid, metres: the farthest a query reaches, so one cell round the
/// query's own holds every triangle that can be nearest.
const CELL: f32 = FAR;
/// The most joints that move one vertex.
const INFLUENCES: usize = 4;

/// The body's skin as garments read it: its triangles in the bind pose and each vertex's weights.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkinSurface {
    /// The joints the weights name.
    pub joints: Vec<JointRef>,
    /// Each vertex's position, metres.
    pub positions: Vec<Vec3>,
    /// Each vertex's weights: up to four joints (indices into `joints`) and their weights, summing to 1.
    pub weights: Vec<[(u16, f32); INFLUENCES]>,
    /// The triangles, as vertex indices.
    pub triangles: Vec<[u32; 3]>,
    /// The mesh each triangle came from, in the order the meshes were given.
    owners: Vec<u32>,
    /// The triangles in each grid cell.
    cells: HashMap<(i32, i32, i32), Vec<u32>>,
}

/// The cell holding `p`.
#[allow(clippy::cast_possible_truncation)] // a body is metres across: far inside i32
fn cell(p: Vec3) -> (i32, i32, i32) {
    let c = (p / CELL).floor();
    (c.x as i32, c.y as i32, c.z as i32)
}

/// The point of triangle `abc` nearest `p`, as barycentric weights of `a`, `b` and `c` (Ericson, Real-Time Collision
/// Detection, 5.1.5).
#[allow(clippy::many_single_char_names)] // p, a, b, c and the barycentric v, w: Ericson's names
fn nearest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> [f32; 3] {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return [1.0, 0.0, 0.0];
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return [0.0, 1.0, 0.0];
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return [1.0 - v, v, 0.0];
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return [0.0, 0.0, 1.0];
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return [1.0 - w, 0.0, w];
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return [0.0, 1.0 - w, w];
    }
    let denom = 1.0 / (va + vb + vc);
    let (v, w) = (vb * denom, vc * denom);
    [1.0 - v - w, v, w]
}

impl SkinSurface {
    /// The skin of `meshes` (the body's skinned, static meshes; the others are left out), without what lies inside
    /// another of them: a body is several closed meshes that overlap where they join (the top of a leg inside the
    /// hips), and a garment over the join must take the weights of the skin you see, not of the leg's hidden end.
    pub fn of<'a>(meshes: impl IntoIterator<Item = &'a PartMesh>) -> Self {
        Self::gather(meshes, true)
    }

    /// The skin of a garment's `meshes`, all of it: cloth is open sheets, one over another, with no inside to hide.
    pub fn sheets<'a>(meshes: impl IntoIterator<Item = &'a PartMesh>) -> Self {
        Self::gather(meshes, false)
    }

    #[allow(clippy::many_single_char_names)] // p, v, k, q and the triangle's a, b, c
    fn gather<'a>(meshes: impl IntoIterator<Item = &'a PartMesh>, closed: bool) -> Self {
        let mut out = Self::default();
        let mut slot: HashMap<JointRef, u16> = HashMap::new();
        let mut first = Vec::new();
        for (owner, mesh) in meshes.into_iter().enumerate() {
            let g = &mesh.geometry;
            if mesh.dynamic || mesh.joint_bones.is_empty() || g.weights.len() != g.positions.len() {
                continue;
            }
            let Ok(base) = u32::try_from(out.positions.len()) else { break };
            let mut ids = Vec::with_capacity(mesh.joint_bones.len());
            for joint in &mesh.joint_bones {
                let next = u16::try_from(out.joints.len()).unwrap_or(u16::MAX);
                let id = *slot.entry(joint.clone()).or_insert_with(|| {
                    out.joints.push(joint.clone());
                    next
                });
                ids.push(id);
            }
            for (i, p) in g.positions.iter().enumerate() {
                let mut w = [(0_u16, 0.0_f32); INFLUENCES];
                for k in 0..INFLUENCES {
                    let joint = g.joints.get(i).map_or(0, |j| usize::from(j[k]));
                    w[k] = (ids.get(joint).copied().unwrap_or(0), g.weights[i][k]);
                }
                out.positions.push(*p);
                out.weights.push(w);
            }
            let owner = u32::try_from(owner).unwrap_or(u32::MAX);
            first.push((base, owner));
            for t in g.indices.as_chunks::<3>().0 {
                out.triangles.push([base + t[0], base + t[1], base + t[2]]);
                out.owners.push(owner);
            }
        }
        out.index();
        if !closed {
            return out;
        }
        // A vertex is inside another mesh when the nearest point of that mesh faces away from it.
        let owner_of =
            |v: usize| first.iter().rev().find(|(base, _)| *base as usize <= v).map_or(u32::MAX, |(_, o)| *o);
        let hidden: Vec<bool> = (0..out.positions.len())
            .map(|v| {
                let (p, mine) = (out.positions[v], owner_of(v));
                out.nearest_where(p, |k| out.owners[k] != mine).is_some_and(|(_, k, bary)| {
                    let [a, b, c] = out.triangles[k].map(|i| out.positions[i as usize]);
                    let q = a * bary[0] + b * bary[1] + c * bary[2];
                    (p - q).dot((b - a).cross(c - a)) < 0.0
                })
            })
            .collect();
        if hidden.iter().any(|h| *h) {
            let keep: Vec<bool> = out.triangles.iter().map(|t| !t.iter().all(|i| hidden[*i as usize])).collect();
            let mut k = 0;
            out.triangles.retain(|_| (keep[k], k += 1).0);
            k = 0;
            out.owners.retain(|_| (keep[k], k += 1).0);
            out.index();
        }
        out
    }

    /// Puts every triangle in the grid cells its bounds touch.
    fn index(&mut self) {
        self.cells.clear();
        for (k, t) in self.triangles.iter().enumerate() {
            let [a, b, c] = t.map(|i| self.positions[i as usize]);
            let (lo, hi) = (cell(a.min(b).min(c)), cell(a.max(b).max(c)));
            let Ok(k) = u32::try_from(k) else { break };
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        self.cells.entry((x, y, z)).or_default().push(k);
                    }
                }
            }
        }
    }

    /// Whether there is any skin to read.
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// The point of the skin nearest `p` within [`FAR`]: its distance and the triangle and barycentric weights there.
    fn nearest(&self, p: Vec3) -> Option<(f32, usize, [f32; 3])> {
        self.nearest_where(p, |_| true)
    }

    /// [`nearest`](Self::nearest) among the triangles `keep` takes.
    #[allow(clippy::many_single_char_names)] // p, k and the triangle's a, b, c
    fn nearest_where(&self, p: Vec3, keep: impl Fn(usize) -> bool) -> Option<(f32, usize, [f32; 3])> {
        let (cx, cy, cz) = cell(p);
        let mut best: Option<(f32, usize, [f32; 3])> = None;
        // The query's own cell first, so the best so far is near and prunes the rest.
        let cells = std::iter::once((0, 0, 0)).chain(
            (-1..=1)
                .flat_map(|x| (-1..=1).flat_map(move |y| (-1..=1).map(move |z| (x, y, z))))
                .filter(|o| *o != (0, 0, 0)),
        );
        for (x, y, z) in cells {
            let Some(list) = self.cells.get(&(cx + x, cy + y, cz + z)) else { continue };
            for &k in list {
                let k = k as usize;
                let [a, b, c] = self.triangles[k].map(|i| self.positions[i as usize]);
                // The triangle's bounds are no nearer than the best so far: it can't be nearer either.
                let reach = best.map_or(FAR, |(e, _, _)| e);
                let outside = (a.min(b).min(c) - p).max(p - a.max(b).max(c)).max(Vec3::ZERO);
                if outside.length_squared() > reach * reach || !keep(k) {
                    continue;
                }
                let bary = nearest_on_triangle(p, a, b, c);
                let d = p.distance(a * bary[0] + b * bary[1] + c * bary[2]);
                if d <= FAR && best.is_none_or(|(e, _, _)| d < e) {
                    best = Some((d, k, bary));
                }
            }
        }
        best
    }

    /// The skin's weights at the point `bary` of triangle `k`: by joint, summing to 1.
    fn weights_at(&self, k: usize, bary: [f32; 3]) -> Vec<(JointRef, f32)> {
        let mut blend: Vec<(u16, f32)> = Vec::with_capacity(INFLUENCES * 3);
        for (corner, share) in self.triangles[k].iter().zip(bary) {
            for (joint, w) in self.weights[*corner as usize] {
                let w = w * share;
                if w <= 0.0 {
                    continue;
                }
                match blend.iter_mut().find(|(j, _)| *j == joint) {
                    Some(entry) => entry.1 += w,
                    None => blend.push((joint, w)),
                }
            }
        }
        blend.into_iter().filter_map(|(j, w)| self.joints.get(usize::from(j)).map(|r| (r.clone(), w))).collect()
    }
}

crate::fact! {
    /// The skinned surfaces garments are built over (#2604): the body's, then each garment's in build order. A garment
    /// takes its weights from the nearest of them.
    pub SkinFact = "skin", crate::fact::Append<Arc<SkinSurface>>;
}

crate::register_fact!(SkinFact);
crate::register_fact_probe!(SkinFact);

/// The point nearest `p` on any of `skins` within [`FAR`]: its distance and the weights there.
fn weights_near(skins: &[Arc<SkinSurface>], p: Vec3) -> Option<(f32, Vec<(JointRef, f32)>)> {
    let mut best: Option<(f32, &SkinSurface, usize, [f32; 3])> = None;
    for skin in skins {
        if let Some((d, k, bary)) = skin.nearest(p)
            && best.as_ref().is_none_or(|(e, ..)| d < *e)
        {
            best = Some((d, skin, k, bary));
        }
    }
    let (distance, skin, k, bary) = best?;
    Some((distance, skin.weights_at(k, bary)))
}

/// The share of a vertex's weight its own skin and the skin under it give the same limb, below which the skin under it
/// is not taken, and above which it is taken in full: a sleeve's inside in the armpit lies as close to the chest as to
/// the arm, and must stay with the arm it was made for.
const AGREE: (f32, f32) = (0.05, 0.25);

/// The limb a joint moves with: each arm, each leg, or the trunk (the spine, the shoulders, the neck, the head and
/// any named bone). A shoulder is the trunk's: the cloth over it is the chest's as much as the arm's.
fn limb(joint: &JointRef) -> u8 {
    let JointRef::Humanoid(bone) = joint else { return 0 };
    let name = bone.name();
    let left = name.starts_with("left");
    let side = |l: u8| if left { l } else { l + 1 };
    if ["Arm", "Hand", "Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|n| name.contains(n)) {
        side(1)
    } else if ["Leg", "Foot", "Toes"].iter().any(|n| name.contains(n)) {
        side(3)
    } else {
        0
    }
}

/// How much of a vertex's weight `own` and `other` (each summing to about 1) give the same limb, 0 to 1.
fn agreement<'a>(own: impl Iterator<Item = (&'a JointRef, f32)>, other: &[(JointRef, f32)]) -> f32 {
    let mut shares = [[0.0_f32; 2]; 5];
    for (joint, w) in own {
        shares[usize::from(limb(joint))][0] += w;
    }
    for (joint, w) in other {
        shares[usize::from(limb(joint))][1] += w;
    }
    shares.iter().map(|[a, b]| a.min(*b)).sum()
}

/// `s` through a smoothstep from 0 at `edge0` to 1 at `edge1`.
fn smoothstep(edge0: f32, edge1: f32, s: f32) -> f32 {
    let t = ((s - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Moves each vertex of `mesh` close to one of `skins` onto the weights of the nearest there ([`NEAR`] fully, fading
/// to its own by [`FAR`]), keeping the four heaviest joints. Returns how many vertices it changed.
pub fn transfer_mesh(mesh: &mut PartMesh, skins: &[Arc<SkinSurface>]) -> usize {
    let g = &mesh.geometry;
    if mesh.joint_bones.is_empty() || g.weights.len() != g.positions.len() || g.joints.len() != g.positions.len() {
        return 0;
    }
    let mut changed = 0;
    for i in 0..mesh.geometry.positions.len() {
        let p = mesh.geometry.positions[i];
        let Some((distance, body)) = weights_near(skins, p) else { continue };
        let (joints, weights) = (mesh.geometry.joints[i], mesh.geometry.weights[i]);
        let own = (0..INFLUENCES).filter_map(|k| mesh.joint_bones.get(usize::from(joints[k])).map(|j| (j, weights[k])));
        let take = smoothstep(FAR, NEAR, distance) * smoothstep(AGREE.0, AGREE.1, agreement(own, &body));
        if take <= 0.0 {
            continue;
        }
        let mut blend: Vec<(JointRef, f32)> = Vec::with_capacity(INFLUENCES * 2);
        let mut add = |joint: &JointRef, w: f32| {
            if w <= 0.0 {
                return;
            }
            match blend.iter_mut().find(|(j, _)| j == joint) {
                Some(entry) => entry.1 += w,
                None => blend.push((joint.clone(), w)),
            }
        };
        for k in 0..INFLUENCES {
            if let Some(joint) = mesh.joint_bones.get(usize::from(joints[k])) {
                add(joint, weights[k] * (1.0 - take));
            }
        }
        for (joint, w) in &body {
            add(joint, w * take);
        }
        blend.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        blend.truncate(INFLUENCES);
        let total: f32 = blend.iter().map(|(_, w)| w).sum();
        if total <= 1e-6 {
            continue;
        }
        let (mut new_joints, mut new_weights) = ([0_u16; INFLUENCES], [0.0_f32; INFLUENCES]);
        for (k, (joint, w)) in blend.into_iter().enumerate() {
            let slot = if let Some(slot) = mesh.joint_bones.iter().position(|j| *j == joint) {
                slot
            } else {
                mesh.joint_bones.push(joint);
                mesh.joint_bones.len() - 1
            };
            new_joints[k] = u16::try_from(slot).unwrap_or(0);
            new_weights[k] = w / total;
        }
        mesh.geometry.joints[i] = new_joints;
        mesh.geometry.weights[i] = new_weights;
        changed += 1;
    }
    changed
}

/// Every mesh of a garment `output` skinned from `skins` where it lies close to them ([`transfer_mesh`]), then its own
/// skin published for the layers over it.
pub fn transfer(output: &mut PartOutput, skins: &[Arc<SkinSurface>]) {
    if !skins.is_empty() {
        for mesh in &mut output.meshes {
            transfer_mesh(mesh, skins);
        }
    }
    put(output, SkinSurface::sheets(&output.meshes));
}

/// Publishes the skin of a body part's `output` ([`SkinFact`]), if it has any.
pub fn publish(output: &mut PartOutput) {
    put(output, SkinSurface::of(&output.meshes));
}

fn put(output: &mut PartOutput, skin: SkinSurface) {
    if !skin.is_empty() {
        output.publishes.put::<SkinFact>(Arc::new(skin));
    }
}
