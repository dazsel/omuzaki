//! Stills: stage a built character on a renderer, settle it, render one picture per shot, and encode the picture as a
//! PNG. Every `render.*` op renders through [`render_stills`] and [`encode_png`]; the ops add the asset store and the
//! op results, nothing here knows about them.

use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::UVec2;
use omuzaki_face_api::ExpressionId;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, EdgeLines, FrameReport, Image, Light, Occlusion, Post, Quality, RenderError, Renderer, Scene,
    TargetDesc, TargetId,
};

use crate::avatar::AvatarStage;
use crate::body::Body;
use crate::error::StageError;
use crate::frame::face::{FacePoints, FaceRig, SETTLE_STEPS};
use crate::frame::pose::seed_base_pose;
use crate::frame::{FrameInput, Stage, StageClock};
use crate::scene::alone::StagedScene;
use crate::upload::UploadOptions;

/// The stage clock's rate for stills: a still has no real time, so the clock only has to be fixed and nonzero.
const STILL_FPS: f32 = 60.0;

/// The light and look of a still: what the render-api `Scene` takes besides the camera and the character.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct StillLight {
    /// The lights, in the order the scene takes them.
    pub lights: Vec<Light>,
    /// What is behind the character.
    pub background: Background,
    /// The post settings (bloom and the like).
    pub post: Post,
    /// How strongly the outline is lit; `1.0` is the scene default.
    pub outline_light: f32,
    /// How strongly lights behind the character edge its silhouette; `0.0` is the scene default.
    pub back_light_rim: f32,
    /// Lines where the depth jumps, if any.
    pub edges: Option<EdgeLines>,
    /// Ambient occlusion and contact shadows, if any.
    pub occlusion: Option<Occlusion>,
}

impl StillLight {
    /// A light with the scene's default outline light (`1.0`, as `Scene::new` sets it).
    #[must_use]
    pub fn new(lights: Vec<Light>, background: Background, post: Post) -> Self {
        Self { lights, background, post, outline_light: 1.0, back_light_rim: 0.0, edges: None, occlusion: None }
    }

    /// The same light with `outline_light` set.
    #[must_use]
    pub fn with_outline_light(mut self, outline_light: f32) -> Self {
        self.outline_light = outline_light;
        self
    }

    /// The same light with lines where the depth jumps.
    #[must_use]
    pub fn with_edges(mut self, edges: Option<EdgeLines>) -> Self {
        self.edges = edges;
        self
    }

    /// The same light with ambient occlusion and contact shadows, or without.
    #[must_use]
    pub fn with_occlusion(mut self, occlusion: Option<Occlusion>) -> Self {
        self.occlusion = occlusion;
        self
    }

    /// The same light with `back_light_rim` set.
    #[must_use]
    pub fn with_back_light_rim(mut self, back_light_rim: f32) -> Self {
        self.back_light_rim = back_light_rim;
        self
    }
}

/// What every still of one call shares.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct StillSpec {
    /// The light and look.
    pub light: StillLight,
    /// Target size, pixels.
    pub size: UVec2,
    /// The render quality of every still.
    pub quality: Quality,
    /// A stored scene the character stands in (its ground, backdrop, lights and post look, and where the subject
    /// stands); `None` for `light` alone, with the character at the origin.
    pub scene: Option<StagedScene>,
    /// The pose every still shows; `None` for the body's own (as sculpted, or an imported avatar at ease).
    pub pose: Option<HumanoidPose>,
}

impl StillSpec {
    /// A spec of `size` pixels, lit by `light`, rendered at `quality`.
    #[must_use]
    pub fn new(light: StillLight, size: UVec2, quality: Quality) -> Self {
        Self { light, size, quality, scene: None, pose: None }
    }

    /// A spec of `size` pixels in `scene`: its light and look, its ground disc and the subject's place on the floor.
    #[must_use]
    pub fn in_scene(scene: StagedScene, size: UVec2, quality: Quality) -> Self {
        let r = &scene.resolved;
        let light = StillLight::new(r.lights.as_slice().to_vec(), r.background, r.post)
            .with_outline_light(r.outline_light)
            .with_back_light_rim(r.back_light_rim)
            .with_occlusion(r.occlusion);
        Self { light, size, quality, scene: Some(scene), pose: None }
    }

