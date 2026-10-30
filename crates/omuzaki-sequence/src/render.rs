//! The render loop: each frame of the sequence is its back layers, then every shot showing at that time (its actor
//! posed and faced by the [`ActorPlayer`], its props and toon volumes placed, its camera keyed, rendered on its own
//! stage and laid over the frame at its opacity), then its front layers.
//!
//! A shot's stage is opened on the first frame that shows it and released after its last, so a cut starts the next
//! shot's springs settled and two shots alive at once crossfade. Nothing reads a wall clock: each stage runs a fixed
//! clock at the sequence's frame rate and the performer steps at its own 60 Hz on the sequence's time, so a sequence
//! renders to the same pixels on every run of the same renderer.

use std::collections::BTreeMap;

use omuzaki_anim::hands::HandRest;
use omuzaki_anim_api::{ChannelBuf, HumanoidBone};
use omuzaki_core::glam::{EulerRot, Mat4, Quat, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, MaterialId, MeshId, Projection, Quality, Renderer, Scene, SurfaceFx,
    TargetDesc, TargetId,
};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::Body;
use omuzaki_stage::cast::StageMember;
use omuzaki_stage::frame::face::{FaceRig, SETTLE_STEPS};
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::ops::render::still::straight_rgba;
use omuzaki_stage::props::staged::ResolvedProps;
use omuzaki_stage::scene::alone::StagedScene;
use omuzaki_stage::stages::{AnyStage, FrameLoop};
use omuzaki_stage_api::GroundKind;
use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};

use crate::actor::ActorPlayer;
use crate::curve::wiggle;
use crate::def::{Backdrop, SequenceDef, Shot};
use crate::error::SequenceError;
use crate::layer::{Depth, Painter};
use crate::volume;

/// The camera's near plane, metres.
const NEAR: f32 = 0.05;
/// The camera's far plane, metres: far enough for a skyline.
const FAR: f32 = 2000.0;
/// What a shake of 1 moves the camera, metres.
const SHAKE_M: f32 = 0.02;
/// What a shake of 1 turns the camera, degrees.
const SHAKE_DEG: f32 = 1.0;

/// What the loop measured.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoopReport {
    /// Frames rendered.
    pub frames: u64,
    /// Shot stages opened.
    pub shots_opened: u32,
    /// Seconds spent rendering and reading back (the GPU half).
    pub render_s: f64,
    /// Seconds spent compositing, drawing layers and encoding (the CPU half).
    pub composite_s: f64,
}

/// What a frame goes to: the writer, a still, or a test's collector.
pub trait FrameSink {
    /// Takes frame `index`: straight RGBA8 of the sequence's size.
    ///
    /// # Errors
    /// Whatever the sink reports.
    fn frame(&mut self, index: u64, rgba: &[u8]) -> Result<(), SequenceError>;
}

impl FrameSink for omuzaki_short::encode::ShortWriter {
    fn frame(&mut self, _index: u64, rgba: &[u8]) -> Result<(), SequenceError> {
        self.push(rgba).map_err(SequenceError::from)
    }
}

/// A sink that keeps every frame: for stills, sheets and tests.
#[derive(Debug, Default)]
pub struct Collect(pub Vec<(u64, Vec<u8>)>);

impl FrameSink for Collect {
    fn frame(&mut self, index: u64, rgba: &[u8]) -> Result<(), SequenceError> {
        self.0.push((index, rgba.to_vec()));
        Ok(())
    }
}

/// A shot's body and the scene it stands in, as the caller loads them (from the project, or a test's fixtures).
#[derive(Debug)]
pub struct ShotStage {
    /// The actor's body.
    pub body: Body,
    /// The scene, its lights and backdrop; the loop moves the actor itself.
    pub staged: StagedScene,
}

fn render_error(what: &str, e: impl std::fmt::Display) -> SequenceError {
    SequenceError::Render(format!("{what}: {e}"))
}

/// A seed for shot `index` of a sequence seeded `seed`.
fn shot_seed(seed: u32, index: usize) -> u32 {
    seed ^ u32::try_from(index).unwrap_or(0).wrapping_mul(0x9e37_79b9)
}

