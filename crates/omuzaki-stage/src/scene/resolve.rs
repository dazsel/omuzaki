//! Resolve: a scene document to fixed-capacity render-api values in metres (anchors, subject units, angles to vectors).
//!
//! [`ResolvedScene::resolve_into`] is design D10 step 1 for the lights and the cameras: the lights go through
//! [`Lighting::resolve_into`](omuzaki_stage_api::Lighting::resolve_into), the cameras through [`frame`] or, for a free
//! camera, its own position and target. A resolve of an unchanged scene allocates nothing and says nothing changed.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Affine3A, Mat4, Vec3};
use omuzaki_render_api::{Camera, Projection};
use omuzaki_stage_api::resolve::{AnchorFrame, LightBuf};
use omuzaki_stage_api::{
    Anchor, CameraDef, CameraId, CastMember, Framing, MAX_CAMERAS, MemberId, SceneDef, SceneError,
};

use super::frame::frame;
use super::landmarks::Landmarks;
use crate::error::StageError;

/// The near plane of a free camera as a share of its distance to the target, with a floor (as in a preset's).
const FREE_NEAR_SHARE: f32 = 0.02;
const FREE_NEAR_FLOOR: f32 = 0.01;
/// A free camera's far plane lies this many subject heights behind its target.
const FREE_FAR_HEIGHTS: f32 = 10.0;

/// The frame of the subject when there is none: the 1 m figure at the origin facing +Z (design D4), which is also what
/// `Lighting::resolve_into` uses, and the frame of the stage, whose units are metres. Everything that needs the
/// subject's frame takes it from [`subject_frame`], so the fallbacks can't drift apart.
const UNIT_FRAME: AnchorFrame = AnchorFrame { root: Affine3A::IDENTITY, height_m: 1.0 };

/// The frame of the subject, or the 1 m figure at the origin without one (design D4).
fn subject_frame(subject: Option<&Figure>) -> AnchorFrame {
    subject.map_or(UNIT_FRAME, Figure::anchor_frame)
}

/// One visible cast member as resolve sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Figure {
    /// Local metres to world metres: the member's position and yaw, with no scale.
    pub root: Affine3A,
    /// The member's height in metres: the unit of its anchors.
    pub height_m: f32,
    /// What framing measured on the member in its bind pose.
    pub landmarks: Landmarks,
}

impl Figure {
    /// The figure of `member`: `root` is the translation `(position[0], 0, position[1])` times a rotation of `yaw`
    /// degrees about +Y, and `height_m` is `landmarks.height_m`.
    pub fn new(member: &CastMember, landmarks: Landmarks) -> Self {
        let [x, z] = member.position;
        let root = Affine3A::from_translation(Vec3::new(x, 0.0, z)) * Affine3A::from_rotation_y(member.yaw.to_radians());
        Self { root, height_m: landmarks.height_m, landmarks }
    }

    /// The stage-api frame of this figure's anchors.
    pub fn anchor_frame(&self) -> AnchorFrame {
        AnchorFrame { root: self.root, height_m: self.height_m }
    }
}

/// What the last resolve changed, so `Stage::set_scene` touches only that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Changes {
    /// A light, or the number of lights, changed.
    pub lights: bool,
    /// A camera, or which camera is active, changed.
    pub cameras: bool,
    /// A camera was added, removed or renamed. Apply re-diffs by id, and may allocate.
    pub structure: bool,
}

impl Changes {
    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// No structural change: apply allocates nothing.
    pub fn value_only(&self) -> bool {
        !self.structure
    }
}

/// A scene resolved into render-api values for one target aspect, reused across resolves.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedScene {
    /// The lights in world space: the ambient first, then the lights by id.
    pub lights: LightBuf,
    /// Every camera by id, framed for the aspect. Capacity [`MAX_CAMERAS`], reserved by `new`.
    pub cameras: Vec<(CameraId, Camera)>,
    /// The camera the scene opens with: `scene.camera`, else the first by id. `None` without cameras.
    pub active: Option<usize>,
}