    /// The same spec with every still in `pose`, when there is one.
    #[must_use]
    pub fn with_pose(mut self, pose: Option<HumanoidPose>) -> Self {
        self.pose = pose;
        self
    }
}

/// One picture: a camera and the face's expression.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Shot {
    /// Where the camera is.
    pub camera: Camera,
    /// An expression of the character's face profile at a weight; `None` is the neutral face.
    pub expression: Option<(ExpressionId, f32)>,
}

impl Shot {
    /// A shot of the neutral face from `camera`.
    #[must_use]
    pub fn new(camera: Camera) -> Self {
        Self { camera, expression: None }
    }

    /// The same shot with the face showing `expression` at `weight`.
    #[must_use]
    pub fn with_expression(mut self, expression: ExpressionId, weight: f32) -> Self {
        self.expression = Some((expression, weight));
        self
    }
}

/// One rendered still.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Still {
    /// The picture, as the renderer reads it back.
    pub image: Image,
    /// The renderer's report for the frame that made the picture.
    pub report: FrameReport,
    /// Where the painted face's eyes, mouth and chin are in the picture; `None` without a painted face (an imported
    /// avatar) or when it isn't visible ([`Stage::face_points`]).
    pub face: Option<FacePoints>,
}

/// Stages `built` on `renderer` once, settles it, and renders one still per shot, in order.
///
/// Settling runs once, so every still shows the character after the same warm-up; a shot changes only the camera and
/// the expression. Nothing the call creates is left on `renderer`, on success or on failure.
///
/// # Errors
/// `StageError::Render` (`RenderError::Invalid`) for no shots or a zero side in `spec.size`, before anything is
/// created; `StageError::Face` for an expression the face profile doesn't have, or any expression on a character
/// without a face; any error of staging or rendering. Whatever the call created is released before it returns.
pub fn render_stills(
    renderer: &mut dyn Renderer,
    built: BuiltCharacter,
    spec: &StillSpec,
    shots: &[Shot],
) -> Result<Vec<Still>, StageError> {
    check(spec, shots)?;
    let target = renderer
        .create_target(&TargetDesc { size: spec.size, alpha: AlphaEncoding::Straight })
        .map_err(|source| StageError::render("the still's target", source))?;
    let outcome = shoot_on(renderer, target, built, spec, shots);
    let released = renderer.release(target.into()).map_err(|source| StageError::render("the still's target", source));
    first_error(outcome, released)
}

/// [`render_stills`] for either kind of body: a kit character as there, an imported avatar staged, its spring bones
/// settled under its base pose, and shot. An imported avatar has no face profile, so a shot with an expression is
/// refused; its own expressions are driven by channels, which `render.*` doesn't take yet.
///
/// # Errors
/// As [`render_stills`]; `StageError::Face` for an expression asked of an imported avatar.
pub fn render_body_stills(
    renderer: &mut dyn Renderer,
    body: Body,
    spec: &StillSpec,
    shots: &[Shot],
) -> Result<Vec<Still>, StageError> {
    let avatar = match body {
        Body::Kit(built) => return render_stills(renderer, *built, spec, shots),
        Body::Avatar(avatar) => avatar,
    };
    check(spec, shots)?;
    if let Some((id, _)) = shots.iter().find_map(|shot| shot.expression.as_ref()) {
        return Err(StageError::Face {
            reason: format!("expression '{id}' asked for, but an imported avatar has no face profile to take it from"),
        });
    }
    let target = renderer
        .create_target(&TargetDesc { size: spec.size, alpha: AlphaEncoding::Straight })
        .map_err(|source| StageError::render("the still's target", source))?;
    let outcome =
        AvatarStage::new(renderer, *avatar, still_scene(spec), target, spec.quality, StageClock::fixed(STILL_FPS))
            .and_then(|mut stage| {
                if let Some(staged) = &spec.scene {
                    stage.set_scene(&staged.resolved, staged.subject_root);
                    if let Err(error) = stage.pay_scene(renderer) {
                        return first_error(Err(error), stage.release(renderer));
                    }
                }
                let base = FrameInput { pose: spec.pose.unwrap_or_else(|| stage.base_pose()), ..FrameInput::default() };
                // As the kit's stills: a frame at the base pose, the springs settled from rest under it, and every shot a
                // redraw of that state, so a shot's pixels don't depend on the shots before it.
                let shot = stage.tick(renderer, &base).and_then(|_| {
                    stage.settle_from_rest(SETTLE_STEPS);
                    let mut stills = Vec::with_capacity(shots.len());
                    for shot in shots {
                        stage.scene_mut().camera = shot.camera;
                        let frame = stage.redraw(renderer, &base)?;
                        let image =
                            renderer.read_target(target).map_err(|source| StageError::render("the still", source))?;
                        stills.push(Still { image, report: frame.report, face: None });
                    }
                    Ok(stills)
                });
                first_error(shot, stage.release(renderer))
            });
    let released = renderer.release(target.into()).map_err(|source| StageError::render("the still's target", source));
    first_error(outcome, released)
}

