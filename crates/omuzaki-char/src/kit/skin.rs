//! Capsule-segment auto-weights (the prototype's `skinKit`) and the weight utilities around them (RG-02).
//!
//! Ported from the prototype's `build.js` (`SEG`, `segDist`, `capsuleWeights`, and `setWeights`' top-four normalise with
//! a hips fallback) and `uniform-geo.js` (`bindWeights`, and the `sstep` joint blends the legs and sleeves use).
//!
//! **The idea.** Every bone is a *capsule axis*: a segment from the bone's joint to its child's joint. A vertex is pulled
//! by each candidate bone with weight `1 / (distance + e)^k`, where `distance` is the vertex's distance to the segment, so
//! the nearest bone wins and neighbouring bones hand over smoothly. The four heaviest are kept and normalised. Parts call
//! [`SegmentSet::weights_at`] from a `Skin::Per` closure, or [`SegmentSet::skin_mesh`] for a whole mesh.
//!
//! | Piece | Does |
//! |---|---|
//! | [`SegmentSet::from_fit`] | each humanoid bone's axis from the template's joints, with the terminal rules |
//! | [`SegmentSet::capsule_weights`], [`SegmentSet::weights_at`], [`SegmentSet::skin_mesh`] | the weights of one point, finished, and a mesh |
//! | [`blend_between`] | the `sstep` hand-over between two joints along an axis |
//! | [`limit_influences`], [`normalise`] | the top-four rule |
//! | [`mirror`], [`smooth`], [`max_neighbour_jump`] | edit a skinned mesh's weights |
//! | [`brush_mirrored`], [`brush_from_stroke`] | helpers for the `rig.weights` ops |
//!
//! **Five-finger hands (RG-09).** [`SegmentSet::from_fit`] gives all 30 finger bones an axis, and [`finger_chains`] lists a
//! hand's five chains. Weight a finger's vertices against that finger's chain plus the hand and forearm only (the `bones`
//! of [`SegmentSet::weights_at`]), never against every finger: fingers lie about two radii apart, so a vertex on the side
//! facing the next finger is nearly as close to that finger's axis as to its own, and no softening separates them.

use std::collections::HashMap;
use std::iter::successors;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BrushFalloff, JointRef, PartMesh, WeightBrush, influence_cut};
use omuzaki_core::glam::{DVec3, Vec3};

use super::KitError;
use super::curve::{clamp, sstep};
use super::grid::mirror_joint;

/// The most joints that move one vertex: the renderer's skinning format and the VRM limit.
pub const MAX_INFLUENCES: usize = 4;

/// How far past the wrist the prototype's hand capsule reaches along the forearm, metres, when the hand has no fingers.
pub const HAND_REACH: f64 = 0.07;
/// How far forward of the toes joint the toes capsule reaches, metres.
pub const TOES_REACH: f64 = 0.04;
/// How far forward of the ankle a toe-less foot's axis reaches, as a fraction of the shin (knee to ankle). From
/// anthropometric ratios: the foot is about 0.15 of stature long, the ball of the foot sits about 0.7 of the way along it,
/// and the shin is about 0.25 of stature, so 0.15 x 0.7 / 0.25 = 0.42. VRM's humanoid has no heel or ball joint, so the
/// forward direction is +Z, as the toes use.
pub const FOOT_BALL_RATIO: f64 = 0.42;
/// How far above the head joint the head capsule reaches when the crown is not given ([`SegmentSet::with_crown`]), metres:
/// a realistic head.
pub const HEAD_REACH: f64 = 0.2;
/// A bone with nothing below it (a fingertip, an eye, the jaw) reaches this fraction of its parent's length further along
/// the parent's direction.
pub const TIP_RATIO: f64 = 0.8;

/// The shape of the falloff of [`SegmentSet::capsule_weights`]: a bone pulls a point with `1 / (distance + softening)^power`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CapsuleParams {
    /// The exponent `k`. Larger hands a vertex over between bones more sharply.
    pub power: f64,
    /// The softening `e`, metres. It keeps the weight finite on the axis and sets how wide a joint's blend is.
    pub softening: f64,
}

impl CapsuleParams {
    /// The prototype's `k = 4`, `e = 0.012`.
    pub const PROTOTYPE: Self = Self { power: 4.0, softening: 0.012 };

    /// Parameters with a positive, finite `power` and `softening`.
    ///
    /// # Errors
    /// [`KitError::NonFinite`] for NaN or infinity, [`KitError::Degenerate`] for zero or less.
    pub fn new(power: f64, softening: f64) -> Result<Self, KitError> {
        for (what, v) in [("capsule power", power), ("capsule softening", softening)] {
            if !v.is_finite() {
                return Err(KitError::NonFinite { what });
            }
            if v <= 0.0 {
                return Err(KitError::Degenerate { what });
            }
        }
        Ok(Self { power, softening })
    }

