//! The render loop: the character on a stage in its scene, one frame per tick of the short's clock, each frame posed
//! and faced by the offline performer, shot by the camera track, captioned and handed to the writer.
//!
//! Nothing here reads a wall clock: the stage runs a fixed clock at the short's frame rate and the performer steps at
//! its own 60 Hz on the short's time, so a short renders to the same pixels on every run of the same renderer.

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::Body;
use omuzaki_stage::frame::face::{FaceRig, SETTLE_STEPS};
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::ops::render::still::straight_rgba;
use omuzaki_stage::scene::alone::StagedScene;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::stages::{AnyStage, FrameLoop};

use crate::camera::CameraRig;
use crate::caption::{Captioner, Ink};
use crate::def::ShortDef;
use crate::error::ShortError;
use crate::insert::InsertPainter;
use crate::perform::OfflinePerformer;
use crate::timeline::Timeline;
use crate::voice::VoiceTrack;

/// What the loop measured.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoopReport {
    /// Frames rendered.
    pub frames: u64,
    /// Seconds spent rendering and reading back (the GPU half).
    pub render_s: f64,
    /// Seconds spent captioning and encoding (the CPU half).
    pub encode_s: f64,
}

/// What a frame goes to: the writer, or a test's collector.
pub trait FrameSink {
    /// Takes frame `index`: straight RGBA8 of the short's size.
    ///
    /// # Errors
    /// Whatever the sink reports.
    fn frame(&mut self, index: u64, rgba: &[u8]) -> Result<(), ShortError>;
}

impl FrameSink for crate::encode::ShortWriter {
    fn frame(&mut self, _index: u64, rgba: &[u8]) -> Result<(), ShortError> {
        self.push(rgba)
    }
}

/// Everything a render needs, ready.
#[derive(Debug)]
pub struct ShortScene<'a> {
    /// The short.
    pub def: &'a ShortDef,
    /// Its timeline.
    pub timeline: &'a Timeline,
    /// Its voice.
    pub voice: &'a VoiceTrack,
    /// The scene, with the character cast alone in it.
    pub staged: &'a StagedScene,
    /// The character, measured in its bind pose.
    pub landmarks: Landmarks,
    /// The render settings.
    pub quality: Quality,
    /// The pose the character performs from; the body's own when `None`.
    pub pose: Option<omuzaki_anim_api::HumanoidPose>,
    /// The frames to draw, by index; every frame when `None`. Frames before the first are performed but not drawn,
    /// so the performer and the camera stand where they would in the whole short, and the hair settles under the
    /// first drawn frame's pose.
    pub frames: Option<std::ops::Range<u64>>,
}

/// The frames of a short of `total` frames that `span` (seconds from and to) covers: from the frame at or after its
/// start to the frame before its end; every frame when `None`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a short is at most 600 s")]
pub fn span_frames(span: Option<[f32; 2]>, fps: u32, total: u64) -> std::ops::Range<u64> {
    let Some([from, to]) = span else { return 0..total };
    // A thousandth of a frame of slack, so 0.6 s at 30 fps is frame 18 though 0.6 is a hair over it in f32.
    let at = |s: f32| ((f64::from(s) * f64::from(fps) - 1e-3).ceil().max(0.0) as u64).min(total);
    let start = at(from);
    start..at(to).max(start)
}

fn render_error(what: &str, e: impl std::fmt::Display) -> ShortError {
    ShortError::Render(format!("{what}: {e}"))
}

/// The number of frames of a short `duration` seconds long at `fps`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a short is at most 600 s")]
pub fn frame_count(duration: f32, fps: u32) -> u64 {
    (f64::from(duration) * f64::from(fps)).ceil().max(1.0) as u64
}

/// Renders the short of `scene` with `body` on `renderer` into `sink`, calling `progress` with the share done after each
/// frame (it stops the render by returning an error). Everything the call creates on the renderer is released, on
/// success or failure.
///
/// # Errors
/// [`ShortError::Render`] when staging or rendering fails; [`ShortError::Invalid`] for a face the character lacks;
/// whatever `sink` or `progress` returns.
pub fn render_short(
    renderer: &mut dyn Renderer,
    body: Body,
    scene: &ShortScene<'_>,
    sink: &mut dyn FrameSink,
    progress: &mut dyn FnMut(f32) -> Result<(), ShortError>,
) -> Result<LoopReport, ShortError> {
    let [w, h] = scene.def.size;
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(w, h), alpha: AlphaEncoding::Straight })
        .map_err(|e| render_error("the short's target", e))?;
    let outcome = on_target(renderer, target, body, scene, sink, progress);
    let released = renderer.release(target.into()).map_err(|e| render_error("releasing the short's target", e));
    match (outcome, released) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    }
}

