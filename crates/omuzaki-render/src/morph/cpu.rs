//! The CPU blend: the reference path and the fallback where the adapter has no compute.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::MorphSet;

use crate::resource::mesh::Shape;

/// `base` blended by `weights` into `out`, with `flags` (all false on entry and exit) marking the vertices whose
/// normal moved. Summed in target order in `f32` like the reference; a target of weight `0.0` is skipped.
pub(super) fn blend(base: &[Shape], set: &MorphSet, weights: &[f32], flags: &mut Vec<bool>, out: &mut Vec<Shape>) {
    out.clear();
    out.extend_from_slice(base);
    if flags.len() < base.len() {
        flags.resize(base.len(), false);
    }
    let active = || set.targets().iter().zip(weights).filter(|(_, w)| **w != 0.0);
    for (target, &weight) in active() {
        for (k, &index) in target.indices.iter().enumerate() {
            let i = index as usize;
            // The set was validated against this vertex count when the mesh was created, so a miss is a broken invariant.
            let (Some(shape), Some(&delta)) = (out.get_mut(i), target.positions.get(k)) else {
                debug_assert!(false, "morph target {:?}: entry {k} (vertex {i}) does not fit the mesh", target.name);
                continue;
            };
            shape.position = (Vec3::from(shape.position) + delta * weight).to_array();
            if let Some(&delta) = target.normals.get(k) {
                shape.normal = (Vec3::from(shape.normal) + delta * weight).to_array();
                if let Some(flag) = flags.get_mut(i) {
                    *flag = true;
                }
            }
        }
    }
    // Normalise once, after every target is summed; a normal that cancels out keeps its base value.
    for (target, _) in active().filter(|(t, _)| !t.normals.is_empty()) {
        for &index in &target.indices {
            let i = index as usize;
            if let Some(flag) = flags.get_mut(i).filter(|f| **f) {
                *flag = false;
                if let (Some(shape), Some(old)) = (out.get_mut(i), base.get(i)) {
                    shape.normal = Vec3::from(shape.normal).try_normalize().map_or(old.normal, |n| n.to_array());
                }
            }
        }
    }
}