/// The camera of `shot` at `t` seconds into it.
#[must_use]
pub fn camera_at(shot: &Shot, t: f32, seed: u32) -> Camera {
    let c = &shot.camera;
    let mut eye = Vec3::from(c.position.at(t));
    let mut target = Vec3::from(c.target.at(t));
    let shake = c.shake.amount.at(t).max(0.0);
    let mut roll = c.roll.at(t);
    if shake > 0.0 {
        let f = c.shake.frequency;
        let drift = Vec3::new(wiggle(t, f, seed), wiggle(t, f, seed ^ 0x0101), wiggle(t, f, seed ^ 0x0202));
        eye += drift * SHAKE_M * shake;
        let distance = (target - eye).length();
        let turn = Vec3::new(wiggle(t, f, seed ^ 0x0303), wiggle(t, f, seed ^ 0x0404), 0.0);
        target += turn * distance * SHAKE_DEG.to_radians() * shake;
        roll += wiggle(t, f, seed ^ 0x0505) * SHAKE_DEG * 0.5 * shake;
    }
    let forward = (target - eye).try_normalize().unwrap_or(Vec3::NEG_Z);
    let up = Quat::from_axis_angle(forward, -roll.to_radians()) * Vec3::Y;
    Camera::look_at(
        eye,
        target,
        up,
        Projection::Perspective { fov_y_deg: c.fov.at(t).clamp(1.0, 170.0), near: NEAR, far: FAR },
    )
}

/// A turn of yaw, pitch and roll in degrees.
fn turn(ypr: [f32; 3]) -> Quat {
    Quat::from_euler(EulerRot::YXZ, ypr[0].to_radians(), ypr[1].to_radians(), ypr[2].to_radians())
}

/// A prop of a shot on the renderer.
#[derive(Debug)]
struct LiveProp {
    parts: Vec<(MeshId, MaterialId, bool)>,
}

/// A volume of a shot: its material, and where its emitter has been (for a bone that carries it).
#[derive(Debug)]
struct LiveVolume {
    material: MaterialId,
    trail: Vec<(f32, Vec3)>,
}

/// A shot showing: its stage and everything it put on the renderer.
struct LiveShot {
    index: usize,
    stage: AnyStage,
    target: TargetId,
    player: Option<ActorPlayer>,
    props: Vec<LiveProp>,
    volumes: Vec<LiveVolume>,
    fx: Option<SurfaceFx>,
    seed: u32,
}

impl LiveShot {
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), SequenceError> {
        let mut first: Option<SequenceError> = None;
        let mut keep = |r: Result<(), SequenceError>| {
            if let Err(e) = r
                && first.is_none()
            {
                first = Some(e);
            }
        };
        for prop in &self.props {
            for (mesh, material, _) in &prop.parts {
                keep(renderer.release((*mesh).into()).map_err(|e| render_error("releasing a prop", e)));
                keep(renderer.release((*material).into()).map_err(|e| render_error("releasing a prop", e)));
            }
        }
        for volume in &self.volumes {
            keep(renderer.release(volume.material.into()).map_err(|e| render_error("releasing a volume", e)));
        }
        keep(self.stage.release(renderer).map_err(|e| render_error("releasing the stage", e)));
        keep(renderer.release(self.target.into()).map_err(|e| render_error("releasing the shot's target", e)));
        first.map_or(Ok(()), Err)
    }
}

/// The loop's state: the shots showing.
struct Loop<'a> {
    def: &'a SequenceDef,
    quality: Quality,
    live: Vec<LiveShot>,
    report: LoopReport,
}

/// Renders frames `frames` (increasing indices) of `def` on `renderer` into `sink`, loading each shot's body and scene
/// through `load` when the shot first shows, and calling `progress` with the share done after each frame (it stops the
/// render by returning an error). Everything the call creates on the renderer is released, on success or failure.
///
/// # Errors
/// [`SequenceError::Render`] when staging or rendering fails; [`SequenceError::Invalid`] for a key the body can't
/// play; whatever `load`, `sink` or `progress` returns.
pub fn render_frames(
    renderer: &mut dyn Renderer,
    def: &SequenceDef,
    quality: Quality,
    load: &mut dyn FnMut(&Shot) -> Result<ShotStage, SequenceError>,
    frames: &[u64],
    sink: &mut dyn FrameSink,
    progress: &mut dyn FnMut(f32) -> Result<(), SequenceError>,
) -> Result<LoopReport, SequenceError> {
    def.check()?;
    let mut state = Loop { def, quality, live: Vec::new(), report: LoopReport::default() };
    let outcome = state.run(renderer, load, frames, sink, progress);
    let mut released = Ok(());
    for shot in state.live.drain(..) {
        if let Err(e) = shot.release(renderer)
            && released.is_ok()
        {
            released = Err(e);
        }
    }
    match (outcome, released) {
        (Ok(()), Ok(())) => Ok(state.report),
        (Err(e), _) | (Ok(()), Err(e)) => Err(e),
    }
}

