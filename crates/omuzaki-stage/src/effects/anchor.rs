//! Anchors (particles.md D12): where a burst starts, from this frame's pose and camera. Bones are read from the
//! solved pose, not the bind pose, so confetti leaves the hand where it is now; an anchor's velocity comes from its
//! last two frames. Offsets are in subject units on members and metres elsewhere (scenes D4).

use omuzaki_anim_api::{HumanoidBone, Pose};
use omuzaki_char_api::{CharacterRig, JointRef};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Projection};
use omuzaki_stage_api::effects::BurstShape;

use super::service::{At, Origin, Point, finding};
use super::weather::yaw_of;

/// How far above the head bone `top` is, in subject units.
const ABOVE_HEAD: f32 = 0.12;
/// Where `top` is on a character without a head bone, in subject units above its root.
const TOP_WITHOUT_HEAD: f32 = 1.05;
/// The chest's height used to measure the camera's distance to the subject, in subject units.
const CHEST: f32 = 0.7;
/// How far above the frame's top edge the camera's strip is, as a share of the frame's half height.
const STRIP_ABOVE: f32 = 1.1;
/// The strip's depth, as a share of the distance.
const STRIP_DEPTH: f32 = 0.1;

/// The subject's pose in one frame.
#[derive(Copy, Clone)]
pub struct Figure<'a> {
    /// The character's world matrix.
    pub root: Mat4,
    /// Every bone's model-space transform.
    pub world: &'a Pose,
}

/// What anchors are resolved against: this frame's and the last frame's subject and camera.
#[derive(Copy, Clone)]
pub struct Anchors<'a> {
    /// The character's skeleton.
    pub rig: &'a CharacterRig,
    /// This frame.
    pub now: Figure<'a>,
    /// The last frame, if there was one.
    pub before: Option<Figure<'a>>,
    /// This frame's camera.
    pub camera: Camera,
    /// The last frame's camera transform.
    pub camera_before: Option<Mat4>,
    /// Seconds between the two frames.
    pub dt: f32,
    /// The subject's height, metres.
    pub unit: f32,
    /// The target's aspect.
    pub aspect: f32,
    /// The key of the subject member's id.
    pub subject: Option<u64>,
}

/// A resolved anchor.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Resolved {
    /// Emitter to world.
    pub emitter: Mat4,
    /// Where in the emitter.
    pub shape: BurstShape,
    /// Metres per second.
    pub velocity: Vec3,
    /// Whether it is a member (so `palette` colours apply).
    pub member: bool,
    /// [`finding`] bits it raised.
    pub findings: u32,
}

impl Anchors<'_> {
    /// Resolves `at`.
    pub fn resolve(&self, at: At) -> Resolved {
        match at.origin {
            Origin::World | Origin::Stage => Resolved {
                emitter: Mat4::from_translation(at.offset),
                shape: BurstShape::Point,
                velocity: Vec3::ZERO,
                member: false,
                findings: 0,
            },
            Origin::Camera => self.camera(at),
            Origin::Subject => self.member(at, 0),
            Origin::Member(key) => self.member(at, if Some(key) == self.subject { 0 } else { finding::MEMBER }),
            Origin::Prop(_) => self.member(at, finding::PROP),
        }
    }

    /// A point on the subject: its position and the root's turn, and the findings on the way.
    fn place(&self, figure: Figure<'_>, at: At) -> (Vec3, Quat, u32) {
        let (_, turn, origin) = figure.root.to_scale_rotation_translation();
        let bone_at = |bone: HumanoidBone| {
            let id = self.rig.bone(&JointRef::Humanoid(bone))?;
            let local = figure.world.local.get(id.index())?;
            Some(figure.root.transform_point3(local.translation))
        };
        let up = Vec3::Y * self.unit;
        let (base, findings) = match at.point {
            Point::Origin | Point::Centre => (origin, 0),
            Point::Bone(bone) => bone_at(bone).map_or((origin, finding::BONE), |p| (p, 0)),
            Point::Top => (bone_at(HumanoidBone::Head).map_or(origin + up * TOP_WITHOUT_HEAD, |p| p + up * ABOVE_HEAD), 0),
        };
        (base + turn * (at.offset * self.unit), turn, findings)
    }

    fn member(&self, at: At, findings: u32) -> Resolved {
        let (position, turn, more) = self.place(self.now, at);
        let velocity = match self.before {
            Some(before) if self.dt > 0.0 => (position - self.place(before, at).0) / self.dt,
            _ => Vec3::ZERO,
        };
        Resolved {
            emitter: Mat4::from_rotation_translation(turn, position),
            shape: BurstShape::Point,
            velocity,
            member: true,
            findings: findings | more,
        }
    }

    /// A point of the camera's frame at the subject's distance: its `centre`, or a strip just above its top edge.
    fn camera_point(&self, transform: Mat4, at: At) -> (Mat4, BurstShape) {
        let eye = transform.w_axis.truncate();
        let forward = (-transform.z_axis.truncate()).normalize_or(Vec3::NEG_Z);
        let up = transform.y_axis.truncate().normalize_or(Vec3::Y);
        let chest = self.now.root.w_axis.truncate() + Vec3::Y * (CHEST * self.unit);
        let distance = (chest - eye).dot(forward).max(0.1);
        let half_height = match self.camera.projection {
            Projection::Perspective { fov_y_deg, .. } => distance * (fov_y_deg.to_radians() * 0.5).tan(),
            Projection::Orthographic { height, .. } => height * 0.5,
        };
        let turn = yaw_of(&transform);
        let centre = eye + forward * distance;
        let (position, shape) = if at.point == Point::Top {
            let half = [half_height * self.aspect, distance * STRIP_DEPTH];
            (centre + up * (half_height * STRIP_ABOVE), BurstShape::rect(half))
        } else {
            (centre, BurstShape::Point)
        };
        (Mat4::from_rotation_translation(turn, position + turn * at.offset), shape)
    }

    fn camera(&self, at: At) -> Resolved {
        let (emitter, shape) = self.camera_point(self.camera.transform, at);
        let velocity = match self.camera_before {
            Some(before) if self.dt > 0.0 => {
                (emitter.w_axis.truncate() - self.camera_point(before, at).0.w_axis.truncate()) / self.dt
            }
            _ => Vec3::ZERO,
        };
        Resolved { emitter, shape, velocity, member: false, findings: 0 }
    }
}
