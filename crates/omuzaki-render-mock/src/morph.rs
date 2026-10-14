//! The reference morph blend: what every renderer's morph path (the GPU compute pass included) is checked against.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{MorphSet, RenderError};

/// Applies `weights` to a mesh's base vertices: `position = base + Σ wᵢ·Δpositionᵢ` and
/// `normal = normalize(base_normal + Σ wᵢ·Δnormalᵢ)`, summed in target order in `f32`. Returns the morphed positions
/// and normals; skinning and the model transform come after.
///
/// A vertex no normal delta reaches keeps its base normal bit for bit, as does one whose blended normal degenerates
/// to zero length. A target with weight `0.0` is skipped, so all-zero weights return the base mesh exactly. `weights`
/// pairs with `set`'s targets one to one.
///
/// # Errors
/// [`RenderError::Invalid`] if `normals` and `positions` differ in length, if `set` doesn't fit that many vertices
/// ([`MorphSet::validate`]), or if there isn't exactly one weight per target. Nothing here can panic.
pub fn morph_vertices(
    positions: &[Vec3],
    normals: &[Vec3],
    set: &MorphSet,
    weights: &[f32],
) -> Result<(Vec<Vec3>, Vec<Vec3>), RenderError> {
    if normals.len() != positions.len() {
        let reason = format!("{} normals for {} positions", normals.len(), positions.len());
        return Err(RenderError::invalid("morph input", reason));
    }
    set.validate(positions.len())?;
    if weights.len() != set.len() {
        let reason = format!("{} weights for {} morph targets", weights.len(), set.len());
        return Err(RenderError::invalid("morph input", reason));
    }
    let mut out_positions = positions.to_vec();
    let mut out_normals = normals.to_vec();
    let mut renormalise = vec![false; positions.len()];
    for (target, &weight) in set.targets().iter().zip(weights) {
        if weight == 0.0 {
            continue;
        }
        for (k, &index) in target.indices.iter().enumerate() {
            let i = index as usize;
            out_positions[i] += target.positions[k] * weight;
            if let Some(&delta) = target.normals.get(k) {
                out_normals[i] += delta * weight;
                renormalise[i] = true;
            }
        }
    }
    for (i, flag) in renormalise.iter().enumerate() {
        if *flag {
            out_normals[i] = out_normals[i].try_normalize().unwrap_or(normals[i]);
        }
    }
    Ok((out_positions, out_normals))
}

#[cfg(test)]
mod tests {
    use omuzaki_core::id::Slug;
    use omuzaki_render_api::SparseMorph;

    use super::*;

    fn set() -> MorphSet {
        let slug = |s: &str| Slug::new(s).unwrap();
        MorphSet::from_targets(
            3,
            vec![
                SparseMorph {
                    name: slug("up"),
                    indices: vec![1],
                    positions: vec![Vec3::Y],
                    normals: vec![Vec3::new(0.0, 1.0, 0.0)],
                },
                SparseMorph { name: slug("right"), indices: vec![1, 2], positions: vec![Vec3::X; 2], normals: vec![] },
            ],
        )
        .unwrap()
    }

    #[test]
    fn zero_weights_return_the_base_exactly() {
        let (p, n) = (vec![Vec3::ZERO, Vec3::X, Vec3::Y], vec![Vec3::Z; 3]);
        assert_eq!(morph_vertices(&p, &n, &set(), &[0.0, 0.0]).unwrap(), (p, n));
    }

    #[test]
    fn weights_scale_and_sum_in_any_range_and_normals_are_renormalised() {
        let (p, n) = (vec![Vec3::ZERO; 3], vec![Vec3::Z; 3]);
        let (mp, mn) = morph_vertices(&p, &n, &set(), &[2.0, -0.5]).unwrap();
        assert_eq!(mp, vec![Vec3::ZERO, Vec3::new(-0.5, 2.0, 0.0), Vec3::new(-0.5, 0.0, 0.0)]);
        assert!((mn[1] - Vec3::new(0.0, 2.0, 1.0).normalize()).length() < 1e-6);
        assert_eq!((mn[0], mn[2]), (Vec3::Z, Vec3::Z), "untouched normals stay as they were");
    }

    #[test]
    fn a_normal_that_cancels_out_falls_back_to_the_base() {
        let (p, n) = (vec![Vec3::ZERO; 3], vec![Vec3::Y; 3]);
        let (_, mn) = morph_vertices(&p, &n, &set(), &[-1.0, 0.0]).unwrap();
        assert_eq!(mn[1], Vec3::Y);
    }

    #[test]
    fn short_slices_and_wrong_weight_counts_are_errors_not_panics() {
        let (p, n) = (vec![Vec3::ZERO; 3], vec![Vec3::Z; 3]);
        let set = set();
        assert!(morph_vertices(&p, &n[..2], &set, &[0.0, 0.0]).is_err(), "fewer normals than positions");
        assert!(morph_vertices(&p[..1], &n[..1], &set, &[0.0, 0.0]).is_err(), "a mesh too short for the set");
        assert!(morph_vertices(&p, &n, &set, &[0.0]).is_err(), "too few weights");
        assert!(morph_vertices(&p, &n, &set, &[0.0; 3]).is_err(), "too many weights");
    }
}
