//! The fit check (#331, #2604): does a kit character's outfit still cover its body when the body bends?
//!
//! A garment is built over the body in the bind pose, and conformance proves it clears the body there. What it can't
//! prove is that the two still agree once the skeleton moves: a garment and the skin under it that are weighted to
//! their bones differently part at a bend, and the body shows through the cloth. This poses the character through its
//! own rig (the arm guard included, as the stage runs it), skins every mesh on the CPU and, for each point of the body
//! the outfit covers in the bind pose, asks what a depth test would: looking at the body from outside, along its own
//! normal, is the cloth still in front of it? A point the cloth no longer covers (it sank in, or slid off and left a
//! gap) is a point the body shows through.
//!
//! The test is a ray from each covered body point out along its normal against the posed garment triangles, through a
//! uniform grid (no all-pairs search). It needs no renderer, so it runs the same in CI and in the op.

use std::collections::HashMap;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Skeleton};
use omuzaki_char_api::{BuiltCharacter, JointRef, PartCategory, PartMesh};
use omuzaki_core::glam::{Mat4, Quat, Vec3};

/// How far out along a body point's normal the cloth may stand for the point to count as covered in the bind pose,
/// metres: a garment's ease and a loose sweater's drape, not a skirt's hem far below.
pub const COVER: f32 = 0.04;
/// [`COVER`] for a garment under another, metres: a bell skirt stands well off the tights it hides.
pub const LAYER_COVER: f32 = 0.1;
/// How far out the cloth may stand over a covered point once posed, metres: room for cloth that a bend pulls away
/// from the body, and for a point that turns to look out under a bell skirt (the hips tipped back), without opening a
/// gap you can see through. A gap lets the ray out with nothing in front of it at all.
pub const POSED_COVER: f32 = 0.12;
/// How far behind a point that shows the cloth over it is looked for, to say how deep the point went through it,
/// metres.
const THROUGH: f32 = 0.03;
/// How far behind a body point the cloth may lie and still cover it, metres: the cloth meeting the skin.
pub const TOUCH: f32 = 0.0005;
/// The side of a cell of the triangle grid, metres.
const CELL: f32 = 0.02;

/// One pose of the stress battery.
#[derive(Clone, Debug, PartialEq)]
pub struct StressPose {
    /// Its name, such as `bend-back-30`.
    pub name: &'static str,
    /// The pose.
    pub pose: HumanoidPose,
}

/// Turns `pose`'s `bone` further by `by` (local).
fn turn(pose: &mut HumanoidPose, bone: B, by: Quat) {
    pose.set(bone, (pose.get(bone) * by).normalize());
}

/// A bend of `degrees` about `axis`, spread evenly over the spine, the chest and the upper chest.
fn spine(pose: &mut HumanoidPose, axis: Vec3, degrees: f32) {
    let each = Quat::from_axis_angle(axis, (degrees / 3.0).to_radians());
    for bone in [B::Spine, B::Chest, B::UpperChest] {
        turn(pose, bone, each);
    }
}

/// The stress battery on a body that rests in `base` (#2604): the spine bent forward, back and to each side by 30
/// degrees and twisted 30 degrees each way, the hips tipped back as a fall tips them, the arms raised and crossed.
pub fn battery(base: &HumanoidPose) -> Vec<StressPose> {
    let with = |name, change: &dyn Fn(&mut HumanoidPose)| {
        let mut pose = *base;
        change(&mut pose);
        StressPose { name, pose }
    };
    // In VRM normalised space a turn about +X tips +Y toward +Z (the character's front); about +Z it tips +Y toward
    // -X (the character's right).
    vec![
        with("rest", &|_| {}),
        with("bend-forward-30", &|p| spine(p, Vec3::X, 30.0)),
        with("bend-back-30", &|p| spine(p, Vec3::X, -30.0)),
        with("bend-left-30", &|p| spine(p, Vec3::Z, -30.0)),
        with("bend-right-30", &|p| spine(p, Vec3::Z, 30.0)),
        with("twist-left-30", &|p| spine(p, Vec3::Y, 30.0)),
        with("twist-right-30", &|p| spine(p, Vec3::Y, -30.0)),
        // A fall (#2581): the hips tip back 18 degrees with the legs held under them, the spine back 21 more.
        with("fall-back", &|p| {
            let back = Quat::from_rotation_x(-18f32.to_radians());
            turn(p, B::Hips, back);
            for leg in [B::LeftUpperLeg, B::RightUpperLeg] {
                p.set(leg, (back.inverse() * p.get(leg)).normalize());
            }
            spine(p, Vec3::X, -21.0);
        }),
        with("arms-up", &|p| {
            p.set(B::LeftUpperArm, Quat::from_rotation_z(70f32.to_radians()));
            p.set(B::RightUpperArm, Quat::from_rotation_z(-70f32.to_radians()));
        }),
        with("arms-crossed", &|p| {
            // Each upper arm down 60 degrees and forward across the chest, the forearm folded in front of it.
            // A turn about +Y by a negative angle swings the left arm (along +X) forward.
            let across = |side: f32| {
                Quat::from_rotation_y(-side * 70f32.to_radians()) * Quat::from_rotation_z(-side * 60f32.to_radians())
            };
            p.set(B::LeftUpperArm, across(1.0));
            p.set(B::RightUpperArm, across(-1.0));
            p.set(B::LeftLowerArm, Quat::from_rotation_y(-80f32.to_radians()));
            p.set(B::RightLowerArm, Quat::from_rotation_y(80f32.to_radians()));
        }),
    ]
}