    /// The pull of a bone whose axis is `distance` away.
    #[allow(clippy::cast_possible_truncation)] // a saturated cast only fails the equality below and takes the `powf` path
    fn weight(self, distance: f64) -> f64 {
        let base = distance + self.softening;
        // A whole exponent (the prototype's 4) is a few multiplications; `powi` loops and `powf` costs several times
        // that, and this runs once per vertex per candidate bone.
        let whole = self.power as i32;
        #[allow(clippy::float_cmp)] // exact on purpose: is the exponent a whole number?
        let is_whole = f64::from(whole) == self.power;
        match (is_whole, whole) {
            (true, 1) => 1.0 / base,
            (true, 2) => 1.0 / (base * base),
            (true, 3) => 1.0 / (base * base * base),
            (true, 4) => {
                let square = base * base;
                1.0 / (square * square)
            }
            (true, _) => 1.0 / base.powi(whole),
            (false, _) => 1.0 / base.powf(self.power),
        }
    }
}

impl Default for CapsuleParams {
    fn default() -> Self {
        Self::PROTOTYPE
    }
}

/// A segment with its direction and reciprocal squared length worked out once, for the per-vertex loop.
#[derive(Copy, Clone)]
struct Axis {
    head: DVec3,
    along: DVec3,
    inv_len2: f64,
}

impl Axis {
    fn new(head: DVec3, tail: DVec3) -> Self {
        let along = tail - head;
        let len2 = along.length_squared();
        // A zero-length segment is a point: every parameter is 0 (the prototype divides by zero here and gets NaN).
        Self { head, along, inv_len2: if len2 > 0.0 { 1.0 / len2 } else { 0.0 } }
    }

    /// The prototype's `segDist`: the distance from `p` to the segment and the `0..=1` parameter of the nearest point.
    fn distance(self, p: DVec3) -> (f64, f64) {
        let rel = p - self.head;
        let t = clamp(rel.dot(self.along) * self.inv_len2, 0.0, 1.0);
        ((rel - self.along * t).length(), t)
    }
}

/// One bone's capsule axis, in bind space.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// The bone.
    pub joint: JointRef,
    /// Where the axis starts: the bone's joint.
    pub head: DVec3,
    /// Where the axis ends: the child's joint, or the terminal rule's point.
    pub tail: DVec3,
}

impl Segment {
    /// The prototype's `segDist`: the distance from `p` to the segment, and the parameter `0..=1` of the nearest point
    /// along it (0 at `head`).
    pub fn distance(&self, p: DVec3) -> (f64, f64) {
        Axis::new(self.head, self.tail).distance(p)
    }
}

/// The capsule axes of a body's bones: the prototype's `SEG` table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegmentSet {
    segments: Vec<Segment>,
}

impl SegmentSet {
    /// The axes of `bones` from the body's joint positions. Each humanoid bone runs from its joint to its child's:
    /// the first child the fit knows, so a body without `chest` or `upperChest` skips them. The terminal rules, which
    /// generalise the prototype's:
    ///
    /// - the **hand** runs to the middle finger's knuckle joint (the first of middle, index, ring, little the fit
    ///   knows), or without fingers [`HAND_REACH`] along the forearm;
    /// - the **toes** run [`TOES_REACH`] forward (+Z); a foot without toes runs forward for [`FOOT_BALL_RATIO`] of its shin;
    /// - the **head** runs [`HEAD_REACH`] up; give the real crown with [`with_crown`](Self::with_crown);
    /// - any other bone with nothing below it (a fingertip, an eye, the jaw) continues its parent's direction for
    ///   [`TIP_RATIO`] of the parent's length.
    ///
    /// A bone the fit does not know is left out (a template may have no toes), and so are extra (named) bones, which have
    /// no place in the humanoid hierarchy: add them with [`with_segment`](Self::with_segment).
    ///
    /// # Errors
    /// [`KitError::NonFinite`] if the fit gives a joint that is NaN or infinite.
    pub fn from_fit(fit: &dyn BodyFit, bones: &[JointRef]) -> Result<Self, KitError> {
        let mut set = Self::default();
        for joint in bones {
            let JointRef::Humanoid(bone) = joint else { continue };
            let Some(head) = joint_at(fit, *bone)? else { continue };
            let tail = match next_joint(fit, *bone)? {
                Some(next) => next,
                None => terminal_tail(fit, *bone, head)?,
            };
            set = set.with_segment(Segment { joint: joint.clone(), head, tail });
        }
        Ok(set)
    }

    /// Adds `segment`, replacing the one already there for its joint.
    #[must_use]
    pub fn with_segment(mut self, segment: Segment) -> Self {
        match self.segments.iter_mut().find(|s| s.joint == segment.joint) {
            Some(slot) => *slot = segment,
            None => self.segments.push(segment),
        }
        self
    }

    /// Ends the head's axis at the crown (the top of the head surface). Without a head axis, the set is unchanged.
    #[must_use]
    pub fn with_crown(mut self, crown: DVec3) -> Self {
        if let Some(head) = self.segments.iter_mut().find(|s| s.joint == JointRef::HEAD) {
            head.tail = crown;
        }
        self
    }

    /// The axes, in the order their bones were given.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The axis of `joint`, if the set has one.
    pub fn get(&self, joint: &JointRef) -> Option<&Segment> {
        self.segments.iter().find(|s| s.joint == *joint)
    }

