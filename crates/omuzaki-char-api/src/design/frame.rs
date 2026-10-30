//! The coordinate frames of the design layer: where on the body or the head a point is, in terms that survive a
//! rebuild (a region and a fraction along it, a direction in the head's own proportions, a point in a part's box).
//!
//! - [`BodyFrame`]: each body region as a polyline of the body fit's sections, so a point locates to a fraction along
//!   the region, the section there and its plane's frame;
//! - [`HeadFrame`]: the head's centre and its radii along the axes, so a direction can be written in the head's own
//!   proportions, plus the face's landmarks (eyes, brows, nose, mouth, cheeks, chin, jaw, temples) read from the face
//!   layout, so a dial on the cheek finds the cheek on any profile;
//! - [`PartFrame`]: a part's bounding box, for parts that are neither.
//!
//! Membership: which [`RegionGroup`] a vertex belongs to is read from its skin weights, so the left thigh's dial never
//! reaches the right thigh where the two touch, and a vertex weighted between the neck and the head takes a share of
//! each.

#![allow(clippy::many_single_char_names)] // vector geometry: points, normals, weights and fractions

use std::collections::BTreeMap;
use std::fmt::{self, Debug};
use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::FaceLayout;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::PartMesh;
use crate::ids::JointRef;
use crate::kind::{BodyFit, BodyRegion, HeadSurface};

/// Sections sampled along each body region.
const ROWS: usize = 32;

/// The groups of the body a vertex can belong to, from its skin weights.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegionGroup {
    /// Hips to the base of the neck, with the shoulders.
    Torso,
    /// The neck.
    Neck,
    /// The head (and the eyes and the jaw).
    Head,
    /// The left arm, shoulder to wrist (the character's left, +x).
    ArmLeft,
    /// The right arm.
    ArmRight,
    /// The left hand and fingers.
    HandLeft,
    /// The right hand and fingers.
    HandRight,
    /// The left leg, hip to ankle.
    LegLeft,
    /// The right leg.
    LegRight,
    /// The left foot.
    FootLeft,
    /// The right foot.
    FootRight,
}

/// How many groups there are.
pub(crate) const GROUPS: usize = 11;

impl RegionGroup {
    /// Every group, in order.
    pub const ALL: [Self; GROUPS] = [
        Self::Torso,
        Self::Neck,
        Self::Head,
        Self::ArmLeft,
        Self::ArmRight,
        Self::HandLeft,
        Self::HandRight,
        Self::LegLeft,
        Self::LegRight,
        Self::FootLeft,
        Self::FootRight,
    ];

    /// The group's index in a [`Membership`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// The body fit's region for this group, if garments wrap it.
    pub fn region(self) -> Option<BodyRegion> {
        Some(match self {
            Self::Torso => BodyRegion::Torso,
            Self::Neck => BodyRegion::Neck,
            Self::ArmLeft => BodyRegion::ArmL,
            Self::ArmRight => BodyRegion::ArmR,
            Self::LegLeft => BodyRegion::LegL,
            Self::LegRight => BodyRegion::LegR,
            Self::FootLeft => BodyRegion::FootL,
            Self::FootRight => BodyRegion::FootR,
            Self::Head | Self::HandLeft | Self::HandRight => return None,
        })
    }

