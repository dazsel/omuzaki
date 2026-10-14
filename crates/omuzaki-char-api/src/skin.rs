//! Weight corrections (RG-02): brushes that move skin weight toward a joint around a point.
//!
//! A generator skins its meshes with the weights its algorithm gives, which is rarely quite right at a shoulder, a
//! skirt hem or a hair root. A [`WeightBrush`] fixes that as data: it is stored in the character
//! ([`CharacterDef::weight_brushes`](crate::CharacterDef)) so a rebuild reapplies it, and the builder applies a part's
//! brushes to each of its skinned meshes right after the part builds ([`apply_brushes`]).

use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{Check, Validate, narrow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::PartMesh;
use crate::ids::JointRef;

/// How a brush's strength fades from its centre to its edge.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrushFalloff {
    /// Full strength everywhere inside the radius.
    Constant,
    /// Strength falls in a straight line from the centre to the edge.
    Linear,
    /// Strength falls along a smoothstep: gentle at both ends, so no visible ring at the edge.
    #[default]
    Smooth,
}

impl BrushFalloff {
    /// The share of full strength at `t` of the way from the edge (`0`) to the centre (`1`).
    fn at(self, t: f32) -> f32 {
        match self {
            Self::Constant => 1.0,
            Self::Linear => t,
            Self::Smooth => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// One brush stroke: within `radius` of `centre`, move `strength` of each vertex's weight onto `joint`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightBrush {
    /// The centre, bind space, metres.
    pub centre: [f64; 3],
    /// The radius, metres.
    pub radius: f64,
    /// The joint that gains weight: a humanoid role or an extra bone's name.
    pub joint: JointRef,
    /// The share of a vertex's weight that moves to `joint` at full falloff, `0..=1`. `0.5` at the centre leaves half of
    /// the vertex's weight where it was.
    pub strength: f64,
    /// How the strength fades toward the edge.
    #[serde(default)]
    pub falloff: BrushFalloff,
}

impl Validate for WeightBrush {
    fn check(&self, c: &mut Check) {
        for (i, v) in self.centre.iter().enumerate() {
            c.range(&format!("centre/{i}"), *v, -10.0, 10.0);
        }
        c.positive("radius", self.radius);
        c.range("strength", self.strength, 0.0, 1.0);
    }
}

/// Validates a part's brushes, naming the brush by position (`2/radius`).
pub(crate) fn check_brushes(brushes: &[WeightBrush]) -> Result<(), omuzaki_face_api::ParamErrors> {
    let mut c = Check::new();
    for (i, brush) in brushes.iter().enumerate() {
        c.nested(&i.to_string(), brush);
    }
    c.finish()
}

/// The most joints that can influence one vertex (the renderer's skinning format).
const MAX_INFLUENCES: usize = 4;

/// How close two weights must be to count as tied at the influence cut: a left and a right weight that mirror each other
/// differ only by float rounding, far below this.
const TIE: f64 = 1e-6;

/// How many of `sorted` (heaviest first, `weight` reads an item's weight) survive a cut to `max` influences. It reads the
/// items in place, so it allocates nothing.
///
/// Normally the first `max`. But when the last one kept and the first one dropped tie (within [`TIE`], relative), the
/// whole tied group goes: keeping one of two equal weights would pick a side by joint index and break a symmetric skin.
/// The kept weights are then fewer than `max`, and the caller renormalises.
///
/// A tied group that starts at index 1 leaves a single influence, which the renormalisation takes to weight 1.0. That is
/// acceptable: the first item is strictly heavier than everything tied below it, so it is the vertex's one clear
/// owner, and a skin of one joint at full weight is valid and symmetric. A group that starts at index 0 (the first
/// `max + 1` weights all tie, so nothing symmetric is left to keep) never empties the skin: the first `max` stay, chosen
/// by the caller's own tie-break (the lower joint index).
#[must_use]
pub fn influence_cut<T>(sorted: &[T], max: usize, weight: impl Fn(&T) -> f64) -> usize {
    let (Some(last), Some(next)) = (sorted.get(max.wrapping_sub(1)), sorted.get(max)) else {
        return sorted.len().min(max);
    };
    let (last, next) = (weight(last), weight(next));
    let tied = |a: f64, b: f64| (a - b).abs() <= TIE * a.abs().max(b.abs());
    if !tied(last, next) {
        return max;
    }
    match sorted[..max].iter().position(|item| tied(weight(item), last)) {
        Some(0) | None => max,
        Some(start) => start,
    }
}

impl WeightBrush {
    /// The brush that does to the opposite side of the body what this one does to this side: the centre's x negated
    /// and the joint swapped for its opposite ([`JointRef::mirrored`]). Radius, strength and falloff are the same. A
    /// brush on the centre plane with a centre-line joint is its own mirror.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        // `0.0 - x` rather than `-x`: a centre on the plane stays +0.0, not -0.0, so the mirror of x = 0 compares equal.
        let [x, y, z] = self.centre;
        Self { centre: [0.0 - x, y, z], joint: self.joint.mirrored(), ..self.clone() }
    }
}

/// A brush prepared for a mesh: its numbers narrowed to the mesh's precision and its joint's index, once known.
struct Stroke<'a> {
    brush: &'a WeightBrush,
    centre: Vec3,
    radius: f32,
    strength: f32,
    target: Option<u16>,
}

