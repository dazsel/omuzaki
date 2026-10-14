//! Humanoid bones and joints between the runtime and the files.
//!
//! The runtime names a bone with [`HumanoidBone`] (`omuzaki-anim-api`); a file's humanoid map uses [`HumanBoneName`]
//! (`omuzaki-io-api`). Both are the 55 VRM 1.0 bones with the same camelCase names (`leftUpperArm`), which is the
//! contract between the two crates (`docs/design/io-api.md`, D8), so converting is a lookup by name and the tests
//! below pin the two tables to each other row by row. The file side is always VRM 1.0 spelling: the 0.x thumb
//! rename belongs to the 0.x importer and exporter.
//!
//! A character mesh follows a [`JointRef`]: a humanoid role or an extra bone (hair, cloth, accessory). Exported
//! scenes name their nodes after the joint they play, so a re-import can find the joint again from the node name
//! alone: [`joint_node_name`] and [`joint_from_node_name`].

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BoneName, JointRef};
use omuzaki_core::id::SlugError;
use omuzaki_io_api::{HumanBoneName, UnknownBone};

/// The file-side name of `bone`.
///
/// # Errors
/// [`UnknownBone`] if the runtime has a bone the file contract lacks. [`HumanoidBone`] is `#[non_exhaustive]`, so the
/// compiler cannot rule that out; the table test fails first if the two crates drift apart.
pub fn to_file(bone: HumanoidBone) -> Result<HumanBoneName, UnknownBone> {
    HumanBoneName::new(bone.name())
}

/// The runtime bone for a file-side name, or `None` if the runtime lacks it (see [`to_file`]).
pub fn from_file(bone: HumanBoneName) -> Option<HumanoidBone> {
    HumanoidBone::from_name(bone.as_str())
}

/// Why a joint and a node name could not be converted into each other.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JointNameError {
    /// An extra bone is called like a humanoid bone (`head`), so its node name would read back as the humanoid one.
    #[error("{0:?} is a VRM humanoid bone name, so an extra bone cannot be called that")]
    Reserved(String),
    /// The node name is not a valid extra bone name.
    #[error("node name {name:?} cannot name an extra bone: {reason}")]
    NotABoneName {
        /// The node name.
        name: String,
        /// Why it is not a bone name.
        reason: SlugError,
    },
}

/// The node name for `joint`: a humanoid bone's VRM name (`leftHand`) or an extra bone's own name (`hair_back_2`).
///
/// # Errors
/// [`JointNameError::Reserved`] for an extra bone whose name is also a humanoid bone's (`head`, `spine`, ...). The
/// humanoid names that are lower-case words are valid extra-bone names, and the two kinds must stay distinguishable.
pub fn joint_node_name(joint: &JointRef) -> Result<&str, JointNameError> {
    match joint {
        JointRef::Humanoid(bone) => Ok(bone.name()),
        JointRef::Named(name) if HumanoidBone::from_name(name.as_str()).is_some() => {
            Err(JointNameError::Reserved(name.as_str().to_owned()))
        }
        JointRef::Named(name) => Ok(name.as_str()),
    }
}

/// The joint a node called `name` plays: the humanoid bone of that VRM name if there is one, otherwise an extra bone.
///
/// # Errors
/// [`JointNameError::NotABoneName`] if `name` is neither (`Armature.001`: bone names are lower-case slugs).
pub fn joint_from_node_name(name: &str) -> Result<JointRef, JointNameError> {
    if let Some(bone) = HumanoidBone::from_name(name) {
        return Ok(JointRef::Humanoid(bone));
    }
    BoneName::new(name)
        .map(JointRef::Named)
        .map_err(|reason| JointNameError::NotABoneName { name: name.to_owned(), reason })
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::HUMAN_BONES;

    use super::*;

    #[test]
    fn the_two_bone_tables_agree_row_by_row() {
        assert_eq!(HumanoidBone::ALL.len(), HUMAN_BONES.len(), "both have all 55 VRM 1.0 bones");
        for (bone, row) in HumanoidBone::ALL.iter().zip(&HUMAN_BONES) {
            assert_eq!(bone.name(), row.name);
            assert_eq!(bone.parent().map(HumanoidBone::name), row.parent, "parent of {}", row.name);
            assert_eq!(bone.is_required(), row.required, "required flag of {}", row.name);
        }
    }

    #[test]
    fn every_bone_converts_to_the_file_and_back() {
        for bone in HumanoidBone::ALL {
            let file = to_file(bone).unwrap();
            assert_eq!(file.as_str(), bone.name());
            assert_eq!(from_file(file), Some(bone));
        }
    }

    #[test]
    fn every_file_bone_converts_to_the_runtime_and_back() {
        let mut seen = Vec::new();
        for row in &HUMAN_BONES {
            let file = HumanBoneName::new(row.name).unwrap();
            let bone = from_file(file).unwrap_or_else(|| panic!("the runtime has no {}", row.name));
            assert_eq!(to_file(bone).unwrap(), file);
            assert!(!seen.contains(&bone), "{bone} is the image of two file bones");
            seen.push(bone);
        }
        assert_eq!(seen.len(), HumanoidBone::ALL.len(), "the conversion is onto: no runtime bone is left out");
    }

    #[test]
    fn humanoid_joints_are_named_after_their_bone() {
        for bone in HumanoidBone::ALL {
            let joint = JointRef::Humanoid(bone);
            let name = joint_node_name(&joint).unwrap();
            assert_eq!(name, bone.name());
            assert_eq!(joint_from_node_name(name).unwrap(), joint);
        }
    }

    #[test]
    fn extra_joints_keep_their_own_name() {
        for name in ["hair_back_2", "skirt-front_0", "tail", "2d-flat"] {
            let joint = JointRef::Named(BoneName::new(name).unwrap());
            assert_eq!(joint_node_name(&joint).unwrap(), name);
            assert_eq!(joint_from_node_name(name).unwrap(), joint);
        }
    }

    #[test]
    fn an_extra_bone_cannot_take_a_humanoid_name() {
        // `head` is a valid slug as well as a humanoid bone, so as a node name it would read back as the humanoid one.
        for name in ["head", "hips", "spine", "chest", "neck", "jaw"] {
            let joint = JointRef::Named(BoneName::new(name).unwrap());
            assert_eq!(joint_node_name(&joint), Err(JointNameError::Reserved(name.to_owned())));
            assert_eq!(joint_from_node_name(name).unwrap(), JointRef::Humanoid(HumanoidBone::from_name(name).unwrap()));
        }
    }

    #[test]
    fn node_names_that_are_not_slugs_are_refused_with_the_reason() {
        let err = joint_from_node_name("Armature.001").unwrap_err();
        assert_eq!(
            err,
            JointNameError::NotABoneName {
                name: "Armature.001".to_owned(),
                reason: SlugError::BadChar { ch: 'A', at: 0 }
            }
        );
        assert!(err.to_string().contains("Armature.001"));
        assert!(matches!(joint_from_node_name(""), Err(JointNameError::NotABoneName { reason: SlugError::Empty, .. })));
    }
}
