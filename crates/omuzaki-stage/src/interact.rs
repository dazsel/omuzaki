//! Touches between cast members (#2415): a hug, palms together, cheek to cheek. Each is held on one member of the scene's
//! cast ([`Interact`](omuzaki_stage_api::Interact)) and solved every frame by the [`CastStage`](crate::cast::CastStage)
//! from both bodies as the last frame left them.
//!
//! # What a touch does
//!
//! - **Spacing.** The second member is moved along the line from the first (its placed root kept otherwise) to the
//!   distance the touch needs on these two bodies: chests meeting for a hug (each torso's measured depth), an arm's
//!   reach apart for palms, shoulders meeting side by side for cheeks. A small body and a tall one each get their own.
//! - **Arms.** Each hand that takes part gets a hand goal on the other's body: a bone of the other member (its upper
//!   chest, its spine, its upper arm) plus an offset in that member's facing, so the hand follows the other body as it
//!   moves. The other body's parts join this body's arm guard, so a forearm round a back stays outside it.
//! - **Heads.** The heads turn and lean so they meet or pass: past each other's shoulder in a hug, cheek against cheek
//!   side by side (a lean through the spine, chest, neck and head toward the other, just far enough for the head
//!   shapes the guards measured to touch), eye to eye for palms.
//! - **Posture.** Palms together lays the arm in the library's `palm-touch` pose first, so the palm faces the other's.
//!
//! The solve reads the last frame's bodies, so a hand follows a moving partner one frame late (16 ms at 60 Hz).
//! Everything here is a pure function of the two bodies; nothing allocates.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose, IkGoals, Limb, LookAt};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_rig::ik::guard::{BodyPart, Obstacle};
use omuzaki_stage_api::InteractKind;

/// The joints a [`MemberBody`] carries, in order.
pub const BODY_JOINTS: [HumanoidBone; 12] = [
    HumanoidBone::Hips,
    HumanoidBone::Spine,
    HumanoidBone::Chest,
    HumanoidBone::UpperChest,
    HumanoidBone::Neck,
    HumanoidBone::Head,
    HumanoidBone::LeftUpperArm,
    HumanoidBone::RightUpperArm,
    HumanoidBone::LeftLowerArm,
    HumanoidBone::RightLowerArm,
    HumanoidBone::LeftHand,
    HumanoidBone::RightHand,
];

/// How many guard parts a [`MemberBody`] carries: the waist and the torso, the pelvis, two frustums for each thigh,
/// the head, and a skirt's two frustums.
pub const BODY_PARTS: usize = 10;

/// One member's body as the last frame left it, in the world: what the other member's half of a touch reads.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MemberBody {
    /// The member's world matrix.
    pub root: Mat4,
    /// The joints of [`BODY_JOINTS`], world space; `None` for one the skeleton lacks.
    pub joints: [Option<Vec3>; BODY_JOINTS.len()],
    /// The arm guard's parts (torso, pelvis, thighs, head), world space; `None` without a guard.
    pub parts: [Option<Obstacle>; BODY_PARTS],
    /// Shoulder to wrist, `[left, right]`, metres.
    pub arm: [f32; 2],
    /// Wrist to the palm's centre, `[left, right]`, metres.
    pub palm: [f32; 2],
    /// The hand's half thickness, `[left, right]`, metres.
    pub hand_thickness: [f32; 2],
    /// The forearm's radius, metres.
    pub forearm: f32,
}

impl MemberBody {
    /// No body: no joints, no parts.
    pub const EMPTY: Self = Self {
        root: Mat4::IDENTITY,
        joints: [None; BODY_JOINTS.len()],
        parts: [None; BODY_PARTS],
        arm: [0.0; 2],
        palm: [0.0; 2],
        hand_thickness: [0.0; 2],
        forearm: 0.0,
    };

    /// The joint of `bone`, if the body has it (and [`BODY_JOINTS`] carries it).
    pub fn joint(&self, bone: HumanoidBone) -> Option<Vec3> {
        BODY_JOINTS.iter().position(|b| *b == bone).and_then(|i| self.joints[i])
    }

    /// The guard part `part`, if measured.
    pub fn part(&self, part: BodyPart) -> Option<Obstacle> {
        self.parts.iter().flatten().find(|p| p.part == part).copied()
    }