    /// The prototype's `capsuleWeights`: the raw, unnormalised pull of each of `bones` on `p`. A bone without an axis in
    /// the set is skipped, and a bone listed twice counts once.
    pub fn capsule_weights(&self, p: DVec3, bones: &[JointRef], params: CapsuleParams) -> Vec<(JointRef, f64)> {
        bones
            .iter()
            .enumerate()
            .filter(|(i, bone)| !bones[..*i].contains(bone))
            .filter_map(|(_, bone)| {
                let segment = self.get(bone)?;
                Some((bone.clone(), params.weight(segment.distance(p).0)))
            })
            .collect()
    }

    /// The finished weights of `p`: the four heaviest of [`capsule_weights`](Self::capsule_weights), normalised (all on
    /// the hips if nothing pulls). Ready for a `Skin::Per` closure.
    ///
    /// Selection uses the same [`Top4`] over the same f64 pulls as [`skin_mesh`](Self::skin_mesh), so the two paths
    /// keep the same bones even when candidates tie once rounded to f32. Only the kept weights are cast to f32.
    pub fn weights_at(&self, p: DVec3, bones: &[JointRef], params: CapsuleParams) -> Vec<(JointRef, f32)> {
        let pulls = self.capsule_weights(p, bones, params);
        let mut top = Top4::default();
        for (i, (_, weight)) in pulls.iter().enumerate() {
            if let Ok(index) = u16::try_from(i) {
                top.offer(index, *weight);
            }
        }
        let Some((candidates, normalised)) = top.finish() else {
            return vec![(JointRef::HIPS, 1.0)];
        };
        candidates
            .iter()
            .zip(&normalised)
            .filter(|(_, weight)| **weight > 0.0)
            .map(|(&candidate, &weight)| (pulls[usize::from(candidate)].0.clone(), weight))
            .collect()
    }

    /// Replaces the skin of `mesh` with the capsule weights of its vertices over `bones`: the same weights
    /// [`weights_at`](Self::weights_at) gives each vertex, without a list per vertex. The mesh's joint list becomes the
    /// joints used, in first-use order.
    ///
    /// # Errors
    /// [`KitError::TooMany`] for more than 65 536 `bones`.
    pub fn skin_mesh(&self, mesh: &mut PartMesh, bones: &[JointRef], params: CapsuleParams) -> Result<(), KitError> {
        if bones.len() > usize::from(u16::MAX) + 1 {
            return Err(KitError::TooMany { what: "skin joints", max: usize::from(u16::MAX) + 1 });
        }
        // `bones.len()` is at most 65 536, so every index fits a u16.
        let axes: Vec<(u16, Axis)> = bones
            .iter()
            .enumerate()
            .filter(|(i, bone)| !bones[..*i].contains(bone))
            .filter_map(|(i, bone)| {
                let segment = self.get(bone)?;
                Some((u16::try_from(i).ok()?, Axis::new(segment.head, segment.tail)))
            })
            .collect();
        let mut joint_bones: Vec<JointRef> = Vec::new();
        let mut slot_of: Vec<Option<u16>> = vec![None; bones.len()];
        let (mut joints, mut weights) = (Vec::new(), Vec::new());
        for position in &mesh.geometry.positions {
            let p = position.as_dvec3();
            let mut top = Top4::default();
            for &(index, axis) in &axes {
                top.offer(index, params.weight(axis.distance(p).0));
            }
            let (mut vertex_joints, mut vertex_weights) = ([0_u16; 4], [0.0_f32; 4]);
            if let Some((candidates, normalised)) = top.finish() {
                for (slot, (&candidate, &weight)) in candidates.iter().zip(&normalised).enumerate() {
                    if weight > 0.0 {
                        let cached = &mut slot_of[usize::from(candidate)];
                        let index = match *cached {
                            Some(index) => index,
                            None => *cached.insert(joint_slot(&mut joint_bones, &bones[usize::from(candidate)])?),
                        };
                        (vertex_joints[slot], vertex_weights[slot]) = (index, weight);
                    }
                }
            } else {
                vertex_joints[0] = joint_slot(&mut joint_bones, &JointRef::HIPS)?;
                vertex_weights[0] = 1.0;
            }
            joints.push(vertex_joints);
            weights.push(vertex_weights);
        }
        mesh.joint_bones = joint_bones;
        mesh.geometry.joints = joints;
        mesh.geometry.weights = weights;
        Ok(())
    }
}

/// The index of `joint` in a joint list, adding it on first use.
fn joint_slot(joint_bones: &mut Vec<JointRef>, joint: &JointRef) -> Result<u16, KitError> {
    let at = joint_bones.iter().position(|j| j == joint).unwrap_or_else(|| {
        joint_bones.push(joint.clone());
        joint_bones.len() - 1
    });
    u16::try_from(at).map_err(|_| KitError::TooMany { what: "skin joints", max: usize::from(u16::MAX) + 1 })
}

/// The heaviest positive pulls seen so far, heaviest first, ties in the order offered: the prototype's stable
/// `sort(...).slice(0, 4)` without sorting every candidate. It holds a fifth so [`finish`](Self::finish) can see a tie at
/// the cut ([`influence_cut`]).
#[derive(Default)]
struct Top4 {
    best: [(f64, u16); MAX_INFLUENCES + 1],
    len: usize,
}