    /// The group of a body fit's region.
    pub fn of_region(region: BodyRegion) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.region() == Some(region))
    }

    /// The same group on the other side (the torso, neck and head are their own mirror).
    #[must_use]
    pub fn mirrored(self) -> Self {
        match self {
            Self::ArmLeft => Self::ArmRight,
            Self::ArmRight => Self::ArmLeft,
            Self::HandLeft => Self::HandRight,
            Self::HandRight => Self::HandLeft,
            Self::LegLeft => Self::LegRight,
            Self::LegRight => Self::LegLeft,
            Self::FootLeft => Self::FootRight,
            Self::FootRight => Self::FootLeft,
            centre => centre,
        }
    }

    /// `+1` for the left side (+x), `-1` for the right, `0` for the middle.
    pub fn side(self) -> f32 {
        match self {
            Self::ArmLeft | Self::HandLeft | Self::LegLeft | Self::FootLeft => 1.0,
            Self::ArmRight | Self::HandRight | Self::LegRight | Self::FootRight => -1.0,
            Self::Torso | Self::Neck | Self::Head => 0.0,
        }
    }

    /// The group a humanoid bone's weight counts toward.
    pub fn of_bone(bone: HumanoidBone) -> Self {
        use HumanoidBone as B;
        match bone {
            B::Neck => Self::Neck,
            B::Head | B::LeftEye | B::RightEye | B::Jaw => Self::Head,
            B::LeftUpperArm | B::LeftLowerArm => Self::ArmLeft,
            B::RightUpperArm | B::RightLowerArm => Self::ArmRight,
            B::LeftUpperLeg | B::LeftLowerLeg => Self::LegLeft,
            B::RightUpperLeg | B::RightLowerLeg => Self::LegRight,
            B::LeftFoot | B::LeftToes => Self::FootLeft,
            B::RightFoot | B::RightToes => Self::FootRight,
            other => {
                let name = other.name();
                let hand = name.ends_with("Hand")
                    || ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| name.contains(f));
                match (hand, name.starts_with("left"), name.starts_with("right")) {
                    (true, true, _) => Self::HandLeft,
                    (true, _, true) => Self::HandRight,
                    // The hips, spine, chests and shoulders.
                    _ => Self::Torso,
                }
            }
        }
    }

    /// The name tools use (`leg_left`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Torso => "torso",
            Self::Neck => "neck",
            Self::Head => "head",
            Self::ArmLeft => "arm_left",
            Self::ArmRight => "arm_right",
            Self::HandLeft => "hand_left",
            Self::HandRight => "hand_right",
            Self::LegLeft => "leg_left",
            Self::LegRight => "leg_right",
            Self::FootLeft => "foot_left",
            Self::FootRight => "foot_right",
        }
    }
}

/// How much a vertex belongs to each [`RegionGroup`]: its skin weights summed per group.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Membership(pub(crate) [f32; GROUPS]);

impl Membership {
    /// Wholly in `group`.
    pub fn only(group: RegionGroup) -> Self {
        let mut m = [0.0; GROUPS];
        m[group.index()] = 1.0;
        Self(m)
    }

    /// The share of `group`.
    pub fn of(&self, group: RegionGroup) -> f32 {
        self.0[group.index()]
    }

    /// The membership of every vertex of `mesh`: its weights summed by the group of each humanoid bone (extra bones
    /// count for nothing). A rigid mesh's vertices belong to `rigid`, if given.
    pub fn of_mesh(mesh: &PartMesh, rigid: Option<RegionGroup>) -> Vec<Self> {
        let g = &mesh.geometry;
        if !g.is_skinned() {
            let m = rigid.map_or_else(Self::default, Self::only);
            return vec![m; g.positions.len()];
        }
        let groups: Vec<Option<RegionGroup>> = mesh
            .joint_bones
            .iter()
            .map(|j| match j {
                JointRef::Humanoid(bone) => Some(RegionGroup::of_bone(*bone)),
                JointRef::Named(_) => None,
            })
            .collect();
        g.joints
            .iter()
            .zip(&g.weights)
            .map(|(joints, weights)| {
                let mut m = [0.0; GROUPS];
                for (j, w) in joints.iter().zip(weights) {
                    if let Some(Some(group)) = groups.get(usize::from(*j)) {
                        m[group.index()] += *w;
                    }
                }
                Self(m)
            })
            .collect()
    }
}

// ------------------------------------------------------------------------------------------------ the body

/// One sampled section of a region, with its plane's frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Row {
    /// The fraction along the region.
    pub t: f32,
    /// The section's centre.
    pub centre: Vec3,
    /// The region's axis (unit, start to end).
    pub axis: Vec3,
    /// Across the region (unit, in the section's plane): world +x projected, or the axis crossed with `through` where
    /// the region runs along x (an arm in the T-pose).
    pub across: Vec3,
    /// Front to back (unit, in the section's plane): world +z projected, up for the feet.
    pub through: Vec3,
    /// The ellipse's half-extents along `across` and `through`.
    pub radii: [f32; 2],
}

impl Row {
    /// The row of a section of `region` at fraction `t`, with its plane's frame.
    pub fn new(region: BodyRegion, t: f32, centre: Vec3, axis: Vec3, radii: [f32; 2]) -> Self {
        let axis = axis.normalize_or(Vec3::Y);
        let front = match region {
            BodyRegion::FootL | BodyRegion::FootR => Vec3::Y,
            _ => Vec3::Z,
        };
        let through = (front - axis * front.dot(axis)).normalize_or(Vec3::Z);
        let x = Vec3::X - axis * axis.x - through * through.x;
        let across = if x.length() > 0.2 { x.normalize() } else { axis.cross(through).normalize_or(Vec3::X) };
        Self { t, centre, axis, across, through, radii }
    }

