//! The turnaround sheet: the character from the front, three-quarter, side and back, in the rest pose or a hero pose,
//! as print-quality cells (EX-10).
//!
//! Every view uses the same camera, lights, pose and settled simulation; the character turns on the spot, so the
//! lighting is identical in all four and hair that was settled under one yaw is settled under the others. A cell is
//! framed by the character's height (crown to sole plus a margin), so `px_per_m` is the same in every view, and the
//! [`Framing`] of each view says where the head and the eye line landed, for the hair trace tool and the character
//! builder to reuse.
//!
//! # Tiles
//! A cell larger than the renderer's `max_texture_size` (or [`TurnaroundSpec::with_max_tile`]) is rendered in tiles and
//! stitched. A tile is a sub-window of the cell's frustum, which the camera has no field for (`docs/design/render-api.md`
//! leaves it to a loop above the API): an [orthographic](Lens::Orthographic) camera moved sideways and up is exactly such
//! a window, so tiles of that lens meet without a seam. A perspective sub-window would need an off-axis camera, so
//! [`Lens::Telephoto`] refuses to tile ([`TurnaroundError::TilingNeedsOrthographic`]).

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_anim_api::Tick;
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Image, Light, Post, Projection, Quality, RenderError, Renderer, ResourceRef,
    Scene, TargetDesc, TargetId,
};

use crate::error::StageError;
use crate::frame::face::SETTLE_STEPS;
use crate::frame::{FrameInput, Stage, TimePin};
use crate::recipe::{LightRig, RecipeError};

/// The long lens' vertical field of view, degrees: far enough to look like a drawing, near enough to keep depth.
pub const TELEPHOTO_FOV_DEG: f32 = 12.0;
/// Empty space above the crown and below the soles, as a fraction of the character's height.
pub const MARGIN: f32 = 0.06;
/// Where the eye line sits between the head joint and the crown, as a fraction of that distance. A stand-in until
/// `Landmarks::measure` (#131) finds the eyes on the character.
pub const EYE_LINE_FRACTION: f32 = 0.6;
/// Where the head centre sits between the head joint and the crown, as a fraction of that distance. A stand-in until
/// `Landmarks::measure` (#131) measures the head.
pub const HEAD_CENTRE_FRACTION: f32 = 0.5;
/// The most bytes one view's cell may take (RGBA8): a 300 dpi A4 page is 35 MB, so this is room for a poster while a
/// mistyped size is refused before it asks the allocator for terabytes.
pub const MAX_CELL_BYTES: u64 = 1 << 30;
/// The one moment every view of a sheet is drawn at: time zero, and no frame time. The stage's time is pinned to it for
/// the whole render ([`Stage::pin_time`]), so anything that reads the frame's time (the face painter, the frame info
/// that seeds sample jitter) sees the same time in all four views, whatever the stage clock is and whatever ran before.
pub const SHEET_TICK: Tick = Tick { index: 0, time: Seconds(0.0), dt: 0.0 };
/// The light rig every sheet is lit with. The sheet always uses it, replacing whatever lights the stage's scene had
/// and every other render setting of the stage's scene (post, background, fog, environment light, outline light and
/// the rest, which the sheet draws from a fresh scene); [`render_turnaround`] puts the caller's settings back afterwards.
pub const LIGHT_RIG: &str = "viewer-studio";

/// Which way the character faces the camera. The side and three-quarter views show the character's left.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum View {
    /// Facing the camera.
    Front,
    /// Turned 45 degrees.
    ThreeQuarter,
    /// Turned 90 degrees.
    Side,
    /// Facing away.
    Back,
}

impl View {
    /// The four views, in sheet order.
    pub const ALL: [Self; 4] = [Self::Front, Self::ThreeQuarter, Self::Side, Self::Back];

    /// How far the character is turned about the vertical axis, degrees; negative turns its left side to the camera.
    pub fn yaw_deg(self) -> f32 {
        match self {
            Self::Front => 0.0,
            Self::ThreeQuarter => -45.0,
            Self::Side => -90.0,
            Self::Back => 180.0,
        }
    }
}

