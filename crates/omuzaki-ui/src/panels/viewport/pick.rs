//! Click to select (#2590): which part is under the pointer.
//!
//! When a character is staged, its meshes are skinned on the CPU with the stage's skinning matrices for the base pose
//! the viewport shows, once, into one triangle soup tagged by part ([`PickMesh::posed`]). A click casts the camera's
//! ray through the pointer ([`OrbitCamera::ray`](super::camera::OrbitCamera::ray)) and takes the nearest triangle it
//! crosses (Möller–Trumbore). A simulated mesh (hair) is taken where it was built, which is where it hangs at rest.

use omuzaki_char_api::{AnchorId, Attach, BoneName, BuiltCharacter, JointRef, PartId};
use omuzaki_core::glam::{Mat4, Vec3};

use super::camera::Bounds;

/// The posed triangles of a staged character, each with the part that made it.
#[derive(Debug, Default)]
pub struct PickMesh {
    parts: Vec<PartId>,
    /// Three corners and the index of the part in `parts`.
    triangles: Vec<([Vec3; 3], u32)>,
    /// Each part's posed box, in `parts` order.
    bounds: Vec<Option<Bounds>>,
    /// The bone each part's meshes follow, in `parts` order.
    follows: Vec<Follows>,
    /// Every bone's frame as posed: its bind frame carried by its skinning matrix.
    bones: Vec<(JointRef, Mat4)>,
    /// Every published anchor's frame as posed, carried by the bone it follows.
    anchors: Vec<((PartId, AnchorId), Mat4)>,
}

/// Which bone a part's meshes follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follows {
    /// None yet: only unskinned meshes were seen.
    Nothing,
    /// One bone, by index.
    One(usize),
    /// Several bones, or the part is simulated (hair, a garment, the body).
    Many,
}

/// What a ray hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The part.
    pub part: PartId,
    /// How far along the ray, in metres.
    pub distance: f32,
    /// Where, in the character's space.
    pub point: Vec3,
}

impl PickMesh {
    /// `built`'s meshes posed by `skin` (the stage's skinning matrices, by bone): skinned meshes are blended by their
    /// weights, rigid and simulated ones are taken as built.
    pub fn posed(built: &BuiltCharacter, skin: &[Mat4]) -> Self {
        let mut out = Self::default();
        let rig = &built.rig;
        for ((bone, inverse), index) in rig.skeleton.bones().iter().zip(rig.inverse_bind()).zip(0..) {
            let joint = match bone.humanoid {
                Some(h) => JointRef::Humanoid(h),
                None => match BoneName::new(&bone.name) {
                    Ok(name) => JointRef::Named(name),
                    Err(_) => continue,
                },
            };
            let posed = skin.get(index).copied().unwrap_or(Mat4::IDENTITY);
            out.bones.push((joint, posed * inverse.inverse()));
        }
        for ((part, anchor), found) in &built.anchors {
            let carried = rig.bone(&found.bone).and_then(|b| skin.get(b.index())).copied().unwrap_or(Mat4::IDENTITY);
            let frame = Mat4::from_rotation_translation(found.rotation, found.position);
            out.anchors.push(((part.clone(), anchor.clone()), carried * frame));
        }
        for mesh in &built.meshes {
            let geometry = &mesh.mesh.geometry;
            let bones: Vec<Option<usize>> = mesh
                .mesh
                .joint_bones
                .iter()
                .map(|joint| built.rig.bone(joint).map(omuzaki_anim_api::BoneId::index))
                .collect();
            let skinned = !mesh.mesh.dynamic && geometry.is_skinned() && !bones.is_empty();
            let posed: Vec<Vec3> = geometry
                .positions
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    if !skinned {
                        return p;
                    }
                    let (joints, weights) = (geometry.joints[i], geometry.weights[i]);
                    let mut sum = Vec3::ZERO;
                    let mut total = 0.0;
                    for (joint, weight) in joints.iter().zip(weights) {
                        let matrix = bones.get(usize::from(*joint)).copied().flatten().and_then(|b| skin.get(b));
                        if let (Some(matrix), true) = (matrix, weight > 0.0) {
                            sum += matrix.transform_point3(p) * weight;
                            total += weight;
                        }
                    }
                    if total > 1e-6 { sum / total } else { p }
                })
                .collect();
            let index = out.parts.iter().position(|p| *p == mesh.part).unwrap_or_else(|| {
                out.parts.push(mesh.part.clone());
                out.bounds.push(None);
                out.follows.push(Follows::Nothing);
                out.parts.len() - 1
            });
            if let Some(follows) = out.follows.get_mut(index) {
                *follows = if mesh.mesh.dynamic { Follows::Many } else { one_bone(*follows, mesh, &bones) };
            }
            if let (Some(slot), Some(b)) = (out.bounds.get_mut(index), Bounds::of(posed.iter().copied())) {
                *slot = Some(slot.map_or(b, |had| had.union(b)));
            }
            let Ok(index) = u32::try_from(index) else { continue };
            for &[a, b, c] in geometry.indices.as_chunks::<3>().0 {
                let corner = |v: u32| posed.get(v as usize).copied();
                if let (Some(a), Some(b), Some(c)) = (corner(a), corner(b), corner(c)) {
                    out.triangles.push(([a, b, c], index));
                }
            }
        }
        out
    }

    /// Part `part`'s box as posed, if it has a mesh.
    pub fn bounds(&self, part: &PartId) -> Option<Bounds> {
        self.parts.iter().position(|p| p == part).and_then(|i| self.bounds.get(i).copied().flatten())
    }

    /// Whether `part` moves as one piece with at most one bone, so it can be placed by an attachment: false for a
    /// simulated part or one bent by several bones (hair, a garment, the body).
    pub fn rigid(&self, part: &PartId) -> bool {
        self.parts.iter().position(|p| p == part).and_then(|i| self.follows.get(i)).is_some_and(|f| *f != Follows::Many)
    }

    /// Where the target of `attach` (its bone or anchor, before the offset) sits as posed: the frame the offset is
    /// in. `None` when the character has no such bone or anchor.
    pub fn target_frame(&self, attach: &Attach) -> Option<Mat4> {
        match attach {
            Attach::Bone { bone, .. } => self.bones.iter().find(|(joint, _)| joint == bone).map(|(_, frame)| *frame),
            Attach::Anchor { part, anchor, .. } => {
                self.anchors.iter().find(|((p, a), _)| p == part && a == anchor).map(|(_, frame)| *frame)
            }
        }
    }

    /// How many triangles it holds.
    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// The nearest part the ray from `origin` along `direction` crosses, either face.
    pub fn hit(&self, origin: Vec3, direction: Vec3) -> Option<Hit> {
        let direction = direction.normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }
        let mut best: Option<(f32, u32)> = None;
        for (corners, part) in &self.triangles {
            if let Some(t) = crossing(origin, direction, corners)
                && best.is_none_or(|(near, _)| t < near)
            {
                best = Some((t, *part));
            }
        }
        let (distance, part) = best?;
        let part = self.parts.get(part as usize)?.clone();
        Some(Hit { part, distance, point: origin + direction * distance })
    }
}

