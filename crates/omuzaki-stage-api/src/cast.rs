//! The cast (design D6): which characters stand where. `cast/<member>.toml`.

use omuzaki_char_api::{CharId, VariantId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::drive::DriveMap;
use crate::ids::MemberId;

fn yes() -> bool {
    true
}

// `skip_serializing_if` hands the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// A character placed in a scene. It has no scale: size is the character's own, so real stature holds on stage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CastMember {
    /// The character (`characters/<id>`).
    pub character: CharId,
    /// A per-use variant of it (CB-13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// Metres on the floor: x toward screen right, z toward the front camera.
    #[serde(default)]
    pub position: [f32; 2],
    /// Degrees; 0 faces the front (+Z). `-180..=180`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub yaw: f32,
    /// Whether it is drawn.
    #[serde(default = "yes")]
    pub visible: bool,
    /// Which drivers move which of its channels when it performs (`docs/design/drivers.md`). Empty: every driver,
    /// everywhere, at full strength.
    #[serde(default, skip_serializing_if = "DriveMap::is_open")]
    pub drive: DriveMap,
    /// A touch with another member (#2415): a hug, palms together, cheek to cheek. The stage places the two so their
    /// bodies meet without passing through each other, and the arms and heads of both reach for the other's body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interact: Option<Interact>,
}

/// A touch between two cast members, held on the first: what kind, with whom, how much (#2415).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Interact {
    /// The other member.
    pub with: MemberId,
    /// The touch.
    pub kind: InteractKind,
    /// How much of it, `0..=1`: 0 leaves both as they are.
    #[serde(default = "one")]
    pub weight: f32,
}

impl Interact {
    /// A touch of `kind` with `with`, at full weight.
    #[must_use]
    pub fn new(with: MemberId, kind: InteractKind) -> Self {
        Self { with, kind, weight: 1.0 }
    }
}

fn one() -> f32 {
    1.0
}

/// The touches two members can share (#2415).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum InteractKind {
    /// Face to face, chests together, each one's arms round the other's back, heads past each other's shoulder.
    Hug,
    /// Face to face an arm's length apart, the first's right palm against the other's left at shoulder height.
    PalmTouch,
    /// Side by side facing the same way (the other on the first's right), heads leaning in until the cheeks meet,
    /// the first's arm over the other's far shoulder and the other's round the first's waist.
    CheekToCheek,
}

impl InteractKind {
    /// Every kind, in the order the docs list them.
    pub const ALL: [Self; 3] = [Self::Hug, Self::PalmTouch, Self::CheekToCheek];

    /// The kind's name in files and ops.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hug => "hug",
            Self::PalmTouch => "palm-touch",
            Self::CheekToCheek => "cheek-to-cheek",
        }
    }
}

impl CastMember {
    /// A visible member of `character` at the origin, facing the front.
    #[must_use]
    pub fn new(character: CharId) -> Self {
        Self {
            character,
            variant: None,
            position: [0.0, 0.0],
            yaw: 0.0,
            visible: true,
            drive: DriveMap::open(),
            interact: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_member_needs_only_its_character() {
        let m: CastMember = serde_json::from_value(json!({"character": "aiko"})).unwrap();
        assert_eq!(m, CastMember::new(CharId::new("aiko").unwrap()));
        assert!(serde_json::from_value::<CastMember>(json!({"character": "aiko", "scale": 2.0})).is_err());
        assert!(serde_json::from_value::<CastMember>(json!({})).is_err());
    }

    #[test]
    fn an_interaction_names_its_partner_and_kind() {
        let m: CastMember = serde_json::from_value(
            json!({"character": "aiko", "interact": {"with": "guest", "kind": "cheek-to-cheek"}}),
        )
        .unwrap();
        let interact = m.interact.clone().unwrap();
        assert_eq!((interact.kind, interact.weight), (InteractKind::CheekToCheek, 1.0));
        assert_eq!(serde_json::to_value(&m).unwrap()["interact"]["kind"], json!("cheek-to-cheek"));
        assert!(serde_json::from_value::<Interact>(json!({"with": "guest", "kind": "kiss"})).is_err());
    }
}