impl Top4 {
    fn offer(&mut self, index: u16, weight: f64) {
        if !(weight.is_finite() && weight > 0.0) {
            return;
        }
        let at = self.best[..self.len].iter().position(|&(w, _)| w < weight).unwrap_or(self.len);
        if at > MAX_INFLUENCES {
            return;
        }
        let end = self.len.min(MAX_INFLUENCES);
        self.best.copy_within(at..end, at + 1);
        self.best[at] = (weight, index);
        self.len = (self.len + 1).min(MAX_INFLUENCES + 1);
    }

    /// The candidate indices and their weights scaled to sum to 1 (unused slots 0), or `None` if nothing pulled.
    #[allow(clippy::cast_possible_truncation)] // normalised weights are in 0..=1; the mesh stores f32
    fn finish(&self) -> Option<([u16; 4], [f32; 4])> {
        let held = &self.best[..self.len];
        let kept = &held[..influence_cut(held, MAX_INFLUENCES, |&(w, _)| w)];
        if kept.is_empty() {
            return None;
        }
        let total: f64 = kept.iter().map(|(w, _)| w).sum();
        let (mut candidates, mut weights) = ([0_u16; 4], [0.0_f32; 4]);
        for (slot, &(w, index)) in kept.iter().enumerate() {
            (candidates[slot], weights[slot]) = (index, (w / total) as f32);
        }
        Some((candidates, weights))
    }
}

// ---------------------------------------------------------------------------------------------------- the humanoid tree

/// The bind-space position of a humanoid bone's joint, if the fit knows it.
fn joint_at(fit: &dyn BodyFit, bone: HumanoidBone) -> Result<Option<DVec3>, KitError> {
    match fit.joint(&JointRef::Humanoid(bone)) {
        Some(at) if at.is_finite() => Ok(Some(at.as_dvec3())),
        Some(_) => Err(KitError::NonFinite { what: "fit joint" }),
        None => Ok(None),
    }
}

/// The bones directly below `bone`, in the humanoid order (the order of [`HumanoidBone::ALL`]).
fn children(bone: HumanoidBone) -> impl Iterator<Item = HumanoidBone> {
    HumanoidBone::ALL.into_iter().filter(move |b| b.parent() == Some(bone))
}

/// The hand's five finger chains, thumb first and each chain from the knuckle outward, or nothing for a bone that is not
/// a hand. Thumbs have a metacarpal, a proximal and a distal bone; the four fingers a proximal, an intermediate and a
/// distal one: 15 bones a hand, 30 for two.
pub fn finger_chains(hand: HumanoidBone) -> Vec<Vec<HumanoidBone>> {
    if !matches!(hand, HumanoidBone::LeftHand | HumanoidBone::RightHand) {
        return Vec::new();
    }
    children(hand).map(|root| successors(Some(root), |b| children(*b).next()).collect()).collect()
}

/// The joint a bone's axis ends at: the first joint the fit knows below it, looking at the bone's children in order and
/// then, for children the fit lacks, theirs. `None` if nothing below is known. The head's end is the crown (not a
/// joint), and the hand's is a knuckle.
fn next_joint(fit: &dyn BodyFit, bone: HumanoidBone) -> Result<Option<DVec3>, KitError> {
    if bone == HumanoidBone::Head {
        return Ok(None);
    }
    if matches!(bone, HumanoidBone::LeftHand | HumanoidBone::RightHand) {
        // A palm ends at a knuckle: middle, then index, ring and little (chains 2, 1, 3 and 4). The thumb's root sits at the
        // wrist, so it never ends the palm.
        let chains = finger_chains(bone);
        for chain in [2, 1, 3, 4] {
            if let Some(joint) =
                chains.get(chain).and_then(|c| c.first()).map(|b| joint_at(fit, *b)).transpose()?.flatten()
            {
                return Ok(Some(joint));
            }
        }
        return Ok(None);
    }
    let mut level = vec![bone];
    while !level.is_empty() {
        let mut below = Vec::new();
        for parent in level {
            for child in children(parent) {
                if let Some(joint) = joint_at(fit, child)? {
                    return Ok(Some(joint));
                }
                below.push(child);
            }
        }
        level = below;
    }
    Ok(None)
}

/// Where a bone with no joint below it ends (the terminal rules of [`SegmentSet::from_fit`]).
fn terminal_tail(fit: &dyn BodyFit, bone: HumanoidBone, head: DVec3) -> Result<DVec3, KitError> {
    let toward_parent = || -> Result<Option<DVec3>, KitError> {
        for ancestor in successors(bone.parent(), |b| b.parent()) {
            if let Some(at) = joint_at(fit, ancestor)? {
                return Ok(Some(head - at));
            }
        }
        Ok(None)
    };
    Ok(match bone {
        HumanoidBone::Head => head + DVec3::Y * HEAD_REACH,
        HumanoidBone::LeftToes | HumanoidBone::RightToes => head + DVec3::Z * TOES_REACH,
        // Without toes the foot still runs forward, as the toes would. Following the shin would point it down through the
        // floor. The fit has no heel or ball joint to aim from, so forward is the +Z the toes use.
        HumanoidBone::LeftFoot | HumanoidBone::RightFoot => {
            head + DVec3::Z * toward_parent()?.map_or(0.0, |shin| FOOT_BALL_RATIO * shin.length())
        }
        HumanoidBone::LeftHand | HumanoidBone::RightHand => {
            let forearm = toward_parent()?.and_then(DVec3::try_normalize).unwrap_or(DVec3::NEG_Y);
            head + forearm * HAND_REACH
        }
        _ => head + toward_parent()?.map_or(DVec3::ZERO, |along| along * TIP_RATIO),
    })
}