    fn lerp(&self, other: &Self, s: f32) -> Self {
        Self {
            t: self.t + (other.t - self.t) * s,
            centre: self.centre.lerp(other.centre, s),
            axis: self.axis.lerp(other.axis, s).normalize_or(self.axis),
            across: self.across.lerp(other.across, s).normalize_or(self.across),
            through: self.through.lerp(other.through, s).normalize_or(self.through),
            radii: [
                self.radii[0] + (other.radii[0] - self.radii[0]) * s,
                self.radii[1] + (other.radii[1] - self.radii[1]) * s,
            ],
        }
    }

    /// The mean of the two radii: the region's local scale.
    pub fn scale(&self) -> f32 {
        f32::midpoint(self.radii[0], self.radii[1])
    }

    /// The unit direction in the section's plane at `angle` radians from the front, turning toward `+across`.
    pub fn direction(&self, angle: f32) -> Vec3 {
        self.through * angle.cos() + self.across * angle.sin()
    }

    /// The ellipse's radius in the in-plane direction at `angle`.
    pub fn radius_at(&self, angle: f32) -> f32 {
        let (s, c) = angle.sin_cos();
        let (rx, ry) = (self.radii[0].max(1e-6), self.radii[1].max(1e-6));
        1.0 / ((s / rx).powi(2) + (c / ry).powi(2)).sqrt()
    }

    /// The angle from the front of the in-plane offset `r`.
    pub fn angle_of(&self, r: Vec3) -> f32 {
        r.dot(self.across).atan2(r.dot(self.through))
    }
}

/// One region as a polyline of sections.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionPath {
    /// The region.
    pub region: BodyRegion,
    /// Its sections, start to end.
    pub rows: Vec<Row>,
}

/// Where a point lies relative to a region.
#[derive(Copy, Clone, Debug)]
pub struct Located {
    /// The section nearest the point, interpolated.
    pub row: Row,
    /// The point's offset from the section's centre, in the section's plane.
    pub radial: Vec3,
}

impl RegionPath {
    /// The region's path through `fit`, or `None` if the fit has no section somewhere along it.
    pub fn of(fit: &dyn BodyFit, region: BodyRegion) -> Option<Self> {
        let rows = (0..=ROWS)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)] // ROWS is small
                let t = i as f32 / ROWS as f32;
                fit.section(region, t).map(|s| Row::new(region, t, s.centre, s.axis, [s.radii.x, s.radii.y]))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { region, rows })
    }

    /// The section at fraction `t` (clamped).
    pub fn at(&self, t: f32) -> Row {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (i, s) = {
            let x = t.clamp(0.0, 1.0) * ROWS as f32;
            let i = (x.floor() as usize).min(ROWS - 1);
            (i, x - i as f32)
        };
        self.rows[i].lerp(&self.rows[i + 1], s)
    }

    /// The nearest point of the region's axis to `p`, the section there and `p`'s in-plane offset from it.
    pub fn locate(&self, p: Vec3) -> Located {
        let mut best = (f32::INFINITY, 0usize, 0.0f32);
        for (i, pair) in self.rows.windows(2).enumerate() {
            let (a, b) = (pair[0].centre, pair[1].centre);
            let ab = b - a;
            let s = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
            let d = (a + ab * s).distance_squared(p);
            if d < best.0 {
                best = (d, i, s);
            }
        }
        let row = self.rows[best.1].lerp(&self.rows[best.1 + 1], best.2);
        let offset = p - row.centre;
        Located { row, radial: offset - row.axis * offset.dot(row.axis) }
    }
}

/// The body's regions as paths, taken from the bare body's fit before any dial moved it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BodyFrame {
    /// Each region's path, by its group.
    pub paths: BTreeMap<RegionGroup, RegionPath>,
}

impl BodyFrame {
    /// The frame of `fit`'s regions (a region without sections everywhere is left out).
    pub fn of(fit: &dyn BodyFit) -> Self {
        let paths = RegionGroup::ALL
            .into_iter()
            .filter_map(|g| g.region().and_then(|r| RegionPath::of(fit, r)).map(|p| (g, p)))
            .collect();
        Self { paths }
    }

    /// The path of `group`.
    pub fn path(&self, group: RegionGroup) -> Option<&RegionPath> {
        self.paths.get(&group)
    }
}

// ------------------------------------------------------------------------------------------------ the head

