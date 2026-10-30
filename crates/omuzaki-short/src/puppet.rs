//! Shorts performed by a flat 2D puppet (spec v3 EX-08): a puppet bundle, or layered art the auto-rig rigs on the
//! spot, in place of the character's 3D body.
//!
//! The same timeline, performer and camera numbers drive it as drive a 3D short, so a short file only changes its
//! `puppet` line to switch: the performer's channels (faces, blinks, lip sync, the life layer's head and body) reach
//! the puppet's parameters through its mapping, its pendulums swing at their fixed step, and the CPU renderer draws
//! it. The camera is a 2D camera: a shot frames the puppet's face (else the top of the puppet) at the height the shot
//! asks, zoom, shift, punch and push scale and move the picture, roll turns it, and a camera's yaw and pitch turn the
//! puppet a little the other way, as an orbit would show it.

use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidPose};
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::ExpressionSet;
use omuzaki_puppet::bundle::Bundle;
use omuzaki_puppet::{Canvas, CpuRenderer, Driver, Evaluator, Physics};
use omuzaki_puppet_api::{NodeIx, NodeKind, PuppetDef};
use omuzaki_render_api::Colour;

use crate::camera::CameraRig;
use crate::caption::{Captioner, Ink};
use crate::def::{ShortDef, ShotFraming, ShotNumbers};
use crate::error::ShortError;
use crate::insert::InsertPainter;
use crate::perform::OfflinePerformer;
use crate::render::{FrameSink, LoopReport, frame_count};
use crate::timeline::Timeline;
use crate::voice::VoiceTrack;

/// The backdrop of a puppet short when the file names none: a deep blue-grey that most art reads against.
pub const DEFAULT_BACKDROP: Colour = Colour { r: 0x1d, g: 0x22, b: 0x2c, a: 0xff };
/// Seconds the pendulums settle before the first frame, so hair starts at rest rather than falling from its drawing.
pub const SETTLE_S: f32 = 1.5;
/// How much of a camera's orbit the puppet turns to show it: a flat puppet turns only so far before it breaks.
pub const ORBIT_SHARE: f32 = 0.6;

/// How a framing places the head: its height as a share of the frame's, and its middle's height from the top.
const fn placement(framing: ShotFraming) -> (f32, f32) {
    match framing {
        ShotFraming::Face => (0.36, 0.40),
        ShotFraming::Bust => (0.20, 0.32),
    }
}

/// The head of a puppet at rest, in model space: the middle and the size of the box around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadBox {
    /// The box's middle.
    pub centre: Vec2,
    /// Its width and height.
    pub size: Vec2,
}

/// Whether `node` is `ancestor` or under it.
fn under(def: &PuppetDef, mut node: NodeIx, ancestor: NodeIx) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        match def.nodes.get(node.get()).and_then(|n| n.parent) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

