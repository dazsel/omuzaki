//! Bakes a cloth sheet to VRM spring-bone chains and a skinned mesh (#386), for exports
//! ([`BuildOptions::rig_export`](omuzaki_char_api::BuildOptions)): other apps have no cloth solver, so a garment
//! exports as bones that swing like its sheet.
//!
//! # The chains
//!
//! Every `k`-th column of the runtime's finest sheet becomes a chain and every `m`-th row a bone along it, from the
//! waist row (the chain's root, hanging from the joint the waist is skinned to) down to the hem (its tail), with `k`
//! and `m` chosen to stay within the bone budget ([`SKIRT_BONES`], [`ROBE_BONES`]): as many bones per chain as the
//! sheet has rows, up to [`MAX_CHAIN_BONES`], then as many chains as fit. Bones rest at their grid vertices in bind
//! space with no rotation (the VRM normalised convention), so the baked mesh at rest **is** the garment at rest.
//!
//! # The mesh
//!
//! Each grid vertex is skinned bilinearly to the two chains around it and the two bones above and below it, blended
//! with its own skin weights by its row's follow weight (a waistband stays on the body) and cut to the four largest
//! influences, as glTF holds them.
//!
//! # The parameters
//!
//! VRM's spring joint is a damped pendulum, not a sheet. The parameters are chosen so a chain swings like the sheet's
//! column (`tests/cloth_bake_springs.rs` in `omuzaki-test-char` steps both through a hips turn):
//!
//! - **stiffness** (pull back toward the rest direction) from the fabric's bending stiffness, `sqrt(2e4 / bend)` at the
//!   root (1 for the default fabric), half that at the tip, within `[0.1, 4]`;
//! - **gravity** at full power downward, as the sheet hangs;
//! - **drag** from the fabric's damping plus the constraint losses a pendulum lacks, `0.4 + 0.05 · damping`, within
//!   `[0, 1]`;
//! - **hit radius** the cloth's half-thickness plus 8 mm, so the joints keep off the legs about as the sheet does (a
//!   radius of half the chains' spacing held them 9 cm off the thighs).
//!
//! Measured against the sheet through a 25° hips turn (`omuzaki-test-char`): once settled the chain tails swing with
//! the hem within 1 mm; during the turn a chain, a pendulum hanging from the hips, trails the sheet, which the legs
//! carry round, by up to 5.3 cm.
//!
//! The body colliders of the runtime ([`ColliderSet`](super::collide::ColliderSet)) export as one collider group the
//! chains collide with: shapes on humanoid bones, in each bone's space. What the bake loses is returned as warnings
//! for `BuildReport.warnings`: the coupling between chains (stretch, shear, bend across columns), wind, the layer
//! shell, friction, any weight cut by the four-influence limit, and colliders it could not export.

use omuzaki_anim_api::{
    BoneId, Collider, ColliderGroup, ColliderShape, HumanoidPose, SpringChain, SpringParams, Transform,
};
use omuzaki_char_api::{BodyFit, BoneName, ExtraBone, JointRef, RigContribution, humanoid_id};
use omuzaki_core::glam::Vec3;

use super::collide::ColliderKind;
use super::runtime::{ClothRuntime, ClothRuntimeError};

/// The bone budget of a skirt's export.
pub const SKIRT_BONES: usize = 80;

/// The bone budget of a robe's export.
pub const ROBE_BONES: usize = 120;

/// Most bones in one chain.
pub const MAX_CHAIN_BONES: usize = 10;

/// What a vertex of the baked mesh is skinned to.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BakeTarget {
    /// One of the garment's own skin joints.
    Joint(JointRef),
    /// One of the bake's bones (a provisional id of the [`RigContribution`]).
    Bone(BoneId),
}

/// A vertex's baked skinning: up to four influences summing to 1.
pub type BakedWeights = Vec<(BakeTarget, f32)>;

/// The bake's numbers, for the report card.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BakeReport {
    /// Chains made.
    pub chains: usize,
    /// Bones per chain.
    pub bones_per_chain: usize,
    /// The largest distance between the baked mesh at rest and the garment at rest, metres.
    pub max_rest_deviation: f32,
    /// The largest weight a vertex lost to the four-influence limit (renormalised away).
    pub max_weight_cut: f32,
    /// Colliders exported.
    pub colliders: usize,
}

/// The bake of one cloth garment.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothBake {
    /// The bones, chains and collider group the garment adds to the rig.
    pub rig: RigContribution,
    /// Each grid vertex's skinning.
    pub weights: Vec<BakedWeights>,
    /// The numbers.
    pub report: BakeReport,
    /// What the export loses, for `BuildReport.warnings`.
    pub warnings: Vec<String>,
}