/// A named point of the face, read from the face layout, that dials and masks aim at.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Landmark {
    /// The left eye's centre (the character's left, +x).
    EyeLeft,
    /// The right eye's centre.
    EyeRight,
    /// Over the left brow.
    BrowLeft,
    /// Over the right brow.
    BrowRight,
    /// Between the brows.
    Glabella,
    /// The forehead's middle.
    Forehead,
    /// The left temple.
    TempleLeft,
    /// The right temple.
    TempleRight,
    /// The nose mark.
    Nose,
    /// The bridge of the nose, between the eyes.
    NoseBridge,
    /// The middle of the mouth line.
    Mouth,
    /// The left corner of the mouth.
    MouthLeft,
    /// The right corner of the mouth.
    MouthRight,
    /// The left cheek, at the blush.
    CheekLeft,
    /// The right cheek.
    CheekRight,
    /// The left cheekbone, under the outer corner of the eye.
    CheekboneLeft,
    /// The right cheekbone.
    CheekboneRight,
    /// The lowest point of the face's front.
    Chin,
    /// The left angle of the jaw.
    JawLeft,
    /// The right angle of the jaw.
    JawRight,
    /// The top of the head.
    Crown,
    /// The back of the head.
    Occiput,
    /// The left side of the cranium, above the ear.
    CraniumLeft,
    /// The right side of the cranium.
    CraniumRight,
}

impl Landmark {
    /// Every landmark, in order.
    pub const ALL: [Self; 24] = [
        Self::EyeLeft,
        Self::EyeRight,
        Self::BrowLeft,
        Self::BrowRight,
        Self::Glabella,
        Self::Forehead,
        Self::TempleLeft,
        Self::TempleRight,
        Self::Nose,
        Self::NoseBridge,
        Self::Mouth,
        Self::MouthLeft,
        Self::MouthRight,
        Self::CheekLeft,
        Self::CheekRight,
        Self::CheekboneLeft,
        Self::CheekboneRight,
        Self::Chin,
        Self::JawLeft,
        Self::JawRight,
        Self::Crown,
        Self::Occiput,
        Self::CraniumLeft,
        Self::CraniumRight,
    ];

    /// The landmark on the other side (a midline landmark is its own mirror).
    #[must_use]
    pub fn mirrored(self) -> Self {
        match self {
            Self::EyeLeft => Self::EyeRight,
            Self::EyeRight => Self::EyeLeft,
            Self::BrowLeft => Self::BrowRight,
            Self::BrowRight => Self::BrowLeft,
            Self::TempleLeft => Self::TempleRight,
            Self::TempleRight => Self::TempleLeft,
            Self::MouthLeft => Self::MouthRight,
            Self::MouthRight => Self::MouthLeft,
            Self::CheekLeft => Self::CheekRight,
            Self::CheekRight => Self::CheekLeft,
            Self::CheekboneLeft => Self::CheekboneRight,
            Self::CheekboneRight => Self::CheekboneLeft,
            Self::JawLeft => Self::JawRight,
            Self::JawRight => Self::JawLeft,
            Self::CraniumLeft => Self::CraniumRight,
            Self::CraniumRight => Self::CraniumLeft,
            middle => middle,
        }
    }
}

/// The head's frame: its centre, its radii along the axes, and where the face's landmarks are.
#[derive(Clone)]
pub struct HeadFrame {
    /// The head's centre, bind space.
    pub centre: Vec3,
    /// The surface's distance from the centre along `+x`, `-x`, `+y`, `-y`, `+z`, `-z`.
    pub radii: [f32; 6],
    /// Each landmark as a unit direction from the centre (bind space).
    pub landmarks: BTreeMap<Landmark, Vec3>,
    surface: Arc<dyn HeadSurface>,
}

impl Debug for HeadFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadFrame")
            .field("centre", &self.centre)
            .field("radii", &self.radii)
            .field("landmarks", &self.landmarks)
            .field("surface", &self.surface)
            .finish()
    }
}

impl PartialEq for HeadFrame {
    fn eq(&self, other: &Self) -> bool {
        self.centre == other.centre && self.radii == other.radii && self.landmarks == other.landmarks
    }
}