/// A mesh's vertices with their skinning resolved to the character's bones.
struct Skinned<'a> {
    mesh: &'a PartMesh,
    /// The bone index of each of the mesh's joints (`usize::MAX` for one the skeleton lacks: it doesn't move).
    bones: Vec<usize>,
}

impl Skinned<'_> {
    /// The blended skinning matrix of vertex `i` under `skin` (one matrix per bone).
    fn matrix(&self, i: usize, skin: &[Mat4]) -> Mat4 {
        let g = &self.mesh.geometry;
        let (joints, weights) = (g.joints.get(i), g.weights.get(i));
        let (Some(joints), Some(weights)) = (joints, weights) else { return Mat4::IDENTITY };
        let mut out = Mat4::ZERO;
        let mut total = 0.0;
        for (j, w) in joints.iter().zip(weights) {
            if *w <= 0.0 {
                continue;
            }
            let m = self.bones.get(usize::from(*j)).and_then(|b| skin.get(*b)).copied().unwrap_or(Mat4::IDENTITY);
            out += m * *w;
            total += *w;
        }
        if total > 1e-6 { out * (1.0 / total) } else { Mat4::IDENTITY }
    }

    /// Every vertex posed by `skin`, and its normal.
    fn posed(&self, skin: &[Mat4]) -> (Vec<Vec3>, Vec<Vec3>) {
        let g = &self.mesh.geometry;
        let mut points = Vec::with_capacity(g.positions.len());
        let mut normals = Vec::with_capacity(g.positions.len());
        for (i, p) in g.positions.iter().enumerate() {
            let m = self.matrix(i, skin);
            points.push(m.transform_point3(*p));
            let n = g.normals.get(i).copied().unwrap_or(Vec3::ZERO);
            normals.push(m.transform_vector3(n).normalize_or_zero());
        }
        (points, normals)
    }

    /// The bone that moves vertex `i` most.
    fn main_bone(&self, i: usize) -> Option<usize> {
        let g = &self.mesh.geometry;
        let (joints, weights) = (g.joints.get(i)?, g.weights.get(i)?);
        let k = (0..4).max_by(|a, b| weights[*a].total_cmp(&weights[*b]))?;
        self.bones.get(usize::from(joints[k])).copied().filter(|b| *b != usize::MAX)
    }
}

/// The bone index a mesh joint means on `skeleton`.
fn bone_index(skeleton: &Skeleton, joint: &JointRef) -> usize {
    let bone = match joint {
        JointRef::Humanoid(h) => skeleton.humanoid_bone(*h),
        JointRef::Named(name) => skeleton.find(name.as_str()),
    };
    bone.map_or(usize::MAX, omuzaki_anim_api::BoneId::index)
}

/// Triangles in a uniform grid, each with the layer it belongs to, for rays a few centimetres long.
struct Grid {
    triangles: Vec<([Vec3; 3], usize)>,
    cells: HashMap<(i32, i32, i32), Vec<u32>>,
}

/// The cell holding `p`.
#[allow(clippy::cast_possible_truncation)] // a body is a few metres across: cells far inside i32
fn cell(p: Vec3) -> (i32, i32, i32) {
    let c = (p / CELL).floor();
    (c.x as i32, c.y as i32, c.z as i32)
}

