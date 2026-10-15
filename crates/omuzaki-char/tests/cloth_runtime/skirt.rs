//! The synthetic garments and rig the cloth runtime tests share: a flared skirt and a robe lofted as grids, skinned to a
//! small humanoid (hips, spine, legs) whose pose is a few angles, and the leg colliders.
//!
//! Included by `#[path]` from each `cloth_*` test file; each uses the part it needs.
#![allow(dead_code, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::unwrap_used)]

use std::f32::consts::TAU;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::cloth::collide::{ColliderKind, ColliderSet};
use omuzaki_char::cloth::runtime::{ClothGrid, ClothRuntime, ClothSpec, SkinWeights};
use omuzaki_char_api::{JointRef, PoseSource};
use omuzaki_core::glam::{Affine3A, Mat4, Quat, Vec3};

/// Joints of the synthetic rig, in the grids' joint-list order.
pub const JOINTS: [HumanoidBone; 6] = [
    HumanoidBone::Hips,
    HumanoidBone::Spine,
    HumanoidBone::LeftUpperLeg,
    HumanoidBone::RightUpperLeg,
    HumanoidBone::LeftLowerLeg,
    HumanoidBone::RightLowerLeg,
];

pub const HIPS: Vec3 = Vec3::new(0.0, 1.0, 0.0);
pub const HIP_L: Vec3 = Vec3::new(0.09, 0.95, 0.0);
pub const HIP_R: Vec3 = Vec3::new(-0.09, 0.95, 0.0);
pub const KNEE_L: Vec3 = Vec3::new(0.09, 0.52, 0.0);
pub const KNEE_R: Vec3 = Vec3::new(-0.09, 0.52, 0.0);
pub const ANKLE_L: Vec3 = Vec3::new(0.09, 0.1, 0.0);
pub const ANKLE_R: Vec3 = Vec3::new(-0.09, 0.1, 0.0);

/// The rig's pose: hips yaw and drop, hip flexion and knee bend per side (radians, metres). The character faces +Z;
/// a positive flexion swings the knee forward.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Pose {
    pub yaw: f32,
    pub drop: f32,
    pub flex_l: f32,
    pub flex_r: f32,
    pub knee_l: f32,
    pub knee_r: f32,
    /// The root's world placement (the character walking through the world).
    pub root: Vec3,
}

fn about(pivot: Vec3, rotation: Quat) -> Affine3A {
    Affine3A::from_translation(pivot) * Affine3A::from_quat(rotation) * Affine3A::from_translation(-pivot)
}

impl Pose {
    /// Each joint's skin matrix (bind to world); bones rest with identity rotation, so bind = rest.
    pub fn matrices(&self) -> [Affine3A; 6] {
        let root = Affine3A::from_translation(self.root);
        let hips = root
            * Affine3A::from_translation(Vec3::new(0.0, -self.drop, 0.0))
            * about(HIPS, Quat::from_rotation_y(self.yaw));
        let upper_l = hips * about(HIP_L, Quat::from_rotation_x(-self.flex_l));
        let upper_r = hips * about(HIP_R, Quat::from_rotation_x(-self.flex_r));
        let lower_l = upper_l * about(KNEE_L, Quat::from_rotation_x(self.knee_l));
        let lower_r = upper_r * about(KNEE_R, Quat::from_rotation_x(self.knee_r));
        [hips, hips, upper_l, upper_r, lower_l, lower_r]
    }

    /// A walk cycle at `t` seconds: hips flex ±35° at 1 Hz in opposition, knees bend on the back swing.
    pub fn walk(t: f32) -> Self {
        let s = (TAU * t).sin();
        let flex = 35f32.to_radians();
        Self {
            flex_l: flex * s,
            flex_r: -flex * s,
            knee_l: 0.5 * (-s).max(0.0),
            knee_r: 0.5 * s.max(0.0),
            yaw: 0.08 * s,
            root: Vec3::new(0.0, 0.0, 1.2 * t),
            ..Self::default()
        }
    }

    /// A squat at depth `d` in `0..=1`: hips drop 30 cm, thighs flex 80°, knees bend 110°.
    pub fn squat(d: f32) -> Self {
        let flex = 80f32.to_radians() * d;
        let knee = 110f32.to_radians() * d;
        Self { drop: 0.3 * d, flex_l: flex, flex_r: flex, knee_l: knee, knee_r: knee, ..Self::default() }
    }
}

impl PoseSource for Pose {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        let JointRef::Humanoid(bone) = joint else { return None };
        let i = JOINTS.iter().position(|b| b == bone)?;
        Some(Mat4::from(self.matrices()[i]))
    }

    fn root(&self) -> Mat4 {
        Mat4::from_translation(self.root)
    }
}