impl HeadFrame {
    /// The frame of `surface`, with the landmarks of `layout` (head-local units, `scale` metres each: the stature's
    /// factor, since the head's layout is authored at the template's size).
    pub fn of(surface: Arc<dyn HeadSurface>, layout: &FaceLayout, scale: f32) -> Self {
        let centre = surface.centre();
        let r = |d: Vec3| surface.radius(d);
        let radii = [r(Vec3::X), r(-Vec3::X), r(Vec3::Y), r(-Vec3::Y), r(Vec3::Z), r(-Vec3::Z)];
        let mut frame = Self { centre, radii, landmarks: BTreeMap::new(), surface };
        #[allow(clippy::cast_possible_truncation)] // layout numbers are small lengths
        let l = |v: f64| v as f32 * scale;
        let (eye_x, eye_y) = (l(layout.eye_spacing), l(layout.eye_y));
        let brow_y = eye_y + l(layout.brow_gap);
        let (mouth_y, nose_y) = (l(layout.mouth_y), l(layout.nose_y));
        let (cheek_x, cheek_y) = (l(layout.blush_spacing), l(layout.blush_y));
        let mouth_half = 0.45 * eye_x;
        let front = |x: f32, y: f32| frame.front_direction(x, y);
        let mut set = Vec::new();
        for (side, sign) in [(true, 1.0f32), (false, -1.0f32)] {
            let pick = |left: Landmark| if side { left } else { left.mirrored() };
            set.push((pick(Landmark::EyeLeft), front(sign * eye_x, eye_y)));
            set.push((pick(Landmark::BrowLeft), front(sign * eye_x, brow_y)));
            set.push((pick(Landmark::MouthLeft), front(sign * mouth_half, mouth_y)));
            set.push((pick(Landmark::CheekLeft), front(sign * cheek_x, cheek_y)));
            set.push((pick(Landmark::CheekboneLeft), front(sign * 1.35 * eye_x, f32::midpoint(eye_y, cheek_y))));
        }
        set.push((Landmark::Glabella, front(0.0, brow_y)));
        set.push((Landmark::Forehead, front(0.0, brow_y + 1.6 * l(layout.brow_gap))));
        set.push((Landmark::Nose, front(0.0, nose_y)));
        set.push((Landmark::NoseBridge, front(0.0, eye_y)));
        set.push((Landmark::Mouth, front(0.0, mouth_y)));
        set.push((Landmark::Chin, frame.chin()));
        for (landmark, d) in set {
            frame.landmarks.insert(landmark, d);
        }
        for (landmark, n) in [
            (Landmark::TempleLeft, Vec3::new(0.82, 0.42, 0.40)),
            (Landmark::JawLeft, Vec3::new(0.72, -0.62, 0.30)),
            (Landmark::Crown, Vec3::Y),
            (Landmark::Occiput, Vec3::new(0.0, 0.1, -1.0)),
            (Landmark::CraniumLeft, Vec3::new(1.0, 0.35, -0.15)),
        ] {
            let d = frame.to_dir(n);
            frame.landmarks.insert(landmark, d);
            let m = frame.to_dir(Vec3::new(-n.x, n.y, n.z));
            frame.landmarks.insert(landmark.mirrored(), m);
        }
        frame
    }

    /// The head surface the frame was taken from.
    pub fn surface(&self) -> &Arc<dyn HeadSurface> {
        &self.surface
    }

    /// The mean of the six radii: the head's scale.
    pub fn scale(&self) -> f32 {
        self.radii.iter().sum::<f32>() / 6.0
    }

    /// The unit direction (bind space) of a direction `n` written in the head's own proportions: each component
    /// scaled by the radius on its side, so `(1, 0, 1)` points at the same place on a wide head and a narrow one.
    pub fn to_dir(&self, n: Vec3) -> Vec3 {
        let pick = |v: f32, pos: f32, neg: f32| v * if v >= 0.0 { pos } else { neg };
        Vec3::new(
            pick(n.x, self.radii[0], self.radii[1]),
            pick(n.y, self.radii[2], self.radii[3]),
            pick(n.z, self.radii[4], self.radii[5]),
        )
        .normalize_or(Vec3::Z)
    }

    /// The inverse of [`to_dir`](Self::to_dir): a bind-space direction in the head's own proportions (unit).
    pub fn from_dir(&self, d: Vec3) -> Vec3 {
        let pick = |v: f32, pos: f32, neg: f32| v / if v >= 0.0 { pos } else { neg }.max(1e-6);
        Vec3::new(
            pick(d.x, self.radii[0], self.radii[1]),
            pick(d.y, self.radii[2], self.radii[3]),
            pick(d.z, self.radii[4], self.radii[5]),
        )
        .normalize_or(Vec3::Z)
    }