/// The pose a sheet shows.
#[derive(Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum SheetPose {
    /// The character's own base pose, its bind (the A-pose it was sculpted in).
    #[default]
    Rest,
    /// A confident stance, [`hero_pose`].
    Hero,
    /// Any pose, in the normalised space of [`HumanoidPose`]: what the pose library (RG-06) will hand a sheet. The
    /// shot is framed by the bind-pose height, so a pose that leaves that frame (sitting, jumping, a raised arm) is
    /// refused with [`TurnaroundError::Unsupported`], never clipped or reframed; posed framing is follow-up #1854.
    Custom(Box<HumanoidPose>),
}

impl SheetPose {
    /// A sheet in `pose`.
    pub fn custom(pose: HumanoidPose) -> Self {
        Self::Custom(Box::new(pose))
    }
}

/// The lens a sheet is shot with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Lens {
    /// A perspective lens of [`TELEPHOTO_FOV_DEG`]. `px_per_m` is exact on the plane through the character's axis.
    Telephoto,
    /// No perspective: `px_per_m` is exact everywhere and a cell can be tiled without a seam.
    #[default]
    Orthographic,
}

/// A confident stance built in code until the pose library (RG-06) exists: feet apart, hands near the hips, chest
/// lifted and chin up. It is symmetric left to right and starts from `bind`, so a character sculpted with other limbs
/// keeps its hips offset.
pub fn hero_pose(bind: &HumanoidPose) -> HumanoidPose {
    let z = |degrees: f32| Quat::from_rotation_z(degrees.to_radians());
    let mut pose = *bind;
    // Arms hang away from the body (T-pose arms lie along the X axis, so a turn about Z lowers them) and bend at
    // the elbow toward the hips; the right side mirrors the left.
    for (sign, arm, forearm, leg) in [
        (1.0, HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftUpperLeg),
        (-1.0, HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::RightUpperLeg),
    ] {
        pose.set(arm, z(-sign * 38.0));
        pose.set(forearm, z(-sign * 32.0));
        pose.set(leg, z(sign * 4.0));
    }
    pose.set(HumanoidBone::Chest, Quat::from_rotation_x((-3.0_f32).to_radians()));
    pose.set(HumanoidBone::Head, Quat::from_rotation_x((-4.0_f32).to_radians()));
    pose
}

/// What to render.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TurnaroundSpec {
    /// One view's size in pixels: the page cell at 300 dpi.
    pub cell: UVec2,
    /// The pose.
    pub pose: SheetPose,
    /// The lens.
    pub lens: Lens,
    /// The background; transparent by default so the sheet layout can place the views on its own paper.
    pub background: Background,
    /// Steps the hair and cloth settle for after the character is turned and before it is drawn.
    pub settle_steps: u32,
    /// The largest tile side, if smaller than the renderer's `max_texture_size`.
    pub max_tile: Option<u32>,
}

impl TurnaroundSpec {
    /// A sheet of `cell`-sized views: rest pose, orthographic lens, transparent background, settled for [`SETTLE_STEPS`].
    pub fn new(cell: UVec2) -> Self {
        Self {
            cell,
            pose: SheetPose::Rest,
            lens: Lens::Orthographic,
            background: Background::Transparent,
            settle_steps: SETTLE_STEPS,
            max_tile: None,
        }
    }

    /// The same sheet in `pose`.
    #[must_use]
    pub fn with_pose(mut self, pose: SheetPose) -> Self {
        self.pose = pose;
        self
    }

    /// The same sheet shot with `lens`.
    #[must_use]
    pub fn with_lens(mut self, lens: Lens) -> Self {
        self.lens = lens;
        self
    }

    /// The same sheet with tiles of at most `side` pixels, whatever the renderer allows.
    #[must_use]
    pub fn with_max_tile(mut self, side: u32) -> Self {
        self.max_tile = Some(side);
        self
    }

    /// Checks the spec, the one place its values are: a cell with pixels that fits [`MAX_CELL_BYTES`] and a tile side
    /// of at least one pixel.
    ///
    /// # Errors
    /// [`TurnaroundError::Invalid`] naming the first value that is unusable.
    pub fn validate(&self) -> Result<(), TurnaroundError> {
        if self.cell.x == 0 || self.cell.y == 0 {
            return Err(TurnaroundError::Invalid(format!("the cell is {}x{} pixels; it needs at least 1x1", self.cell.x, self.cell.y)));
        }
        if cell_bytes(self.cell).is_none() {
            return Err(TurnaroundError::Invalid(format!(
                "a {}x{} cell is more than the {MAX_CELL_BYTES} bytes a view may take",
                self.cell.x, self.cell.y
            )));
        }
        if self.max_tile == Some(0) {
            return Err(TurnaroundError::Invalid("the largest tile side is 0; it needs at least 1 pixel".to_owned()));
        }
        Ok(())
    }