    /// The way the member faces (its root's `+Z`), level.
    pub fn facing(&self) -> Vec3 {
        let f = self.root.transform_vector3(Vec3::Z);
        Vec3::new(f.x, 0.0, f.z).try_normalize().unwrap_or(Vec3::Z)
    }

    /// The member's own right (its root's `-X`), level.
    pub fn right(&self) -> Vec3 {
        self.facing().cross(Vec3::Y)
    }

    /// Where the root stands.
    pub fn at(&self) -> Vec3 {
        self.root.w_axis.truncate()
    }

    /// The torso as one piece from the waist to the neck: the guard measures it as the waist and the upper torso that
    /// flares from it to the shoulders (#2688), and a touch meets the whole of it. Its flare reaches the upper torso's
    /// width at the top.
    fn upper(&self) -> Option<Obstacle> {
        let torso = self.part(BodyPart::Torso)?;
        let Some(waist) = self.part(BodyPart::Waist) else { return Some(torso) };
        let flare = torso.radius * torso.flare / waist.radius.max(1e-4);
        Some(Obstacle { part: BodyPart::Torso, b: torso.b, flare, ..waist })
    }

    /// The torso as the other member meets it: [`upper`](Self::upper) with its top end lowered by [`TORSO_TOP`] of its
    /// width there, so its cap stands no higher than the neck.
    fn trunk(&self) -> Option<Obstacle> {
        let whole = self.upper()?;
        let mut out = whole;
        let axis = (whole.b - whole.a).normalize_or_zero();
        let top = whole.b - axis * (whole.radius * whole.flare * TORSO_TOP);
        let (kept, length) = ((top - whole.a).dot(axis), whole.a.distance(whole.b));
        if kept > 0.0 {
            out.b = top;
            // The new top end has the width the torso had there.
            out.flare = 1.0 + (whole.flare - 1.0) * kept / length.max(1e-6);
        }
        Some(out)
    }

    /// The torso's half width and half depth at the chest (the upper chest's height) and how far its axis stands ahead
    /// of the root, metres: the guard's measure, else a nominal body.
    fn torso(&self) -> (f32, f32, f32) {
        match self.upper() {
            Some(t) => {
                let axis = t.b - t.a;
                let chest = self.joint(HumanoidBone::UpperChest).or_else(|| self.joint(HumanoidBone::Chest));
                let along = chest.map_or(0.5, |c| (c - t.a).dot(axis) / axis.length_squared().max(1e-8));
                let widen = 1.0 + (t.flare - 1.0) * along.clamp(0.0, 1.0);
                (t.radius * widen, t.depth * widen, ((t.a + t.b) * 0.5 - self.at()).dot(self.facing()))
            }
            None => (0.14, 0.09, 0.0),
        }
    }

    /// The head's centre and radius: the guard's head sphere, else the head joint and a nominal size.
    fn head(&self) -> Option<(Vec3, f32)> {
        self.part(BodyPart::Head)
            .map(|h| (h.a, h.radius * SKULL))
            .or_else(|| self.joint(HumanoidBone::Head).map(|h| (h + Vec3::Y * 0.08, 0.1)))
    }

    /// The same body moved by `by` in the world: what a frame that moves the member's root expects before it runs.
    #[must_use]
    pub fn shifted(&self, by: Vec3) -> Self {
        let mut out = *self;
        out.root = Mat4::from_translation(by) * self.root;
        for joint in out.joints.iter_mut().flatten() {
            *joint += by;
        }
        for part in out.parts.iter_mut().flatten() {
            *part = part.moved(Mat4::from_translation(by));
        }
        out
    }

    /// The parts the other member's arms keep out of: every one measured, the head as its skull ([`SKULL`]).
    pub fn obstacles(&self, out: &mut [Obstacle; BODY_PARTS]) -> usize {
        let mut n = 0;
        for part in self.parts.iter().flatten() {
            out[n] = *part;
            match part.part {
                BodyPart::Head => {
                    out[n].radius *= SKULL;
                    out[n].depth *= SKULL;
                }
                // Part of the torso, as one piece below.
                BodyPart::Waist => continue,
                BodyPart::Torso => out[n] = self.trunk().unwrap_or(*part),
                _ => {}
            }
            n += 1;
        }
        n
    }