// ------------------------------------------------------------------------------------------------------ joint blends

/// A hand-over between two joints along one coordinate: the prototype's `sstep` blends (a thigh handing over to the hip,
/// a sleeve to the shoulder).
#[derive(Clone, Debug, PartialEq)]
pub struct JointBlend {
    a: JointRef,
    b: JointRef,
    from: f64,
    to: f64,
}

/// A blend from `a` to `b` as a coordinate runs from `y0` to `y1`: all `a` at `y0` and before, all `b` at `y1` and after,
/// a smoothstep between. `y0 > y1` is a falling ramp, so `blend_between(hips, upper_leg, 0.425, 0.335)` is hips above
/// 0.425 m and the thigh below 0.335 m.
pub fn blend_between(a: JointRef, b: JointRef, y0: f64, y1: f64) -> JointBlend {
    JointBlend { a, b, from: y0, to: y1 }
}

impl JointBlend {
    /// The share of `b` at `y`, `0..=1`.
    pub fn amount(&self, y: f64) -> f64 {
        sstep(self.from, self.to, y)
    }

    /// The two joints' weights at `y`: `a` gets what `b` does not. A weight of 0 is kept, so [`limit_influences`] drops it.
    #[allow(clippy::cast_possible_truncation)] // a share is in 0..=1; the mesh stores f32
    pub fn at(&self, y: f64) -> [(JointRef, f32); 2] {
        let share = self.amount(y);
        [(self.a.clone(), (1.0 - share) as f32), (self.b.clone(), share as f32)]
    }
}

// ---------------------------------------------------------------------------------------------------- the top-four rule

/// The prototype's `setWeights` selection: drops weights that are zero, negative or not finite, adds up repeats of a joint,
/// and keeps the `max` heaviest, heaviest first (ties keep their given order, except that a tie at the cut drops the whole tied group: [`influence_cut`]). Does not normalise: see [`normalise`].
pub fn limit_influences(blend: &mut Vec<(JointRef, f32)>, max: usize) {
    let mut merged: Vec<(JointRef, f32)> = Vec::with_capacity(blend.len());
    for (joint, weight) in blend.drain(..) {
        if !(weight.is_finite() && weight > 0.0) {
            continue;
        }
        match merged.iter_mut().find(|(j, _)| *j == joint) {
            Some(entry) => entry.1 += weight,
            None => merged.push((joint, weight)),
        }
    }
    merged.sort_by(|a, b| b.1.total_cmp(&a.1)); // stable: ties keep their order
    let kept = influence_cut(&merged, max, |entry| f64::from(entry.1));
    merged.truncate(kept);
    *blend = merged;
}

/// Scales the weights to sum to 1. A blend with nothing usable (empty, or no positive sum) becomes all hips, as in the
/// prototype.
pub fn normalise(blend: &mut Vec<(JointRef, f32)>) {
    let total: f32 = blend.iter().map(|(_, w)| w).sum();
    if blend.is_empty() || !(total.is_finite() && total > 0.0) {
        *blend = vec![(JointRef::HIPS, 1.0)];
        return;
    }
    for (_, w) in blend.iter_mut() {
        *w /= total;
    }
}

// ------------------------------------------------------------------------------------------------ editing a skinned mesh

/// One vertex's skin: four joint indices and their weights.
type Skin = ([u16; 4], [f32; 4]);

/// The total weight a skin gives `joint`.
fn weight_on(skin: &Skin, joint: u16) -> f32 {
    skin.0.iter().zip(&skin.1).filter(|(j, w)| **j == joint && **w > 0.0).map(|(_, w)| w).sum()
}

/// How far apart two skins are: the largest difference in any one joint's weight.
fn jump(a: &Skin, b: &Skin) -> f32 {
    let from = |x: &Skin, y: &Skin| {
        x.0.iter()
            .zip(&x.1)
            .filter(|(_, w)| **w > 0.0)
            .map(|(j, _)| (weight_on(x, *j) - weight_on(y, *j)).abs())
            .fold(0.0, f32::max)
    };
    from(a, b).max(from(b, a))
}

/// The biggest jump in skin weights across one edge of the mesh: for each triangle edge, the largest difference in any
/// one joint's weight between its two vertices. 0 for a rigid mesh. Skin seams should have none, and a good skin has no
/// edge near 1 except where it means to.
pub fn max_neighbour_jump(mesh: &PartMesh) -> f32 {
    let g = &mesh.geometry;
    if !g.is_skinned() || g.joints.len() != g.weights.len() {
        return 0.0;
    }
    let skin = |v: u32| g.joints.get(v as usize).copied().zip(g.weights.get(v as usize).copied());
    let mut worst = 0.0_f32;
    for (a, b) in triangle_edges(&g.indices) {
        if let (Some(x), Some(y)) = (skin(a), skin(b)) {
            worst = worst.max(jump(&x, &y));
        }
    }
    worst
}

/// The three edges of every whole triangle, as vertex index pairs (an edge shared by two triangles appears twice).
fn triangle_edges(indices: &[u32]) -> impl Iterator<Item = (u32, u32)> + '_ {
    indices.as_chunks::<3>().0.iter().flat_map(|&[a, b, c]| [(a, b), (b, c), (c, a)])
}