    /// The same sheet settled for `steps` steps.
    #[must_use]
    pub fn with_settle_steps(mut self, steps: u32) -> Self {
        self.settle_steps = steps;
        self
    }
}

/// Why a turnaround could not be rendered.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TurnaroundError {
    /// The stage or the renderer failed.
    #[error(transparent)]
    Stage(#[from] StageError),
    /// The light rig could not be built.
    #[error("the sheet's light rig: {0}")]
    Lights(#[from] RecipeError),
    /// The cell is empty, or the character has no height to frame.
    #[error("{0}")]
    Invalid(String),
    /// The pose, as drawn, reaches outside the frame the shot was made for, so the sheet would be clipped. The shot is
    /// framed by the character's bind-pose height; framing a pose that sits, jumps or reaches is follow-up #1854.
    #[error("{0}")]
    Unsupported(String),
    /// The cell needs tiles but the lens is a perspective one, whose sub-windows are off-axis.
    #[error("a {cell}-pixel cell needs tiles of at most {tile} pixels, which only the orthographic lens can stitch without a seam")]
    TilingNeedsOrthographic {
        /// The cell.
        cell: UVec2,
        /// The largest tile side.
        tile: u32,
    },
}

/// Where the character landed in a view's cell. Rows count down from the top edge, columns right from the left edge,
/// both in pixels at the pixel's centre convention (the middle of the top-left pixel is `(0.5, 0.5)`).
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Framing {
    /// Pixels per metre of the character, at its axis.
    pub px_per_m: f32,
    /// The row of the eye line ([`EYE_LINE_FRACTION`] of the way from the head joint to the crown).
    pub eye_line_row: f32,
    /// The head's centre ([`HEAD_CENTRE_FRACTION`] of the way from the head joint to the crown).
    pub head_centre: Vec2,
    /// The row of the lowest point of the picture: every vertex as drawn (skinned by the pose, deformed by the settled
    /// hair and cloth, turned with the view) projected through the view's camera, so in a perspective lens a vertex
    /// nearer the camera counts where it lands.
    pub ground_row: f32,
    /// The row of the highest point of the picture, found the same way as [`ground_row`](Self::ground_row).
    pub crown_row: f32,
}

/// One rendered view.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TurnaroundView {
    /// Which view.
    pub view: View,
    /// The cell, straight alpha.
    pub image: Image,
    /// Where things landed in it.
    pub framing: Framing,
}

/// The four views of one pose.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TurnaroundSheet {
    /// The pose shown.
    pub pose: SheetPose,
    /// The views, in [`View::ALL`] order.
    pub views: Vec<TurnaroundView>,
}

/// The vertical extent of the character in the pose it was built in, and its head joint: the facts framing needs.
struct Subject {
    sole: f32,
    crown: f32,
    head_joint: Vec3,
}

impl Subject {
    fn of(built: &BuiltCharacter) -> Result<Self, TurnaroundError> {
        let (sole, crown) = built
            .meshes
            .iter()
            .flat_map(|m| m.mesh.geometry.positions.iter())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        if crown.partial_cmp(&sole) != Some(std::cmp::Ordering::Greater) {
            return Err(TurnaroundError::Invalid("the character has no meshes to frame".to_owned()));
        }
        let head = built
            .rig
            .skeleton
            .humanoid_bone(HumanoidBone::Head)
            .ok_or_else(|| TurnaroundError::Invalid("the character has no head bone".to_owned()))?;
        let bind_head = built.rig.inverse_bind().get(head.index()).map(|m| m.inverse().transform_point3(Vec3::ZERO));
        let head_joint = bind_head.ok_or_else(|| TurnaroundError::Invalid("the head bone has no bind".to_owned()))?;
        Ok(Self { sole, crown, head_joint })
    }
}

/// The camera of a view, and the maths that puts world points in its cell.
struct Shot {
    camera: Camera,
    cell: UVec2,
    px_per_m: f32,
}

