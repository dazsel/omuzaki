//! [`SceneDef`]: the typed view of a scene node (`scenes/<id>`), its document paths and its limits (design D1).

use std::collections::BTreeMap;

use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::camera::CameraDef;
use crate::cast::CastMember;
use crate::effects::WeatherDef;
use crate::error::did_you_mean;
use crate::ids::{CameraId, MemberId, PropId};
use crate::lighting::Lighting;
use crate::props::PropDef;
use crate::settings::SceneSettings;

pub use omuzaki_render_api::limits::MAX_LIGHTS;

/// Cameras in one scene.
pub const MAX_CAMERAS: usize = 16;
/// Cast members in one scene.
pub const MAX_CAST: usize = 8;
/// Props in one scene.
pub const MAX_PROPS: usize = 128;
/// The stature, in metres, of the figure the prototype's rigs were tuned on (soft-sd's authored height). Converting a
/// rig divides its local-light lengths by it and its local-light intensities by its square (design D4).
pub const REFERENCE_HEIGHT_M: f32 = 0.966;

/// Where scenes live in the document tree, as typed paths. Ops build patches against these; nothing else spells a
/// scene path by hand.
pub mod paths {
    use omuzaki_core::id::Slug;
    use omuzaki_doc_api::DocPath;

    use crate::ids::{CameraId, LightId, MemberId, PropId, SceneId};

    /// `scenes/<id>`: the whole [`SceneDef`](super::SceneDef).
    #[must_use]
    pub fn scene(id: &SceneId) -> DocPath {
        DocPath::root().join("scenes").join(id.as_str())
    }

    /// `scenes/<id>/scene`: the [`SceneSettings`](crate::SceneSettings) (`scene.toml`).
    #[must_use]
    pub fn settings(id: &SceneId) -> DocPath {
        scene(id).join("scene")
    }

    /// `scenes/<id>/lighting`: the [`Lighting`](crate::Lighting) (`lighting.toml`).
    #[must_use]
    pub fn lighting(id: &SceneId) -> DocPath {
        scene(id).join("lighting")
    }

    /// `scenes/<id>/lighting/lights/<light>`: one [`LightDef`](crate::LightDef).
    #[must_use]
    pub fn light(id: &SceneId, light: &LightId) -> DocPath {
        lighting(id).join("lights").join(light.as_str())
    }

    /// `scenes/<id>/weather`: the [`WeatherDef`](crate::effects::WeatherDef) (`weather.toml`), absent for none.
    #[must_use]
    pub fn weather(id: &SceneId) -> DocPath {
        scene(id).join("weather")
    }

    /// `scenes/<id>/cameras/<camera>`: one [`CameraDef`](crate::CameraDef).
    #[must_use]
    pub fn camera(id: &SceneId, camera: &CameraId) -> DocPath {
        scene(id).join("cameras").join(camera.as_str())
    }

    /// `scenes/<id>/cast/<member>`: one [`CastMember`](crate::CastMember).
    #[must_use]
    pub fn member(id: &SceneId, member: &MemberId) -> DocPath {
        scene(id).join("cast").join(member.as_str())
    }

    /// `scenes/<id>/props/<prop>`: one [`PropDef`](crate::PropDef).
    #[must_use]
    pub fn prop(id: &SceneId, prop: &PropId) -> DocPath {
        scene(id).join("props").join(prop.as_str())
    }

    /// `scenes/<id>/applied/<app>`: the library's record of an application (ADR 0019 D5), by slot or item name.
    #[must_use]
    pub fn applied(id: &SceneId, app: &Slug) -> DocPath {
        scene(id).join("applied").join(app.as_str())
    }

    /// `project/default_scene`: the id of the scene the project opens in.
    #[must_use]
    pub fn default_scene() -> DocPath {
        DocPath::root().join("project").join("default_scene")
    }
}

/// A scene: everything under `scenes/<id>/` except the library's `applied` records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneDef {
    /// Name, subject, opening camera, backdrop, ground and post (`scene.toml`).
    pub scene: SceneSettings,
    /// The light rig as values (`lighting.toml`).
    #[serde(default)]
    pub lighting: Lighting,
    /// The cameras, by id (`cameras/<id>.toml`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cameras: BTreeMap<CameraId, CameraDef>,
    /// The placed characters, by member id (`cast/<member>.toml`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cast: BTreeMap<MemberId, CastMember>,
    /// The props, by id (`props/<prop>.toml`, design D7).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: BTreeMap<PropId, PropDef>,
    /// The weather (`weather.toml`, particles D10): materialised preset values, absent for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<WeatherDef>,
}

