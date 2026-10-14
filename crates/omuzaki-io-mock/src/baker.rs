//! A deterministic [`ExpressionBaker`] for tests: every requested channel becomes one morph target that lifts the
//! first vertex of each primitive by a millimetre-scale step, bound to a same-named expression. It has no face to
//! paint, which is the point: the exporter and the checks can be tested end to end without one.

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Avatar, BakeReport, BakeRequest, ChannelBake, Expression, ExpressionBaker, Finding, IoError, IoScene, MorphTarget,
    Progress, Realisation,
};

/// `done / total` as a progress fraction. Channel counts are tiny, so the `u16` detour loses nothing.
fn fraction(done: usize, total: usize) -> f32 {
    let f = |n: usize| f32::from(u16::try_from(n).unwrap_or(u16::MAX));
    f(done) / f(total)
}

/// Bakes one morph target per channel onto the mesh of `request.target`.
#[derive(Default)]
pub struct MockBaker;

impl ExpressionBaker for MockBaker {
    fn bake(&self, scene: &mut IoScene, request: &BakeRequest, progress: &dyn Progress) -> Result<BakeReport, IoError> {
        let mesh_ix = scene
            .nodes
            .get(request.target.index())
            .and_then(|n| n.mesh)
            .filter(|m| m.index() < scene.meshes.len())
            .ok_or_else(|| IoError::unsupported("a bake target without a mesh", "pass the head node"))?;
        let total = request.channels.len().max(1);
        let mut report = BakeReport::default();
        let mut expressions = Vec::new();
        for (i, channel) in request.channels.iter().enumerate() {
            progress.checkpoint("bake", fraction(i, total))?;
            let mesh = &mut scene.meshes[mesh_ix.index()];
            let used = mesh.morph_target_count();
            let over_budget = request.budget.max_morph_targets.is_some_and(|max| used >= max as usize);
            if over_budget || mesh.primitives.is_empty() {
                let reason =
                    if over_budget { "the morph target budget is used up" } else { "the mesh has no geometry" };
                report.channels.push(ChannelBake {
                    channel: channel.to_string(),
                    realisation: Realisation::Inert { reason: reason.into() },
                });
                report.findings.push(Finding::warning("perfect_sync.inert_channel", format!("{channel}: {reason}")));
                expressions.push((channel.clone(), Expression::new()));
                continue;
            }
            let lift = 0.001 * f32::from(u16::try_from(i + 1).unwrap_or(u16::MAX));
            for prim in &mut mesh.primitives {
                let mut deltas = vec![Vec3::ZERO; prim.mesh.positions.len()];
                if let Some(first) = deltas.first_mut() {
                    *first = Vec3::new(0.0, lift, 0.0);
                }
                prim.morph_targets.push(MorphTarget::new(deltas));
            }
            mesh.morph_target_names.resize(used, String::new());
            mesh.morph_target_names.push(channel.to_string());
            let index = u32::try_from(used).map_err(|_| IoError::Internal("more than 2^32 morph targets".into()))?;
            report
                .channels
                .push(ChannelBake { channel: channel.to_string(), realisation: Realisation::Morph { targets: 1 } });
            expressions.push((channel.clone(), Expression::morph(request.target, index)));
        }
        let avatar = scene.avatar.get_or_insert_with(Avatar::new);
        avatar.expressions.extend(expressions);
        Ok(report)
    }
}