    /// Whether the body has what a touch reads: the spine, the head and both arms.
    fn is_complete(&self) -> bool {
        self.joints.iter().all(Option::is_some) && self.arm.iter().all(|a| *a > 1e-3)
    }
}

/// One member's half of a touch for one frame, in the world.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct Overlay {
    /// The hand goals, `[left, right]`: where the wrist goes and a point the elbow bends toward, world space.
    pub hands: [Option<(Vec3, Vec3)>; 2],
    /// Where the head looks, world space, and the eyes' share of the turn.
    pub look: Option<(Vec3, f32)>,
    /// How far the body leans toward its own right, radians (negative: its left), through the spine, chest, neck and
    /// head.
    pub lean: f32,
    /// Whether the lean bends the spine and chest too (side by side), or only the neck and head (face to face, where a
    /// bent chest would turn into the other's arms).
    pub bend: bool,
    /// Whether the arm takes the `palm-touch` posture first: `[left, right]`.
    pub palm_arm: [bool; 2],
}

/// The bones a lean turns, with each one's share of it.
const LEAN: [(HumanoidBone, f32); 4] = [
    (HumanoidBone::Spine, 0.2),
    (HumanoidBone::Chest, 0.25),
    (HumanoidBone::Neck, 0.25),
    (HumanoidBone::Head, 0.3),
];

/// How far below the neck the torso another body meets ends, as a share of its half width: the guard's torso rounds
/// off above the neck by its whole half width, a cap that would stand round the shoulders and neck, where an arm over
/// the shoulders lies.
const TORSO_TOP: f32 = 1.0;
/// How many steps along a torso's axis a report looks for where it comes closest to the other.
const TORSO_SAMPLES: u16 = 8;

/// The share of the head's measured sphere another body meets: the guard measures the head with its hair cap, and hair
/// gives under a cheek or an arm where the skull doesn't.
const SKULL: f32 = 0.75;

/// The gap a touch leaves between bodies that meet, metres: they touch, they don't pass through.
const TOUCH_GAP: f32 = 0.004;
/// How far toward the other's back surface a hug's hand aims, as a share of the way from the axis: inside, so the arm
/// guard lays it on the surface.
const INTO_BACK: f32 = 0.85;
/// The largest lean, radians.
const MAX_LEAN: f32 = 0.35;

/// The distance the second member stands from the first along the line between them for `kind`, metres, on these
/// bodies.
pub fn spacing(kind: InteractKind, a: &MemberBody, b: &MemberBody) -> f32 {
    let ((wa, da, za), (wb, db, zb)) = (a.torso(), b.torso());
    match kind {
        // Chest to chest: each torso's front.
        InteractKind::Hug => (za + da) + (zb + db) + TOUCH_GAP,
        // Each hand reaches a little over half its arm to meet the other halfway.
        InteractKind::PalmTouch => 0.62 * (a.arm[1] + b.arm[0]).max(0.2),
        // Side by side: torso to torso across the shoulders, with room for the arms between that go round the other.
        InteractKind::CheekToCheek => wa + wb + a.forearm + b.forearm + TOUCH_GAP,
    }
}

/// Both halves of `kind` between `a` (who holds the touch) and `b`, from the last frame's bodies, which `last` (the
/// halves that frame was given) shaped: `[a's, b's]`. The leans carry on from `last`'s, corrected by what the heads
/// still miss, so they settle in a few frames however far a body has to bend. A body without a humanoid spine, head
/// and arms takes no part (its overlay is empty).
pub fn solve(kind: InteractKind, a: &MemberBody, b: &MemberBody, last: &[Overlay; 2]) -> [Overlay; 2] {
    if !a.is_complete() || !b.is_complete() {
        return [Overlay::default(); 2];
    }
    let mut out = match kind {
        InteractKind::Hug => hug(a, b),
        InteractKind::PalmTouch => palms(a, b),
        InteractKind::CheekToCheek => cheeks(a, b),
    };
    for (now, before) in out.iter_mut().zip(last) {
        now.lean = (before.lean + now.lean).clamp(-MAX_LEAN, MAX_LEAN);
    }
    out
}

