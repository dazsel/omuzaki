//! Resolve: a scene document to fixed-capacity render-api values in metres (anchors, subject units, angles to vectors).
//!
//! [`ResolvedScene::resolve_into`] is design D10 step 1: the lights go through
//! [`Lighting::resolve_into`](omuzaki_stage_api::Lighting::resolve_into), the cameras through [`frame`] or, for a free
//! camera, its own position and target. It also resolves the backdrop, the post look (with the depth-of-field focus
//! measured from the active camera), the outline light, the ground and the skin floor. A resolve of an unchanged
//! scene allocates nothing and says nothing changed.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Affine3A, Mat3A, Mat4, Vec3};
use omuzaki_render_api::{
    BackdropFit, Background, Bloom, Camera, Colour, DepthOfField, FilmGrain, Fog, Light, Occlusion, Post,
    Projection,
};
use omuzaki_stage_api::resolve::{AnchorFrame, LightBuf};
use omuzaki_stage_api::{
    Anchor, Backdrop, Bars, CameraDef, CameraId, CastMember, EnvironmentSource, Focus, Framing, GroundKind, Lighting,
    MAX_CAMERAS, MemberId, SceneDef, SceneError, Smears, Squash, Timing,
};

use super::backdrop::ImagePixels;
use super::environment::HdriPixels;
use super::frame::frame;
use super::landmarks::{Bounds, Landmarks};
use crate::effects::weather::SceneEffects;
use crate::props::staged::ResolvedProps;
use crate::error::StageError;

/// The near plane of a free camera as a share of its distance to the target, with a floor (as in a preset's).
const FREE_NEAR_SHARE: f32 = 0.02;
const FREE_NEAR_FLOOR: f32 = 0.01;
/// A free camera's far plane lies this many subject heights behind its target.
const FREE_FAR_HEIGHTS: f32 = 10.0;

/// How far past the farthest member an unset ground radius reaches, in subject units (the design's D2 ground fit, in the subject's units as D4 has it).
const GROUND_MARGIN: f32 = 0.83;

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
        let root =
            Affine3A::from_translation(Vec3::new(x, 0.0, z)) * Affine3A::from_rotation_y(member.yaw.to_radians());
        Self { root, height_m: landmarks.height_m, landmarks }
    }

    /// The stage-api frame of this figure's anchors.
    pub fn anchor_frame(&self) -> AnchorFrame {
        AnchorFrame { root: self.root, height_m: self.height_m }
    }
}

/// A scene that has passed [`SceneDef::check`] (design D13), so resolving it can rely on every rule of the contract.
///
/// The check allocates, so it runs once per edit here and not inside [`ResolvedScene::resolve_into`], which stays
/// allocation-free for a scene that hasn't changed. Holding the scene by reference means an edit needs a new proof.
#[derive(Clone, Copy, Debug)]
pub struct CheckedScene<'a>(&'a SceneDef);

impl<'a> CheckedScene<'a> {
    /// Checks `def` against every rule that doesn't need the rest of the project.
    ///
    /// # Errors
    /// [`StageError::Scene`] with the first rule `def` breaks.
    pub fn new(def: &'a SceneDef) -> Result<Self, StageError> {
        def.check().map_err(scene_error)?;
        Ok(Self(def))
    }

    /// The checked scene.
    pub fn def(&self) -> &'a SceneDef {
        self.0
    }
}