impl Loop<'_> {
    fn run(
        &mut self,
        renderer: &mut dyn Renderer,
        load: &mut dyn FnMut(&Shot) -> Result<ShotStage, SequenceError>,
        frames: &[u64],
        sink: &mut dyn FrameSink,
        progress: &mut dyn FnMut(f32) -> Result<(), SequenceError>,
    ) -> Result<(), SequenceError> {
        let def = self.def;
        let [w, h] = def.size;
        let size = IntSize::from_wh(w, h).ok_or_else(|| SequenceError::Invalid("the frame has no area".into()))?;
        let painter = Painter::new(&def.layers, def.size);
        #[allow(clippy::cast_precision_loss, reason = "frame indices far below 2^24")]
        let time = |index: u64| index as f32 / def.fps as f32;
        for (done, &index) in frames.iter().enumerate() {
            let t = time(index);
            let started = std::time::Instant::now();
            // The shots that have ended go first, so a cut frees the GPU before the next shot opens.
            let mut kept = Vec::with_capacity(self.live.len());
            for shot in self.live.drain(..) {
                let s = &def.shots[shot.index];
                if s.start <= t && t < s.end {
                    kept.push(shot);
                } else {
                    shot.release(renderer)?;
                }
            }
            self.live = kept;
            let mut canvas = Pixmap::new(w, h).ok_or_else(|| SequenceError::Invalid("the frame has no area".into()))?;
            painter.paint(&mut canvas, Depth::Back, t, def.fps);
            let mut order: Vec<usize> = (0..def.shots.len())
                .filter(|i| {
                    let s = &def.shots[*i];
                    s.start <= t && t < s.end
                })
                .collect();
            order.sort_by(|a, b| def.shots[*a].start.total_cmp(&def.shots[*b].start).then(a.cmp(b)));
            let mut render_s = 0.0;
            for index in order {
                if !self.live.iter().any(|l| l.index == index) {
                    let shot = self.open(renderer, load, index, t)?;
                    self.live.push(shot);
                    self.report.shots_opened += 1;
                }
                let drawn = std::time::Instant::now();
                let Some(live) = self.live.iter_mut().find(|l| l.index == index) else { continue };
                let rgba = shot_frame(renderer, def, live, t)?;
                render_s += drawn.elapsed().as_secs_f64();
                let shot = &def.shots[index];
                let opacity = shot.opacity.at(t - shot.start).clamp(0.0, 1.0);
                if opacity > 0.0 {
                    let layer = premultiplied(rgba, size)?;
                    let paint = PixmapPaint { opacity, ..PixmapPaint::default() };
                    canvas.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
                }
            }
            painter.paint(&mut canvas, Depth::Front, t, def.fps);
            let rgba = straight(&canvas);
            sink.frame(index, &rgba)?;
            self.report.frames += 1;
            self.report.render_s += render_s;
            self.report.composite_s += started.elapsed().as_secs_f64() - render_s;
            #[allow(clippy::cast_precision_loss, reason = "a share")]
            progress((done + 1) as f32 / frames.len() as f32)?;
        }
        Ok(())
    }

    /// Opens shot `index` at sequence time `t`: loads its body and scene, stages them, uploads its props and volume
    /// materials, and settles the body at the shot's first pose.
    fn open(
        &self,
        renderer: &mut dyn Renderer,
        load: &mut dyn FnMut(&Shot) -> Result<ShotStage, SequenceError>,
        index: usize,
        t: f32,
    ) -> Result<LiveShot, SequenceError> {
        let def = self.def;
        let shot = &def.shots[index];
        let ShotStage { body, mut staged } = load(shot)?;
        if shot.bare {
            staged.resolved.ground.kind = GroundKind::None;
            staged.resolved.props = ResolvedProps::default();
        }
        match &shot.backdrop {
            Backdrop::None => staged.resolved.background = Background::Transparent,
            Backdrop::Colour { colour } => staged.resolved.background = Background::Solid { colour: *colour },
            Backdrop::Gradient { top, bottom } => {
                staged.resolved.background = Background::Gradient { top: *top, bottom: *bottom };
            }
            Backdrop::Scene => {}
        }
        let hand_rest = {
            let skeleton = match &body {
                Body::Kit(built) => &built.rig.skeleton,
                Body::Avatar(avatar) => &avatar.skeleton,
            };
            HandRest::of(skeleton).unwrap_or_else(|_| HandRest::nominal())
        };
        let has_face = matches!(&body, Body::Kit(built) if built.face.is_some());
        let [w, h] = def.size;
        let target = renderer
            .create_target(&TargetDesc { size: UVec2::new(w, h), alpha: AlphaEncoding::Straight })
            .map_err(|e| render_error("the shot's target", e))?;
        #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
        let clock = StageClock::fixed(def.fps as f32);
        let stage = match body.stage(
            renderer,
            UploadOptions::default(),
            Scene::new(Camera::default()),
            target,
            self.quality,
            clock,
        ) {
            Ok(stage) => stage,
            Err(e) => {
                let _ = renderer.release(target.into());
                return Err(render_error("staging the character", e));
            }
        };
        let seed = shot_seed(def.seed, index);
        let mut live =
            LiveShot { index, stage, target, player: None, props: Vec::new(), volumes: Vec::new(), fx: None, seed };
        match fill(renderer, shot, &staged, &mut live, hand_rest, has_face, t) {
            Ok(()) => Ok(live),
            Err(e) => {
                let _ = live.release(renderer);
                Err(e)
            }
        }
    }
}