/// A joint the caller has checked is present ([`MemberBody::is_complete`]).
fn j(body: &MemberBody, bone: HumanoidBone) -> Vec3 {
    body.joint(bone).unwrap_or_else(|| body.at())
}

/// A point the elbow of `side` (0 left, 1 right) bends toward: out to that side and down from the shoulder.
fn elbow_out(body: &MemberBody, side: usize, out: f32, down: f32) -> Vec3 {
    let shoulder = j(body, if side == 0 { HumanoidBone::LeftUpperArm } else { HumanoidBone::RightUpperArm });
    let away = if side == 0 { -body.right() } else { body.right() };
    shoulder + (away * out - Vec3::Y * down) * body.arm[side]
}

/// A point on `on`'s back, `across` of its half width toward `side_dir` and `up` metres above `bone`: `into` of the way
/// from the torso's axis to its back surface there (below 1 inside it, where the guard lays the hand on the surface).
fn on_back(on: &MemberBody, bone: HumanoidBone, side_dir: Vec3, across: f32, up: f32, into: f32) -> Vec3 {
    let (w, d, z) = on.torso();
    let back = d * (1.0 - across * across).max(0.0).sqrt();
    j(on, bone) + side_dir * (across * w) + on.facing() * (z - back * into) + Vec3::Y * up
}

/// The further lean each of two heads needs, toward each other along `toward_b` (unit, from a to b), for their spheres
/// to be `want` apart sideways: half the missing distance each, over each one's lever from the hips. Radians.
fn leans_to_meet(a: &MemberBody, b: &MemberBody, want: f32, toward_b: Vec3, pivot: HumanoidBone) -> (f32, f32) {
    let (Some((ha, _)), Some((hb, _))) = (a.head(), b.head()) else { return (0.0, 0.0) };
    let apart = (hb - ha).dot(toward_b);
    let missing = apart - want;
    let lean = |body: &MemberBody, head: Vec3| {
        let lever = (head.y - j(body, pivot).y).max(0.05);
        (0.5 * missing / lever).clamp(-1.0, 1.0).asin()
    };
    (lean(a, ha), lean(b, hb))
}

/// Face to face: a's arms high round b's back (shoulder blades), b's low round a's (the waist), the heads past each
/// other's shoulder, each turned and leaning to its own right.
fn hug(a: &MemberBody, b: &MemberBody) -> [Overlay; 2] {
    let one = |me: &MemberBody, other: &MemberBody, high: bool| {
        // My right is the other's left.
        let (bone, up, across) = if high {
            (HumanoidBone::UpperChest, [0.0, 0.03], [0.5, 0.55])
        } else {
            (HumanoidBone::Chest, [0.0, 0.02], [0.3, 0.35])
        };
        let left = on_back(other, bone, -me.right(), across[0], up[0], INTO_BACK);
        let right = on_back(other, bone, me.right(), across[1], up[1], INTO_BACK);
        let elbow_down = if high { 0.25 } else { 0.45 };
        // The elbows go out and round, past the other's sides.
        let round = me.facing() * 0.35 * me.arm[0];
        Overlay {
            hands: [
                Some((left, elbow_out(me, 0, 0.7, elbow_down) + round)),
                Some((right, elbow_out(me, 1, 0.7, elbow_down) + round)),
            ],
            look: me.head().map(|(h, _)| (h + (me.facing() * 0.6 + me.right() * 0.6 - Vec3::Y * 0.12), 0.3)),
            lean: 0.0,
            bend: false,
            palm_arm: [false; 2],
        }
    };
    let (mut oa, mut ob) = (one(a, b, true), one(b, a, false));
    // The heads pass each other: each leans to its own right until the spheres clear by the gap, measured across the
    // line between the bodies (a's right is the way a's head goes; b's goes the other way, to b's right).
    if let (Some((ha, ra)), Some((hb, rb))) = (a.head(), b.head()) {
        let ahead = (hb - ha).dot(a.facing()).abs();
        let want = ((ra + rb + TOUCH_GAP).powi(2) - ahead * ahead).max(0.0).sqrt();
        // b sits on a's right-hand side of the line by its own lean, so the two leans add.
        let (la, lb) = leans_to_meet(a, b, want, a.right(), HumanoidBone::Neck);
        oa.lean = -la;
        ob.lean = -lb;
    }
    [oa, ob]
}

