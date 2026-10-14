//! The prototype's bone names as [`JointRef`]s, for parity tests only.
//!
//! The JavaScript prototype names bones after its own skeleton (`upper_leg.L`, `hand.R`, `hips`); the Rust kit names
//! them by VRM role or by extra-bone slug. Parity tests that compare against the prototype's dumps use [`joint`] to
//! translate. Nothing in the character build path calls it.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BoneName, JointRef};

/// The joint for a prototype bone name, or `None` if the name cannot be a bone name.
///
/// The prototype's body bones map to the VRM humanoid roles (`.L` is the character's left, +X): `hips`, `spine`,
/// `chest`, `neck`, `head`, `upper_arm`, `lower_arm`, `hand`, `upper_leg`, `lower_leg`, `foot` and `toes`. Any other
/// name (`col_head`, `hair_B_2`) is an extra bone: lower-cased, with `.` as `_`.
pub fn joint(name: &str) -> Option<JointRef> {
    let (stem, side) = match name.rsplit_once('.') {
        Some((stem, "L")) => (stem, Some(true)),
        Some((stem, "R")) => (stem, Some(false)),
        _ => (name, None),
    };
    let role = |left: HumanoidBone, right: HumanoidBone| side.map(|is_left| if is_left { left } else { right });
    let bone = match stem {
        "hips" if side.is_none() => Some(HumanoidBone::Hips),
        "spine" if side.is_none() => Some(HumanoidBone::Spine),
        "chest" if side.is_none() => Some(HumanoidBone::Chest),
        "neck" if side.is_none() => Some(HumanoidBone::Neck),
        "head" if side.is_none() => Some(HumanoidBone::Head),
        "upper_arm" => role(HumanoidBone::LeftUpperArm, HumanoidBone::RightUpperArm),
        "lower_arm" => role(HumanoidBone::LeftLowerArm, HumanoidBone::RightLowerArm),
        "hand" => role(HumanoidBone::LeftHand, HumanoidBone::RightHand),
        "upper_leg" => role(HumanoidBone::LeftUpperLeg, HumanoidBone::RightUpperLeg),
        "lower_leg" => role(HumanoidBone::LeftLowerLeg, HumanoidBone::RightLowerLeg),
        "foot" => role(HumanoidBone::LeftFoot, HumanoidBone::RightFoot),
        "toes" => role(HumanoidBone::LeftToes, HumanoidBone::RightToes),
        _ => None,
    };
    match bone {
        Some(bone) => Some(JointRef::Humanoid(bone)),
        None => BoneName::new(name.to_ascii_lowercase().replace('.', "_")).ok().map(JointRef::Named),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_bones_become_humanoid_roles() {
        assert_eq!(joint("hips"), Some(JointRef::HIPS));
        assert_eq!(joint("head"), Some(JointRef::HEAD));
        assert_eq!(joint("upper_leg.L"), Some(JointRef::Humanoid(HumanoidBone::LeftUpperLeg)));
        assert_eq!(joint("hand.R"), Some(JointRef::Humanoid(HumanoidBone::RightHand)));
        assert_eq!(joint("toes.L"), Some(JointRef::Humanoid(HumanoidBone::LeftToes)));
    }

    #[test]
    fn every_prototype_body_bone_is_a_distinct_role() {
        let mut seen = Vec::new();
        for stem in ["upper_arm", "lower_arm", "hand", "upper_leg", "lower_leg", "foot", "toes"] {
            for side in ["L", "R"] {
                let j = joint(&format!("{stem}.{side}")).unwrap();
                assert!(matches!(j, JointRef::Humanoid(_)), "{stem}.{side}");
                assert!(!seen.contains(&j));
                seen.push(j);
            }
        }
    }

    #[test]
    fn extra_bones_become_slugs() {
        assert_eq!(joint("col_head"), Some(JointRef::Named(BoneName::new("col_head").unwrap())));
        assert_eq!(joint("hair_B_2"), Some(JointRef::Named(BoneName::new("hair_b_2").unwrap())));
        assert_eq!(joint("hair_BL_0"), Some(JointRef::Named(BoneName::new("hair_bl_0").unwrap())));
    }

    #[test]
    fn a_side_suffix_on_a_centre_bone_or_a_bad_name_is_not_a_humanoid_role() {
        assert!(matches!(joint("hips.L"), Some(JointRef::Named(_))), "falls back to a slug");
        assert_eq!(joint(""), None);
        assert_eq!(joint("bad name!"), None);
    }
}