/// The scene every still of `spec` starts from: its light and look, and the default camera each shot replaces.
fn still_scene(spec: &StillSpec) -> Scene {
    let mut scene = Scene::new(Camera::default());
    scene.lights.clone_from(&spec.light.lights);
    scene.background = spec.light.background;
    scene.post = spec.light.post;
    scene.outline_light = spec.light.outline_light;
    scene.back_light_rim = spec.light.back_light_rim;
    scene.edges = spec.light.edges;
    scene.occlusion = spec.light.occlusion;
    scene
}

/// Refuses what would render nothing, before the renderer is touched.
fn check(spec: &StillSpec, shots: &[Shot]) -> Result<(), StageError> {
    if shots.is_empty() {
        return Err(StageError::render("the stills", RenderError::invalid("shots", "no shots: give at least one")));
    }
    if spec.size.x == 0 || spec.size.y == 0 {
        let reason = format!("a {}x{} still: give both sides at least one pixel", spec.size.x, spec.size.y);
        return Err(StageError::render("the still's size", RenderError::invalid("size", reason)));
    }
    Ok(())
}

/// Stages the character on `target`, settles it and shoots every shot. The stage is released on every path.
fn shoot_on(
    renderer: &mut dyn Renderer,
    target: TargetId,
    built: BuiltCharacter,
    spec: &StillSpec,
    shots: &[Shot],
) -> Result<Vec<Still>, StageError> {
    // Each shot sets its own camera before it renders, so the scene starts with the default one.
    let scene = still_scene(spec);
    // Read before `built` moves into the stage.
    let has_face = built.face.is_some();
    // With nothing animating it the stage shows the character as sculpted (decision 0014 §4), which is the pose the
    // landmarks that framing reads are measured in; the T-pose of `FrameInput::default()` would put the hands elsewhere.
    let mut base = HumanoidPose::REST;
    seed_base_pose(&built.rig, &mut base);
    if let Some(pose) = spec.pose {
        base = pose;
    }
    let mut stage = Stage::new(
        renderer,
        built,
        UploadOptions::default(),
        scene,
        target,
        spec.quality,
        StageClock::fixed(STILL_FPS),
    )?;
    let outcome = spec
        .scene
        .as_ref()
        .map_or(Ok(()), |staged| {
            stage.set_scene(&staged.resolved, staged.subject_root);
            stage.pay_scene(renderer)
        })
        .and_then(|()| render_shots(renderer, &mut stage, has_face, &base, spec.size, shots));
    let released = stage.release(renderer);
    first_error(outcome, released)
}

