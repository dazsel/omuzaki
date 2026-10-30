//! A pose ready to combine: [`Posture`], [`PoseMask`] and the three operations (blend, mirror, layer).

use omuzaki_anim_api::{HumanoidBone, HumanoidPose, Reaches};

use crate::hands::{Finger, FingerCurl, HandRest, HandShape, Side, finger_targets, set_bone};

/// One bone's share of a hand shape: the finger's four angles and how strongly this bone takes them.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BoneMix {
    /// The finger's angles. A bone reads the ones it moves with: the knuckle bone the spread and knuckle flexion, the
    /// others their own flexion.
    pub curl: FingerCurl,
    /// `0..=1`; `0` leaves the bone as the body says (and then `curl` has no influence at all).
    pub weight: f32,
}

/// The bones of one hand as a pose sets them: a hand shape resolved to plain numbers, one [`BoneMix`] per finger bone.
///
/// Fixed-size and `Copy`, with no ids or names, so blending, mirroring and layering never allocate. Per bone, so a mask
/// can take one joint of an overlay's finger and leave the rest of that finger as the base has it.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Hand {
    /// Thumb to little finger, in [`Finger::ALL`] order; each finger's bones base first, as [`Finger::bones`].
    pub bones: [[BoneMix; 3]; 5],
}

/// Weights are held to `0..=1`; a NaN counts as `0`.
fn unit(w: f32) -> f32 {
    if w.is_nan() { 0.0 } else { w.clamp(0.0, 1.0) }
}

fn mix_curl(a: FingerCurl, b: FingerCurl, t: f32) -> FingerCurl {
    let m = |x: f32, y: f32| x + (y - x) * t;
    FingerCurl { spread: m(a.spread, b.spread), mcp: m(a.mcp, b.mcp), pip: m(a.pip, b.pip), dip: m(a.dip, b.dip) }
}

/// Moves one bone `w` of the way from `a` to `b`. Each side contributes in proportion to its *effective influence*
/// (its weight times its share of the blend), and the angles are the influence-weighted mean: a shape at weight `0`
/// has no say, so it blends exactly like no shape at all, and a bone one pose leaves alone fades in or out.
fn blend_bone(a: BoneMix, b: BoneMix, w: f32) -> BoneMix {
    let (ia, ib) = (a.weight * (1.0 - w), b.weight * w);
    let total = ia + ib;
    let curl = if ia <= 0.0 {
        b.curl
    } else if ib <= 0.0 {
        a.curl
    } else {
        mix_curl(a.curl, b.curl, ib / total)
    };
    BoneMix { curl, weight: total }
}

impl Hand {
    /// No shape: every bone as the body says.
    pub const NONE: Self = Self {
        bones: [[BoneMix { curl: FingerCurl { spread: 0.0, mcp: 0.0, pip: 0.0, dip: 0.0 }, weight: 0.0 }; 3]; 5],
    };

    /// `shape` on all five fingers at `weight`.
    pub fn of(shape: &HandShape, weight: f32) -> Self {
        Self { bones: Finger::ALL.map(|f| [BoneMix { curl: shape.curl(f), weight }; 3]) }
    }

    /// Moves `w` of the way from `a` to `b`, bone by bone: each side counts in proportion to its weight, so a hand at
    /// weight `0` blends exactly like no shape at all. Exact at both ends.
    pub fn blend(a: &Self, b: &Self, w: f32) -> Self {
        let mut out = *a;
        for (out_finger, (fa, fb)) in out.bones.iter_mut().zip(a.bones.iter().zip(&b.bones)) {
            for (o, (x, y)) in out_finger.iter_mut().zip(fa.iter().zip(fb)) {
                *o = blend_bone(*x, *y, w);
            }
        }
        out
    }
}

/// A set of humanoid bones, for [`Posture::layer`]: a wave over an idle takes the arm and leaves the rest.
///
/// The mask is per bone, hand bones included: a mask of the wrist alone leaves every finger as the base has it, and a
/// mask of one finger joint takes only that joint.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PoseMask(u64);

impl PoseMask {
    /// No bones.
    pub const NONE: Self = Self(0);
    /// Every bone, and so the hips offset and both hands.
    pub const ALL: Self = Self((1 << omuzaki_anim_api::HUMANOID_COUNT) - 1);

    /// The mask of exactly these bones.
    pub fn of(bones: impl IntoIterator<Item = HumanoidBone>) -> Self {
        Self(bones.into_iter().fold(0, |m, b| m | 1 << b.index()))
    }

