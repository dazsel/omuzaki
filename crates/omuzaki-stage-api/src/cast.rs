//! The cast (design D6): which characters stand where. `cast/<member>.toml`.

use omuzaki_char_api::{CharId, VariantId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
}

impl CastMember {
    /// A visible member of `character` at the origin, facing the front.
    #[must_use]
    pub fn new(character: CharId) -> Self {
        Self { character, variant: None, position: [0.0, 0.0], yaw: 0.0, visible: true }
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
}