impl Grid {
    fn new(triangles: Vec<([Vec3; 3], usize)>) -> Self {
        let mut cells: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        for (k, (t, _)) in triangles.iter().enumerate() {
            let (lo, hi) = (cell(t[0].min(t[1]).min(t[2])), cell(t[0].max(t[1]).max(t[2])));
            let Ok(k) = u32::try_from(k) else { break };
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        cells.entry((x, y, z)).or_default().push(k);
                    }
                }
            }
        }
        Self { triangles, cells }
    }

    /// The nearest distance along `dir` (unit) from `from` at which the ray meets a triangle of a layer `keep` takes,
    /// within `range` (`lo..=hi`, metres; `lo` may be below 0 for a triangle just behind `from`).
    fn hit(
        &self,
        from: Vec3,
        dir: Vec3,
        (lo, hi): (f32, f32),
        keep: impl Fn(usize) -> bool,
        seen: &mut Vec<u32>,
    ) -> Option<f32> {
        let (a, b) = (from + dir * lo, from + dir * hi);
        let (c0, c1) = (cell(a.min(b)), cell(a.max(b)));
        seen.clear();
        let mut best: Option<f32> = None;
        for x in c0.0..=c1.0 {
            for y in c0.1..=c1.1 {
                for z in c0.2..=c1.2 {
                    let Some(list) = self.cells.get(&(x, y, z)) else { continue };
                    for &k in list {
                        if seen.contains(&k) {
                            continue;
                        }
                        seen.push(k);
                        let (tri, layer) = &self.triangles[k as usize];
                        if !keep(*layer) {
                            continue;
                        }
                        let Some(t) = ray_triangle(from, dir, tri) else { continue };
                        if (lo..=hi).contains(&t) && best.is_none_or(|b| t.abs() < b.abs()) {
                            best = Some(t);
                        }
                    }
                }
            }
        }
        best
    }
}

/// Where the line `from + dir * t` crosses triangle `tri`, from either side (Möller and Trumbore).
#[allow(clippy::many_single_char_names)] // Möller and Trumbore's names: p, q, s, t, u, v
fn ray_triangle(from: Vec3, dir: Vec3, tri: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (tri[1] - tri[0], tri[2] - tri[0]);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = from - tri[0];
    let u = s.dot(p) * inv;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    Some(e2.dot(q) * inv)
}

/// Where on a body triangle a sample sits, as barycentric weights: the middle and a point toward each corner, so a
/// coarse body (a torso of a few hundred vertices) is tested across its triangles, as it is drawn, not only at its
/// corners. A garment is dense: its triangles' middles are enough.
const BODY_SAMPLES: [[f32; 3]; 4] = [
    [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0],
    [2.0 / 3.0, 1.0 / 6.0, 1.0 / 6.0],
    [1.0 / 6.0, 2.0 / 3.0, 1.0 / 6.0],
    [1.0 / 6.0, 1.0 / 6.0, 2.0 / 3.0],
];
/// How far out from a surface a ray starts when it looks for the same layer in front of a point, metres: clear of the
/// point's own triangle.
const LIFT: f32 = 0.001;

/// A point of one layer that the layers over it cover in the bind pose.
#[derive(Clone, Copy, Debug)]
struct Covered {
    /// Which mesh.
    mesh: usize,
    /// The triangle's corners.
    corners: [usize; 3],
    /// Where on it.
    at: [f32; 3],
    /// Its share of the surface, square metres.
    area: f32,
}

impl Covered {
    /// The point and its normal among `points` and `normals`.
    fn place(&self, points: &[Vec3], normals: &[Vec3]) -> (Vec3, Vec3) {
        let (mut p, mut n) = (Vec3::ZERO, Vec3::ZERO);
        for (c, w) in self.corners.iter().zip(self.at) {
            p += points[*c] * w;
            n += normals[*c] * w;
        }
        (p, n.normalize_or_zero())
    }
}

/// One skinned mesh in its layer: the body is layer 0, each garment the next in build order (inner first).
struct Layered<'a> {
    skinned: Skinned<'a>,
    layer: usize,
}

/// A character's body and outfit, ready to be checked in any pose.
pub struct FitCheck<'a> {
    meshes: Vec<Layered<'a>>,
    /// The layers by name: `body`, then each garment part.
    layers: Vec<String>,
    covered: Vec<Covered>,
    /// The surface the layers over it cover in the bind pose, by layer, square metres.
    covered_area: Vec<f32>,
    names: Vec<String>,
}

/// Where something shows through what should cover it, in one pose.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    /// What shows: `body`, or the garment part under another.
    pub layer: String,
    /// The bone that moves it there.
    pub bone: String,
    /// How much shows, square centimetres.
    pub cm2: f32,
}