fn on_target(
    renderer: &mut dyn Renderer,
    target: TargetId,
    body: Body,
    scene: &ShortScene<'_>,
    sink: &mut dyn FrameSink,
    progress: &mut dyn FnMut(f32) -> Result<(), ShortError>,
) -> Result<LoopReport, ShortError> {
    let base = scene.pose.unwrap_or_else(|| body.base_pose());
    let has_face = matches!(&body, Body::Kit(built) if built.face.is_some());
    let fps = scene.def.fps;
    #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
    let clock = StageClock::fixed(fps as f32);
    let mut stage = body
        .stage(renderer, UploadOptions::default(), Scene::new(Camera::default()), target, scene.quality, clock)
        .map_err(|e| render_error("staging the character", e))?;
    let outcome = run(renderer, &mut stage, has_face, &base, scene, sink, progress);
    let released = stage.release(renderer).map_err(|e| render_error("releasing the stage", e));
    match (outcome, released) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    renderer: &mut dyn Renderer,
    stage: &mut AnyStage,
    has_face: bool,
    base: &omuzaki_anim_api::HumanoidPose,
    scene: &ShortScene<'_>,
    sink: &mut dyn FrameSink,
    progress: &mut dyn FnMut(f32) -> Result<(), ShortError>,
) -> Result<LoopReport, ShortError> {
    let base = *base;
    let def = scene.def;
    stage.set_scene(&scene.staged.resolved, scene.staged.subject_root);
    stage.pay_scene(renderer).map_err(|e| render_error("the scene", e))?;
    if has_face && let AnyStage::Kit(kit) = stage {
        kit.set_face(renderer, FaceRig::standard()).map_err(|e| render_error("the face", e))?;
    }
    let expressions = stage.trigger_expressions();
    let mut performer = OfflinePerformer::new(scene.timeline, &expressions, ChannelBuf::NEUTRAL, base, def.seed)?;
    let [w, h] = def.size;
    #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
    let rig = CameraRig {
        landmarks: scene.landmarks,
        root: scene.staged.subject_root,
        aspect: w as f32 / h as f32,
        seed: def.seed,
    };
    // Warm up as the stills do: a frame at the base pose, the springs and part runtimes settled under it, so the first
    // frame doesn't start with hair falling from its rest shape.
    let warm = FrameInput { pose: base, ..FrameInput::default() };
    stage.scene_mut().camera = rig.camera_at(scene.timeline, 0.0)?;
    stage.tick(renderer, &warm).map_err(|e| render_error("the first frame", e))?;
    match stage {
        AnyStage::Kit(kit) => kit.settle(SETTLE_STEPS),
        AnyStage::Avatar(avatar) => avatar.settle(SETTLE_STEPS),
    }
    let captioner = Captioner::new(Ink::of(&def.captions))?;
    let mut inserts = InsertPainter::default();
    let total = frame_count(scene.timeline.duration, def.fps);
    let frames = scene.frames.clone().map_or(0..total, |r| r.start.min(total)..r.end.min(total));
    let mut report = LoopReport::default();
    for index in 0..frames.end {
        #[allow(clippy::cast_precision_loss, reason = "frame counts far below 2^24")]
        let t = index as f32 / def.fps as f32;
        let started = std::time::Instant::now();
        let input = performer.advance(scene.timeline, scene.voice, t).clone();
        if index < frames.start {
            continue;
        }
        stage.scene_mut().camera = rig.camera_at(scene.timeline, t)?;
        if index == frames.start && index > 0 {
            // A span starting mid-short: the hair settles under this frame's pose, as it did under the base one.
            stage.tick(renderer, &input).map_err(|e| render_error(&format!("frame {index}"), e))?;
            match stage {
                AnyStage::Kit(kit) => kit.settle(SETTLE_STEPS),
                AnyStage::Avatar(avatar) => avatar.settle(SETTLE_STEPS),
            }
        }
        stage.tick(renderer, &input).map_err(|e| render_error(&format!("frame {index}"), e))?;
        let image =
            renderer.read_target(stage.target()).map_err(|e| render_error(&format!("reading frame {index}"), e))?;
        let mut rgba = straight_rgba(&image);
        inserts.draw(&def.inserts, t, &mut rgba, [w, h])?;
        let drawn = std::time::Instant::now();
        if def.captions.enabled
            && let Some(caption) = scene.timeline.caption_at(t)
        {
            captioner.draw(&mut rgba, [w, h], caption, t, def.captions.y, def.captions.size);
        }
        sink.frame(index, &rgba)?;
        report.render_s += (drawn - started).as_secs_f64();
        report.encode_s += drawn.elapsed().as_secs_f64();
        report.frames += 1;
        #[allow(clippy::cast_precision_loss, reason = "a share")]
        progress((index + 1 - frames.start) as f32 / (frames.end - frames.start).max(1) as f32)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_covers_its_frames_and_stays_inside_the_short() {
        assert_eq!(span_frames(None, 30, 90), 0..90);
        assert_eq!(span_frames(Some([1.0, 2.0]), 30, 90), 30..60);
        assert_eq!(span_frames(Some([2.5, 9.0]), 30, 90), 75..90, "the end is clamped to the short");
        assert_eq!(span_frames(Some([5.0, 6.0]), 30, 90), 90..90, "past the end draws nothing");
        assert_eq!(span_frames(Some([0.51, 0.6]), 30, 90), 16..18, "the first frame at or after the start");
    }
}