impl SceneDef {
    /// The keys of `scenes/<id>` that make up a [`SceneDef`].
    pub const NODE_KEYS: [&'static str; 6] = ["scene", "lighting", "cameras", "cast", "props", "weather"];

    /// The other keys of `scenes/<id>`: files with their own contracts (the library's `applied` records). They are not
    /// part of the definition; [`from_node`](Self::from_node) skips them and [`to_node`](Self::to_node) leaves them
    /// alone. A new file of the scene is added here.
    pub const SIBLING_KEYS: [&'static str; 1] = ["applied"];

    /// A scene with the given settings and nothing else.
    #[must_use]
    pub fn new(scene: SceneSettings) -> Self {
        Self {
            scene,
            lighting: Lighting::default(),
            cameras: BTreeMap::new(),
            cast: BTreeMap::new(),
            props: BTreeMap::new(),
            weather: None,
        }
    }

    /// The definition stored in the scene node `node` (`scenes/<id>`): reads [`NODE_KEYS`](Self::NODE_KEYS) and skips
    /// the [`SIBLING_KEYS`](Self::SIBLING_KEYS).
    ///
    /// A key in neither list is refused with the nearest valid one, so a misspelt `camera` can't read as an absent
    /// `cameras` and be lost on the next save. This checks the format only; [`check`](Self::check) checks the rules.
    ///
    /// # Errors
    /// The [`serde_json::Error`] naming the unknown key or the field that doesn't match the format, or a node that
    /// is not a table.
    pub fn from_node(node: &Value) -> Result<Self, serde_json::Error> {
        let own = match node {
            Value::Object(map) => {
                if let Some(unknown) = map
                    .keys()
                    .find(|k| !Self::NODE_KEYS.contains(&k.as_str()) && !Self::SIBLING_KEYS.contains(&k.as_str()))
                {
                    let all: Vec<&str> = Self::NODE_KEYS.into_iter().chain(Self::SIBLING_KEYS).collect();
                    let message = format!("{} in the scene", did_you_mean(unknown, &all));
                    return Err(<serde_json::Error as serde::de::Error>::custom(message));
                }
                Value::Object(
                    map.iter()
                        .filter(|(k, _)| Self::NODE_KEYS.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            }
            other => other.clone(),
        };
        serde_json::from_value(own)
    }

    /// Writes this definition into the scene node `into`: sets the [`NODE_KEYS`](Self::NODE_KEYS) it has a value for,
    /// removes the ones it has none for (an empty collection is removed, never written as `{}`), and keeps every other
    /// key as it was. A node that is not a table (a new scene starts as `null`) becomes one.
    ///
    /// # Errors
    /// The [`serde_json::Error`] if the definition cannot be turned into a table.
    pub fn to_node(&self, into: &mut Value) -> Result<(), serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        shorten_floats(&mut value);
        let Value::Object(written) = value else {
            return Err(<serde_json::Error as serde::ser::Error>::custom("a scene definition is a table"));
        };
        match into {
            Value::Object(node) => {
                node.retain(|key, _| !Self::NODE_KEYS.contains(&key.as_str()) || written.contains_key(key));
                node.extend(written);
            }
            other => *other = Value::Object(written),
        }
        Ok(())
    }
}

/// Rewrites every number that is exactly an `f32` as the shortest decimal that reads back as that `f32`.
///
/// `serde_json` widens an `f32` to `f64` (`0.9` becomes `0.899_999_976_158_142_1`), and the store would save that, so a
/// scene file would show digits nobody wrote. Reading `0.9` back as `f32` gives the same value, so nothing is lost.
// Exact equality is the point: only a number that is already an `f32` is rewritten.
#[allow(clippy::float_cmp)]
fn shorten_floats(value: &mut Value) {
    match value {
        Value::Number(n) => {
            if let Some(wide) = n.as_f64().filter(|_| n.is_f64()) {
                #[allow(clippy::cast_possible_truncation)]
                let narrow = wide as f32;
                if f64::from(narrow) == wide
                    && let Some(short) = narrow.to_string().parse::<f64>().ok().and_then(serde_json::Number::from_f64)
                {
                    *n = short;
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(shorten_floats),
        Value::Object(map) => map.values_mut().for_each(shorten_floats),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scene() -> SceneDef {
        SceneDef::new(SceneSettings::named("Main"))
    }

    #[test]
    fn the_node_keys_are_exactly_the_fields_of_the_scene_format() {
        let schema = serde_json::to_value(schemars::schema_for!(SceneDef)).unwrap();
        let mut fields: Vec<&str> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort_unstable();
        let mut keys = SceneDef::NODE_KEYS.to_vec();
        keys.sort_unstable();
        assert_eq!(keys, fields, "a field added to SceneDef must be listed in NODE_KEYS");
        assert!(SceneDef::SIBLING_KEYS.iter().all(|k| !SceneDef::NODE_KEYS.contains(k)));
    }

    #[test]
    fn a_misspelt_key_is_refused_with_the_nearest_valid_one() {
        let err = SceneDef::from_node(&json!({"scene": {"name": "x"}, "camera": {}})).unwrap_err().to_string();
        assert!(err.contains("did you mean `cameras`"), "{err}");
        let err = SceneDef::from_node(&json!({"scene": {"name": "x"}, "lightning": {}})).unwrap_err().to_string();
        assert!(err.contains("did you mean `lighting`"), "{err}");
        let err = SceneDef::from_node(&json!({"scene": {"name": "x"}, "zzzzzzzz": 1})).unwrap_err().to_string();
        assert!(err.contains("expected one of") && err.contains("`cast`"), "{err}");
    }

    #[test]
    fn sibling_keys_are_skipped_on_read_and_kept_on_write() {
        let mut node = json!({"scene": {"name": "Old"}, "applied": {"light-rig": {"item": "x"}}, "cast": {}});
        assert_eq!(SceneDef::from_node(&node).unwrap().scene.name, "Old");
        scene().to_node(&mut node).unwrap();
        assert_eq!(node["applied"], json!({"light-rig": {"item": "x"}}));
        assert_eq!(node["scene"]["name"], "Main");
        assert!(node.get("cast").is_none(), "an empty collection is removed, not written as {{}}");
    }

    #[test]
    fn empty_collections_are_never_written() {
        let mut node = Value::Null;
        scene().to_node(&mut node).unwrap();
        let keys: Vec<&String> = node.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["lighting", "scene"]);
        assert!(node["lighting"].get("lights").is_none());
        assert_eq!(SceneDef::from_node(&node).unwrap(), scene());
    }

    #[test]
    fn floats_are_written_as_the_digits_a_person_would_type() {
        let mut def = scene();
        def.lighting.exposure = 0.9;
        def.lighting.ambient.intensity = 0.75;
        let mut node = Value::Null;
        def.to_node(&mut node).unwrap();
        assert_eq!(node["lighting"]["exposure"], json!(0.9));
        assert_eq!(serde_json::to_string(&node["lighting"]["ambient"]["intensity"]).unwrap(), "0.75");
        assert_eq!(SceneDef::from_node(&node).unwrap(), def, "reads back as the same f32");
    }

    #[test]
    fn props_are_written_with_their_floats_shortened() {
        let mut def = scene();
        let mut prop = crate::PropDef::new(crate::PropKindId::new("cup").unwrap());
        prop.params = json!({"height": f64::from(0.9_f32)});
        prop.position = [0.25, 0.74, 0.1];
        def.props.insert(PropId::new("mug").unwrap(), prop);
        let mut node = Value::Null;
        def.to_node(&mut node).unwrap();
        assert_eq!(serde_json::to_string(&node["props"]["mug"]["params"]["height"]).unwrap(), "0.9");
        assert_eq!(serde_json::to_string(&node["props"]["mug"]["position"]).unwrap(), "[0.25,0.74,0.1]");
        let back = SceneDef::from_node(&node).unwrap();
        let mug = &back.props[&PropId::new("mug").unwrap()];
        assert_eq!(mug.position, [0.25, 0.74, 0.1], "reads back as the same f32");
        #[allow(clippy::cast_possible_truncation)]
        let height = mug.params["height"].as_f64().unwrap() as f32;
        assert_eq!(height, 0.9, "a kind decodes the same f32");
    }

    #[test]
    fn a_scene_node_needs_its_settings() {
        assert!(SceneDef::from_node(&json!({"lighting": {}})).is_err());
        assert!(SceneDef::from_node(&json!(3)).is_err());
    }

    #[test]
    fn paths_follow_the_documented_layout() {
        use crate::ids::{LightId, SceneId};
        let id = SceneId::new("desk").unwrap();
        let s = |p: omuzaki_doc_api::DocPath| p.to_string();
        assert_eq!(s(paths::scene(&id)), "scenes/desk");
        assert_eq!(s(paths::settings(&id)), "scenes/desk/scene");
        assert_eq!(s(paths::lighting(&id)), "scenes/desk/lighting");
        assert_eq!(s(paths::light(&id, &LightId::new("key").unwrap())), "scenes/desk/lighting/lights/key");
        assert_eq!(s(paths::camera(&id, &CameraId::new("full").unwrap())), "scenes/desk/cameras/full");
        assert_eq!(s(paths::member(&id, &MemberId::new("kiri").unwrap())), "scenes/desk/cast/kiri");
        assert_eq!(s(paths::prop(&id, &PropId::new("mug").unwrap())), "scenes/desk/props/mug");
        assert_eq!(s(paths::weather(&id)), "scenes/desk/weather");
        let app = omuzaki_core::id::Slug::new("light-rig").unwrap();
        assert_eq!(s(paths::applied(&id, &app)), "scenes/desk/applied/light-rig");
        assert_eq!(s(paths::default_scene()), "project/default_scene");
    }

    #[test]
    fn limits_and_reference_height_are_the_documented_numbers() {
        assert_eq!((MAX_LIGHTS, MAX_CAMERAS, MAX_CAST, MAX_PROPS), (8, 16, 8, 128));
        assert!((REFERENCE_HEIGHT_M - 0.966).abs() < f32::EPSILON);
    }
}