/// The joints as the grid lists them.
pub fn joints() -> Vec<JointRef> {
    JOINTS.iter().map(|&b| JointRef::Humanoid(b)).collect()
}

/// A wrapped garment grid of `rows x cols` from a waist ring (`y0`, radius `r0`) down to a hem ring (`y1`, `r1`), the
/// lower rows weighted toward the thigh on their side.
pub fn tube(rows: usize, cols: usize, y0: f32, r0: f32, y1: f32, r1: f32) -> ClothGrid {
    let mut positions = Vec::with_capacity(rows * cols);
    let mut skin = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        let radius = r0 + (r1 - r0) * t;
        let y = y0 + (y1 - y0) * t;
        for c in 0..cols {
            let angle = TAU * c as f32 / cols as f32;
            positions.push(Vec3::new(radius * angle.sin(), y, radius * angle.cos()));
            // Toward the thighs, as a skirt's auto weights put it: the share grows over the top 40% of the garment
            // to 80%, from the back (a fifth of it) to the front (all of it), split between the left (+x, sin > 0)
            // and right thighs by side.
            let leg = 0.8 * (t / 0.4).min(1.0) * (0.6 + 0.4 * angle.cos());
            let left = (0.5 + angle.sin() / 0.6).clamp(0.0, 1.0);
            skin.push(SkinWeights { joints: [0, 2, 3, 0], weights: [1.0 - leg, leg * left, leg * (1.0 - left), 0.0] });
        }
    }
    ClothGrid { rows, cols, wrap: true, positions, skin, joints: joints() }
}

/// The skirt: 48 x 32 vertices, waist 14 cm at 1.02 m flaring to a 24 cm hem at 0.58 m.
pub fn skirt_grid() -> ClothGrid {
    tube(32, 48, 1.02, 0.15, 0.58, 0.24)
}

/// The robe: 48 x 32 vertices over the skirt, from a 17 cm waist at 1.04 m to a 29 cm hem at 0.40 m.
pub fn robe_grid() -> ClothGrid {
    tube(32, 48, 1.04, 0.18, 0.40, 0.29)
}

/// The thigh capsules' radius, metres.
pub const THIGH: f32 = 0.065;

/// Thighs, shins and a hips capsule, on the rig's bones, all inside the skirt at rest.
pub fn leg_colliders() -> ColliderSet {
    let mut set = ColliderSet::new();
    let j = |b| JointRef::Humanoid(b);
    set.add_capsule(HIP_L, KNEE_L, THIGH, &j(HumanoidBone::LeftUpperLeg), ColliderKind::Body);
    set.add_capsule(HIP_R, KNEE_R, THIGH, &j(HumanoidBone::RightUpperLeg), ColliderKind::Body);
    set.add_capsule(KNEE_L, ANKLE_L, 0.05, &j(HumanoidBone::LeftLowerLeg), ColliderKind::Body);
    set.add_capsule(KNEE_R, ANKLE_R, 0.05, &j(HumanoidBone::RightLowerLeg), ColliderKind::Body);
    set.add_capsule(
        Vec3::new(0.04, 0.98, 0.0),
        Vec3::new(-0.04, 0.98, 0.0),
        0.1,
        &j(HumanoidBone::Hips),
        ColliderKind::Body,
    );
    set
}

/// A skirt runtime with the default spec (24 x 16, 12 x 8, 6 x 4, live) and the leg colliders.
pub fn skirt() -> ClothRuntime {
    ClothRuntime::new(&skirt_grid(), &ClothSpec::default(), leg_colliders()).unwrap()
}

/// The thigh capsules (axis ends and radius) in world space under `pose`.
pub fn thighs(pose: &Pose) -> [(Vec3, Vec3, f32); 2] {
    let m = pose.matrices();
    [
        (m[2].transform_point3(HIP_L), m[2].transform_point3(KNEE_L), THIGH),
        (m[3].transform_point3(HIP_R), m[3].transform_point3(KNEE_R), THIGH),
    ]
}

/// Distance from `p` to the segment `a`-`b`.
pub fn to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let e = b - a;
    let s = ((p - a).dot(e) / e.length_squared()).clamp(0.0, 1.0);
    p.distance(a + e * s)
}

/// The skinned static mesh of `grid` under `pose`, in character space.
pub fn skinned(grid: &ClothGrid, pose: &Pose) -> Vec<Vec3> {
    let m = pose.matrices();
    let root_inv = Affine3A::from_translation(-pose.root);
    grid.positions
        .iter()
        .zip(&grid.skin)
        .map(|(&p, s)| {
            let total: f32 = s.weights.iter().sum();
            let w: Vec3 = s
                .joints
                .iter()
                .zip(&s.weights)
                .map(|(&j, &w)| m[usize::from(j)].transform_point3(p) * (w / total))
                .sum();
            root_inv.transform_point3(w)
        })
        .collect()
}