/// Everything a shot's stage holds once opened: its scene, face, actor, props and volumes, settled at time `t`.
#[allow(clippy::too_many_arguments, reason = "the shot's parts, each used once")]
fn fill(
    renderer: &mut dyn Renderer,
    shot: &Shot,
    staged: &StagedScene,
    live: &mut LiveShot,
    hand_rest: HandRest,
    has_face: bool,
    t: f32,
) -> Result<(), SequenceError> {
    let stage = &mut live.stage;
    stage.set_scene(&staged.resolved, Mat4::IDENTITY);
    stage.pay_scene(renderer).map_err(|e| render_error("the scene", e))?;
    if has_face && let AnyStage::Kit(kit) = stage {
        kit.set_face(renderer, FaceRig::standard()).map_err(|e| render_error("the face", e))?;
    }
    if let Some(actor) = shot.actors.first() {
        let expressions = stage.trigger_expressions();
        live.player = Some(ActorPlayer::new(
            actor,
            &format!("shot '{}'.actor", shot.id),
            &expressions,
            ChannelBuf::NEUTRAL,
            hand_rest,
            live.seed,
        )?);
    }
    for (i, prop) in shot.props.iter().enumerate() {
        let built = omuzaki_stage_api::prop_kind(&prop.kind)
            .and_then(|kind| kind.build(&prop.params))
            .map_err(|e| SequenceError::Invalid(format!("shot '{}'.prop[{i}]: {e}", shot.id)))?;
        let mut parts = Vec::with_capacity(built.parts.len());
        let mut created = || -> Result<(), SequenceError> {
            for part in &built.parts {
                let mesh = renderer.create_mesh(&part.mesh).map_err(|e| render_error("a prop's mesh", e))?;
                let material = match renderer.create_material(&part.material) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = renderer.release(mesh.into());
                        return Err(render_error("a prop's material", e));
                    }
                };
                parts.push((mesh, material, part.casts_shadow));
            }
            Ok(())
        };
        let outcome = created();
        live.props.push(LiveProp { parts });
        outcome?;
    }
    for volume in &shot.volumes {
        let material =
            renderer.create_material(&volume.material()).map_err(|e| render_error("a volume's material", e))?;
        live.volumes.push(LiveVolume { material, trail: Vec::new() });
    }
    // Warm up as the stills do: a frame at the shot's first pose, the springs and part runtimes settled under it, so
    // the first frame doesn't start with hair falling from its rest shape.
    let local = t - shot.start;
    let warm = live.player.as_mut().map_or_else(FrameInput::default, |p| p.advance(local, local).clone());
    live.stage.scene_mut().camera = camera_at(shot, local, live.seed);
    live.stage.tick(renderer, &warm).map_err(|e| render_error("the shot's first frame", e))?;
    match &mut live.stage {
        AnyStage::Kit(kit) => kit.settle(SETTLE_STEPS),
        AnyStage::Avatar(avatar) => avatar.settle(SETTLE_STEPS),
    }
    Ok(())
}

