//! Typed ids. Each is a validated slug (`a-z`, `0-9`, `-`, `_`), so it is safe as a file name and a TOML key, and a
//! `PartId` can never be passed where a `BoneId` is expected. Ids are stable contracts: a part's id appears in
//! variants' override paths and in saved projects, so renaming one is a migration.

use omuzaki_core::slug_id;

slug_id!(
    /// A character in a project (`characters/<id>/`).
    CharId
);
slug_id!(
    /// A part within one character: `head`, `hair`, `shirt`, `robe`. Unique per character.
    PartId
);
slug_id!(
    /// The kind of a part, which selects its generator and parameter schema: `head`, `hair`, `body`, `skirt`...
    PartKindId
);
slug_id!(
    /// A per-use variant of a character (CB-13): `game`, `close-up`, `emote`.
    VariantId
);
slug_id!(
    /// The name of a bone that is not a VRM humanoid bone: hair, cloth and accessory bones, named by the part that
    /// made them (`hair_back_2`; the rig contract's convention is `hair_<part>_<i>`).
    BoneName
);
slug_id!(
    /// A named attachment point a part publishes (`top`, `ear_l`, `tip`), used by accessories.
    AnchorId
);
slug_id!(
    /// The name of a material of a built character, unique per character; parts prefix theirs with the part id
    /// (`hair`, `robe-lining`). (The renderer's `MaterialId` is a handle to an uploaded material; this is its name.)
    MaterialName
);
slug_id!(
    /// The name of a procedural texture of a built character, unique per character. (The renderer's `TextureId` is a
    /// handle to an uploaded texture.)
    TextureName
);
slug_id!(
    /// A group of clumps inside a hair part: `cap`, `bangs`, `curtain`.
    GroupId
);
slug_id!(
    /// A body template (CB-01): `soft-sd`, `mini`, `standard`. The template fixes the skeleton and its default sizes.
    BodyTemplateId
);
slug_id!(
    /// A body-shape preset (CB-04): `slim`, `athletic`, `curvy`. Informational, like [`ProfileId`] on the head: the
    /// numbers in `BodyShape` are what builds.
    BodyShapeId
);
slug_id!(
    /// A skin tone from the library (`light-warm`, `deep-cool`). Informational: the colours are explicit palette slots
    /// (`skin`, `skin_shade`, `skin_line`, `skin_rim`), so choosing a tone is a patch to the palette plus this id.
    SkinToneId
);
slug_id!(
    /// A named colourway of a character (LK-03): `default`, `winter`, `summer-stream`. Stored as a whole palette at
    /// `characters/<id>/colourways/<name>`.
    ColourwayId
);
slug_id!(
    /// A face-shape preset (CB-10): `round-u`, `egg-oval`, `mochi`, `heart`, `long`.
    ProfileId
);

/// A joint a mesh is skinned to, or an anchor follows: a **VRM humanoid role** (portable across skeletons: `head` is the
/// head of whatever skeleton the character wears) or the **name** of an extra bone (hair, cloth, accessories). The
/// stage resolves a `JointRef` to an index in the skeleton it uses (`omuzaki_anim_api::Skeleton::humanoid_bone` for
/// roles, a name lookup for the rest); the character kit never sees skeleton indices.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum JointRef {
    /// A humanoid bone by its VRM 1.0 name (`"hips"`, `"head"`, `"leftUpperArm"`).
    Humanoid(omuzaki_anim_api::HumanoidBone),
    /// An extra bone by name.
    Named(BoneName),
}

impl JointRef {
    /// The head bone.
    pub const HEAD: Self = Self::Humanoid(omuzaki_anim_api::HumanoidBone::Head);
    /// The neck bone.
    pub const NECK: Self = Self::Humanoid(omuzaki_anim_api::HumanoidBone::Neck);
    /// The chest bone.
    pub const CHEST: Self = Self::Humanoid(omuzaki_anim_api::HumanoidBone::Chest);
    /// The hips bone.
    pub const HIPS: Self = Self::Humanoid(omuzaki_anim_api::HumanoidBone::Hips);

    /// The joint on the opposite side of the body. A humanoid bone takes its VRM counterpart (`leftHand` and
    /// `rightHand`); a named bone swaps its `l` and `r` segments (`ear_l_1` and `ear_r_1`); anything else is its own
    /// mirror.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        match self {
            Self::Humanoid(bone) => Self::Humanoid(bone.mirrored()),
            Self::Named(name) => {
                let swapped: Vec<&str> = name
                    .as_str()
                    .split('_')
                    .map(|segment| match segment {
                        "l" => "r",
                        "r" => "l",
                        other => other,
                    })
                    .collect();
                // Swapping `l` and `r` keeps the characters of a slug valid; if it ever did not, the bone is its own
                // mirror.
                BoneName::new(swapped.join("_")).map_or_else(|_| self.clone(), Self::Named)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joints_are_roles_or_names_and_serialise_readably() {
        assert_eq!(serde_json::to_string(&JointRef::HEAD).unwrap(), r#"{"humanoid":"head"}"#);
        let named: JointRef = serde_json::from_str(r#"{"named":"hair_back_2"}"#).unwrap();
        assert_eq!(named, JointRef::Named(BoneName::new("hair_back_2").unwrap()));
        let upper: JointRef = serde_json::from_str(r#"{"humanoid":"leftUpperArm"}"#).unwrap();
        assert!(matches!(upper, JointRef::Humanoid(_)));
        assert!(serde_json::from_str::<JointRef>(r#"{"humanoid":"tail"}"#).is_err());
    }
}