impl<'a> Stroke<'a> {
    fn new(brush: &'a WeightBrush) -> Self {
        Self {
            brush,
            centre: Vec3::new(narrow(brush.centre[0]), narrow(brush.centre[1]), narrow(brush.centre[2])),
            radius: narrow(brush.radius),
            strength: narrow(brush.strength),
            target: None,
        }
    }

    /// The share of a vertex's weight at `position` that moves to the joint, `0` out of reach.
    fn amount_at(&self, position: Vec3) -> f32 {
        let distance = position.distance(self.centre);
        if distance >= self.radius {
            return 0.0;
        }
        (self.strength * self.brush.falloff.at(1.0 - distance / self.radius)).max(0.0)
    }

    /// The joint's index in the mesh, appended on first use. `None` only for a mesh with more than 65 535 joints.
    fn joint(&mut self, mesh: &mut PartMesh) -> Option<u16> {
        if self.target.is_none() {
            self.target = joint_index(mesh, &self.brush.joint);
        }
        self.target
    }
}

/// For each brush, the index of the brush it is paired with, if any. A brush pairs with the first later brush that is
/// its exact mirror image by value (same radius, strength and falloff, mirrored centre and joint). A brush that is its
/// own mirror pairs with nothing, and neither does a spare copy: pairing is one to one.
fn mirror_partners(brushes: &[WeightBrush]) -> Vec<Option<usize>> {
    let mut partner = vec![None; brushes.len()];
    for i in 0..brushes.len() {
        if partner[i].is_some() {
            continue;
        }
        let image = brushes[i].mirrored();
        if image == brushes[i] {
            continue;
        }
        let found = (i + 1..brushes.len()).find(|&j| partner[j].is_none() && brushes[j] == image);
        if let Some(j) = found {
            partner[i] = Some(j);
            partner[j] = Some(i);
        }
    }
    partner
}

/// Applies `brushes` to a skinned mesh, in order. A rigid mesh has no weights and is left alone. A brush whose joint
/// the mesh does not yet list adds it to `joint_bones` (only if some vertex is in reach). Every touched vertex ends
/// with at most four influences whose weights sum to 1.
///
/// A brush and its exact mirror image (found by value, wherever they sit in the list) are applied together, at the
/// earlier one's place: both shifts are worked out from the weights the vertex had before either, then combined and
/// renormalised. So where the two overlap, on the centre plane, neither favours the other and the skin is symmetric.
/// Applied in turn, a plane vertex would give the later brush only its share of what the first left. Every
/// other brush still paints over what came before it. The result does not depend on which of a pair comes first,
/// except for the order of the new joints in `joint_bones`.
pub fn apply_brushes(mesh: &mut PartMesh, brushes: &[WeightBrush]) {
    if !mesh.geometry.is_skinned() {
        return;
    }
    let partner = mirror_partners(brushes);
    for (i, brush) in brushes.iter().enumerate() {
        match partner[i] {
            Some(j) if j < i => {} // applied with its partner, at the partner's place
            Some(j) => apply_strokes(mesh, &mut [Stroke::new(brush), Stroke::new(&brushes[j])]),
            None => apply_strokes(mesh, &mut [Stroke::new(brush)]),
        }
    }
}