impl Shot {
    /// Frames `subject` so its height plus [`MARGIN`] fills the cell's height, looking along -Z at its mid-height.
    fn new(subject: &Subject, spec: &TurnaroundSpec) -> Self {
        let framed = (subject.crown - subject.sole) * (1.0 + 2.0 * MARGIN);
        let mid = Vec3::new(0.0, f32::midpoint(subject.crown, subject.sole), 0.0);
        let distance = 0.5 * framed / (0.5 * TELEPHOTO_FOV_DEG.to_radians()).tan();
        let (near, far) = (0.5 * distance, 2.0 * distance);
        let projection = match spec.lens {
            Lens::Telephoto => Projection::Perspective { fov_y_deg: TELEPHOTO_FOV_DEG, near, far },
            Lens::Orthographic => Projection::Orthographic { height: framed, near, far },
        };
        let camera = Camera::look_at(mid + Vec3::new(0.0, 0.0, distance), mid, Vec3::Y, projection);
        Self { camera, cell: spec.cell, px_per_m: spec.cell.as_vec2().y / framed }
    }

    /// The pixel of the cell a world point lands on, through the renderer's own projection.
    fn pixel(&self, world: Vec3) -> Vec2 {
        let cell = self.cell.as_vec2();
        let clip = (self.camera.projection_matrix(cell.x / cell.y) * self.camera.view()) * world.extend(1.0);
        let ndc = clip.truncate().truncate() / clip.w;
        Vec2::new((ndc.x * 0.5 + 0.5) * cell.x, (0.5 - ndc.y * 0.5) * cell.y)
    }

    /// The camera for the tile `[origin, origin + size)` of the cell: moved so its frustum is that window.
    fn tile_camera(&self, origin: UVec2, size: UVec2) -> Camera {
        let Projection::Orthographic { height, near, far } = self.camera.projection else { return self.camera };
        let (cell, size) = (self.cell.as_vec2(), size.as_vec2());
        let shift = (origin.as_vec2() + 0.5 * size - 0.5 * cell) / self.px_per_m;
        let right = self.camera.transform.x_axis.truncate();
        let up = self.camera.transform.y_axis.truncate();
        let mut camera = self.camera;
        camera.transform.w_axis += (right * shift.x - up * shift.y).extend(0.0);
        camera.projection = Projection::Orthographic { height: height * size.y / cell.y, near, far };
        camera
    }
}

/// Renders the four views of `spec.pose` of the character on `stage`.
///
/// The stage ticks once first, so a staged face variant is in before anything is measured. The stage's scene is
/// borrowed for the sheet (camera, lights from [`LIGHT_RIG`], background, and every other setting at its default) and
/// its settings are put back after.
///
/// Each view ticks the character to its yaw and pose, settles the hair and cloth from rest (so no view depends on the
/// views before it or on what the stage did earlier), redraws to upload the settled meshes, and then draws offline from
/// that scene, tile by tile where the cell is too big for one target. The tick and the redraw also draw a throwaway
/// frame into the stage's own target.
///
/// The stage's time is pinned to [`SHEET_TICK`] for the whole render, so no view depends on the stage clock or on
/// earlier frames, and afterwards the stage's last frame (tick, root, pose, goals) is put back; what the hair and cloth
/// settled to stays.
///
/// # Errors
/// [`TurnaroundError`] for an unusable spec ([`TurnaroundSpec::validate`]), a character with nothing to frame, a
/// perspective cell that needs tiles, or a stage or renderer failure. The scene's settings are restored and the
/// sheet's target released either way.
pub fn render_turnaround(
    stage: &mut Stage,
    renderer: &mut dyn Renderer,
    spec: &TurnaroundSpec,
) -> Result<TurnaroundSheet, TurnaroundError> {
    spec.validate()?;
    let (lights, look) = LightRig::load(LIGHT_RIG).and_then(|rig| Ok((rig.to_lights()?, rig.post(false))))?;
    let mut scope = SheetScope::enter(stage, renderer, spec.background, lights, look)?;
    let sheet = draw_sheet(&mut scope, spec);
    let closed = scope.close();
    match (sheet, closed) {
        (Ok(sheet), Ok(())) => Ok(sheet),
        (Ok(_), Err(e)) => Err(e.into()),
        // The drawing failure is the one to report; a failed cleanup behind it is logged, as the stage does.
        (Err(error), closed) => {
            if let Err(cleanup) = closed {
                tracing::warn!(%cleanup, "releasing the turnaround sheet's target failed too");
            }
            Err(error)
        }
    }
}

