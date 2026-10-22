//! The export bake (#185): built for export (`BuildOptions::rig_export`), the simulated hair has no runtime, so each
//! simulated group that is not a tail hangs on spring-bone chains of its own, which VRM and glTF viewers move.
//!
//! **Chains.** Up to [`STRANDS_PER_GROUP`] clumps of a group, evenly spread over it, each give a chain of
//! [`STRAND_BONES`] bones placed at equal arc lengths along the clump's rest curve, root to tip, named
//! `hair_<part>_<group>_s<k>_<i>` (the rig's convention, as a tail's are). The part skins the group's mesh to them
//! (`bind_to_strands` in [`part`](super::part)): each vertex follows its nearest chain by arc length, blending into the
//! head over the first [`STRAND_ROOT_BAND`], so the rest pose reproduces the built hair exactly.
//!
//! **Springs.** A chain's spring parameters come from what drives the live simulation, so a preset or a group's
//! stiffness reads the same in another app: the spring stiffness from the group's stiffness falloff at the root and
//! the tip, gravity from the physics' `gravity_scale` (the default's equal to a tail's), and drag from the velocity the
//! simulation keeps each step ([`springs`]).

use std::collections::BTreeMap;

use omuzaki_anim_api::SpringParams;
use omuzaki_char_api::{BoneName, Falloff, GroupId, HairParams, HairPhysics, PartId};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::narrow;

use super::compile::CompiledEntry;
use super::r#gen::tail::Chain;
use super::part::chain_prefix;

/// The most strand chains an exported group gets: enough for the curtain to swing in parts, few enough for a VRM
/// viewer's spring-bone budget.
pub const STRANDS_PER_GROUP: usize = 8;
/// Bones along each strand chain, root to tip.
pub const STRAND_BONES: usize = 5;
/// Length from a strand's root over which its locks blend from the head to the chain, metres.
pub const STRAND_ROOT_BAND: f32 = 0.03;

/// The default physics' gravity scale, which maps to a tail's gravity power.
const GRAVITY_SCALE: f32 = 0.35;
/// A tail chain's gravity power at the root and the tip, at the default gravity scale.
const GRAVITY_POWER: (f32, f32) = (0.3, 0.5);
/// Spring stiffness for a falloff of 0 (free) and 1 (rigid).
const STIFFNESS: (f32, f32) = (0.4, 1.0);
/// The collision radius of every strand joint, metres.
const HIT_RADIUS: f32 = 0.02;
/// Drag per unit of velocity lost each step, and its range.
const DRAG_PER_LOSS: f32 = 10.0;
const DRAG: (f32, f32) = (0.05, 0.9);

/// The strand chains of the simulated groups of `params` that are not tails (`tails` holds those, which have chains of
/// their own); see the module docs.
///
/// # Errors
/// A message naming a bone name that isn't valid.
pub fn strand_chains(
    part: &PartId,
    params: &HairParams,
    entries: &[CompiledEntry],
    tails: &BTreeMap<GroupId, Vec<Chain>>,
) -> Result<Vec<Chain>, String> {
    let mut out = Vec::new();
    for entry in entries.iter().filter(|e| e.simulate && !tails.contains_key(&e.id)) {
        let stiffness = params.groups.get(&entry.id).map(|g| g.dynamics.stiffness).unwrap_or_default();
        let springs = springs(&stiffness, &params.physics);
        let clumps: Vec<_> = entry.group.clumps.iter().filter(|c| c.simulate && c.rest.len() >= 2).collect();
        let count = clumps.len().min(STRANDS_PER_GROUP);
        for k in 0..count {
            let clump = clumps[if count < 2 { 0 } else { k * (clumps.len() - 1) / (count - 1) }];
            let name = format!("{}_s{k}", chain_prefix(part, &entry.id));
            let bones = (0..STRAND_BONES)
                .map(|i| BoneName::new(format!("{name}_{i}")).map_err(|e| format!("a strand bone name: {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            out.push(Chain {
                name,
                bones,
                rest: by_arc_length(&clump.rest, STRAND_BONES),
                root_band: STRAND_ROOT_BAND,
                springs,
            });
        }
    }
    Ok(out)
}

/// The spring parameters at the root and the tip of a strand chain of a group with `stiffness`, under `physics`.
pub fn springs(stiffness: &Falloff, physics: &HairPhysics) -> [SpringParams; 2] {
    let gravity = narrow(physics.gravity_scale) / GRAVITY_SCALE;
    let drag = ((1.0 - narrow(physics.damping)) * DRAG_PER_LOSS).clamp(DRAG.0, DRAG.1);
    let at = |t: f32, power: f32| SpringParams {
        hit_radius: HIT_RADIUS,
        stiffness: STIFFNESS.0 + (STIFFNESS.1 - STIFFNESS.0) * stiffness.at(t).clamp(0.0, 1.0),
        gravity_power: power * gravity,
        gravity_dir: Vec3::NEG_Y,
        drag_force: drag,
    };
    [at(0.0, GRAVITY_POWER.0), at(1.0, GRAVITY_POWER.1)]
}

/// `n` points (at least 2) at equal arc lengths along the polyline `rest`, its first and last among them.
fn by_arc_length(rest: &[Vec3], n: usize) -> Vec<Vec3> {
    let mut arc = Vec::with_capacity(rest.len());
    let mut total = 0.0f32;
    arc.push(0.0);
    for pair in rest.windows(2) {
        total += pair[0].distance(pair[1]);
        arc.push(total);
    }
    let mut out = Vec::with_capacity(n);
    let mut seg = 0;
    for i in 0..n {
        #[allow(clippy::cast_precision_loss)] // a handful of bones
        let target = total * i as f32 / (n - 1).max(1) as f32;
        while seg + 2 < arc.len() && arc[seg + 1] < target {
            seg += 1;
        }
        let (from, to) = (arc[seg], arc.get(seg + 1).copied().unwrap_or(arc[seg]));
        let along = if to > from { ((target - from) / (to - from)).clamp(0.0, 1.0) } else { 0.0 };
        let next = rest.get(seg + 1).copied().unwrap_or(rest[seg]);
        out.push(rest[seg].lerp(next, along));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bones_sit_at_equal_arc_lengths_from_root_to_tip() {
        let rest = [Vec3::ZERO, Vec3::new(0.0, -0.1, 0.0), Vec3::new(0.0, -0.1, 0.3)];
        let points = by_arc_length(&rest, 5);
        assert_eq!(points[0], rest[0]);
        assert!(points[4].distance(rest[2]) < 1e-6);
        let gaps: Vec<f32> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
        assert!(gaps.iter().all(|g| (g - 0.1).abs() < 1e-5), "{gaps:?}");
    }

    #[test]
    fn the_default_springs_match_a_tails_and_follow_the_physics() {
        let [root, tip] = springs(&Falloff::default(), &HairPhysics::default());
        assert!((root.stiffness - 1.0).abs() < 1e-6 && tip.stiffness < 0.5, "{root:?} {tip:?}");
        assert!((root.gravity_power - 0.3).abs() < 1e-6 && (tip.gravity_power - 0.5).abs() < 1e-6);
        let heavy = HairPhysics { damping: 0.9, gravity_scale: 0.8, ..HairPhysics::default() };
        let [_, heavy_tip] = springs(&Falloff::default(), &heavy);
        assert!(heavy_tip.drag_force > tip.drag_force && heavy_tip.gravity_power > tip.gravity_power);
    }
}