/// Merges repeats, keeps the four heaviest (ties keep the lower joint index, but a tie at the cut drops the whole tied group, see [`influence_cut`]) and normalises: a skin from loose entries.
fn pack(entries: &mut Vec<(u16, f32)>) -> Skin {
    entries.sort_by_key(|&(joint, _)| joint);
    entries.dedup_by(|later, earlier| {
        let same = later.0 == earlier.0;
        if same {
            earlier.1 += later.1;
        }
        same
    });
    entries.retain(|&(_, w)| w > 0.0);
    entries.sort_by(|a, b| b.1.total_cmp(&a.1)); // stable over the joint order above
    let kept = influence_cut(entries, MAX_INFLUENCES, |entry| f64::from(entry.1));
    entries.truncate(kept);
    let total: f32 = entries.iter().map(|(_, w)| w).sum();
    if entries.is_empty() {
        return ([0; 4], [1.0, 0.0, 0.0, 0.0]); // nothing positive to keep: never produced for a skin that has weights
    }
    let (mut joints, mut weights) = ([0_u16; 4], [0.0_f32; 4]);
    for (slot, &(joint, w)) in entries.iter().enumerate() {
        (joints[slot], weights[slot]) = (joint, w / total);
    }
    (joints, weights)
}

/// The mesh with vertices that share a position (a grid or texture seam) merged into one node, and the node-to-node edges
/// of its triangles.
struct WeldedGraph {
    /// The node of each vertex.
    node_of: Vec<usize>,
    /// The first vertex of each node.
    first: Vec<usize>,
    /// `neighbours[offsets[n]..offsets[n + 1]]` are the nodes sharing an edge with node `n`, in ascending order.
    offsets: Vec<usize>,
    neighbours: Vec<usize>,
}

impl WeldedGraph {
    fn new(positions: &[Vec3], indices: &[u32]) -> Self {
        let mut by_position: HashMap<[u32; 3], usize> = HashMap::with_capacity(positions.len());
        let (mut node_of, mut first) = (Vec::with_capacity(positions.len()), Vec::new());
        for (vertex, p) in positions.iter().enumerate() {
            // `+ 0.0` turns -0.0 into 0.0, so a mirrored seam on the centre plane still matches its twin.
            let key = p.to_array().map(|c| (c + 0.0).to_bits());
            let node = *by_position.entry(key).or_insert_with(|| {
                first.push(vertex);
                first.len() - 1
            });
            node_of.push(node);
        }
        let mut edges: Vec<(usize, usize)> = Vec::with_capacity(indices.len() * 2);
        for (a, b) in triangle_edges(indices) {
            if let (Some(&a), Some(&b)) = (node_of.get(a as usize), node_of.get(b as usize))
                && a != b
            {
                edges.extend([(a, b), (b, a)]);
            }
        }
        edges.sort_unstable();
        edges.dedup();
        let mut offsets = vec![0; first.len() + 1];
        for &(from, _) in &edges {
            offsets[from + 1] += 1;
        }
        for n in 0..first.len() {
            offsets[n + 1] += offsets[n];
        }
        Self { node_of, first, offsets, neighbours: edges.into_iter().map(|(_, to)| to).collect() }
    }

    fn of(&self, node: usize) -> &[usize] {
        &self.neighbours[self.offsets[node]..self.offsets[node + 1]]
    }
}

/// The share of the way to the neighbours' mean that [`smooth`] tries first, then half and a quarter of it.
const SMOOTH_STEPS: [f32; 3] = [0.5, 0.25, 0.125];

/// Smooths a skinned mesh's weights: `iterations` passes, each moving every vertex part of the way to the mean of its
/// neighbours (the vertices it shares a triangle edge with), then keeping the four heaviest influences.
///
/// It never sharpens: a vertex only moves if every edge it touches still jumps no more than the mesh's biggest jump
/// before smoothing ([`max_neighbour_jump`]); if half a step would break that it tries a quarter and an eighth, then
/// stays. So an irregular mesh, where plain averaging can steepen an edge, still ends no more abrupt than it began.
/// Vertices at one position (a seam) are one node and end with the same weights. Joints are never added: a vertex only
/// takes up joints its neighbours already use. A rigid mesh is left alone.
pub fn smooth(mesh: &mut PartMesh, iterations: u32) {
    let g = &mut mesh.geometry;
    if !g.is_skinned() || iterations == 0 || g.joints.len() != g.positions.len() || g.weights.len() != g.positions.len()
    {
        return;
    }
    let graph = WeldedGraph::new(&g.positions, &g.indices);
    let mut state: Vec<Skin> = graph.first.iter().map(|&v| (g.joints[v], g.weights[v])).collect();
    let bound = (0..state.len())
        .flat_map(|n| graph.of(n).iter().map(move |&m| (n, m)))
        .map(|(n, m)| jump(&state[n], &state[m]))
        .fold(0.0, f32::max);
    let joint_count = g.joints.iter().flatten().max().map_or(0, |&j| usize::from(j) + 1);
    let mut mean = vec![0.0_f32; joint_count];
    let mut touched: Vec<u16> = Vec::new();
    let mut entries: Vec<(u16, f32)> = Vec::new();
    for _ in 0..iterations {
        for node in 0..state.len() {
            let around = graph.of(node);
            if around.is_empty() {
                continue;
            }
            for joint in touched.drain(..) {
                mean[usize::from(joint)] = 0.0;
            }
            #[allow(clippy::cast_precision_loss)] // a vertex has a handful of neighbours
            let share = 1.0 / around.len() as f32;
            for &other in around {
                for (&joint, &w) in state[other].0.iter().zip(&state[other].1).filter(|(_, w)| **w > 0.0) {
                    if mean[usize::from(joint)] == 0.0 {
                        touched.push(joint);
                    }
                    mean[usize::from(joint)] += w * share;
                }
            }
            for step in SMOOTH_STEPS {
                entries.clear();
                let own = &state[node];
                entries.extend(own.0.iter().zip(&own.1).map(|(&j, &w)| (j, w * (1.0 - step))));
                entries.extend(touched.iter().map(|&j| (j, mean[usize::from(j)] * step)));
                let proposal = pack(&mut entries);
                if around.iter().all(|&other| jump(&proposal, &state[other]) <= bound) {
                    state[node] = proposal;
                    break;
                }
            }
        }
    }
    for (vertex, &node) in graph.node_of.iter().enumerate() {
        (g.joints[vertex], g.weights[vertex]) = state[node];
    }
}