/// The RGBA8 byte length of a cell, or `None` if it overflows or passes [`MAX_CELL_BYTES`].
fn cell_bytes(cell: UVec2) -> Option<usize> {
    let bytes = u64::from(cell.x).checked_mul(u64::from(cell.y))?.checked_mul(4)?;
    if bytes > MAX_CELL_BYTES {
        return None;
    }
    usize::try_from(bytes).ok()
}

/// Everything the sheet changes on the stage and the renderer, given back on every way out of
/// [`render_turnaround`]: by [`close`](Self::close), or by `Drop` on an early return or a panic. One guard, so no exit
/// path has a hand-written restore to forget. It restores:
/// - the scene's render settings: `camera`, `lights`, `background`, `fog`, `environment`, `outline_light` and `post`
///   ([`restore_settings`]);
/// - the stage's last frame: its pin (an outer caller's, if any), `tick`, `root`, `pose` and `goals`, and with them the
///   rig, the scene's `instances` and `joints`, and `frame` ([`Stage::unpin_time`]);
/// - the face: the sheet's frames paint the neutral face, so the caller's last channels are painted again at the
///   caller's last tick, into the face rig the stage has after the sheet (a variant may have swapped in);
/// - the sheet's own target, released.
///
/// What it does not give back is what the hair and cloth settled to.
struct SheetScope<'a> {
    stage: &'a mut Stage,
    renderer: &'a mut dyn Renderer,
    target: TargetId,
    saved: Option<Scene>,
    pin: Option<TimePin>,
    /// The face channels and tick of the caller's last frame, if it painted a face.
    face: Option<(omuzaki_anim_api::ChannelBuf, omuzaki_anim_api::Tick)>,
}

impl<'a> SheetScope<'a> {
    /// Makes the sheet's target, swaps in the neutral scene and pins the stage's time.
    fn enter(
        stage: &'a mut Stage,
        renderer: &'a mut dyn Renderer,
        background: Background,
        lights: Vec<Light>,
        look: Post,
    ) -> Result<Self, StageError> {
        // What can fail without touching the stage comes first, so an error here leaves the caller's scene as it was.
        let target = renderer
            .create_target(&TargetDesc { size: UVec2::splat(1), alpha: AlphaEncoding::Straight })
            .map_err(|e| StageError::render("the sheet's target", e))?;
        let saved = stage.scene().clone();
        let face = stage.face_state();
        *stage.scene_mut() = neutral_scene(&saved, Camera::default(), lights, look, background);
        let pin = stage.pin_time(SHEET_TICK);
        Ok(Self { stage, renderer, target, saved: Some(saved), pin: Some(pin), face })
    }

    /// Gives everything back, once.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses to release the target, or what repainting the caller's face
    /// reports; the rest is restored regardless.
    fn restore(&mut self) -> Result<(), StageError> {
        let (Some(saved), Some(pin)) = (self.saved.take(), self.pin.take()) else { return Ok(()) };
        self.stage.unpin_time(&pin);
        let face = match self.face.take() {
            Some((channels, tick)) => self.stage.repaint_face(self.renderer, channels, tick),
            None => Ok(()),
        };
        let released = self.renderer.release(ResourceRef::from(self.target));
        restore_settings(self.stage.scene_mut(), saved);
        released.map_err(|e| StageError::render("the sheet's target", e)).and(face)
    }

    /// Gives everything back and reports a failed release.
    fn close(mut self) -> Result<(), StageError> {
        self.restore()
    }
}

impl Drop for SheetScope<'_> {
    fn drop(&mut self) {
        if let Err(cleanup) = self.restore() {
            tracing::warn!(%cleanup, "giving the stage back after the turnaround sheet failed");
        }
    }
}