/// What the last resolve changed, so `Stage::set_scene` touches only that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
// One flag per part of the scene is the contract: apply reads each to touch only what changed.
#[allow(clippy::struct_excessive_bools)]
pub struct Changes {
    /// A light, or the number of lights, changed.
    pub lights: bool,
    /// A camera, or which camera is active, changed.
    pub cameras: bool,
    /// A camera was added, removed or renamed. Apply re-diffs by id, and may allocate.
    pub structure: bool,
    /// The background changed.
    pub backdrop: bool,
    /// The post settings changed (the exposure, the tone map, bloom, depth of field, grain, grade or occlusion).
    pub post: bool,
    /// The ground changed. Apply may recreate its mesh, so this is not a value-only change.
    pub ground: bool,
    /// The skin-floor override, or the outline light, changed.
    pub skin_floor: bool,
    /// The fog changed.
    pub fog: bool,
    /// The environment changed: its source, or how it is used (intensity, turn, toon ramp).
    pub environment: bool,
    /// The environment's source changed (a sky's colours, another HDRI, one added or removed). Apply re-creates it,
    /// so this is not a value-only change.
    pub sky: bool,
    /// The backdrop's picture changed (another asset, one added or removed). Apply uploads it, so this is not a
    /// value-only change; its fit and bars are values.
    pub image: bool,
    /// What the effects take from the scene changed: its weather, the subject's unit, the weather's floor bounds.
    pub effects: bool,
    /// A prop moved, turned, scaled, hid or showed: values only.
    pub props: bool,
    /// A prop was added, removed or rebuilt (a kind or a param changed). Apply uploads it, so this is not a value-only
    /// change.
    pub prop_builds: bool,
    /// The animated timing changed.
    pub timing: bool,
    /// The squash and stretch changed.
    pub squash: bool,
    /// The smear frames changed.
    pub smears: bool,
}

impl Changes {
    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Only values changed: apply allocates nothing. A ground change is not one, because apply recreates or releases
    /// the ground-disc mesh (#274), nor is a change of the environment's source or the backdrop's picture, which apply
    /// re-creates.
    pub fn value_only(&self) -> bool {
        !(self.structure || self.ground || self.sky || self.image || self.prop_builds)
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
    /// What shows behind the cast.
    pub background: Background,
    /// The post look: the post settings with the lighting's exposure, and its bloom when the scene turns it on.
    pub post: Post,
    /// Ambient occlusion and contact shadows (the post settings' `occlusion`).
    pub occlusion: Option<Occlusion>,
    /// Scales outline colours (the lighting's `outline_light`).
    pub outline_light: f32,
    /// How strongly lights behind the cast edge its silhouette (the lighting's `back_light_rim`).
    pub back_light_rim: f32,
    /// How soft shadows are (the lighting's `shadow_softness`).
    pub shadow_softness: f32,
    /// The ground under the cast.
    pub ground: ResolvedGround,
    /// Overrides every cast skin material's shadow floor while set.
    pub skin_floor: Option<f32>,
    /// Distance fog, in metres from the camera.
    pub fog: Option<Fog>,
    /// Image-based light, and the backdrop's sky when the backdrop is the environment.
    pub environment: Option<ResolvedEnvironment>,
    /// The pixels of the environment's HDRI, once loaded ([`load_hdri`](Self::load_hdri)); a resolve keeps them while
    /// the asset stays the same.
    pub hdri: Option<HdriPixels>,
    /// The backdrop's picture, for an image backdrop. Until its texture is up, `background` shows its bars.
    pub backdrop_image: Option<ResolvedImage>,
    /// The picture's pixels, once loaded ([`load_backdrop`](Self::load_backdrop)); a resolve keeps them while the asset
    /// stays the same.
    pub image: Option<ImagePixels>,
    /// What the stage's effects take from the scene: its weather in metres, the subject's unit and the floor bounds
    /// `"auto"` weather falls over (particles.md D10).
    pub effects: SceneEffects,
    /// The props: their builds, where each is drawn, and their lights (already in `lights`).
    pub props: ResolvedProps,
    /// How the cast moves in time: every frame, or held on twos or threes (`scene.timing`).
    pub timing: Timing,
    /// How the cast's bodies squash and stretch (`scene.squash`).
    pub squash: Squash,
    /// How the cast's fast moves smear (`scene.smears`).
    pub smears: Smears,
}

/// An image backdrop as the renderer takes it: the picture's asset, how it fits, and the bars' gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedImage {
    /// The picture's content hash in the asset store.
    pub asset: omuzaki_store_api::ContentHash,
    /// How it meets the frame.
    pub fit: BackdropFit,
    /// The bars' colour at the frame's top edge.
    pub top: Colour,
    /// The bars' colour at its bottom edge.
    pub bottom: Colour,
}

impl ResolvedImage {
    /// `asset`, covering the frame, with black bars.
    pub fn new(asset: omuzaki_store_api::ContentHash) -> Self {
        Self { asset, fit: BackdropFit::Cover, top: Colour::BLACK, bottom: Colour::BLACK }
    }
}