/// Face to face an arm's length apart: a's right palm and b's left meet halfway at a little over shoulder height,
/// both arms in the `palm-touch` posture; eyes on each other.
fn palms(a: &MemberBody, b: &MemberBody) -> [Overlay; 2] {
    let (sa, sb) = (j(a, HumanoidBone::RightUpperArm), j(b, HumanoidBone::LeftUpperArm));
    let meet = (sa + sb) * 0.5 + Vec3::Y * 0.08 * (a.arm[1] + b.arm[0]);
    let wrist = |me: &MemberBody, side: usize| {
        meet - me.facing() * me.hand_thickness[side].max(0.008) - Vec3::Y * me.palm[side].max(0.02)
    };
    let eyes = |other: &MemberBody| other.head().map(|(h, _)| (h, 0.6));
    let oa = Overlay {
        hands: [None, Some((wrist(a, 1), elbow_out(a, 1, 0.5, 1.0)))],
        look: eyes(b),
        lean: 0.0,
        bend: false,
        palm_arm: [false, true],
    };
    let ob = Overlay {
        hands: [Some((wrist(b, 0), elbow_out(b, 0, 0.5, 1.0))), None],
        look: eyes(a),
        lean: 0.0,
        bend: false,
        palm_arm: [true, false],
    };
    [oa, ob]
}

/// Side by side facing the same way, b on a's right: the heads lean in until the cheeks meet, a's right arm over b's
/// shoulders (its hand on b's far shoulder), b's left arm round a's back (its hand at a's waist); both look ahead.
fn cheeks(a: &MemberBody, b: &MemberBody) -> [Overlay; 2] {
    let want = a.head().zip(b.head()).map_or(0.2, |((_, ra), (_, rb))| ra + rb + TOUCH_GAP * 0.5);
    let (la, lb) = leans_to_meet(a, b, want, a.right(), HumanoidBone::Hips);
    let ahead = |me: &MemberBody| me.head().map(|(h, _)| (h + me.facing() * 2.0, 0.4));
    // a's hand rests on b's far shoulder; the forearm lies across b's shoulders, the elbow behind b's neck.
    // The wrist stands a palm short of the shoulder, so the palm, not the fingertips, is what lies on it.
    let far = j(b, HumanoidBone::RightUpperArm) + Vec3::Y * 0.05 - b.facing() * 0.02 - a.right() * (0.8 * a.palm[1]);
    let near = j(a, HumanoidBone::RightUpperArm);
    let over = (near + far) * 0.5 - a.facing() * 0.3 + Vec3::Y * 0.05;
    // b's hand at the small of a's back, on b's side.
    // On the surface, not inside it: near the side, the guard would push a hand inside out through the side.
    let waist = on_back(a, HumanoidBone::Chest, b.right(), 0.6, -0.03, 1.0) - a.facing() * b.hand_thickness[0];
    // The forearm runs along a's back from b's side, so the hand lies flat on it rather than meeting it fingers first.
    let under = waist + a.right() * (0.5 * b.arm[0]) - a.facing() * 0.05;
    let oa = Overlay {
        hands: [None, Some((far, over))],
        look: ahead(a),
        // Toward b: a's right.
        lean: la,
        bend: true,
        palm_arm: [false; 2],
    };
    let ob = Overlay {
        hands: [Some((waist, under)), None],
        look: ahead(b),
        // Toward a: b's left.
        lean: -lb,
        bend: true,
        palm_arm: [false; 2],
    };
    [oa, ob]
}