/// The bone a part's meshes follow so far (`had`), with `mesh` added.
fn one_bone(had: Follows, mesh: &omuzaki_char_api::BuiltMesh, bones: &[Option<usize>]) -> Follows {
    let geometry = &mesh.mesh.geometry;
    if !geometry.is_skinned() {
        return had;
    }
    let mut follows = had;
    for (joints, weights) in geometry.joints.iter().zip(&geometry.weights) {
        for (joint, weight) in joints.iter().zip(weights) {
            if *weight <= 1e-4 {
                continue;
            }
            follows = match (follows, bones.get(usize::from(*joint)).copied().flatten()) {
                (Follows::Nothing, Some(bone)) => Follows::One(bone),
                (Follows::One(had), Some(bone)) if had == bone => follows,
                _ => return Follows::Many,
            };
        }
    }
    follows
}

/// Möller–Trumbore: how far along the ray it crosses the triangle, in front of the origin.
#[allow(clippy::many_single_char_names, reason = "the algorithm's own names")]
fn crossing(origin: Vec3, direction: Vec3, [a, b, c]: &[Vec3; 3]) -> Option<f32> {
    let (ab, ac) = (*b - *a, *c - *a);
    let p = direction.cross(ac);
    let det = ab.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - *a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(ab);
    let v = direction.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = ac.dot(q) * inv;
    (t > 1e-5).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::many_single_char_names, reason = "a square's corners")]
    fn square(z: f32, part: u32) -> [([Vec3; 3], u32); 2] {
        let (a, b, c, d) =
            (Vec3::new(-1.0, -1.0, z), Vec3::new(1.0, -1.0, z), Vec3::new(1.0, 1.0, z), Vec3::new(-1.0, 1.0, z));
        [([a, b, c], part), ([a, c, d], part)]
    }

    fn mesh() -> PickMesh {
        let parts = vec![
            PartId::new("back").unwrap_or_else(|e| panic!("{e}")),
            PartId::new("front").unwrap_or_else(|e| panic!("{e}")),
        ];
        let mut triangles = square(0.0, 0).to_vec();
        triangles.extend(square(0.5, 1));
        PickMesh {
            parts,
            triangles,
            bounds: vec![None, None],
            follows: vec![Follows::Nothing, Follows::Nothing],
            bones: Vec::new(),
            anchors: Vec::new(),
        }
    }

    #[test]
    fn the_nearest_crossing_wins_from_either_side() {
        let mesh = mesh();
        let from_front = mesh.hit(Vec3::new(0.2, 0.3, 3.0), Vec3::NEG_Z).map(|h| h.part.as_str().to_owned());
        assert_eq!(from_front.as_deref(), Some("front"));
        let from_back = mesh.hit(Vec3::new(0.2, 0.3, -3.0), Vec3::Z).map(|h| h.part.as_str().to_owned());
        assert_eq!(from_back.as_deref(), Some("back"));
        let hit = mesh.hit(Vec3::new(0.0, 0.0, 3.0), Vec3::NEG_Z).unwrap_or_else(|| panic!("a hit"));
        assert!((hit.distance - 2.5).abs() < 1e-5 && (hit.point.z - 0.5).abs() < 1e-5, "{hit:?}");
    }

    #[test]
    fn a_ray_beside_it_or_pointing_away_misses() {
        let mesh = mesh();
        assert_eq!(mesh.hit(Vec3::new(1.5, 0.0, 3.0), Vec3::NEG_Z), None);
        assert_eq!(mesh.hit(Vec3::new(0.0, 0.0, 3.0), Vec3::Z), None);
        assert_eq!(mesh.hit(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO), None);
    }
}