/// The scene's environment as the renderer takes it: where its light comes from and how a frame uses it.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedEnvironment {
    /// A sky's colours or an HDRI's asset.
    pub source: EnvironmentSource,
    /// Brightness, `>= 0`.
    pub intensity: f32,
    /// The sky's turn about the vertical, in degrees.
    pub rotation_deg: f32,
    /// Whether its diffuse light goes through the toon ramp.
    pub toon_ramp: bool,
}

impl ResolvedEnvironment {
    /// `source` at full strength, unturned, through the toon ramp.
    pub fn new(source: EnvironmentSource) -> Self {
        Self { source, intensity: 1.0, rotation_deg: 0.0, toon_ramp: true }
    }

    /// The HDRI's asset, for an HDRI.
    pub fn hdri(&self) -> Option<omuzaki_store_api::ContentHash> {
        match self.source {
            EnvironmentSource::Hdri { asset } => Some(asset),
            EnvironmentSource::Sky { .. } => None,
        }
    }
}

/// The ground under the cast, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedGround {
    /// Which ground.
    pub kind: GroundKind,
    /// How dark the shadow is, `0..=1`.
    pub opacity: f32,
    /// The disc's radius.
    pub radius_m: f32,
    /// A floor's colour.
    pub colour: Colour,
}

impl Default for ResolvedGround {
    fn default() -> Self {
        Self { kind: GroundKind::default(), opacity: 0.0, radius_m: 0.0, colour: omuzaki_stage_api::Ground::FLOOR }
    }
}

impl Default for ResolvedScene {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolvedScene {
    /// An empty resolution, with room for [`MAX_CAMERAS`] cameras so resolving never grows the list.
    pub fn new() -> Self {
        Self {
            lights: LightBuf::new(),
            cameras: Vec::with_capacity(MAX_CAMERAS),
            active: None,
            background: Background::default(),
            post: Post::default(),
            occlusion: None,
            outline_light: 1.0,
            back_light_rim: 0.0,
            shadow_softness: 0.0,
            ground: ResolvedGround::default(),
            skin_floor: None,
            fog: None,
            environment: None,
            hdri: None,
            backdrop_image: None,
            image: None,
            effects: SceneEffects::default(),
            props: ResolvedProps::default(),
            timing: Timing::SMOOTH,
            squash: Squash::OFF,
            smears: Smears::OFF,
        }
    }

    /// Lets go of the pixels of an HDRI or a backdrop picture the scene no longer uses, here, off the render thread.
    fn release_unused(&mut self) {
        if self.hdri.as_ref().map(HdriPixels::asset) != self.environment.as_ref().and_then(ResolvedEnvironment::hdri) {
            self.hdri = None;
        }
        if self.image.as_ref().map(ImagePixels::asset) != self.backdrop_image.map(|i| i.asset) {
            self.image = None;
        }
    }