/// Lays `overlay` on a member's frame at `weight` (`0..=1`): its hand goals and look in the root space `root`, the lean
/// on `pose`, and the `palm-touch` arm from `palm` (the posture on both sides, `[left, right]`). A goal the frame
/// already has for a hand stays. Allocation-free.
pub fn apply(
    overlay: &Overlay,
    weight: f32,
    root: Mat4,
    palm: &[HumanoidPose; 2],
    pose: &mut HumanoidPose,
    goals: &mut IkGoals,
) {
    let w = if weight.is_finite() { weight.clamp(0.0, 1.0) } else { 0.0 };
    if w <= 0.0 {
        return;
    }
    let to_root = root.inverse();
    for (side, slot) in [&mut goals.left_hand, &mut goals.right_hand].into_iter().enumerate() {
        if let Some((target, pole)) = overlay.hands[side]
            && slot.is_none_or(|g| g.weight <= 0.0)
        {
            *slot = Some(Limb {
                target: to_root.transform_point3(target),
                pole: to_root.transform_point3(pole),
                weight: w,
            });
        }
    }
    if let Some((target, eye_share)) = overlay.look {
        goals.look_at = Some(LookAt { target: to_root.transform_point3(target), weight: w, eye_share });
    }
    if overlay.lean != 0.0 {
        let (spine, chest) = if overlay.bend { (1.0, 1.0) } else { (0.0, 0.0) };
        // Without the bend, the neck and head take the whole lean between them.
        let total = LEAN.iter().map(|(bone, share)| share * bent(*bone, spine, chest)).sum::<f32>().max(1e-3);
        for (bone, share) in LEAN.map(|(bone, share)| (bone, share * bent(bone, spine, chest) / total)) {
            // `+Z` turns `+Y` toward `-X`: the character's right.
            pose.set(bone, Quat::from_rotation_z(overlay.lean * share * w) * pose.get(bone));
        }
    }
    for (side, arm) in palm.iter().enumerate() {
        if !overlay.palm_arm[side] {
            continue;
        }
        for bone in arm_bones(side) {
            pose.set(bone, pose.get(bone).slerp(arm.get(bone), w));
        }
    }
}

/// How much of its share of a lean `bone` takes: the spine's and chest's only with the bend.
fn bent(bone: HumanoidBone, spine: f32, chest: f32) -> f32 {
    match bone {
        HumanoidBone::Spine => spine,
        HumanoidBone::Chest => chest,
        _ => 1.0,
    }
}

/// The bones of one arm and hand, `side` 0 left, 1 right.
fn arm_bones(side: usize) -> impl Iterator<Item = HumanoidBone> {
    HumanoidBone::ALL.into_iter().filter(move |b| {
        let name = b.name();
        let (prefix, arm) = if side == 0 { ("left", "leftShoulder") } else { ("right", "rightShoulder") };
        name.starts_with(prefix) && name != arm && !is_leg(name) && !name.contains("Eye")
    })
}

/// Whether a humanoid bone name is a leg's.
fn is_leg(name: &str) -> bool {
    name.contains("Leg") || name.contains("Foot") || name.contains("Toes")
}

/// How close each hand came to its goal and how near the bodies are, for one touch in the last frame: what a test or a
/// report reads.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TouchReport {
    /// Each goal's miss, metres, `[a left, a right, b left, b right]`; `None` for a hand without a goal.
    pub hand_miss: [Option<f32>; 4],
    /// The smallest gap between either member's forearms and hands and the other's body, metres (negative: inside).
    pub arms_clear: f32,
    /// The gap between the two torsos, metres.
    pub torsos: f32,
    /// The gap between the two heads, metres.
    pub heads: f32,
}

/// A placeholder for the unused slots of a fixed buffer of parts; never measured against.
pub const PLACEHOLDER: Obstacle =
    Obstacle { part: BodyPart::Torso, a: Vec3::ZERO, b: Vec3::ZERO, radius: 0.0, depth: 0.0, forward: Vec3::Z, flare: 1.0 };