/// Draws the four views from the stage's neutral scene.
fn draw_sheet(scope: &mut SheetScope<'_>, spec: &TurnaroundSpec) -> Result<TurnaroundSheet, TurnaroundError> {
    let target = scope.target;
    let (stage, renderer): (&mut Stage, &mut dyn Renderer) = (&mut *scope.stage, &mut *scope.renderer);
    // A face variant staged before the call swaps in at the next tick, and with it the character's meshes and rig, so
    // that tick comes first and everything the sheet measures is read from the character it will draw.
    stage.tick(renderer, &FrameInput::default())?;
    let subject = Subject::of(stage.built())?;
    let shot = Shot::new(&subject, spec);
    let tile = tile_side(renderer, spec);
    if matches!(spec.lens, Lens::Telephoto) && (spec.cell.x > tile || spec.cell.y > tile) {
        return Err(TurnaroundError::TilingNeedsOrthographic { cell: spec.cell, tile });
    }
    let pose = match &spec.pose {
        SheetPose::Rest => stage.built().rig.bind,
        SheetPose::Hero => hero_pose(&stage.built().rig.bind),
        SheetPose::Custom(pose) => **pose,
    };
    stage.scene_mut().camera = shot.camera;
    let run = Run { spec, subject, shot, pose, target, tile };
    let views = View::ALL.into_iter().map(|view| run.view(stage, renderer, view)).collect::<Result<Vec<_>, _>>()?;
    Ok(TurnaroundSheet { pose: spec.pose.clone(), views })
}

/// The scene a sheet is drawn from: built fresh, so nothing of the caller's look can reach it. Every render setting is
/// the sheet's own or the [`Scene::new`] default (`frame`, `fog`, `environment` and `outline_light`; a new field of
/// `Scene` is neutral here until the sheet sets it). Only what belongs to the character is carried over: its instances,
/// joint palette and morph weights, which the next tick refills anyway.
fn neutral_scene(saved: &Scene, camera: Camera, lights: Vec<Light>, post: Post, background: Background) -> Scene {
    Scene {
        lights,
        post,
        background,
        instances: saved.instances.clone(),
        joints: saved.joints.clone(),
        morph_weights: saved.morph_weights.clone(),
        ..Scene::new(camera)
    }
}

/// Puts the caller's render settings back on the stage's scene. The scene itself is not swapped back: a face variant
/// that swapped in during the sheet replaced the character's instances and joint palette, and the caller's copies would
/// name meshes that are gone. The destructuring lists every field, so a new `Scene` field must be placed here.
fn restore_settings(scene: &mut Scene, saved: Scene) {
    let Scene {
        frame: _,
        camera,
        lights,
        instances: _,
        joints: _,
        morph_weights: _,
        background,
        fog,
        environment,
        outline_light,
        post,
    } = saved;
    scene.camera = camera;
    scene.lights = lights;
    scene.background = background;
    scene.fog = fog;
    scene.environment = environment;
    scene.outline_light = outline_light;
    scene.post = post;
}

/// The largest tile side: the renderer's limit, lowered by the spec.
fn tile_side(renderer: &dyn Renderer, spec: &TurnaroundSpec) -> u32 {
    let limit = renderer.capabilities().max_texture_size;
    spec.max_tile.map_or(limit, |side| side.min(limit))
}

/// What every view of one sheet shares.
struct Run<'a> {
    spec: &'a TurnaroundSpec,
    subject: Subject,
    shot: Shot,
    pose: HumanoidPose,
    target: TargetId,
    tile: u32,
}