    /// `root` and every bone below it in the humanoid hierarchy: `subtree(LeftShoulder)` is the whole left arm with its
    /// fingers, `subtree(Spine)` the torso, arms and head.
    pub fn subtree(root: HumanoidBone) -> Self {
        // Parents come before children in `ALL`, so one pass finds every descendant.
        let mut mask = Self::of([root]);
        for bone in HumanoidBone::ALL {
            if bone.parent().is_some_and(|p| mask.contains(p)) {
                mask = mask.union(Self::of([bone]));
            }
        }
        mask
    }

    /// Whether `bone` is in the mask.
    pub fn contains(self, bone: HumanoidBone) -> bool {
        self.0 >> bone.index() & 1 == 1
    }

    /// The bones in either mask.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The same mask on the opposite side (left arm becomes right arm).
    #[must_use]
    pub fn mirrored(self) -> Self {
        Self::of(HumanoidBone::ALL.into_iter().filter(|b| self.contains(*b)).map(HumanoidBone::mirrored))
    }
}

/// A pose that can be blended, mirrored and layered; see the [module](super) for why it is not a [`PoseDef`](super::PoseDef).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Posture {
    /// The hips offset and every bone's rotation (fingers as the bones say, before any hand shape).
    pub body: HumanoidPose,
    /// The character's left hand shape.
    pub left: Hand,
    /// The character's right hand shape.
    pub right: Hand,
    /// Where the hands reach on the body (the rig's IK, after the bones above).
    pub reach: Reaches,
}

impl Posture {
    fn hand(&self, side: Side) -> &Hand {
        if side == Side::Left { &self.left } else { &self.right }
    }

    /// Moves `w` of the way from `a` to `b` (`w` held to `0..=1`): per-bone slerp, hips lerp, hand shapes blended.
    /// Exact at both ends: `w = 0` is `a`, `w = 1` is `b`.
    #[must_use]
    pub fn blend(a: &Self, b: &Self, w: f32) -> Self {
        let w = unit(w);
        if w <= 0.0 {
            return *a;
        }
        if w >= 1.0 {
            return *b;
        }
        Self {
            body: a.body.lerp(&b.body, w),
            left: Hand::blend(&a.left, &b.left, w),
            right: Hand::blend(&a.right, &b.right, w),
            reach: Reaches::blend(&a.reach, &b.reach, w),
        }
    }

    /// The posture reflected left to right: sides swap and rotations reflect (`HumanoidPose::mirrored`). Hand shapes
    /// are side-neutral, so they only change sides. Exact: mirroring twice gives back the same posture.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        Self { body: self.body.mirrored(), left: self.right, right: self.left, reach: self.reach.mirrored() }
    }

    /// `overlay` over `base`, `w` strong (held to `0..=1`), on the bones in `mask` only; everything outside the mask is
    /// `base`, bit for bit. The hips offset is layered when the hips are in the mask, and each hand bone when it is.
    #[must_use]
    pub fn layer(base: &Self, overlay: &Self, mask: PoseMask, w: f32) -> Self {
        let w = unit(w);
        let mixed = Self::blend(base, overlay, w);
        let mut out = *base;
        for bone in HumanoidBone::ALL.into_iter().filter(|b| mask.contains(*b)) {
            out.body.set(bone, mixed.body.get(bone));
        }
        if mask.contains(HumanoidBone::Hips) {
            out.body.hips_offset = mixed.body.hips_offset;
        }
        // A reach moves its arm: it is layered where the mask holds that arm.
        if mask.contains(HumanoidBone::LeftUpperArm) {
            out.reach.left = mixed.reach.left;
        }
        if mask.contains(HumanoidBone::RightUpperArm) {
            out.reach.right = mixed.reach.right;
        }
        for (side, hand, from) in
            [(Side::Left, &mut out.left, &mixed.left), (Side::Right, &mut out.right, &mixed.right)]
        {
            for finger in Finger::ALL {
                let f = finger as usize;
                for (i, bone) in finger.bones(side).into_iter().enumerate() {
                    if mask.contains(bone) {
                        hand.bones[f][i] = from.bones[f][i];
                    }
                }
            }
        }
        out
    }

    /// The pose the animator plays: the bones with each hand bone's shape applied on top, on a rig whose thumbs rest as
    /// `rest`. Never allocates.
    #[must_use]
    pub fn to_humanoid(&self, rest: &HandRest) -> HumanoidPose {
        let mut pose = self.body;
        for side in [Side::Left, Side::Right] {
            let hand = self.hand(side);
            for finger in Finger::ALL {
                let mixes = &hand.bones[finger as usize];
                for (i, (bone, mix)) in finger.bones(side).into_iter().zip(mixes).enumerate() {
                    if mix.weight <= 0.0 {
                        continue;
                    }
                    let target = finger_targets(side, rest, finger, mix.curl)[i];
                    set_bone(&mut pose, bone, target, mix.weight);
                }
            }
        }
        pose
    }
}