/// Measures a touch on the bodies a frame left, against the goals `overlays` set for that frame.
pub fn report(a: &MemberBody, b: &MemberBody, overlays: &[Overlay; 2]) -> TouchReport {
    let mut out = TouchReport { arms_clear: f32::INFINITY, ..TouchReport::default() };
    for (k, (me, other, overlay)) in [(a, b, &overlays[0]), (b, a, &overlays[1])].into_iter().enumerate() {
        for side in 0..2 {
            let (lower, hand) = if side == 0 {
                (HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand)
            } else {
                (HumanoidBone::RightLowerArm, HumanoidBone::RightHand)
            };
            let (Some(elbow), Some(wrist)) = (me.joint(lower), me.joint(hand)) else { continue };
            if let Some((target, _)) = overlay.hands[side] {
                out.hand_miss[k * 2 + side] = Some(wrist.distance(target));
            }
            let mut parts = [PLACEHOLDER; BODY_PARTS];
            let n = other.obstacles(&mut parts);
            for part in &parts[..n] {
                for t in [0.25, 0.5, 0.75, 1.0] {
                    let (gap, _) = part.gap_to(elbow.lerp(wrist, t), me.forearm);
                    out.arms_clear = out.arms_clear.min(gap);
                }
            }
        }
    }
    out.torsos = match (a.trunk(), b.trunk()) {
        // Where they come closest along b's axis: the torsos flare, so the chests can meet above the middle.
        (Some(ta), Some(tb)) => (0..=TORSO_SAMPLES)
            .map(|k| {
                let on = tb.a.lerp(tb.b, f32::from(k) / f32::from(TORSO_SAMPLES));
                let (gap, normal) = ta.gap_to(on, 0.0);
                // From b's axis back out to its surface toward a.
                gap - tb_toward(&tb, on, -normal)
            })
            .fold(f32::INFINITY, f32::min),
        _ => f32::INFINITY,
    };
    out.heads = match (a.head(), b.head()) {
        (Some((ha, ra)), Some((hb, rb))) => ha.distance(hb) - ra - rb,
        _ => f32::INFINITY,
    };
    out
}