/// Which half of the body keeps its weights when mirroring.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// The character's left, +X: its weights are copied to the right.
    Left,
    /// The character's right, -X: its weights are copied to the left.
    Right,
}

/// A mirror in the plane x = 0 ([`mirror`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Symmetry {
    /// The half whose weights are kept and copied across.
    pub source: Side,
    /// How far, in metres, a vertex may be from its mirror image's position and still be its partner. Also the half-width
    /// of the centre strip that counts as on the plane.
    pub tolerance: f32,
}

impl Symmetry {
    /// The usual tolerance: 0.1 mm, far finer than any mesh the kit builds.
    pub const TOLERANCE: f32 = 1e-4;

    /// Mirror from `source` with the usual tolerance.
    pub const fn from_side(source: Side) -> Self {
        Self { source, tolerance: Self::TOLERANCE }
    }
}

/// What [`mirror`] did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorReport {
    /// Vertices on the target half that took their partner's weights.
    pub mirrored: usize,
    /// Vertices on the plane that were made symmetric.
    pub centred: usize,
    /// Vertices on the target half with no partner within the tolerance (or no room for a mirrored joint): left alone.
    pub unmatched: usize,
}

/// A grid of the mesh's vertices by position, for finding a vertex near a point.
struct SpatialIndex<'a> {
    positions: &'a [Vec3],
    tolerance: f32,
    cells: HashMap<[i32; 3], Vec<usize>>,
}

impl<'a> SpatialIndex<'a> {
    #[allow(clippy::cast_possible_truncation)] // a cell index far outside i32 means a point far outside any character
    fn cell(p: Vec3, size: f32) -> [i32; 3] {
        p.to_array().map(|c| (c / size).floor() as i32)
    }

    fn new(positions: &'a [Vec3], tolerance: f32) -> Self {
        let mut cells: HashMap<[i32; 3], Vec<usize>> = HashMap::new();
        for (vertex, &p) in positions.iter().enumerate() {
            cells.entry(Self::cell(p, tolerance)).or_default().push(vertex);
        }
        Self { positions, tolerance, cells }
    }

    /// The vertex nearest `q` within the tolerance; the lowest index among equals.
    fn nearest(&self, q: Vec3) -> Option<usize> {
        let [cx, cy, cz] = Self::cell(q, self.tolerance);
        let mut best: Option<(f32, usize)> = None;
        for key in (-1..=1).flat_map(|dx| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dz| [dx, dy, dz]))) {
            let cell = [cx.saturating_add(key[0]), cy.saturating_add(key[1]), cz.saturating_add(key[2])];
            for &vertex in self.cells.get(&cell).into_iter().flatten() {
                let d = self.positions[vertex].distance(q);
                if d <= self.tolerance && best.is_none_or(|(bd, bv)| d.total_cmp(&bd).then(vertex.cmp(&bv)).is_lt()) {
                    best = Some((d, vertex));
                }
            }
        }
        best.map(|(_, vertex)| vertex)
    }
}

/// Finds, and adds when missing, the mirror image of each joint a mesh uses.
struct JointSwap<'a> {
    joint_bones: &'a mut Vec<JointRef>,
    known: HashMap<u16, u16>,
}

impl JointSwap<'_> {
    fn of(&mut self, joint: u16) -> Option<u16> {
        if let Some(&mirrored) = self.known.get(&joint) {
            return Some(mirrored);
        }
        let image = mirror_joint(self.joint_bones.get(usize::from(joint))?);
        let at = self.joint_bones.iter().position(|j| *j == image).unwrap_or_else(|| {
            self.joint_bones.push(image);
            self.joint_bones.len() - 1
        });
        let mirrored = u16::try_from(at).ok()?;
        self.known.insert(joint, mirrored);
        Some(mirrored)
    }

    /// `skin` with every joint replaced by its opposite-side joint.
    fn skin(&mut self, skin: &Skin) -> Option<Skin> {
        let mut out = *skin;
        for (joint, w) in out.0.iter_mut().zip(&skin.1) {
            if *w > 0.0 {
                *joint = self.of(*joint)?;
            }
        }
        Some(out)
    }
}

