//! The built-in default scene every project starts with (design D11), and the scene a project opens in (D1).
//!
//! A project with no scene still renders: [`scene_for`] falls back to [`default_scene`], unsaved, so a viewport is never
//! black. `stage.create_scene` writes the same default as a real scene.

use omuzaki_char_api::{CharId, paths as char_paths};
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_ops_api::OpError;
use omuzaki_stage_api::{
    Backdrop, CameraDef, CameraId, CastMember, Framing, Ground, MemberId, PostDef, SceneDef, SceneId, SceneSettings,
    paths,
};

use crate::recipe::LightRig;

/// The default scene's id.
pub const DEFAULT_SCENE: &str = "main";
/// The light rig the default scene is lit by.
pub const DEFAULT_RIG: &str = "viewer-studio";
/// The cast member the default scene puts the default character in.
pub const DEFAULT_MEMBER: &str = "main";

/// `project/default_character`: the character a new scene casts.
#[must_use]
pub fn default_character_path() -> DocPath {
    DocPath::root().join("project").join("default_character")
}

/// The character a new scene casts (D11): `project.default_character` when it names a character of the project, else
/// the project's only character, else none.
#[must_use]
pub fn default_character(doc: &dyn DocRead) -> Option<CharId> {
    let named = doc
        .get(&default_character_path())
        .and_then(|v| v.as_str())
        .and_then(|s| CharId::new(s).ok())
        .filter(|id| doc.get(&char_paths::character(id)).is_some());
    if named.is_some() {
        return named;
    }
    let characters = doc.get(&DocPath::root().join("characters"))?.as_object()?;
    let mut ids = characters.keys();
    match (ids.next(), ids.next()) {
        (Some(only), None) => CharId::new(only.as_str()).ok(),
        _ => None,
    }
}

/// The default scene for `doc` (D11): "Main", lit by the studio rig against its own backdrop, a shadow ground at 0.25,
/// cameras `full` (three-quarter: yaw 30, pitch 6), `bust` and `face` (front), opening on `full`, bloom on with ACES,
/// and the [`default_character`] cast as `main` when there is one.
///
/// # Errors
/// `Internal` if the shipped studio rig doesn't load (a broken build, never a broken project).
pub fn default_scene(doc: &dyn DocRead) -> Result<SceneDef, OpError> {
    let internal = |e: &dyn std::fmt::Display| OpError::internal(format!("the default scene's rig: {e}"));
    let lighting = LightRig::load(DEFAULT_RIG).and_then(|rig| rig.to_lighting()).map_err(|e| internal(&e))?;
    let id = |s: &str| CameraId::new(s).map_err(|e| internal(&e));
    let mut settings = SceneSettings::named("Main");
    settings.backdrop = Backdrop::Lighting {};
    settings.ground = Ground::default();
    settings.post = PostDef::default();
    settings.camera = Some(id("full")?);
    let mut scene = SceneDef::new(settings);
    scene.lighting = lighting;
    let mut full = CameraDef::preset(Framing::Full);
    full.yaw = 30.0;
    full.pitch = 6.0;
    scene.cameras.insert(id("full")?, full);
    scene.cameras.insert(id("bust")?, CameraDef::preset(Framing::Bust));
    scene.cameras.insert(id("face")?, CameraDef::preset(Framing::Face));
    if let Some(character) = default_character(doc) {
        let member = MemberId::new(DEFAULT_MEMBER).map_err(|e| internal(&e))?;
        scene.cast.insert(member.clone(), CastMember::new(character));
        scene.scene.subject = Some(member);
    }
    Ok(scene)
}

/// The id of the scene `doc` opens in (D1): `project.default_scene` when it names a scene, else the first scene by id;
/// `None` for a project with no scene.
#[must_use]
pub fn opening_scene(doc: &dyn DocRead) -> Option<SceneId> {
    let named = doc
        .get(&paths::default_scene())
        .and_then(|v| v.as_str())
        .and_then(|s| SceneId::new(s).ok())
        .filter(|id| doc.get(&paths::scene(id)).is_some());
    named.or_else(|| {
        let scenes = doc.get(&DocPath::root().join("scenes"))?.as_object()?;
        scenes.keys().filter_map(|k| SceneId::new(k.as_str()).ok()).min_by(|a, b| a.as_str().cmp(b.as_str()))
    })
}

/// The scene to stage (D1): `scene` when given, else the one the project opens in, else the built-in default,
/// unsaved (its id is then `None`). It never writes.
///
/// # Errors
/// `NotFound` for a named scene the project doesn't have; `Invalid` for a scene node that doesn't read.
pub fn scene_for(doc: &dyn DocRead, scene: Option<&SceneId>) -> Result<(Option<SceneId>, SceneDef), OpError> {
    let id = match scene {
        Some(id) => {
            if doc.get(&paths::scene(id)).is_none() {
                return Err(OpError::not_found(format!("there is no scene '{id}'"))
                    .hint("stage.list_scenes lists the project's scenes"));
            }
            Some(id.clone())
        }
        None => opening_scene(doc),
    };
    let Some(id) = id else { return Ok((None, default_scene(doc)?)) };
    let node = doc.get(&paths::scene(&id)).ok_or_else(|| OpError::not_found(format!("there is no scene '{id}'")))?;
    let def = SceneDef::from_node(node)
        .map_err(|e| OpError::invalid("scene", format!("scene '{id}' doesn't read: {e}")))?;
    Ok((Some(id), def))
}