/// The `n` indices picked evenly from `m` (both ends of an open line; the seam's spacing for a wrapped one).
fn pick(n: usize, m: usize, wrap: bool) -> Vec<usize> {
    if wrap || n < 2 {
        (0..n).map(|j| j * m / n.max(1)).collect()
    } else {
        (0..n).map(|j| (j * (m - 1) * 2 + (n - 1)) / (2 * (n - 1))).collect()
    }
}

/// For grid index `g`, the two picks around it and the fraction between them (through the seam when wrapped).
#[allow(clippy::cast_precision_loss)] // grid indices
fn between(picked: &[usize], g: usize, m: usize, wrap: bool) -> (usize, usize, f32) {
    let k = picked.partition_point(|&p| p <= g).saturating_sub(1);
    let (hi, hi_g) = if k + 1 < picked.len() {
        (k + 1, picked[k + 1])
    } else if wrap {
        (0, m + picked[0])
    } else {
        return (k, k, 0.0);
    };
    let lo_g = picked[k];
    (k, hi, if hi_g > lo_g { (g as f32 - lo_g as f32) / (hi_g as f32 - lo_g as f32) } else { 0.0 })
}

/// Bakes `runtime`'s garment, named `part` (bone, chain and group names start `cloth_<part>`), within `budget` bones,
/// with the joints of `fit` (the body under the garment) for the colliders.
///
/// # Errors
/// [`ClothRuntimeError::Spec`] for a budget under 4 bones (two chains of two) or a part name that makes no valid bone
/// name.
#[allow(clippy::too_many_lines)] // one pass: bones, chains, weights, colliders, report
pub fn bake(
    runtime: &ClothRuntime,
    part: &str,
    budget: usize,
    fit: &dyn BodyFit,
) -> Result<ClothBake, ClothRuntimeError> {
    let binding = runtime.binding(0).ok_or(ClothRuntimeError::Spec("a runtime has at least one level"))?;
    let size = binding.size();
    let (grid_rows, grid_cols, wrap) = runtime.grid_shape();
    let per_chain = size.rows.min(MAX_CHAIN_BONES);
    let chains = size.cols.min(budget / per_chain.max(1));
    if per_chain < 2 || chains < 2 {
        return Err(ClothRuntimeError::Spec("a cloth bake needs a budget of at least two chains of two bones"));
    }
    // Chain columns and bone rows, as grid indices.
    let sheet_cols = pick(chains, size.cols, wrap);
    let cols: Vec<usize> = sheet_cols.iter().map(|&c| binding.grid_cols()[c]).collect();
    let sheet_rows = pick(per_chain, size.rows, false);
    let rows: Vec<usize> = sheet_rows.iter().map(|&r| binding.grid_rows()[r]).collect();
    let rest: Vec<Vec3> = runtime.grid_rest().collect();
    let skin = runtime.grid_skin();
    let joints = runtime.joints();

    let mut rig = RigContribution::default();
    let mut bone_ids = vec![vec![BoneId(0); per_chain]; chains];
    let name = |c: usize, r: usize| BoneName::new(format!("cloth_{part}_{c}_{r}"));
    for (c, &gc) in cols.iter().enumerate() {
        // The chain hangs from the joint its root vertex is mostly skinned to.
        let root_vertex = rows[0] * grid_cols + gc;
        let s = &skin[root_vertex];
        let main = s.weights.iter().enumerate().fold(0, |best, (i, &w)| if w > s.weights[best] { i } else { best });
        let mut parent = joints[usize::from(s.joints[main])].clone();
        for (r, &gr) in rows.iter().enumerate() {
            let bone_name =
                name(c, r).map_err(|_| ClothRuntimeError::Spec("the part name makes no valid bone name"))?;
            let id = rig.add_bone(ExtraBone {
                name: bone_name.clone(),
                parent: parent.clone(),
                rest: Transform { translation: rest[gr * grid_cols + gc], ..Transform::IDENTITY },
            });
            bone_ids[c][r] = id;
            parent = JointRef::Named(bone_name);
        }
    }

    // Parameters from the fabric.
    let params = runtime.cloth_params();
    let stiffness = (2.0e4 / params.bend.max(1e-6)).sqrt().clamp(0.1, 4.0);
    let drag = (0.4 + 0.05 * params.damping).clamp(0.0, 1.0);
    let root = SpringParams {
        hit_radius: params.thickness + 0.008,
        stiffness,
        gravity_power: 1.0,
        gravity_dir: Vec3::NEG_Y,
        drag_force: drag,
    };
    let tip = SpringParams { stiffness: 0.5 * stiffness, ..root };
    let group = format!("cloth_{part}");

    // Colliders: the body shapes on humanoid bones, in the bone's space (identity rest rotation at the joint). An
    // arm bone is turned by an A-pose bind, so arms export only when the body is bound in the T-pose.
    let t_pose = fit.bind() == HumanoidPose::REST;
    let set = runtime.colliders();
    let mut colliders = Vec::new();
    let mut skipped = 0;
    for s in set.shapes() {
        let bone = match s.joint().map(|j| &set.joints()[j]) {
            Some(JointRef::Humanoid(bone)) if matches!(s.kind, ColliderKind::Body | ColliderKind::Hair) => *bone,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let arm = format!("{bone:?}").contains("Arm") || format!("{bone:?}").contains("Hand");
        let Some(origin) = fit.joint(&JointRef::Humanoid(bone)).filter(|_| t_pose || !arm) else {
            skipped += 1;
            continue;
        };
        let (a, b) = (s.a - origin, s.b - origin);
        let shape = if a == b {
            ColliderShape::Sphere { offset: a, radius: s.radius }
        } else {
            ColliderShape::Capsule { offset: a, radius: s.radius, tail: b }
        };
        colliders.push(Collider { bone: humanoid_id(bone), shape });
    }
    let exported = colliders.len();
    if exported > 0 {
        rig.collider_groups.push(ColliderGroup { name: group.clone(), colliders });
    }
    for (c, ids) in bone_ids.iter().enumerate() {
        let mut chain = SpringChain::tapered(format!("cloth_{part}_{c}"), ids, root, tip);
        if exported > 0 {
            chain.collider_groups.push(group.clone());
        }
        rig.chains.push(chain);
    }

    // Weights.
    let follow = runtime.follow();
    let mut weights = Vec::with_capacity(rest.len());
    let mut max_cut = 0.0_f32;
    #[allow(clippy::needless_range_loop)] // `v` indexes the rest, skin and follow lists and makes the grid row
    for v in 0..rest.len() {
        let (gr, gc) = (v / grid_cols, v % grid_cols);
        let f = follow[gr];
        let mut influences: Vec<(BakeTarget, f32)> = Vec::with_capacity(8);
        let s = &skin[v];
        for (&j, &w) in s.joints.iter().zip(&s.weights) {
            if w > 0.0 && f < 1.0 {
                influences.push((BakeTarget::Joint(joints[usize::from(j)].clone()), w * (1.0 - f)));
            }
        }
        if f > 0.0 {
            let (c0, c1, tu) = between(&cols, gc, grid_cols, wrap);
            let (r0, r1, tv) = between(&rows, gr.min(grid_rows - 1), grid_rows, false);
            for (c, r, w) in [
                (c0, r0, (1.0 - tu) * (1.0 - tv)),
                (c1, r0, tu * (1.0 - tv)),
                (c0, r1, (1.0 - tu) * tv),
                (c1, r1, tu * tv),
            ] {
                if w > 0.0 {
                    let target = BakeTarget::Bone(bone_ids[c][r]);
                    match influences.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, existing)) => *existing += w * f,
                        None => influences.push((target, w * f)),
                    }
                }
            }
        }
        // Largest first, ties in a fixed order (by position in the list), cut to four, renormalised.
        influences.sort_by(|a, b| b.1.total_cmp(&a.1));
        let cut: f32 = influences.iter().skip(4).map(|(_, w)| w).sum();
        max_cut = max_cut.max(cut);
        influences.truncate(4);
        let total: f32 = influences.iter().map(|(_, w)| w).sum();
        if total > 0.0 {
            for (_, w) in &mut influences {
                *w /= total;
            }
        }
        weights.push(influences);
    }

    // At rest every bone and joint is where it was bound, so each vertex is a weighted sum of itself.
    let max_rest_deviation = 0.0;
    let report = BakeReport {
        chains,
        bones_per_chain: per_chain,
        max_rest_deviation,
        max_weight_cut: max_cut,
        colliders: exported,
    };
    let mut warnings = vec![format!(
        "cloth bake '{part}': {chains} spring chains of {per_chain} bones ({} of {budget}) stand in for the sheet; lost in export: \
         the coupling between columns (stretch, shear and bend across chains), wind, friction and the layer below's shell",
        chains * per_chain
    )];
    if max_cut > 1e-4 {
        warnings.push(format!(
            "cloth bake '{part}': up to {:.1}% of a vertex's weight cut to fit 4 influences",
            max_cut * 100.0
        ));
    }
    if skipped > 0 {
        warnings.push(format!(
            "cloth bake '{part}': {skipped} collider(s) not exported (layer shells, non-humanoid bones, or arms of an A-pose bind)"
        ));
    }
    Ok(ClothBake { rig, weights, report, warnings })
}