    /// Resolves `def` for a target of `aspect` into `self` and says what changed. `figures` holds the visible members
    /// by id. The subject is `scene.subject`, else the first member by id.
    ///
    /// `scene` proves that `def` passed [`SceneDef::check`], so a hand-built or deserialised scene is held to the
    /// same rules as an edited one.
    ///
    /// Nothing is written on an error, so `self` still holds the last scene that resolved.
    ///
    /// # Errors
    /// [`StageError::Scene`] for a figure whose root isn't a floor translation (`y = 0`) and a yaw, whose height isn't finite and above
    /// 0 or whose landmarks aren't finite (path `cast.<id>`), for a light, the depth of field or the ground radius
    /// that comes out non-finite (a huge position; paths `lighting.lights`, `scene.post.depth_of_field`,
    /// `scene.ground`), for a cast whose subject has no figure (path `scene.subject`; an empty cast
    /// uses the 1 m figure at the origin instead), for a lighting error (an unknown member anchor, too many lights), for more than
    /// [`MAX_CAMERAS`] cameras, for a `scene.camera` that isn't one of them, for a preset camera whose subject has no
    /// figure (path `cameras.<id>.subject`), for a free camera without a position, a target or its anchor's figure,
    /// and for depth of field focused on the subject when there is no subject figure or no camera to measure from.
    /// [`StageError::Framing`] from [`frame`], and for a preset or free camera that isn't usable once placed in the
    /// world (a non-finite transform, a far plane at or below the near).
    pub fn resolve_into(
        &mut self,
        scene: CheckedScene<'_>,
        figures: &BTreeMap<MemberId, Figure>,
        aspect: f32,
    ) -> Result<Changes, StageError> {
        let def = scene.def();
        // Everything below applies a figure's root, height and landmarks; an unusable one fails here, once, and
        // `finite_or_error` below catches what even valid figures can overflow into.
        for (id, figure) in figures {
            check_figure(id, figure)?;
        }
        // The default subject is the first member of the cast, visible or not: a hidden one leaves the subject
        // unavailable rather than handing its role to the next member.
        let subject_id = def.scene.subject.as_ref().or_else(|| def.cast.keys().next());
        let subject = subject_id.and_then(|id| figures.get(id));
        // The 1 m figure at the origin (D4) stands in for the subject only when the cast is empty. With a cast, an
        // unavailable subject would otherwise move the lights and cameras and rescale the ground without a word.
        if let (Some(id), None) = (subject_id, subject) {
            return Err(scene_error(SceneError::new("scene.subject", format!("`{id}` has no visible figure"))));
        }
        let frame_of = |anchor: &Anchor| match anchor {
            Anchor::Stage => None,
            Anchor::Subject => subject.map(Figure::anchor_frame),
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
            // A preset is validated by `frame` in local space and a free camera is built here; either can still be
            // unusable in the world (an overflowing distance, a far plane at or below the near).
            slot.validate().map_err(|e| StageError::Framing { reason: format!("camera '{id}': {e}") })?;
        }
        let resolved = &cameras[..def.cameras.len()];
        let active = active_camera(def)?;
        let unit = subject.map_or(1.0, |f| f.height_m);
        let post = resolve_post(def, subject, unit, active.map(|i| cameras[i]))?;
        let environment = def.lighting.environment.map(|e| ResolvedEnvironment {
            source: e.source,
            intensity: e.intensity,
            rotation_deg: e.rotation,
            toon_ramp: e.toon_ramp,
        });
        let (background, backdrop_image) = resolve_backdrop(def, environment.is_some());
        let ground = resolve_ground(def, figures, unit);
        // Fog is in subject units from the camera (D4), so a rig fogs every stature alike.
        let fog = def.lighting.fog.map(|f| Fog { colour: f.colour, near: f.near * unit, far: f.far * unit });
        finite_or_error(&lights, &post, &ground)?;
        if fog.is_some_and(|f| !(f.near.is_finite() && f.far.is_finite() && f.far > f.near)) {
            return Err(scene_error(SceneError::new("lighting.fog", "the fog's distances aren't usable numbers")));
        }

        // After every step that can fail: the props are written into `self` here.
        let prop_changes = self.props.resolve_into(def, figures, subject_id, &mut lights);
        let active_camera = active.map(|i| cameras[i]);
        let effects = SceneEffects::resolve(
            def,
            figures,
            subject_id.zip(subject),
            active_camera.as_ref(),
            aspect,
        );
        let mut changes = Changes {
            effects: replace(&mut self.effects, effects),
            backdrop: replace(&mut self.background, background),
            post: replace(&mut self.post, post) | replace(&mut self.occlusion, occlusion_of(def)),
            ground: replace(&mut self.ground, ground),
            skin_floor: self.replace_look(&def.lighting),
            fog: replace(&mut self.fog, fog),
            sky: self.environment.map(|e| e.source) != environment.map(|e| e.source),
            environment: replace(&mut self.environment, environment),
            image: self.backdrop_image.map(|i| i.asset) != backdrop_image.map(|i| i.asset),
            props: prop_changes.placements,
            prop_builds: prop_changes.builds,
            timing: replace(&mut self.timing, def.scene.timing),
            squash: replace(&mut self.squash, def.scene.squash),
            smears: replace(&mut self.smears, def.scene.smears),
            ..Changes::default()
        };
        changes.backdrop |= replace(&mut self.backdrop_image, backdrop_image);
        self.release_unused();
        if lights != self.lights {
            self.lights = lights;
            changes.lights = true;
        }
        let same_ids = self.cameras.len() == def.cameras.len()
            && self.cameras.iter().zip(&def.cameras).all(|(a, (b, _))| a.0 == *b);
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

/// Tolerance for the shape of a figure's root.
const ROOT_TOLERANCE: f32 = 1e-4;

/// True when `root` is a translation along the floor and a yaw (`Figure::root`'s contract): finite, with orthonormal
/// axes, a determinant of +1, the vertical axis still +Y (a pitch or a roll is refused) and no lift, since a cast
/// member's root is `(x, 0, z)` and stands on the ground.
fn is_translation_and_yaw(root: &Affine3A) -> bool {
    let axes = root.matrix3;
    root.is_finite()
        && root.translation.y.abs() <= ROOT_TOLERANCE
        && (axes.transpose() * axes).abs_diff_eq(Mat3A::IDENTITY, ROOT_TOLERANCE)
        && (axes.determinant() - 1.0).abs() <= ROOT_TOLERANCE
        && axes.y_axis.abs_diff_eq(Vec3::Y.into(), ROOT_TOLERANCE)
}

fn bounds_finite(bounds: &Bounds) -> bool {
    bounds.min.is_finite() && bounds.max.is_finite()
}

/// True when every number in `landmarks` is finite.
fn landmarks_finite(landmarks: &Landmarks) -> bool {
    let all_bounds = [Some(landmarks.body), landmarks.head, landmarks.bust, landmarks.hands];
    let singles = [Some(landmarks.height_m), landmarks.eyes, landmarks.chest];
    all_bounds.iter().flatten().all(bounds_finite) && singles.iter().flatten().all(|v| v.is_finite())
}

/// The one check of a figure, before anything is derived from it: a finite root of translation and yaw only, a finite
/// height above 0, and finite landmarks.
fn check_figure(id: &MemberId, figure: &Figure) -> Result<(), StageError> {
    let refuse = |why: &str| Err(scene_error(SceneError::new(format!("cast.{id}"), why)));
    if !is_translation_and_yaw(&figure.root) {
        return refuse("the figure's root must be a translation along the floor (y = 0) and a yaw rotation");
    }
    if !(figure.height_m.is_finite() && figure.height_m > 0.0) {
        return refuse("the figure's height must be finite and above 0");
    }
    if !landmarks_finite(&figure.landmarks) {
        return refuse("the figure's landmarks must be finite");
    }
    Ok(())
}

/// True when every number of `light` is finite and a range is above 0, which the render contract requires: a range
/// of `1e-30` times a height of `1e-30` underflows to 0 here, after the scene check has passed it.
fn light_usable(light: &Light) -> bool {
    match *light {
        Light::Directional { to_light, intensity, .. } => to_light.is_finite() && intensity.is_finite(),
        Light::Hemisphere { intensity, .. } => intensity.is_finite(),
        Light::Point { position, intensity, range, .. } => {
            position.is_finite() && intensity.is_finite() && range.is_finite() && range > 0.0
        }
        Light::Spot { position, direction, intensity, range, cone_outer_deg, penumbra, .. } => {
            position.is_finite()
                && direction.is_finite()
                && [intensity, range, cone_outer_deg, penumbra].iter().all(|v| v.is_finite())
                && range > 0.0
        }
        // `Light` is not non-exhaustive today; a new kind must be added here, not waved through.
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// The last gate before `Ok`: a valid figure can still overflow a derived number (a huge position, a long distance),
/// so the lights, the post settings and the ground are checked here and the previous scene is kept if not.
/// The cameras are checked as they are built.
fn finite_or_error(lights: &LightBuf, post: &Post, ground: &ResolvedGround) -> Result<(), StageError> {
    let unusable =
        |path: &str, what: &str| Err(scene_error(SceneError::new(path, format!("{what} isn't a finite number"))));
    if !lights.as_slice().iter().all(light_usable) {
        return Err(scene_error(SceneError::new(
            "lighting.lights",
            "a resolved light isn't a finite number, or its range came out at or below 0",
        )));
    }
    // The renderer refuses a post out of its ranges (`Post::validate`), so a scene that resolves to one (a subject
    // focus behind the camera, a derived number that overflowed) fails here and keeps the previous scene instead.
    if let Err(error) = post.validate() {
        let reason = match error {
            omuzaki_render_api::RenderError::Invalid { reason, .. } => reason,
            other => other.to_string(),
        };
        return Err(scene_error(SceneError::new("scene.post", reason)));
    }
    if !(ground.radius_m.is_finite() && ground.opacity.is_finite()) {
        return unusable("scene.ground", "the ground radius");
    }
    Ok(())
}

/// Stores `new` in `slot`; true if that changed it.
impl ResolvedScene {
    /// Takes the lighting's skin floor, outline light, back rim and shadow softness; whether any changed.
    fn replace_look(&mut self, l: &Lighting) -> bool {
        replace(&mut self.skin_floor, l.skin_floor)
            | replace(&mut self.outline_light, l.outline_light)
            | replace(&mut self.back_light_rim, l.back_light_rim)
            | replace(&mut self.shadow_softness, l.shadow_softness)
    }
}

fn replace<T: PartialEq>(slot: &mut T, new: T) -> bool {
    let changed = *slot != new;
    if changed {
        *slot = new;
    }
    changed
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
        return free_camera(id, camera, subject, figures).map(|free| rolled(free, camera.roll));
    }
    let figure = figure.ok_or_else(|| {
        scene_error(SceneError::new(
            format!("cameras.{id}.subject"),
            format!("`{}` has no visible figure to frame", camera.subject),
        ))
    })?;
    let mut framed = frame(camera.framing, &figure.landmarks, camera, aspect)?;
    framed.transform = Mat4::from(figure.root) * framed.transform;
    Ok(rolled(framed, camera.roll))
}

/// `camera` turned `roll` degrees about its own view axis: + turns the picture clockwise (the camera itself turns
/// counter-clockwise as it looks). Where it stands and what it looks at stay.
fn rolled(mut camera: Camera, roll: f32) -> Camera {
    if roll != 0.0 {
        // The camera looks down its local -Z, so a turn about +Z is counter-clockwise as seen through it.
        camera.transform *= Mat4::from_rotation_z(roll.to_radians());
    }
    camera
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
    let (position, target) =
        (camera.position.ok_or_else(|| missing("position"))?, camera.target.ok_or_else(|| missing("target"))?);
    // Without a subject the anchor is the 1 m figure at the origin (D4), as for the lights.
    let unit_frame = (Affine3A::IDENTITY, 1.0);
    let (root, unit) = match anchor {
        Anchor::Stage => unit_frame,
        Anchor::Subject => subject.map_or(unit_frame, |f| (f.root, f.height_m)),
        Anchor::Member(member) => figures.get(member).map(|f| (f.root, f.height_m)).ok_or_else(|| {
            scene_error(SceneError::new(
                format!("cameras.{id}.anchor"),
                format!("`{anchor}` has no visible figure to measure from"),
            ))
        })?,
    };
    let place = |p: [f32; 3]| root.transform_point3(Vec3::from_array(p) * unit);
    let (eye, at) = (place(position), place(target));
    if !(eye.is_finite() && at.is_finite() && camera.fov.is_finite() && camera.fov > 0.0 && camera.fov < 180.0) {
        return Err(StageError::Framing {
            reason: format!("camera '{id}': the position, target or fov ({}) isn't a usable number", camera.fov),
        });
    }
    let distance = eye.distance(at);
    // The 1 m figure of D4 when there is no subject, the same fallback the anchors use.
    let height = subject.map_or(1.0, |f| f.height_m);
    let projection = Projection::Perspective {
        fov_y_deg: camera.fov,
        near: (FREE_NEAR_SHARE * distance).max(FREE_NEAR_FLOOR),
        far: distance + FREE_FAR_HEIGHTS * height,
    };
    let framed = Camera::look_at(eye, at, Vec3::Y, projection);
    Ok(framed)
}

/// The render-api post: the lighting's exposure and bloom (when the scene switches it on) with the scene's own
/// settings. A subject focus is the depth, along the view axis of `active`, of the middle of the subject's head box, else its body box.
fn resolve_post(
    def: &SceneDef,
    subject: Option<&Figure>,
    unit: f32,
    active: Option<Camera>,
) -> Result<Post, StageError> {
    let (l, p) = (&def.lighting, &def.scene.post);
    let depth_of_field = p
        .depth_of_field
        .map(|dof| {
            let focus_distance = match dof.focus {
                Focus::Metres(metres) => metres,
                Focus::Subject(_) => {
                    let (figure, camera) = subject.zip(active).ok_or_else(|| {
                        scene_error(SceneError::new(
                            "scene.post.depth_of_field.focus",
                            "focusing on the subject needs a visible subject and a camera",
                        ))
                    })?;
                    let aim = figure.landmarks.head.unwrap_or(figure.landmarks.body).centre();
                    // The shader compares depth along the view axis, not the distance to the eye.
                    -camera.view().transform_point3(figure.root.transform_point3(aim)).z
                }
            };
            Ok(DepthOfField {
                bokeh: dof.bokeh,
                bokeh_threshold: dof.bokeh_threshold.unwrap_or(DepthOfField::BOKEH_THRESHOLD),
                ..DepthOfField::new(focus_distance, dof.range * unit, dof.max_blur)
            })
        })
        .transpose()?;
    Ok(Post {
        exposure: l.exposure,
        tonemap: p.tonemap,
        bloom: l.bloom.filter(|_| p.bloom).map(Bloom::from),
        depth_of_field,
        grain: p.grain.map(|strength| FilmGrain { strength }),
        grade: p.grade.map(Into::into),
        stylize: p.stylize.unwrap_or_default(),
        glare: p.glare,
    })
}

/// What shows behind the cast, and the picture the stage lays there for an image backdrop.
fn resolve_backdrop(def: &SceneDef, environment: bool) -> (Background, Option<ResolvedImage>) {
    let image = match &def.scene.backdrop {
        Backdrop::Image { asset, fit, bars } => {
            let rig = Bars { top: def.lighting.background, bottom: def.lighting.background };
            let Bars { top, bottom } = bars.unwrap_or(rig);
            Some(ResolvedImage { asset: *asset, fit: *fit, top, bottom })
        }
        _ => None,
    };
    let background = match &def.scene.backdrop {
        Backdrop::Environment {} if environment => Background::Environment,
        // Without an environment the check reports it and the rig's colour shows.
        Backdrop::Lighting {} | Backdrop::Environment {} => Background::Solid { colour: def.lighting.background },
        // Shown as authored, so a green screen keys at exactly its colour whatever the exposure and tone curve.
        Backdrop::Colour { colour } => Background::Gradient { top: *colour, bottom: *colour },
        Backdrop::Transparent {} => Background::Transparent,
        Backdrop::Gradient { top, bottom } => Background::Gradient { top: *top, bottom: *bottom },
        // The bars until the stage has the picture up; the stage shows the picture over them from then on.
        Backdrop::Image { .. } => {
            image.map_or(Background::Transparent, |i| Background::Gradient { top: i.top, bottom: i.bottom })
        }
    };
    (background, image)
}

/// The ground in metres: the radius in subject units, else the farthest member plus [`GROUND_MARGIN`].
fn resolve_ground(def: &SceneDef, figures: &BTreeMap<MemberId, Figure>, unit: f32) -> ResolvedGround {
    let g = &def.scene.ground;
    let radius_m = g.radius.map_or_else(
        || {
            let farthest =
                figures.values().map(|f| f.root.translation.x.hypot(f.root.translation.z)).fold(0.0, f32::max);
            farthest + GROUND_MARGIN * unit
        },
        |r| r * unit,
    );
    ResolvedGround {
        kind: g.kind,
        opacity: g.opacity,
        radius_m,
        colour: g.colour.unwrap_or(omuzaki_stage_api::Ground::FLOOR),
    }
}

/// The scene's ambient occlusion, if it asks for any.
fn occlusion_of(def: &SceneDef) -> Option<Occlusion> {
    def.scene.post.occlusion.map(|o| Occlusion::new(o.radius, o.strength).with_contact(o.contact))
}