/// How a pose's outfit covers the body and its own inner layers.
#[derive(Clone, Debug, PartialEq)]
pub struct PoseFit {
    /// The covered body that shows, square centimetres.
    pub body_cm2: f32,
    /// The share of the covered body that shows, 0 to 1.
    pub body_share: f32,
    /// Inner garments that show through the ones over them, square centimetres.
    pub layers_cm2: f32,
    /// The deepest anything stands out through the cloth over it, millimetres (0 where it only shows through a gap).
    pub deepest_mm: f32,
    /// Where it shows, largest first.
    pub shown: Vec<Shown>,
    /// Where it shows most, in the bind pose (model space, metres), if anything shows: what a render frames.
    pub worst_at: Option<Vec3>,
}

/// Every mesh of `meshes` posed by `skin` (`None` for the bind pose): positions and normals.
fn posed_all(meshes: &[Layered<'_>], skin: Option<&[Mat4]>) -> Vec<(Vec<Vec3>, Vec<Vec3>)> {
    meshes
        .iter()
        .map(|m| match skin {
            Some(skin) => m.skinned.posed(skin),
            None => (m.skinned.mesh.geometry.positions.clone(), m.skinned.mesh.geometry.normals.clone()),
        })
        .collect()
}

/// The triangles of `meshes` placed at `posed`, with their layers.
fn placed(meshes: &[Layered<'_>], posed: &[(Vec<Vec3>, Vec<Vec3>)]) -> Grid {
    let mut out = Vec::new();
    for (m, (points, _)) in meshes.iter().zip(posed) {
        for t in m.skinned.mesh.geometry.indices.as_chunks::<3>().0 {
            let at = |i: u32| points.get(i as usize).copied();
            if let (Some(a), Some(b), Some(c)) = (at(t[0]), at(t[1]), at(t[2])) {
                out.push(([a, b, c], m.layer));
            }
        }
    }
    Grid::new(out)
}

impl<'a> FitCheck<'a> {
    /// The body (its body-category meshes) and the outfit (its garments, a layer each in build order) of `built`, with
    /// what the outfit covers in the bind pose: each sample of a layer whose outward ray meets a layer over it within
    /// [`COVER`] before it meets its own layer or one under it (a leg's top inside the hips is hidden by the hips, not
    /// by the cloth). Cold.
    #[allow(clippy::cast_precision_loss, clippy::many_single_char_names)] // the sample count; k, m, t and a, b, c
    pub fn new(built: &'a BuiltCharacter) -> Self {
        let skeleton = &built.rig.skeleton;
        let category = |part| {
            let kind = built.def.parts.get(part).and_then(|p| omuzaki_char_api::find_kind(p.kind.as_str()));
            kind.map(|k| (k.meta)().category)
        };
        let mut layers = vec!["body".to_owned()];
        let mut meshes = Vec::new();
        for m in &built.meshes {
            let g = &m.mesh.geometry;
            if m.mesh.joint_bones.is_empty() || g.indices.is_empty() || g.normals.len() != g.positions.len() {
                continue;
            }
            let layer = match category(&m.part) {
                Some(PartCategory::Body) => 0,
                Some(PartCategory::Garment) => {
                    if let Some(k) = layers.iter().position(|l| l == m.part.as_str()) {
                        k
                    } else {
                        layers.push(m.part.as_str().to_owned());
                        layers.len() - 1
                    }
                }
                _ => continue,
            };
            let bones = m.mesh.joint_bones.iter().map(|j| bone_index(skeleton, j)).collect();
            meshes.push(Layered { skinned: Skinned { mesh: &m.mesh, bones }, layer });
        }
        let bind = posed_all(&meshes, None);
        let grid = placed(&meshes, &bind);
        let (mut covered, mut covered_area, mut seen) = (Vec::new(), vec![0.0; layers.len()], Vec::new());
        for (k, m) in meshes.iter().enumerate() {
            let (points, normals) = &bind[k];
            let samples: &[[f32; 3]] = if m.layer == 0 { &BODY_SAMPLES } else { &BODY_SAMPLES[..1] };
            for t in m.skinned.mesh.geometry.indices.as_chunks::<3>().0 {
                let corners = [t[0] as usize, t[1] as usize, t[2] as usize];
                if corners.iter().any(|c| *c >= points.len()) {
                    continue;
                }
                let [a, b, c] = corners.map(|i| points[i]);
                let area = (b - a).cross(c - a).length() * 0.5 / samples.len() as f32;
                let cover = if m.layer == 0 { COVER } else { LAYER_COVER };
                for at in samples {
                    let sample = Covered { mesh: k, corners, at: *at, area };
                    let (p, n) = sample.place(points, normals);
                    if n == Vec3::ZERO {
                        continue;
                    }
                    let over = |l: usize| l > m.layer;
                    let Some(hit) = grid.hit(p, n, (-TOUCH * 2.0, cover), over, &mut seen) else { continue };
                    let under = |l: usize| l <= m.layer;
                    let hidden = grid.hit(p + n * LIFT, n, (0.0, cover), under, &mut seen).is_some_and(|b| b + LIFT < hit);
                    if !hidden {
                        covered.push(sample);
                        covered_area[m.layer] += area;
                    }
                }
            }
        }
        let names = skeleton.bones().iter().map(|b| b.name.clone()).collect();
        Self { meshes, layers, covered, covered_area, names }
    }

    /// The body's surface the outfit covers in the bind pose, square centimetres.
    pub fn covered_cm2(&self) -> f32 {
        self.covered_area.first().copied().unwrap_or(0.0) * 1e4
    }

    /// Each layer (`body`, then each garment part, inner first) and how much of it the layers over it cover in the
    /// bind pose, square centimetres.
    pub fn layers(&self) -> impl Iterator<Item = (&str, f32)> {
        self.layers.iter().zip(&self.covered_area).map(|(l, a)| (l.as_str(), a * 1e4))
    }

    /// How the outfit covers the body and its inner layers posed by `skin` (one skinning matrix per bone, bind space
    /// to model space): each covered point shows when its outward ray meets neither a layer over it nor its own (an
    /// arm in front of the chest hides it too) within [`POSED_COVER`].
    pub fn check(&self, skin: &[Mat4]) -> PoseFit {
        let posed = posed_all(&self.meshes, Some(skin));
        let grid = placed(&self.meshes, &posed);
        let mut deepest = 0.0_f32;
        let mut shown: HashMap<(usize, usize), (f32, Vec3)> = HashMap::new();
        let mut seen = Vec::new();
        for c in &self.covered {
            let layer = self.meshes[c.mesh].layer;
            let (points, normals) = &posed[c.mesh];
            let (p, n) = c.place(points, normals);
            let over = |l: usize| l > layer;
            if n == Vec3::ZERO
                || grid.hit(p, n, (-TOUCH, POSED_COVER), over, &mut seen).is_some()
                || grid.hit(p + n * LIFT, n, (0.0, POSED_COVER), |_| true, &mut seen).is_some()
            {
                continue;
            }
            // Through the cloth: the nearest cloth over it lies just behind the point.
            if let Some(t) = grid.hit(p, n, (-THROUGH, -TOUCH), over, &mut seen) {
                deepest = deepest.max(-t);
            }
            let mesh = &self.meshes[c.mesh].skinned;
            let bind = c.place(&mesh.mesh.geometry.positions, &mesh.mesh.geometry.normals).0;
            let bone = mesh.main_bone(c.corners[0]).unwrap_or(usize::MAX);
            let entry = shown.entry((layer, bone)).or_insert((0.0, Vec3::ZERO));
            entry.0 += c.area;
            entry.1 += bind * c.area;
        }
        let worst_at = shown.values().max_by(|a, b| a.0.total_cmp(&b.0)).map(|(a, sum)| *sum / a.max(1e-12));
        let body = shown.iter().filter(|((l, _), _)| *l == 0).map(|(_, (a, _))| a).sum::<f32>();
        let all = shown.values().map(|(a, _)| a).sum::<f32>();
        let mut list: Vec<Shown> = shown
            .into_iter()
            .map(|((layer, bone), (area, _))| Shown {
                layer: self.layers[layer].clone(),
                bone: self.names.get(bone).cloned().unwrap_or_default(),
                cm2: area * 1e4,
            })
            .collect();
        list.sort_by(|a, b| b.cm2.total_cmp(&a.cm2).then_with(|| (&a.layer, &a.bone).cmp(&(&b.layer, &b.bone))));
        let covered_body = self.covered_area.first().copied().unwrap_or(0.0);
        PoseFit {
            body_cm2: body * 1e4,
            body_share: if covered_body > 0.0 { body / covered_body } else { 0.0 },
            layers_cm2: (all - body) * 1e4,
            deepest_mm: deepest * 1e3,
            shown: list,
            worst_at,
        }
    }
}
