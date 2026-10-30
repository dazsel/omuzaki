//! Makeup presets (#2472), behind `face.makeup.preset`.
//!
//! The makeup is a fixed set of a face's fields ([`MAKEUP_FIELDS`]): eyeshadow, lip colour, blush style and base,
//! contour and the nose's shade and highlight. A preset in `library/makeup.toml` names what differs from no makeup,
//! and [`preset_makeup`] gives every makeup field's value under it (the default for each field the preset leaves out),
//! so applying one replaces the whole look and never leaves an earlier look's fields behind.

use omuzaki_face_api::FaceProfile;
use serde::Deserialize;
use serde_json::Value;

use crate::eye_presets::merge;

/// The preset library.
const LIBRARY: &str = include_str!("../library/makeup.toml");

/// The face fields a makeup preset owns, as paths under the face. The blush channel's strength, the blush's size and the
/// lips' rim stay the character's: they are its expression and its drawing, not its makeup.
pub const MAKEUP_FIELDS: [&[&str]; 10] = [
    &["eyes", "shadow"],
    &["mouth", "lips", "tint"],
    &["mouth", "lips", "colour"],
    &["cheeks", "kind"],
    &["cheeks", "base"],
    &["cheeks", "colour"],
    &["cheeks", "contour"],
    &["cheeks", "contour_colour"],
    &["nose", "shadow"],
    &["nose", "highlight"],
];

/// One makeup preset.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MakeupPreset {
    /// Its id, as `face.makeup.preset` takes it.
    pub id: String,
    /// One line on what it looks like.
    pub summary: String,
    /// What it changes of the bare face, as a partial face.
    #[serde(default = "nothing")]
    pub face: Value,
}

fn nothing() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    preset: Vec<MakeupPreset>,
}

/// Every preset, in the library's order.
///
/// # Errors
/// The library's parse error, which would be a bug in `library/makeup.toml` (a test reads it).
pub fn presets() -> Result<Vec<MakeupPreset>, String> {
    toml::from_str::<Library>(LIBRARY).map(|l| l.preset).map_err(|e| format!("library/makeup.toml: {e}"))
}

/// Each makeup field's value under preset `id`, in [`MAKEUP_FIELDS`]' order.
///
/// # Errors
/// An unknown id (with the ids there are), or a preset that sets a field outside the makeup or doesn't read.
pub fn preset_makeup(id: &str) -> Result<Vec<(&'static [&'static str], Value)>, String> {
    let all = presets()?;
    let Some(preset) = all.iter().find(|p| p.id == id) else {
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        return Err(format!("there is no makeup preset '{id}'; the presets are {}", ids.join(", ")));
    };
    let bare = serde_json::to_value(FaceProfile::default()).map_err(|e| e.to_string())?;
    let mut face = bare.clone();
    merge(&mut face, &preset.face);
    // Read it back, so a preset with a typo fails here and not in a face, and refuse one that reaches past the makeup.
    serde_json::from_value::<FaceProfile>(face.clone()).map_err(|e| format!("makeup preset '{id}': {e}"))?;
    let mut owned = bare.clone();
    let fields = MAKEUP_FIELDS
        .iter()
        .map(|path| {
            let value = get(&face, path).cloned().unwrap_or(Value::Null);
            set(&mut owned, path, value.clone());
            (*path, value)
        })
        .collect();
    if owned != face {
        return Err(format!("makeup preset '{id}' sets a field that isn't makeup"));
    }
    Ok(fields)
}

/// The value at `path` under `node`.
pub(crate) fn get<'a>(node: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(node, |node, key| node.get(*key))
}

/// Sets `path` under `node` to `value`, making the objects on the way.
pub(crate) fn set(node: &mut Value, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut node = node;
    for key in parents {
        if !node.is_object() {
            *node = Value::Object(serde_json::Map::new());
        }
        let Value::Object(map) = node else { return };
        node = map.entry((*key).to_owned()).or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    if let Value::Object(map) = node {
        map.insert((*last).to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_face_api::Validate;

    use super::*;

    #[test]
    fn every_preset_reads_validates_and_owns_only_makeup() {
        let all = presets().unwrap();
        assert!(all.len() >= 5);
        for preset in &all {
            let mut face = serde_json::to_value(FaceProfile::default()).unwrap();
            for (path, value) in preset_makeup(&preset.id).unwrap() {
                set(&mut face, path, value);
            }
            let face: FaceProfile = serde_json::from_value(face).unwrap();
            face.validate().unwrap_or_else(|e| panic!("{}: {e}", preset.id));
        }
    }

    #[test]
    fn none_is_the_bare_face() {
        let bare = serde_json::to_value(FaceProfile::default()).unwrap();
        for (path, value) in preset_makeup("none").unwrap() {
            assert_eq!(get(&bare, path), Some(&value), "{path:?}");
        }
    }

    #[test]
    fn an_unknown_preset_names_the_ones_there_are() {
        let e = preset_makeup("clown").unwrap_err();
        assert!(e.contains("natural"), "{e}");
    }

    #[test]
    fn set_makes_the_objects_on_the_way() {
        let mut v = serde_json::json!({});
        set(&mut v, &["a", "b", "c"], Value::from(1));
        assert_eq!(v, serde_json::json!({ "a": { "b": { "c": 1 } } }));
    }
}