impl Default for ResolvedScene {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolvedScene {
    /// An empty resolution, with room for [`MAX_CAMERAS`] cameras so resolving never grows the list.
    pub fn new() -> Self {
        Self { lights: LightBuf::new(), cameras: Vec::with_capacity(MAX_CAMERAS), active: None }
    }

    /// Resolves `def` for a target of `aspect` into `self` and says what changed. `figures` holds the visible members
    /// by id. The subject is `scene.subject`, else the first member by id.
    ///
    /// Nothing is written on an error, so `self` still holds the last scene that resolved.
    ///
    /// # Errors
    /// [`StageError::Scene`] for a lighting error (an unknown member anchor, too many lights), for more than
    /// [`MAX_CAMERAS`] cameras, for a `scene.camera` that isn't one of them, for a preset camera whose subject has no
    /// figure (path `cameras.<id>.subject`) and for a free camera without a position, a target or its anchor's figure.
    /// [`StageError::Framing`] from [`frame`].
    pub fn resolve_into(
        &mut self,
        def: &SceneDef,
        figures: &BTreeMap<MemberId, Figure>,
        aspect: f32,
    ) -> Result<Changes, StageError> {
        check_figures(figures)?;
        let subject = def.scene.subject.as_ref().map_or_else(|| figures.values().next(), |id| figures.get(id));
        let frame_of = |anchor: &Anchor| match anchor {
            Anchor::Stage => None,
            Anchor::Subject => Some(subject_frame(subject)),
            Anchor::Member(id) => figures.get(id).map(Figure::anchor_frame),
        };

        let mut lights = LightBuf::new();
        def.lighting.resolve_into(&frame_of, &mut lights).map_err(scene_error)?;

        if def.cameras.len() > MAX_CAMERAS {
            return Err(scene_error(SceneError::new(
                "cameras",
                format!("{} cameras are more than the {MAX_CAMERAS} a scene may have", def.cameras.len()),
            )));
        }
        let mut cameras = [Camera::default(); MAX_CAMERAS];
        for (slot, (id, camera)) in cameras.iter_mut().zip(&def.cameras) {
            *slot = resolve_camera(id, camera, subject, figures, aspect)?;
        }
        let resolved = &cameras[..def.cameras.len()];
        let active = active_camera(def)?;

        let mut changes = Changes::default();
        if lights != self.lights {
            self.lights = lights;
            changes.lights = true;
        }
        let same_ids = self.cameras.len() == def.cameras.len() && self.cameras.iter().zip(&def.cameras).all(|(a, (b, _))| a.0 == *b);
        if same_ids {
            for (stored, camera) in self.cameras.iter_mut().zip(resolved) {
                if stored.1 != *camera {
                    stored.1 = *camera;
                    changes.cameras = true;
                }
            }
        } else {
            self.cameras.clear();
            self.cameras.extend(def.cameras.keys().cloned().zip(resolved.iter().copied()));
            changes.cameras = true;
            changes.structure = true;
        }
        if active != self.active {
            self.active = active;
            changes.cameras = true;
        }
        Ok(changes)
    }
}

fn scene_error(source: SceneError) -> StageError {
    StageError::Scene { source }
}

/// The index of the camera the scene opens with: `scene.camera`, else the first.
fn active_camera(def: &SceneDef) -> Result<Option<usize>, StageError> {
    let Some(wanted) = &def.scene.camera else { return Ok((!def.cameras.is_empty()).then_some(0)) };
    def.cameras.keys().position(|id| id == wanted).map(Some).ok_or_else(|| {
        scene_error(SceneError::new("scene.camera", format!("`{wanted}` isn't one of the scene's cameras")))
    })
}

/// One camera in world space.
fn resolve_camera(
    id: &CameraId,
    camera: &CameraDef,
    subject: Option<&Figure>,
    figures: &BTreeMap<MemberId, Figure>,
    aspect: f32,
) -> Result<Camera, StageError> {
    let figure = match &camera.subject {
        Anchor::Subject => subject,
        Anchor::Member(member) => figures.get(member),
        Anchor::Stage => None,
    };
    if camera.framing == Framing::Free {
        return free_camera(id, camera, subject, figures);
    }
    let figure = figure.ok_or_else(|| {
        scene_error(SceneError::new(
            format!("cameras.{id}.subject"),
            format!("`{}` has no visible figure to frame", camera.subject),
        ))
    })?;
    let mut framed = frame(camera.framing, &figure.landmarks, camera, aspect)?;
    framed.transform = Mat4::from(figure.root) * framed.transform;
    Ok(framed)
}

/// A free camera: `position` and `target` in its anchor's units (metres for the stage; a member's height otherwise),
/// moved by the anchor's root. Yaw, pitch, zoom and shift belong to presets and are not applied.
fn free_camera(
    id: &CameraId,
    camera: &CameraDef,
    subject: Option<&Figure>,
    figures: &BTreeMap<MemberId, Figure>,
) -> Result<Camera, StageError> {
    let anchor = camera.anchor.as_ref().unwrap_or(&Anchor::Stage);
    let missing = |field: &str| scene_error(SceneError::new(format!("cameras.{id}.{field}"), "a free camera needs it"));
    let (position, target) = (camera.position.ok_or_else(|| missing("position"))?, camera.target.ok_or_else(|| missing("target"))?);
    let frame = match anchor {
        Anchor::Stage => UNIT_FRAME,
        Anchor::Subject => subject_frame(subject),
        Anchor::Member(member) => figures.get(member).map(Figure::anchor_frame).ok_or_else(|| {
            scene_error(SceneError::new(
                format!("cameras.{id}.anchor"),
                format!("`{anchor}` has no visible figure to measure from"),
            ))
        })?,
    };
    let (root, unit) = (frame.root, frame.height_m);
    let place = |p: [f32; 3]| root.transform_point3(Vec3::from_array(p) * unit);
    let (eye, at) = (place(position), place(target));
    if !(eye.is_finite() && at.is_finite() && camera.fov.is_finite() && camera.fov > 0.0 && camera.fov < 180.0) {
        return Err(StageError::Framing {
            reason: format!("camera '{id}': the position, target or fov ({}) isn't a usable number", camera.fov),
        });
    }
    let distance = eye.distance(at);
    if distance <= 0.0 {
        return Err(scene_error(SceneError::new(
            format!("cameras.{id}.target"),
            "a free camera's target is where it stands, so there is no direction to look in",
        )));
    }
    let height = subject_frame(subject).height_m;
    let projection = Projection::Perspective {
        fov_y_deg: camera.fov,
        near: (FREE_NEAR_SHARE * distance).max(FREE_NEAR_FLOOR),
        far: distance + FREE_FAR_HEIGHTS * height,
    };
    let placed = Camera::look_at(eye, at, Vec3::Y, projection);
    placed.validate().map_err(|e| StageError::Framing { reason: format!("camera '{id}' isn't usable: {e}") })?;
    Ok(placed)
}

/// Refuses a figure whose placement or height isn't usable: the height scales every anchored length and the lights'
/// intensities, so a zero or non-finite one would spread through the whole resolve.
fn check_figures(figures: &BTreeMap<MemberId, Figure>) -> Result<(), StageError> {
    match figures.iter().find(|(_, f)| !(f.height_m.is_finite() && f.height_m > 0.0 && f.root.is_finite())) {
        None => Ok(()),
        Some((id, f)) => Err(scene_error(SceneError::new(
            format!("cast.{id}"),
            format!("the figure's height {} m and placement must be finite, with a height above zero", f.height_m),
        ))),
    }
}