/// Mirrors a skinned mesh's weights across the plane x = 0: each vertex on the target half takes the weights of the
/// vertex at its mirror position on the source half, with every joint swapped for its opposite (`leftUpperArm` for
/// `rightUpperArm`, `hair_l_2` for `hair_r_2`; joints the mesh did not yet list are added). Vertices on the plane become
/// symmetric: half of their own weights, half of the swapped ones. Source-half vertices are unchanged. A rigid mesh is left
/// alone.
///
/// # Errors
/// [`KitError::NonFinite`] or [`KitError::Degenerate`] if the tolerance is not a positive, finite number.
pub fn mirror(mesh: &mut PartMesh, symmetry: Symmetry) -> Result<MirrorReport, KitError> {
    let tolerance = symmetry.tolerance;
    if !tolerance.is_finite() {
        return Err(KitError::NonFinite { what: "mirror tolerance" });
    }
    if tolerance <= 0.0 {
        return Err(KitError::Degenerate { what: "mirror tolerance" });
    }
    let PartMesh { geometry: g, joint_bones, .. } = mesh;
    let mut report = MirrorReport::default();
    if !g.is_skinned() || g.joints.len() != g.positions.len() || g.weights.len() != g.positions.len() {
        return Ok(report);
    }
    let index = SpatialIndex::new(&g.positions, tolerance);
    let mut swap = JointSwap { joint_bones, known: HashMap::new() };
    let sign = if symmetry.source == Side::Left { 1.0 } else { -1.0 };
    for vertex in 0..g.positions.len() {
        let p = g.positions[vertex];
        let side = p.x * sign; // positive on the source half
        let own = (g.joints[vertex], g.weights[vertex]);
        if side.abs() <= tolerance {
            let Some(image) = swap.skin(&own) else {
                report.unmatched += 1;
                continue;
            };
            let mut entries: Vec<(u16, f32)> = Vec::with_capacity(2 * MAX_INFLUENCES);
            for skin in [&own, &image] {
                entries.extend(skin.0.iter().zip(&skin.1).map(|(&j, &w)| (j, w * 0.5)));
            }
            (g.joints[vertex], g.weights[vertex]) = pack(&mut entries);
            report.centred += 1;
        } else if side < 0.0 {
            let partner = index.nearest(Vec3::new(-p.x, p.y, p.z)).filter(|&q| g.positions[q].x * sign > tolerance);
            match partner.and_then(|q| swap.skin(&(g.joints[q], g.weights[q]))) {
                Some(skin) => {
                    (g.joints[vertex], g.weights[vertex]) = skin;
                    report.mirrored += 1;
                }
                None => report.unmatched += 1,
            }
        }
    }
    Ok(report)
}

// ----------------------------------------------------------------------------------------------------------- brushes

/// The brush that does to the opposite side of the body what `brush` does to this one: the centre's x negated and the
/// joint swapped for its opposite (`.L` for `.R`). Radius, strength and falloff are the same. A brush on the centre plane
/// with a centre-line joint is its own mirror, which the caller can see by comparing the two.
pub fn brush_mirrored(brush: &WeightBrush) -> WeightBrush {
    brush.mirrored()
}

/// The brush for a painted stroke: centred on the middle of the stroke's bounding box, reaching to its farthest point,
/// with the default smooth falloff. Vertices at the middle take `strength` of their weight to `joint` (`strength` is limited
/// to `0..=1`); the strength fades to nothing at the stroke's farthest point.
///
/// # Errors
/// [`KitError::TooFew`] for no points, [`KitError::NonFinite`] for a NaN or infinite coordinate or strength, and
/// [`KitError::Degenerate`] when every point is the same (a brush needs a radius).
pub fn brush_from_stroke(points: &[[f64; 3]], joint: JointRef, strength: f64) -> Result<WeightBrush, KitError> {
    if points.is_empty() {
        return Err(KitError::TooFew { what: "stroke points", need: 1, got: 0 });
    }
    if !strength.is_finite() || points.iter().flatten().any(|c| !c.is_finite()) {
        return Err(KitError::NonFinite { what: "stroke" });
    }
    let corner = |pick: fn(f64, f64) -> f64| {
        points
            .iter()
            .map(|p| DVec3::from_array(*p))
            .reduce(|a, b| DVec3::new(pick(a.x, b.x), pick(a.y, b.y), pick(a.z, b.z)))
    };
    let (Some(low), Some(high)) = (corner(f64::min), corner(f64::max)) else {
        return Err(KitError::TooFew { what: "stroke points", need: 1, got: 0 });
    };
    let centre = (low + high) * 0.5;
    let radius = points.iter().map(|p| DVec3::from_array(*p).distance(centre)).fold(0.0, f64::max);
    if radius <= 0.0 {
        return Err(KitError::Degenerate { what: "stroke extent" });
    }
    Ok(WeightBrush {
        centre: centre.to_array(),
        radius,
        joint,
        strength: strength.clamp(0.0, 1.0),
        falloff: BrushFalloff::default(),
    })
}