/// Applies one stroke, or a pair together, to every vertex in reach.
fn apply_strokes<const N: usize>(mesh: &mut PartMesh, strokes: &mut [Stroke<'_>; N]) {
    for i in 0..mesh.geometry.positions.len() {
        let position = mesh.geometry.positions[i];
        let mut shifts = [(0_u16, 0.0_f32); N];
        let mut count = 0;
        for stroke in strokes.iter_mut() {
            let amount = stroke.amount_at(position);
            if amount <= 0.0 {
                continue;
            }
            let Some(joint) = stroke.joint(mesh) else { return };
            shifts[count] = (joint, amount);
            count += 1;
        }
        if count > 0 {
            shift_weight(&mut mesh.geometry.joints[i], &mut mesh.geometry.weights[i], &shifts[..count]);
        }
    }
}

/// The index of `joint` in the mesh's joint list, appending it if absent. `None` only for a mesh with more than 65 535
/// joints, which no generator makes.
fn joint_index(mesh: &mut PartMesh, joint: &JointRef) -> Option<u16> {
    let at = mesh.joint_bones.iter().position(|j| j == joint).unwrap_or_else(|| {
        mesh.joint_bones.push(joint.clone());
        mesh.joint_bones.len() - 1
    });
    u16::try_from(at).ok()
}

/// Moves a share of a vertex's weight onto each `(target, amount)`, all worked out from the weight it had before: what
/// stays is scaled by the product of `1 - amount`, which does not depend on the order of `shifts`. Then it keeps the
/// four heaviest influences and renormalises. Ties keep the lower joint index, so the result is deterministic. Two
/// shifts onto one joint add their amounts first, so the order cannot change the last bit either.
fn shift_weight(joints: &mut [u16; 4], weights: &mut [f32; 4], shifts: &[(u16, f32)]) {
    let keep: f32 = shifts.iter().map(|(_, amount)| 1.0 - amount).product();
    let mut entries = [(0_u16, 0.0_f32); MAX_INFLUENCES + 2];
    let mut count = 0;
    for (&joint, &weight) in joints.iter().zip(weights.iter()) {
        let kept = weight * keep;
        if kept > 0.0 {
            entries[count] = (joint, kept);
            count += 1;
        }
    }
    for (n, &(target, _)) in shifts.iter().enumerate() {
        if shifts[..n].iter().any(|(joint, _)| *joint == target) {
            continue; // an earlier shift onto this joint already took this one's amount
        }
        let amount: f32 = shifts[n..].iter().filter(|(joint, _)| *joint == target).map(|(_, a)| a).sum();
        if let Some(at) = entries[..count].iter().position(|(joint, _)| *joint == target) {
            entries[at].1 += amount;
        } else {
            entries[count] = (target, amount);
            count += 1;
        }
    }
    let kept = &mut entries[..count];
    kept.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let kept = &kept[..influence_cut(kept, MAX_INFLUENCES, |entry| f64::from(entry.1))];
    let total: f32 = kept.iter().map(|(_, w)| w).sum();
    *joints = [0; 4];
    *weights = [0.0; 4];
    for (slot, &(joint, weight)) in kept.iter().enumerate() {
        joints[slot] = joint;
        weights[slot] = weight / total;
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::mesh::MeshData;

    use super::*;
    use crate::ids::MaterialName;

    fn mesh(positions: &[Vec3], joints: [u16; 4], weights: [f32; 4]) -> PartMesh {
        let n = positions.len();
        PartMesh {
            name: "m".to_owned(),
            geometry: MeshData {
                positions: positions.to_vec(),
                normals: vec![Vec3::Z; n],
                joints: vec![joints; n],
                weights: vec![weights; n],
                indices: vec![0, 1, 2],
                ..MeshData::default()
            },
            joint_bones: vec![JointRef::HIPS, JointRef::CHEST],
            material: MaterialName::new("m").unwrap(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        }
    }

    fn brush(centre: Vec3, radius: f64, strength: f64, falloff: BrushFalloff) -> WeightBrush {
        WeightBrush { centre: centre.to_array().map(f64::from), radius, joint: JointRef::HEAD, strength, falloff }
    }

    fn sum(w: [f32; 4]) -> f32 {
        w.iter().sum()
    }

    #[test]
    fn a_brush_moves_its_share_of_a_vertexs_weight_to_the_joint() {
        let mut m = mesh(&[Vec3::ZERO, Vec3::X, Vec3::Y], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]);
        apply_brushes(&mut m, &[brush(Vec3::ZERO, 0.5, 0.5, BrushFalloff::Smooth)]);
        let head = u16::try_from(m.joint_bones.iter().position(|j| *j == JointRef::HEAD).unwrap()).unwrap();
        assert_eq!(m.joint_bones.len(), 3, "the brush's joint was appended");
        let (j, w) = (m.geometry.joints[0], m.geometry.weights[0]);
        assert_eq!((j[0], w[0]), (0, 0.5), "half stays on the hips");
        assert_eq!((j[1], w[1]), (head, 0.5), "half moved to the head");
        assert!((sum(w) - 1.0).abs() < 1e-6);
        assert_eq!(m.geometry.weights[1], [1.0, 0.0, 0.0, 0.0], "out of reach: untouched");
        m.validate().unwrap();
    }

    /// The weight vertex `v` gives the brush's joint (the head), which the brush appended to the joint list.
    fn head_weight(m: &PartMesh, v: usize) -> f32 {
        let head = u16::try_from(m.joint_bones.iter().position(|j| *j == JointRef::HEAD).unwrap()).unwrap();
        let (joints, weights) = (m.geometry.joints[v], m.geometry.weights[v]);
        joints.iter().zip(weights).filter(|(j, w)| **j == head && *w > 0.0).map(|(_, w)| w).sum()
    }

    #[test]
    fn falloff_shapes_the_strength_and_the_edge_is_outside() {
        let at = |falloff, d: f32| {
            let mut m = mesh(&[Vec3::X * d, Vec3::Y * 9.0, Vec3::Z * 9.0], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]);
            apply_brushes(&mut m, &[brush(Vec3::ZERO, 1.0, 1.0, falloff)]);
            if m.joint_bones.contains(&JointRef::HEAD) { head_weight(&m, 0) } else { 0.0 }
        };
        assert!((at(BrushFalloff::Constant, 0.9) - 1.0).abs() < 1e-6);
        assert!((at(BrushFalloff::Linear, 0.25) - 0.75).abs() < 1e-6);
        assert!((at(BrushFalloff::Smooth, 0.5) - 0.5).abs() < 1e-6, "smoothstep is 0.5 halfway");
        assert_eq!(at(BrushFalloff::Constant, 1.0), 0.0, "the radius itself is out");
    }

    #[test]
    fn a_vertex_never_keeps_more_than_four_influences() {
        let mut m = mesh(&[Vec3::ZERO, Vec3::X, Vec3::Y], [0, 1, 2, 3], [0.4, 0.3, 0.2, 0.1]);
        m.joint_bones = vec![JointRef::HIPS, JointRef::CHEST, JointRef::NECK, JointRef::HEAD];
        apply_brushes(
            &mut m,
            &[WeightBrush {
                joint: JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::Jaw),
                ..brush(Vec3::ZERO, 1.0, 0.3, BrushFalloff::Constant)
            }],
        );
        let (j, w) = (m.geometry.joints[0], m.geometry.weights[0]);
        assert_eq!(m.joint_bones.len(), 5);
        assert!(w.iter().all(|w| *w > 0.0), "four influences remain: {w:?}");
        assert!(!j.contains(&3), "the lightest (joint 3, 0.07) was dropped: {j:?}");
        assert!((sum(w) - 1.0).abs() < 1e-6, "{w:?}");
        m.validate().unwrap();
    }

    #[test]
    fn brushes_merge_with_an_existing_influence_and_leave_rigid_meshes_alone() {
        let mut m = mesh(&[Vec3::ZERO, Vec3::X, Vec3::Y], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
        apply_brushes(
            &mut m,
            &[WeightBrush { joint: JointRef::CHEST, ..brush(Vec3::ZERO, 1.0, 1.0, BrushFalloff::Constant) }],
        );
        assert_eq!((m.geometry.joints[0][0], m.geometry.weights[0][0]), (1, 1.0), "all of it on the chest");
        assert_eq!(m.joint_bones.len(), 2, "no joint appended for one it already lists");

        let mut rigid = mesh(&[Vec3::ZERO, Vec3::X, Vec3::Y], [0; 4], [1.0, 0.0, 0.0, 0.0]);
        rigid.geometry.joints.clear();
        rigid.geometry.weights.clear();
        rigid.joint_bones.clear();
        let before = rigid.clone();
        apply_brushes(&mut rigid, &[brush(Vec3::ZERO, 1.0, 1.0, BrushFalloff::Constant)]);
        assert_eq!(rigid, before);
    }

    #[test]
    fn a_brush_out_of_reach_adds_no_joint() {
        let mut m = mesh(&[Vec3::ZERO, Vec3::X, Vec3::Y], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]);
        let before = m.clone();
        apply_brushes(&mut m, &[brush(Vec3::splat(9.0), 0.1, 1.0, BrushFalloff::Smooth)]);
        assert_eq!(m, before);
    }

    #[test]
    fn invalid_brushes_are_named_by_position_and_field() {
        let good = brush(Vec3::ZERO, 0.1, 0.5, BrushFalloff::Linear);
        let bad = WeightBrush { radius: 0.0, strength: 1.5, ..good.clone() };
        let e = check_brushes(&[good, bad]).unwrap_err();
        let paths: Vec<_> = e.0.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["1/radius", "1/strength"]);
    }

    #[test]
    fn brushes_round_trip_and_default_their_falloff() {
        let b: WeightBrush =
            serde_json::from_str(r#"{"centre":[0,1,0],"radius":0.1,"joint":{"humanoid":"head"},"strength":0.5}"#)
                .unwrap();
        assert_eq!(b.falloff, BrushFalloff::Smooth);
        assert_eq!(serde_json::from_str::<WeightBrush>(&serde_json::to_string(&b).unwrap()).unwrap(), b);
        assert!(
            serde_json::from_str::<WeightBrush>(
                r#"{"centre":[0,0,0],"radius":1,"joint":{"humanoid":"head"},"strength":1,"x":1}"#
            )
            .is_err()
        );
    }

    /// The weight vertex `v` gives `joint`, by name.
    fn weight_on(m: &PartMesh, v: usize, joint: &JointRef) -> f32 {
        let Some(at) = m.joint_bones.iter().position(|j| j == joint) else { return 0.0 };
        let at = u16::try_from(at).unwrap();
        m.geometry.joints[v].iter().zip(m.geometry.weights[v]).filter(|(j, _)| **j == at).map(|(_, w)| w).sum()
    }

    fn side(centre_x: f64, bone: omuzaki_anim_api::HumanoidBone) -> WeightBrush {
        WeightBrush {
            centre: [centre_x, 0.0, 0.0],
            joint: JointRef::Humanoid(bone),
            ..brush(Vec3::ZERO, 0.3, 0.4, BrushFalloff::Smooth)
        }
    }

    #[test]
    fn a_brush_and_its_mirror_treat_a_plane_vertex_alike_in_either_order() {
        use omuzaki_anim_api::HumanoidBone::{LeftUpperArm, RightUpperArm};
        // Vertex 0 is on the plane, between the two centres; vertex 1 is on the left side only.
        let positions = [Vec3::new(0.0, 0.1, 0.0), Vec3::new(0.3, 0.0, 0.0)];
        let (left, right) = (side(0.05, LeftUpperArm), side(-0.05, RightUpperArm));
        assert_eq!(left.mirrored(), right);
        let run = |order: &[WeightBrush]| {
            let mut m = mesh(&positions, [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
            apply_brushes(&mut m, order);
            assert!(m.geometry.weights.iter().all(|w| (sum(*w) - 1.0).abs() < 1e-6));
            m
        };
        let (a, b) = (run(&[left.clone(), right.clone()]), run(&[right, left]));
        let (l, r) = (JointRef::Humanoid(LeftUpperArm), JointRef::Humanoid(RightUpperArm));
        let on_plane = (weight_on(&a, 0, &l), weight_on(&a, 0, &r));
        assert!(on_plane.0 > 0.0, "the brushes reach the plane vertex");
        assert!((on_plane.0 - on_plane.1).abs() < 1e-6, "{on_plane:?}");
        for v in 0..2 {
            for joint in [&l, &r, &JointRef::HIPS, &JointRef::CHEST] {
                assert!(
                    (weight_on(&a, v, joint) - weight_on(&b, v, joint)).abs() < 1e-6,
                    "swapping the pair changed v{v}"
                );
            }
        }
        assert_eq!(weight_on(&a, 1, &r), 0.0, "out of the right brush's reach");
    }

    #[test]
    fn brushes_that_are_not_mirrors_still_apply_in_list_order() {
        use omuzaki_anim_api::HumanoidBone::LeftUpperArm;
        let (one, two) =
            (side(0.05, LeftUpperArm), WeightBrush { strength: 0.9, joint: JointRef::HEAD, ..side(0.0, LeftUpperArm) });
        let run = |order: &[WeightBrush]| {
            let mut m = mesh(&[Vec3::ZERO], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
            apply_brushes(&mut m, order);
            weight_on(&m, 0, &JointRef::Humanoid(LeftUpperArm))
        };
        let (a, b) = (run(&[one.clone(), two.clone()]), run(&[two.clone(), one.clone()]));
        assert!((a - b).abs() > 1e-3, "painting over is sequential: {a} vs {b}");
        // The same list with one brush at a time gives the same weights as the whole list.
        let mut m = mesh(&[Vec3::ZERO], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
        apply_brushes(&mut m, std::slice::from_ref(&one));
        apply_brushes(&mut m, std::slice::from_ref(&two));
        assert!((weight_on(&m, 0, &JointRef::Humanoid(LeftUpperArm)) - a).abs() < 1e-7);
    }

    #[test]
    fn a_pair_onto_one_centre_joint_adds_its_amounts_and_stays_normalised() {
        // `side` brushes on a centre-line joint are mirrors of each other (the x flips, the joint stays).
        let (a, b) = (
            WeightBrush { joint: JointRef::HEAD, ..side(0.05, omuzaki_anim_api::HumanoidBone::Head) },
            side(-0.05, omuzaki_anim_api::HumanoidBone::Head),
        );
        assert_eq!(a.mirrored(), b);
        let mut m = mesh(&[Vec3::ZERO], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
        apply_brushes(&mut m, &[a, b]);
        assert!((sum(m.geometry.weights[0]) - 1.0).abs() < 1e-6);
        assert!(weight_on(&m, 0, &JointRef::HEAD) > 0.4, "both brushes moved weight onto the head");
    }

    #[test]
    fn the_cut_drops_a_whole_tied_group_and_otherwise_keeps_the_heaviest() {
        let cut = |weights: &[f64]| influence_cut(weights, 4, |w| *w);
        assert_eq!(cut(&[0.4, 0.3, 0.2]), 3, "nothing to cut");
        assert_eq!(cut(&[0.4, 0.3, 0.2, 0.1, 0.05]), 4, "no tie at the cut");
        assert_eq!(cut(&[0.4, 0.3, 0.2, 0.2, 0.1]), 4, "the 4th and 5th differ");
        assert_eq!(cut(&[0.4, 0.3, 0.2, 0.2, 0.2]), 2, "the 3rd to 5th tie: all go");
        assert_eq!(cut(&[0.4, 0.3, 0.2, 0.2 + 1e-8, 0.2]), 2, "rounding noise still ties");
        assert_eq!(cut(&[0.5, 0.2, 0.2, 0.2, 0.2]), 1, "a group from the 2nd on leaves the one clear owner");
        assert_eq!(cut(&[0.3; 5]), 4, "everything ties: the first four stay");
        assert_eq!(cut(&[]), 0);
    }
}