/// The transform of a rotation, translation and uniform scale.
fn placed(position: [f32; 3], rotation: [f32; 3], scale: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(Vec3::splat(scale), turn(rotation), Vec3::from(position))
}

/// `a` moved toward `b` by `w`: translation and scale lerped, rotation slerped.
fn blend(a: Mat4, b: Mat4, w: f32) -> Mat4 {
    if w <= 0.0 {
        return a;
    }
    if w >= 1.0 {
        return b;
    }
    let (sa, ra, ta) = a.to_scale_rotation_translation();
    let (sb, rb, tb) = b.to_scale_rotation_translation();
    Mat4::from_scale_rotation_translation(sa.lerp(sb, w), ra.slerp(rb, w), ta.lerp(tb, w))
}

/// Where the emitter of `volume` was at `s`, from the trail its bone left (held before its first and after its last
/// point).
fn trail_at(trail: &[(f32, Vec3)], s: f32) -> Vec3 {
    let Some(&(t0, p0)) = trail.first() else { return Vec3::ZERO };
    if s <= t0 {
        return p0;
    }
    let next = trail.partition_point(|(t, _)| *t <= s);
    if next >= trail.len() {
        return trail.last().map_or(p0, |(_, p)| *p);
    }
    let ((ta, pa), (tb, pb)) = (trail[next - 1], trail[next]);
    let u = if tb > ta { (s - ta) / (tb - ta) } else { 1.0 };
    pa.lerp(pb, u)
}

/// Renders `live`'s frame at sequence time `t` and returns it as straight RGBA.
fn shot_frame(
    renderer: &mut dyn Renderer,
    def: &SequenceDef,
    live: &mut LiveShot,
    t: f32,
) -> Result<Vec<u8>, SequenceError> {
    let shot = &def.shots[live.index];
    let local = t - shot.start;
    // Characters move on `motion_on`s: the pose holds between the frames it moves on.
    #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
    let step = def.motion_on as f32 / def.fps as f32;
    let pose_t = if def.motion_on > 1 { (local / step + 1e-4).floor() * step } else { local };
    let input = live.player.as_mut().map_or_else(FrameInput::default, |p| p.advance(local, pose_t).clone());
    if let Some(actor) = shot.actors.first()
        && let Some(surface) = &actor.surface
    {
        let fx = surface.at(local, live.seed);
        if fx != live.fx {
            live.stage
                .set_surface_fx(renderer, fx.clone(), &surface.spare)
                .map_err(|e| render_error("the surface fx", e))?;
            live.fx = fx;
        }
    }
    live.stage.scene_mut().camera = camera_at(shot, local, live.seed);
    let prepared = StageMember::prepare(&mut live.stage, renderer, &input).map_err(|e| render_error("the frame", e))?;
    // Props: in the world, or riding a bone (and let go toward their world place as the hold's weight drops).
    let bones: BTreeMap<String, Option<Mat4>> = shot
        .props
        .iter()
        .filter_map(|p| p.attach.as_ref().map(|a| a.bone.clone()))
        .chain(shot.volumes.iter().filter_map(|v| v.emitter.bone.clone()))
        .map(|name| {
            let world = HumanoidBone::from_name(&name).and_then(|b| live.stage.humanoid_world(b));
            (name, world)
        })
        .collect();
    let mut instances = Vec::new();
    for (prop, item) in live.props.iter().zip(&shot.props) {
        if let Some([from, to]) = item.visible
            && !(from <= local && local < to)
        {
            continue;
        }
        let own = placed(item.position.at(local), item.rotation.at(local), item.scale.at(local));
        let transform = match &item.attach {
            Some(attach) => {
                let bone = bones.get(&attach.bone).copied().flatten().unwrap_or(Mat4::IDENTITY);
                let held = bone * own;
                let weight = attach.weight.at(local).clamp(0.0, 1.0);
                let world = placed(
                    item.world_position.as_ref().map_or(item.position.at(local), |p| p.at(local)),
                    item.world_rotation.as_ref().map_or(item.rotation.at(local), |r| r.at(local)),
                    item.scale.at(local),
                );
                blend(world, held, weight)
            }
            None => own,
        };
        for (mesh, material, casts) in &prop.parts {
            let mut instance = Instance::new(*mesh, *material, transform);
            instance.casts_shadow = *casts;
            instances.push(instance);
        }
    }
    // Volumes: meshed for this frame from the puffs alive now.
    let mut frame_meshes = Vec::new();
    let mut failed = None;
    for (volume, vdef) in live.volumes.iter_mut().zip(&shot.volumes) {
        let carried = vdef.emitter.bone.as_ref().and_then(|b| bones.get(b).copied().flatten());
        let at = Vec3::from(vdef.emitter.at);
        if let Some(bone) = carried {
            volume.trail.push((local, bone.transform_point3(at)));
        }
        let trail = &volume.trail;
        let emitter = |s: f32| if carried.is_some() { trail_at(trail, s) } else { at };
        let Some(mesh) = volume::mesh(vdef, local, live.seed ^ vdef.seed, &emitter) else { continue };
        match renderer.create_mesh(&mesh) {
            Ok(id) => {
                frame_meshes.push(id);
                let mut instance = Instance::new(id, volume.material, Mat4::IDENTITY);
                instance.casts_shadow = false;
                instances.push(instance);
            }
            Err(e) => {
                failed = Some(render_error("a volume's mesh", e));
                break;
            }
        }
    }
    live.stage.scene_mut().instances.extend(instances);
    let rendered = match failed {
        Some(e) => Err(e),
        None => StageMember::submit(&mut live.stage, renderer, prepared, "the frame")
            .map(|_| ())
            .map_err(|e| render_error("the frame", e)),
    };
    for mesh in frame_meshes {
        let _ = renderer.release(mesh.into());
    }
    rendered?;
    let image = renderer.read_target(live.target).map_err(|e| render_error("reading the frame", e))?;
    Ok(straight_rgba(&image))
}