    /// The surface's distance from the centre along the unit direction `d`.
    pub fn radius(&self, d: Vec3) -> f32 {
        self.surface.radius(d)
    }

    /// The direction of a landmark.
    pub fn landmark(&self, l: Landmark) -> Vec3 {
        self.landmarks.get(&l).copied().unwrap_or(Vec3::Z)
    }

    /// The direction of the point of the face's front at head-local `x`, `y` (metres from the centre): the front of
    /// the surface found by refining `z` twice against the radius.
    fn front_direction(&self, x: f32, y: f32) -> Vec3 {
        let mut z = self.radii[4];
        for _ in 0..3 {
            let d = Vec3::new(x, y, z).normalize_or(Vec3::Z);
            let r = self.radius(d);
            z = (r * r - x * x - y * y).max((0.2 * r).powi(2)).sqrt();
        }
        Vec3::new(x, y, z).normalize_or(Vec3::Z)
    }

    /// The direction of the chin: the lowest point of the front meridian, among the points well in front of the
    /// centre.
    fn chin(&self) -> Vec3 {
        let mut best = (f32::INFINITY, Vec3::new(0.0, -0.6, 0.8).normalize());
        for i in 0..=48u8 {
            let a = -std::f32::consts::FRAC_PI_2 + f32::from(i) / 48.0 * std::f32::consts::FRAC_PI_2;
            let d = Vec3::new(0.0, a.sin(), a.cos());
            let p = d * self.radius(d);
            if p.z > 0.25 * self.radii[4] && p.y < best.0 {
                best = (p.y, d);
            }
        }
        best.1
    }
}

// ------------------------------------------------------------------------------------------------ any part

/// A part's bounding box, before the design layer moved it: the frame of parts that are neither the body nor the
/// head.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PartFrame {
    /// The box's lower corner.
    pub min: Vec3,
    /// Its upper corner.
    pub max: Vec3,
}

impl PartFrame {
    /// The box round `meshes`, or `None` if they have no vertices.
    pub fn of(meshes: &[PartMesh]) -> Option<Self> {
        let mut it = meshes.iter().flat_map(|m| m.geometry.positions.iter().copied()).peekable();
        it.peek()?;
        let (min, max) =
            it.fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), p| (lo.min(p), hi.max(p)));
        Some(Self { min, max })
    }

    /// The point at box coordinates `uvw` (`0..=1` on each axis).
    pub fn point(&self, uvw: Vec3) -> Vec3 {
        self.min + (self.max - self.min) * uvw
    }

    /// The box coordinates of `p`.
    pub fn coordinates(&self, p: Vec3) -> Vec3 {
        (p - self.min) / (self.max - self.min).max(Vec3::splat(1e-6))
    }

    /// The box's diagonal: the part's scale.
    pub fn scale(&self) -> f32 {
        (self.max - self.min).length().max(1e-6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bones_fall_into_their_groups() {
        assert_eq!(RegionGroup::of_bone(HumanoidBone::LeftLowerLeg), RegionGroup::LegLeft);
        assert_eq!(RegionGroup::of_bone(HumanoidBone::LeftIndexDistal), RegionGroup::HandLeft);
        assert_eq!(RegionGroup::of_bone(HumanoidBone::RightHand), RegionGroup::HandRight);
        assert_eq!(RegionGroup::of_bone(HumanoidBone::UpperChest), RegionGroup::Torso);
        assert_eq!(RegionGroup::of_bone(HumanoidBone::LeftShoulder), RegionGroup::Torso);
        assert_eq!(RegionGroup::of_bone(HumanoidBone::Jaw), RegionGroup::Head);
        for g in RegionGroup::ALL {
            assert_eq!(g.mirrored().mirrored(), g);
            assert_eq!(RegionGroup::ALL[g.index()], g);
        }
    }

    #[test]
    fn a_row_measures_its_ellipse_from_the_front() {
        let row = Row::new(BodyRegion::LegL, 0.5, Vec3::ZERO, -Vec3::Y, [0.05, 0.08]);
        assert!((row.radius_at(0.0) - 0.08).abs() < 1e-5);
        assert!((row.radius_at(std::f32::consts::FRAC_PI_2) - 0.05).abs() < 1e-5);
        assert!(row.through.dot(Vec3::Z) > 0.99);
        assert!(row.across.dot(Vec3::X) > 0.99);
        let d = row.direction(0.7);
        assert!((row.angle_of(d) - 0.7).abs() < 1e-5);
    }
}