impl Run<'_> {
    /// Turns the character to `view`, settles it and draws the cell.
    fn view(&self, stage: &mut Stage, renderer: &mut dyn Renderer, view: View) -> Result<TurnaroundView, TurnaroundError> {
        let root = Mat4::from_rotation_y(view.yaw_deg().to_radians());
        let input = FrameInput { pose: self.pose, root, ..FrameInput::default() };
        // The tick turns the character and sets its pose; the settle then starts from rest, so a view depends on the
        // pose and the step count alone, not on the views before it or on frames the caller ran; the redraw uploads the
        // settled meshes and draws.
        stage.tick(renderer, &input)?;
        stage.settle_from_rest(self.spec.settle_steps);
        stage.redraw(renderer, &input)?;
        let drawn = stage.drawn_vertices();
        self.check_fits(view, &drawn)?;
        let image = self.tiles(stage, renderer)?;
        Ok(TurnaroundView { view, image, framing: framing(stage, &self.subject, &self.shot, root, &drawn) })
    }

    /// Refuses a view whose drawn vertices leave the cell vertically: the shot is framed by the bind-pose height, so
    /// a pose that sits, jumps or reaches can fall outside it, and a clipped sheet is never returned. The width is the
    /// cell's own: the shot frames height, and how wide a figure fits is the caller's cell shape.
    fn check_fits(&self, view: View, drawn: &[Vec3]) -> Result<(), TurnaroundError> {
        let (min, max) = pixel_bounds(drawn, &self.shot);
        let rows = self.shot.cell.as_vec2().y;
        if min.y < 0.0 || max.y > rows {
            return Err(TurnaroundError::Unsupported(format!(
                "the {view:?} view's pose spans rows {:.0} to {:.0}, outside the {} rows of the cell the shot was framed \
                 for from the bind pose's height; posed framing is follow-up #1854",
                min.y, max.y, self.shot.cell.y
            )));
        }
        Ok(())
    }

    /// Draws the stage's current scene into the cell, in tiles of at most `self.tile` pixels, and stitches them.
    fn tiles(&self, stage: &mut Stage, renderer: &mut dyn Renderer) -> Result<Image, TurnaroundError> {
        let quality = Quality::offline();
        let cell = self.shot.cell;
        let bytes = cell_bytes(cell).ok_or_else(|| TurnaroundError::Invalid(format!("the cell is more than {MAX_CELL_BYTES} bytes")))?;
        let mut rgba8 = vec![0_u8; bytes];
        for y0 in (0..cell.y).step_by(self.tile as usize) {
            for x0 in (0..cell.x).step_by(self.tile as usize) {
                let origin = UVec2::new(x0, y0);
                let size = UVec2::new(self.tile.min(cell.x - x0), self.tile.min(cell.y - y0));
                stage.scene_mut().camera =
                    if size == cell { self.shot.camera } else { self.shot.tile_camera(origin, size) };
                let drawn = renderer
                    .resize_target(self.target, size)
                    .and_then(|()| renderer.render(self.target, stage.scene(), &quality))
                    .and_then(|_| renderer.read_target(self.target))
                    .map_err(|e| sheet_error(&e))?;
                copy_rows(&mut rgba8, cell, origin, &drawn)?;
            }
        }
        stage.scene_mut().camera = self.shot.camera;
        Image::new(cell, AlphaEncoding::Straight, rgba8).map_err(|e| sheet_error(&e))
    }
}

/// Copies a tile's rows into the cell at `origin`.
fn copy_rows(cell_rgba: &mut [u8], cell: UVec2, origin: UVec2, part: &Image) -> Result<(), TurnaroundError> {
    let row = part.size.x as usize * 4;
    for (y, src) in part.rgba8.chunks_exact(row).enumerate() {
        let start = ((origin.y as usize + y) * cell.x as usize + origin.x as usize) * 4;
        cell_rgba
            .get_mut(start..start + row)
            .ok_or_else(|| TurnaroundError::Invalid("a tile falls outside its cell".to_owned()))?
            .copy_from_slice(src);
    }
    Ok(())
}

fn sheet_error(error: &RenderError) -> TurnaroundError {
    StageError::render("a turnaround tile", error.clone()).into()
}

/// Where the soles, crown, head centre and eye line landed, from the posed head joint and the view's projection.
fn framing(stage: &Stage, subject: &Subject, shot: &Shot, root: Mat4, drawn: &[Vec3]) -> Framing {
    let head = stage.built().rig.skeleton.humanoid_bone(HumanoidBone::Head).and_then(|id| stage.skin().get(id.index()));
    let skin = head.copied().unwrap_or(Mat4::IDENTITY);
    let joint = skin.transform_point3(subject.head_joint);
    let up = skin.transform_vector3(Vec3::Y);
    let along = |fraction: f32| root.transform_point3(joint + up * ((subject.crown - subject.head_joint.y) * fraction));
    let head_centre = shot.pixel(along(HEAD_CENTRE_FRACTION));
    let (min, max) = pixel_bounds(drawn, shot);
    let (crown_row, ground_row) = (min.y, max.y);
    Framing {
        px_per_m: shot.px_per_m,
        eye_line_row: shot.pixel(along(EYE_LINE_FRACTION)).y,
        head_centre,
        ground_row,
        crown_row,
    }
}

/// The smallest box, in cell pixels, around every drawn vertex projected through the view's camera: the rows and columns
/// the picture's geometry spans, in every lens (see [`Stage::drawn_vertices`]).
fn pixel_bounds(drawn: &[Vec3], shot: &Shot) -> (Vec2, Vec2) {
    drawn.iter().map(|p| shot.pixel(*p)).fold((Vec2::INFINITY, Vec2::NEG_INFINITY), |(lo, hi), px| (lo.min(px), hi.max(px)))
}