/// Binds the face, settles the stage and renders each shot in order, with where the face is in each picture of `size`.
fn render_shots(
    renderer: &mut dyn Renderer,
    stage: &mut Stage,
    has_face: bool,
    base: &HumanoidPose,
    size: UVec2,
    shots: &[Shot],
) -> Result<Vec<Still>, StageError> {
    if has_face {
        stage.set_face(renderer, FaceRig::standard())?;
    }
    let base_frame = FrameInput { pose: *base, ..FrameInput::default() };
    // The stage starts in the T-pose and `settle` warms the spring bones and the part runtimes under the last frame's
    // pose, so the base pose has to be that frame before the warm-up, not only after it.
    stage.tick(renderer, &base_frame)?;
    stage.settle(SETTLE_STEPS);
    // One tick after the warm-up puts the rig and the face in their frame state; every shot then redraws that state, so
    // a shot's pixels don't depend on how many shots came before it.
    stage.tick(renderer, &base_frame)?;
    let target = stage.target();
    let mut stills = Vec::with_capacity(shots.len());
    for shot in shots {
        stage.scene_mut().camera = shot.camera;
        let input = FrameInput { channels: channels_of(stage, shot)?, pose: *base, ..FrameInput::default() };
        let frame = stage.redraw(renderer, &input)?;
        let image = renderer.read_target(target).map_err(|source| StageError::render("the still", source))?;
        stills.push(Still { image, report: frame.report, face: stage.face_points(size) });
    }
    Ok(stills)
}

/// The face channels a shot asks for: the neutral bus, or the expression blended onto it at its weight.
fn channels_of(stage: &Stage, shot: &Shot) -> Result<ChannelBuf, StageError> {
    let Some((id, weight)) = &shot.expression else {
        return Ok(ChannelBuf::NEUTRAL);
    };
    let face = stage.built().face.as_ref().ok_or_else(|| StageError::Face {
        reason: format!("expression '{id}' asked for, but the character has no face to show it on"),
    })?;
    face.profile.expressions.blend(&ChannelBuf::NEUTRAL, &[(id, *weight)]).map_err(|unknown| StageError::Face {
        reason: format!("expression '{}' is not in the character's face profile", unknown.0),
    })
}

/// The outcome of an operation followed by its cleanup. The first error wins; a cleanup error after it is logged, as
/// [`Stage::new`] logs its own.
fn first_error<T>(outcome: Result<T, StageError>, cleanup: Result<(), StageError>) -> Result<T, StageError> {
    match (outcome, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            tracing::warn!(%cleanup, "releasing a still's resources failed after it had failed");
            Err(error)
        }
    }
}

/// `image` as an 8-bit RGBA PNG (the `image` crate's encoder, default settings). The same image gives the same bytes.
///
/// PNG stores straight alpha. A straight image is written as it is; a premultiplied one has each colour channel divided
/// by its alpha first (rounded, and black where the alpha is 0), so a translucent pixel shows at its true brightness.
///
/// # Errors
/// `StageError::Render` (`RenderError::Invalid`) when `rgba8` doesn't hold `size.x * size.y * 4` bytes.
pub fn encode_png(image: &Image) -> Result<Vec<u8>, StageError> {
    let (width, height) = (image.size.x, image.size.y);
    let expected = (width as usize).checked_mul(height as usize).and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(image.rgba8.len()) {
        let reason = format!("{} bytes for a {width}x{height} RGBA image (needs {expected:?})", image.rgba8.len());
        return Err(StageError::render("the PNG", RenderError::invalid("image", reason)));
    }
    let straight = straight_rgba(image);
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&straight, width, height, ExtendedColorType::Rgba8)
        .map_err(|error| StageError::render("the PNG", RenderError::invalid("image", error.to_string())))?;
    Ok(bytes)
}

/// `image`'s pixels with straight alpha: as they are, or unpremultiplied ([`unpremultiply`]).
pub fn straight_rgba(image: &Image) -> Vec<u8> {
    match image.alpha {
        AlphaEncoding::Straight => image.rgba8.clone(),
        AlphaEncoding::Premultiplied => unpremultiply(&image.rgba8),
    }
}

/// The straight-alpha pixels of premultiplied `rgba8`: each colour channel `c` with alpha `a` becomes `c * 255 / a`,
/// rounded and clamped to 255, and a pixel with alpha 0 becomes black, since its colour is lost.
fn unpremultiply(rgba8: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba8.len());
    for pixel in rgba8.as_chunks::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        for &channel in &pixel[..3] {
            let straight = (u32::from(channel) * 255 + alpha / 2) / alpha;
            out.push(u8::try_from(straight.min(255)).unwrap_or(u8::MAX));
        }
        out.push(pixel[3]);
    }
    out
}