/// How far `part`'s surface stands from its axis toward `out` (square to the axis) at `on`, a point on the axis.
fn tb_toward(part: &Obstacle, on: Vec3, out: Vec3) -> f32 {
    let probe = on + out * 10.0;
    10.0 - part.gap_to(probe, 0.0).0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain body at `at` facing `yaw` (radians about +Y): joints of an adult, a torso 0.15 wide and 0.09 deep, a
    /// head of 0.11.
    fn body(at: Vec3, yaw: f32) -> MemberBody {
        let root = Mat4::from_translation(at) * Mat4::from_rotation_y(yaw);
        let p = |x: f32, y: f32, z: f32| root.transform_point3(Vec3::new(x, y, z));
        let mut b = MemberBody { root, ..MemberBody::EMPTY };
        let joints = [
            p(0.0, 0.95, 0.0),
            p(0.0, 1.05, 0.0),
            p(0.0, 1.18, 0.0),
            p(0.0, 1.27, 0.0),
            p(0.0, 1.4, 0.0),
            p(0.0, 1.47, 0.0),
            p(0.17, 1.38, 0.0),
            p(-0.17, 1.38, 0.0),
            p(0.32, 1.12, 0.0),
            p(-0.32, 1.12, 0.0),
            p(0.4, 0.88, 0.0),
            p(-0.4, 0.88, 0.0),
        ];
        b.joints = joints.map(Some);
        let forward = root.transform_vector3(Vec3::Z);
        b.parts[0] = Some(Obstacle {
            part: BodyPart::Torso,
            a: p(0.0, 0.95, 0.01),
            b: p(0.0, 1.4, 0.01),
            radius: 0.15,
            depth: 0.09,
            forward,
            flare: 1.0,
        });
        let head = p(0.0, 1.57, 0.02);
        b.parts[1] =
            Some(Obstacle { part: BodyPart::Head, a: head, b: head, radius: 0.11, depth: 0.11, forward, flare: 1.0 });
        b.arm = [0.5; 2];
        b.palm = [0.05; 2];
        b.hand_thickness = [0.015; 2];
        b.forearm = 0.035;
        b
    }

    #[test]
    fn a_hug_puts_chests_together_and_hands_on_the_other_back() {
        let a = body(Vec3::ZERO, 0.0);
        let d = spacing(InteractKind::Hug, &a, &a);
        assert!((d - (2.0 * (0.09 + 0.01) + TOUCH_GAP)).abs() < 1e-5, "{d}");
        let b = body(Vec3::new(0.0, 0.0, d), std::f32::consts::PI);
        let [oa, ob] = solve(InteractKind::Hug, &a, &b, &[Overlay::default(); 2]);
        // a's hands land behind b's back (b's back is further from a than b's axis), on either side of b's spine.
        let (left, _) = oa.hands[0].unwrap();
        let (right, _) = oa.hands[1].unwrap();
        assert!(left.z > d && right.z > d, "{left} {right}");
        assert!(left.x > 0.0 && right.x < 0.0, "a's left hand on a's left: {left} {right}");
        // b's hands lower, behind a.
        let (bl, _) = ob.hands[0].unwrap();
        assert!(bl.z < 0.0 && bl.y < left.y, "{bl}");
        // The heads lean apart, each to its own right, so they pass.
        assert!(oa.lean > 0.0 && ob.lean > 0.0, "{} {}", oa.lean, ob.lean);
        let report = report(&a, &b, &[oa, ob]);
        assert!(report.torsos.abs() < 0.01, "{report:?}");
    }

    #[test]
    fn cheeks_lean_in_just_enough_and_palms_meet_halfway() {
        let a = body(Vec3::ZERO, 0.0);
        let w = spacing(InteractKind::CheekToCheek, &a, &a);
        // b on a's right (-X), facing the same way.
        let b = body(Vec3::new(-w, 0.0, 0.0), 0.0);
        let [oa, ob] = solve(InteractKind::CheekToCheek, &a, &b, &[Overlay::default(); 2]);
        assert!(oa.lean > 0.0 && ob.lean < 0.0, "a leans right, b left: {} {}", oa.lean, ob.lean);
        // The lean closes the gap: each head moves its lever times sin(lean) toward the other.
        let lever = 1.57 - 0.95;
        let moved = lever * oa.lean.sin() + lever * (-ob.lean).sin();
        let gap = w - (2.0 * 0.11 * SKULL + TOUCH_GAP * 0.5);
        assert!((moved - gap).abs() < 0.01, "{moved} {gap}");

        let d = spacing(InteractKind::PalmTouch, &a, &a);
        let b = body(Vec3::new(0.0, 0.0, d), std::f32::consts::PI);
        let [oa, ob] = solve(InteractKind::PalmTouch, &a, &b, &[Overlay::default(); 2]);
        let (ra, _) = oa.hands[1].unwrap();
        let (lb, _) = ob.hands[0].unwrap();
        assert!(oa.hands[0].is_none() && ob.hands[1].is_none());
        assert!((ra.x - lb.x).abs() < 1e-4 && ra.z < d * 0.5 && lb.z > d * 0.5, "{ra} {lb}");
        assert!(oa.palm_arm[1] && ob.palm_arm[0]);
    }

    #[test]
    fn applying_puts_goals_in_root_space_and_keeps_a_frames_own() {
        let a = body(Vec3::new(1.0, 0.0, 2.0), 0.5);
        let b = body(Vec3::new(1.0, 0.0, 2.3), 0.5 + std::f32::consts::PI);
        let [oa, _] = solve(InteractKind::Hug, &a, &b, &[Overlay::default(); 2]);
        let mut pose = HumanoidPose::REST;
        let own = Limb { target: Vec3::ONE, pole: Vec3::Z, weight: 1.0 };
        let mut goals = IkGoals { left_hand: Some(own), ..IkGoals::NONE };
        let palm = [HumanoidPose::REST; 2];
        apply(&oa, 0.5, a.root, &palm, &mut pose, &mut goals);
        assert_eq!(goals.left_hand, Some(own));
        let right = goals.right_hand.unwrap();
        assert!((a.root.transform_point3(right.target) - oa.hands[1].unwrap().0).length() < 1e-4);
        assert!((right.weight - 0.5).abs() < 1e-6);
        assert!(goals.look_at.is_some());
        assert_ne!(pose.get(HumanoidBone::Head), Quat::IDENTITY, "the lean turned the head");
        // No weight, no change.
        let mut untouched = IkGoals::NONE;
        apply(&oa, 0.0, a.root, &palm, &mut pose, &mut untouched);
        assert_eq!(untouched, IkGoals::NONE);
        // A body without a skeleton takes no part.
        assert_eq!(solve(InteractKind::Hug, &MemberBody::EMPTY, &b, &[Overlay::default(); 2]), [Overlay::default(); 2]);
    }
}