/// The face box of the evaluated puppet `e`, what a shot frames: the parts under its `head pivot` named for the face
/// (`face`, `Face Base`), else every part under it but the hair, ears and accessories, else the top two fifths of the
/// whole puppet.
#[must_use]
pub fn head_box(e: &Evaluator) -> HeadBox {
    let def = e.def();
    let named = |i: usize, keep: &dyn Fn(&str) -> bool| {
        let node = &def.nodes[i];
        let ix = NodeIx(u32::try_from(i).unwrap_or(u32::MAX));
        matches!(node.kind, NodeKind::Part(_))
            && def.node("head pivot").is_some_and(|head| under(def, ix, head))
            && keep(&node.name.to_lowercase())
    };
    let face = |n: &str| n.contains("face") && !n.contains("hair");
    let not_hair = |n: &str| !["hair", "bang", "ear", "hat", "ribbon", "accessor"].iter().any(|w| n.contains(w));
    let mut lo = Vec2::splat(f32::MAX);
    let mut hi = Vec2::splat(f32::MIN);
    for keep in [&face as &dyn Fn(&str) -> bool, &not_hair] {
        for i in (0..def.nodes.len()).filter(|i| named(*i, keep)) {
            for p in e.points(NodeIx(u32::try_from(i).unwrap_or(u32::MAX))) {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        if lo.x <= hi.x {
            break;
        }
    }
    if lo.x > hi.x {
        let half = [def.canvas[0] * 0.5, def.canvas[1] * 0.5];
        let [a, b] = e.draw_list().bounds().unwrap_or([[-half[0], -half[1]], half]);
        lo = Vec2::from(a);
        hi = Vec2::new(b[0], a[1] + (b[1] - a[1]) * 0.4);
    }
    HeadBox { centre: (lo + hi) * 0.5, size: (hi - lo).max(Vec2::ONE) }
}

/// The view (model space to frame pixels) of shot `n` on `head` in a `size` frame.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
pub fn view_for(n: &ShotNumbers, head: HeadBox, size: [u32; 2]) -> Affine2 {
    let (w, h) = (size[0] as f32, size[1] as f32);
    let (share, middle) = placement(n.framing);
    let scale = share * h / head.size.y * n.zoom.max(0.05);
    let anchor = Vec2::new(w * (0.5 + n.shift[0]), h * (middle - n.shift[1]));
    Affine2::from_translation(anchor)
        * Affine2::from_angle(n.roll.to_radians())
        * Affine2::from_scale(Vec2::splat(scale))
        * Affine2::from_translation(-head.centre)
}

/// Adds the camera's orbit to the performer's channels: a camera that swings right sees the puppet turned left.
fn orbit(channels: &mut ChannelBuf, n: &ShotNumbers) {
    for (name, degrees) in [("headYaw", -n.yaw), ("bodyTurn", -n.yaw), ("headPitch", n.pitch)] {
        if let Some(c) = Channel::from_name(name) {
            let info = c.info();
            channels[c] = (channels[c] + ORBIT_SHARE * degrees.to_radians()).clamp(info.min, info.max);
        }
    }
}

/// Renders the short `def` (with its `timeline` and `voice`) performed by `puppet` into `sink`, calling `progress`
/// with the share done after each frame (it stops the render by returning an error).
///
/// # Errors
/// [`ShortError::Invalid`] for a puppet that fails its checks or a face the expression library lacks; whatever `sink`
/// or `progress` returns.
pub fn render_puppet_short(
    puppet: Bundle,
    def: &ShortDef,
    timeline: &Timeline,
    voice: &VoiceTrack,
    sink: &mut dyn FrameSink,
    progress: &mut dyn FnMut(f32) -> Result<(), ShortError>,
) -> Result<LoopReport, ShortError> {
    let Bundle { def: puppet_def, mapping, textures } = puppet;
    let mut driver = Driver::new(&puppet_def, &mapping);
    let mut physics = Physics::new(puppet_def.physics.len());
    let mut eval = Evaluator::new(puppet_def).map_err(|e| ShortError::invalid("puppet", e.to_string()))?;
    let expressions = ExpressionSet::library().map_err(ShortError::Render)?;
    let mut performer =
        OfflinePerformer::new(timeline, &expressions, ChannelBuf::NEUTRAL, HumanoidPose::default(), def.seed)?;
    eval.evaluate();
    let head = head_box(&eval);

    driver.apply(&ChannelBuf::NEUTRAL, &mut eval);
    let fps = def.fps.max(1);
    #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
    let dt = 1.0 / fps as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a few dozen steps")]
    for _ in 0..(SETTLE_S / dt).ceil() as u32 {
        physics.step(&mut eval, dt);
    }

    let [w, h] = def.size;
    let backdrop = def.look.backdrop.unwrap_or(DEFAULT_BACKDROP);
    let a = f32::from(backdrop.a) / 255.0;
    let fill =
        [f32::from(backdrop.r) / 255.0 * a, f32::from(backdrop.g) / 255.0 * a, f32::from(backdrop.b) / 255.0 * a, a];
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let mut renderer = CpuRenderer::new(threads);
    let mut canvas = Canvas::new(w, h);
    let captioner = Captioner::new(Ink::of(&def.captions))?;
    let mut inserts = InsertPainter::default();
    let frames = frame_count(timeline.duration, def.fps);
    let mut report = LoopReport::default();
    for index in 0..frames {
        #[allow(clippy::cast_precision_loss, reason = "frame counts far below 2^24")]
        let t = index as f32 / fps as f32;
        let started = std::time::Instant::now();
        let mut channels = performer.advance(timeline, voice, t).channels;
        let shot = CameraRig::shot_at(timeline, def.seed, t);
        orbit(&mut channels, &shot);
        driver.apply(&channels, &mut eval);
        physics.step(&mut eval, dt);
        eval.evaluate();
        canvas.fill(fill);
        renderer.draw(eval.draw_list(), &textures, view_for(&shot, head, def.size), &mut canvas);
        let mut rgba = canvas.to_straight_rgba8();
        inserts.draw(&def.inserts, t, &mut rgba, [w, h])?;
        let drawn = std::time::Instant::now();
        if def.captions.enabled
            && let Some(caption) = timeline.caption_at(t)
        {
            captioner.draw(&mut rgba, [w, h], caption, t, def.captions.y, def.captions.size);
        }
        sink.frame(index, &rgba)?;
        report.render_s += (drawn - started).as_secs_f64();
        report.encode_s += drawn.elapsed().as_secs_f64();
        report.frames += 1;
        #[allow(clippy::cast_precision_loss, reason = "a share")]
        progress((index + 1) as f32 / frames as f32)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_shot_puts_the_head_at_its_share_of_the_frame() {
        let head = HeadBox { centre: Vec2::new(10.0, -200.0), size: Vec2::new(300.0, 400.0) };
        let n = ShotNumbers::default();
        let view = view_for(&n, head, [1080, 1920]);
        let top = view.transform_point2(head.centre - Vec2::new(0.0, 200.0));
        let bottom = view.transform_point2(head.centre + Vec2::new(0.0, 200.0));
        assert!(((bottom.y - top.y) / 1920.0 - 0.36).abs() < 1e-4, "{top} {bottom}");
        let middle = view.transform_point2(head.centre);
        assert!((middle - Vec2::new(540.0, 0.40 * 1920.0)).length() < 1e-3, "{middle}");
        let zoomed = view_for(&ShotNumbers { zoom: 2.0, shift: [0.1, 0.0], ..n }, head, [1080, 1920]);
        let middle = zoomed.transform_point2(head.centre);
        assert!((middle.x - 0.6 * 1080.0).abs() < 1e-3, "a shift right moves the head right: {middle}");
    }
}
