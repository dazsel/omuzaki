//! One character in a stored scene (design D5, D9, D10): the scene's lighting, cameras, backdrop, ground and post look,
//! with the character as its only member and subject. A preview, a turnaround or a viewport shows "this character in
//! that scene" through it on an unsaved copy, so the document never changes.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, SceneLook, VariantId};
use omuzaki_core::glam::Mat4;
use omuzaki_render_api::{FilmGrain, Post};
use omuzaki_stage_api::{Anchor, CameraDef, CameraId, CastMember, Framing, MemberId, SceneDef, SceneError};

use super::landmarks::Landmarks;
use super::resolve::{CheckedScene, Figure, ResolvedScene};
use crate::error::StageError;

/// The member id a character takes when the scene has no subject to stand in for.
const ALONE: &str = "alone";

/// The camera a shot adds when it asks for a framing without naming a scene camera.
const SHOT: &str = "shot";

/// `def` with `character` as its only member: in the subject's place (its id, position and yaw) when the scene has a
/// subject, else at the stage centre. Lights and cameras anchored to another member follow the subject instead.
/// Returns the member's id.
///
/// # Errors
/// [`StageError::Scene`] if no member id can be made (never for a scene that passed its check).
pub fn cast_alone(def: &mut SceneDef, character: &CharId, variant: Option<&VariantId>) -> Result<MemberId, StageError> {
    let subject = def.scene.subject.clone().or_else(|| def.cast.keys().next().cloned());
    let stand = subject.as_ref().and_then(|id| def.cast.get(id)).map(|m| (m.position, m.yaw));
    let id = match subject {
        Some(id) => id,
        None => MemberId::new(ALONE).map_err(|e| StageError::Scene { source: SceneError::new("cast", e.to_string()) })?,
    };
    let mut member = CastMember::new(character.clone());
    member.variant = variant.cloned();
    if let Some((position, yaw)) = stand {
        member.position = position;
        member.yaw = yaw;
    }
    def.cast = BTreeMap::from([(id.clone(), member)]);
    def.scene.subject = Some(id.clone());
    let keep = Anchor::Member(id.clone());
    let follow = |anchor: &mut Anchor| {
        if matches!(anchor, Anchor::Member(_)) && *anchor != keep {
            *anchor = Anchor::Subject;
        }
    };
    for light in def.lighting.lights.values_mut() {
        follow(&mut light.anchor);
    }
    for camera in def.cameras.values_mut() {
        follow(&mut camera.subject);
        if let Some(anchor) = camera.anchor.as_mut() {
            follow(anchor);
        }
    }
    Ok(id)
}

/// Which camera of a scene a shot uses, and what it changes about it.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct ShotChoice {
    /// A camera of the scene; the opening one when left out.
    pub camera: Option<CameraId>,
    /// A preset framing: with `camera`, it replaces that camera's; without, the shot is a fresh camera of this framing
    /// from the front.
    pub framing: Option<Framing>,
    /// Degrees around the subject, replacing the camera's.
    pub yaw: Option<f32>,
    /// Degrees from above, replacing the camera's.
    pub pitch: Option<f32>,
    /// Zoom, replacing the camera's.
    pub zoom: Option<f32>,
}

impl ShotChoice {
    /// Makes the chosen camera the scene's opening camera, with the changes asked for, and returns its id. A scene with
    /// no camera gets a `full` one.
    ///
    /// # Errors
    /// [`SceneError`] naming `camera` for a camera the scene doesn't have (with the ones it has), or `framing` for a
    /// free framing (a shot frames by a preset; a free camera of the scene is shot as it is).
    pub fn apply(&self, def: &mut SceneDef) -> Result<CameraId, SceneError> {
        if self.framing == Some(Framing::Free) {
            return Err(SceneError::new("framing", "a shot frames by a preset: face, bust, full or hands"));
        }
        let shot = || CameraId::new(SHOT).map_err(|e| SceneError::new("camera", e.to_string()));
        let id = match (&self.camera, self.framing) {
            (Some(id), _) => {
                if !def.cameras.contains_key(id) {
                    let known: Vec<String> = def.cameras.keys().map(ToString::to_string).collect();
                    return Err(SceneError::new(
                        "camera",
                        format!("the scene has no camera '{id}'; it has: {}", known.join(", ")),
                    ));
                }
                id.clone()
            }
            (None, Some(framing)) => {
                let id = shot()?;
                def.cameras.insert(id.clone(), CameraDef::preset(framing));
                id
            }
            (None, None) => {
                if let Some(id) = def.scene.camera.clone().or_else(|| def.cameras.keys().next().cloned()) {
                    id
                } else {
                    let id = shot()?;
                    def.cameras.insert(id.clone(), CameraDef::preset(Framing::Full));
                    id
                }
            }
        };
        if let Some(camera) = def.cameras.get_mut(&id) {
            if let Some(framing) = self.framing {
                camera.framing = framing;
                camera.anchor = None;
                camera.position = None;
                camera.target = None;
            }
            camera.yaw = self.yaw.unwrap_or(camera.yaw);
            camera.pitch = self.pitch.unwrap_or(camera.pitch);
            camera.zoom = self.zoom.unwrap_or(camera.zoom);
        }
        def.scene.camera = Some(id.clone());
        Ok(id)
    }
}

/// A scene with one member resolved for the stage: what [`Stage::set_scene`](crate::frame::Stage::set_scene) takes.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct StagedScene {
    /// The scene in render-api values.
    pub resolved: ResolvedScene,
    /// Where the member stands: its position and yaw on the floor.
    pub subject_root: Mat4,
}

impl StagedScene {
    /// The camera the scene opens with, if it has one.
    #[must_use]
    pub fn camera(&self) -> Option<omuzaki_render_api::Camera> {
        self.resolved.active.and_then(|i| self.resolved.cameras.get(i)).map(|(_, camera)| *camera)
    }
}

/// Checks `def` and resolves it for a target of `aspect`, with `landmarks` (measured on the member's body in its bind
/// pose) as the figure of `member`, its only member.
///
/// # Errors
/// [`StageError::Scene`] for a scene that breaks a rule or has no member `member`; what
/// [`ResolvedScene::resolve_into`] refuses.
pub fn stage_alone(
    def: &SceneDef,
    member: &MemberId,
    landmarks: Landmarks,
    aspect: f32,
) -> Result<StagedScene, StageError> {
    let checked = CheckedScene::new(def)?;
    let cast = def
        .cast
        .get(member)
        .ok_or_else(|| StageError::Scene { source: SceneError::new("cast", format!("the scene has no member `{member}`")) })?;
    let figure = Figure::new(cast, landmarks);
    let mut resolved = ResolvedScene::new();
    resolved.resolve_into(checked, &BTreeMap::from([(member.clone(), figure)]), aspect)?;
    Ok(StagedScene { resolved, subject_root: Mat4::from(figure.root) })
}

/// `post` with a character's frame look over it (LK-05): bloom and depth of field switched off when the look says so (a
/// look that switches one on keeps the scene's, as only the scene's lighting and camera carry their settings), film
/// grain at the look's strength (0 removes it), and the look's colour grade. Unset fields keep the scene's.
pub fn post_in_look(post: &mut Post, look: &SceneLook) {
    if look.bloom == Some(false) {
        post.bloom = None;
    }
    if look.depth_of_field == Some(false) {
        post.depth_of_field = None;
    }
    if let Some(strength) = look.grain {
        post.grain = (strength > 0.0).then_some(FilmGrain { strength });
    }
    if let Some(grade) = look.grade.to_grade() {
        post.grade = Some(grade);
    }
}