/// Straight RGBA8 as a premultiplied pixmap.
fn premultiplied(mut rgba: Vec<u8>, size: IntSize) -> Result<Pixmap, SequenceError> {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        for c in &mut px[..3] {
            *c = u8::try_from((u16::from(*c) * a + 127) / 255).unwrap_or(u8::MAX);
        }
    }
    Pixmap::from_vec(rgba, size).ok_or_else(|| SequenceError::Render("a shot's frame is the wrong size".into()))
}

/// A premultiplied pixmap as straight RGBA8.
fn straight(canvas: &Pixmap) -> Vec<u8> {
    let mut rgba = canvas.data().to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = u8::try_from((u16::from(*c) * 255 + a / 2) / a).unwrap_or(u8::MAX);
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplying_round_trips() {
        let size = IntSize::from_wh(2, 1).unwrap();
        let rgba = vec![200, 100, 50, 128, 10, 20, 30, 255];
        let pix = premultiplied(rgba.clone(), size).unwrap();
        let back = straight(&pix);
        for (a, b) in rgba.iter().zip(&back) {
            assert!((i16::from(*a) - i16::from(*b)).abs() <= 2, "{rgba:?} vs {back:?}");
        }
    }

    #[test]
    fn a_trail_interpolates_and_holds() {
        let trail = [(0.0, Vec3::ZERO), (1.0, Vec3::X)];
        assert_eq!(trail_at(&trail, -1.0), Vec3::ZERO);
        assert!((trail_at(&trail, 0.5) - Vec3::new(0.5, 0.0, 0.0)).length() < 1e-6);
        assert_eq!(trail_at(&trail, 3.0), Vec3::X);
    }

    #[test]
    fn a_held_prop_lets_go_smoothly() {
        let a = placed([0.0; 3], [0.0; 3], 1.0);
        let b = placed([2.0, 0.0, 0.0], [90.0, 0.0, 0.0], 1.0);
        let half = blend(a, b, 0.5);
        let (_, r, t) = half.to_scale_rotation_translation();
        assert!((t.x - 1.0).abs() < 1e-5);
        assert!((r.angle_between(Quat::IDENTITY) - 45f32.to_radians()).abs() < 1e-4);
    }
}
