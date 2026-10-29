//! The CPU face's partial repaint (#2443): a tracked face is repainted from a snapshot of the commands that did not
//! change, inside the rectangles of those that did. It must draw exactly what a whole repaint draws, every frame, while
//! drawing far fewer pixels; and the layer painter must send the texture only what changed.
//!
//! `tracked_face_timing` prints the whole and the partial repaint's times on a talking, blinking, glancing face with
//! tracker noise (run it with `--release -- --nocapture` for the real numbers).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::any::Any;
use std::time::Instant;

use omuzaki_anim_api::{ARKIT_COUNT, CHANNELS, Channel, ChannelBuf};
use omuzaki_char_api::{HeadParams, face_texture_desc};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::{CpuLayer, Repaint, SkiaRasterizer};
use omuzaki_face::{FaceSystem, RealPainter};
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceProfile, FaceRasterizer, FaceView, Palette,
    Raster,
};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError, TextureDesc};

/// A talking, blinking, glancing face with tracker noise on every channel, at 60 frames a second; from frame 40 a
/// blush fades in, from 60 the gloom shows, and from 80 a wide surprised face holds for twenty frames.
fn tracked(frame: u32) -> ChannelBuf {
    let t = frame as f32 / 60.0;
    let mut buf = ChannelBuf::NEUTRAL;
    // Tracker noise: up to two hundredths on every channel, the same on every run.
    for (i, info) in CHANNELS.iter().take(ARKIT_COUNT).enumerate() {
        let h = (frame.wrapping_mul(2_654_435_761) ^ (i as u32).wrapping_mul(40_503)).rotate_left(13);
        buf.set(info.channel, 0.02 * (h % 1000) as f32 / 1000.0);
    }
    let talk = (0.5 + 0.5 * (t * 23.0).sin() * (t * 3.1).sin()).max(0.0) * 0.45;
    buf.set(Channel::JAW_OPEN, talk);
    buf.set(Channel::MOUTH_FUNNEL, 0.2 * (0.5 + 0.5 * (t * 5.3).sin()));
    buf.set(Channel::MOUTH_SMILE_LEFT, 0.25 + 0.1 * (t * 0.7).sin());
    buf.set(Channel::MOUTH_SMILE_RIGHT, 0.25 + 0.1 * (t * 0.7).sin());
    let phase = t % 3.5;
    let blink = if phase < 0.15 { (phase / 0.15 * std::f32::consts::PI).sin() } else { 0.0 };
    buf.set(Channel::EYE_BLINK_LEFT, blink);
    buf.set(Channel::EYE_BLINK_RIGHT, blink);
    let look = 0.3 * (t.floor() * 1.7).sin();
    buf.set(if look > 0.0 { Channel::EYE_LOOK_OUT_LEFT } else { Channel::EYE_LOOK_IN_LEFT }, look.abs());
    buf.set(if look > 0.0 { Channel::EYE_LOOK_IN_RIGHT } else { Channel::EYE_LOOK_OUT_RIGHT }, look.abs());
    buf.set(Channel::BROW_INNER_UP, 0.2 + 0.15 * (t * 0.9).sin());
    if frame >= 40 {
        buf.set(Channel::BLUSH, ((frame - 40) as f32 / 10.0).min(0.8));
    }
    if frame >= 60 {
        buf.set(Channel::EYES_GLOOM, 1.0);
    }
    if (80..100).contains(&frame) {
        buf.set(Channel::EYE_WIDE_LEFT, 1.0);
        buf.set(Channel::EYE_WIDE_RIGHT, 1.0);
        buf.set(Channel::JAW_OPEN, 0.7);
        buf.set(Channel::BROW_OUTER_UP_LEFT, 1.0);
        buf.set(Channel::BROW_OUTER_UP_RIGHT, 1.0);
    }
    buf
}

/// The default face painted for `frame` at `px_per_m`.
struct Face {
    profile: FaceProfile,
    painter: RealPainter,
    drawing: FaceDrawing,
}

impl Face {
    fn new(px_per_m: f32) -> Self {
        let profile = FaceProfile::default().resolve_colours(&Palette::default()).unwrap();
        let painter = RealPainter::new(HeadParams::default().face_projection(), px_per_m);
        Self { profile, painter, drawing: FaceDrawing::new() }
    }

    fn paint(&mut self, frame: u32) {
        let params = self.profile.mapping.apply(&tracked(frame));
        let input = FaceInput {
            profile: &self.profile,
            params: &params,
            time: Seconds(f64::from(frame) / 60.0),
            view: FaceView::default(),
        };
        self.painter.paint(&input, &mut self.drawing).unwrap();
    }

    fn spec(&self, layer: FaceLayerId) -> FaceLayerSpec {
        *self.painter.layers().iter().find(|s| s.id == layer).unwrap()
    }
}

fn whole(spec: &FaceLayerSpec, face: &Face) -> Raster {
    let mut raster = Raster::new(0, 0);
    SkiaRasterizer::new().rasterize(spec, &face.drawing[spec.id], spec.px_per_m, &mut raster).unwrap();
    raster
}

/// The first pixel where two rasters differ, for the failure message.
fn first_difference(a: &Raster, b: &Raster) -> Option<(u32, u32, [u8; 4], [u8; 4])> {
    let at = a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)).position(|(p, q)| p != q)?;
    let (x, y) = (at as u32 % a.width, at as u32 / a.width);
    let px = |r: &Raster| r.pixels[at * 4..at * 4 + 4].try_into().unwrap();
    Some((x, y, px(a), px(b)))
}

#[test]
fn a_partial_repaint_draws_exactly_what_a_whole_one_does() {
    let mut face = Face::new(900.0);
    let mut cpu = SkiaRasterizer::new();
    let mut layers: Vec<(FaceLayerId, CpuLayer)> =
        FaceLayerId::ALL.into_iter().map(|id| (id, CpuLayer::default())).collect();
    let mut parts = 0;
    for frame in 0..110 {
        face.paint(frame);
        for (id, layer) in &mut layers {
            let spec = face.spec(*id);
            let repaint = layer.repaint(&mut cpu, &spec, &face.drawing[*id]).unwrap();
            parts += usize::from(matches!(repaint, Repaint::Part(_)));
            let expected = whole(&spec, &face);
            assert!(
                layer.raster() == &expected,
                "frame {frame}, {id:?} ({repaint:?}): first difference {:?}",
                first_difference(layer.raster(), &expected)
            );
        }
    }
    assert!(parts > 100, "most repaints were partial ({parts})");
}

#[test]
fn a_tracked_face_redraws_a_small_share_of_its_layer() {
    let mut face = Face::new(900.0);
    let mut cpu = SkiaRasterizer::new();
    let mut layer = CpuLayer::default();
    let spec = face.spec(FaceLayerId::Base);
    let area = f64::from(spec.size_px().0) * f64::from(spec.size_px().1);
    let (mut drawn, mut wholes, mut frames) = (0.0, 0, 0);
    // The talking face before the blush: after a few frames to settle the snapshot.
    for frame in 0..40 {
        face.paint(frame);
        let repaint = layer.repaint(&mut cpu, &spec, &face.drawing[FaceLayerId::Base]).unwrap();
        if frame < 10 {
            continue;
        }
        frames += 1;
        match repaint {
            Repaint::Whole => wholes += 1,
            Repaint::Part(_) => drawn += layer.damage().iter().map(|r| r.area() as f64).sum::<f64>(),
            Repaint::Unchanged => {}
        }
    }
    let share = drawn / f64::from(frames) / area;
    assert_eq!(wholes, 0, "no whole repaint once the snapshot settled");
    assert!(share < 0.25, "a tracked frame redraws {:.0}% of the layer", share * 100.0);
    assert!(layer.base() > 0, "the snapshot holds the wash");
}

/// A texture that keeps what the painter writes, rectangle by rectangle.
struct Texture {
    desc: TextureDesc,
    pixels: Vec<u8>,
    writes: Vec<PixelRect>,
    refuse: bool,
}

impl PaintTarget for Texture {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        if self.refuse {
            return Err(RenderError::invalid("texture", "refused"));
        }
        let width = self.desc.size.x as usize;
        let row = rect.width as usize * 4;
        for (i, line) in rgba.chunks_exact(row).enumerate() {
            let at = ((rect.y as usize + i) * width + rect.x as usize) * 4;
            self.pixels[at..at + row].copy_from_slice(line);
        }
        self.writes.push(rect);
        Ok(())
    }

    fn native(&mut self) -> &mut dyn Any {
        &mut self.refuse
    }
}

#[test]
fn the_texture_gets_only_what_changed_and_always_ends_up_whole() {
    let face = Face::new(900.0);
    let mut system = FaceSystem::real(
        RealPainter::new(HeadParams::default().face_projection(), 900.0),
        Box::new(SkiaRasterizer::new()),
    );
    let spec = face.spec(FaceLayerId::Base);
    let desc = face_texture_desc(&spec);
    let mut texture = Texture { pixels: vec![0; desc.byte_len()], desc, writes: Vec::new(), refuse: false };
    let full = PixelRect::full(texture.desc.size);
    let mut partial = 0;
    for frame in 0..30 {
        let update = system.update(&tracked(frame), &face.profile, Seconds(0.0), FaceView::default()).unwrap();
        // A refused write, then the stage's notice: the next write must send the whole layer.
        texture.refuse = frame == 20;
        if update.is_changed(FaceLayerId::Base) || frame == 21 {
            let result = system.layer_painter(FaceLayerId::Base).unwrap().paint(&mut texture);
            if frame == 20 {
                assert!(result.is_err());
                system.gpu_paint_failed(FaceLayerId::Base);
                continue;
            }
            result.unwrap();
        }
        let last = texture.writes.last().copied();
        if frame == 0 || frame == 21 {
            assert_eq!(last, Some(full), "frame {frame} writes the whole layer");
        } else if last.is_some_and(|r| r != full) {
            partial += 1;
        }
        assert!(texture.pixels == system.raster(FaceLayerId::Base).unwrap().pixels, "frame {frame}: the texture lags");
        texture.writes.clear();
    }
    assert!(partial > 20, "most writes send part of the layer ({partial})");
}

#[test]
fn tracked_face_timing() {
    let mut face = Face::new(omuzaki_face_api::DEFAULT_PX_PER_M);
    let spec = face.spec(FaceLayerId::Base);
    let (mut cpu, mut layer, mut out) = (SkiaRasterizer::new(), CpuLayer::default(), Raster::new(0, 0));
    let (mut whole_ms, mut part_ms) = (Vec::new(), Vec::new());
    for frame in 0..40 {
        face.paint(frame);
        let list = &face.drawing[FaceLayerId::Base];
        let t = Instant::now();
        cpu.rasterize(&spec, list, spec.px_per_m, &mut out).unwrap();
        let whole = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        layer.repaint(&mut cpu, &spec, list).unwrap();
        let part = t.elapsed().as_secs_f64() * 1e3;
        // The first frames draw whole and settle the snapshot.
        if frame >= 10 {
            whole_ms.push(whole);
            part_ms.push(part);
        }
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[v.len() * 95 / 100])
    };
    let ((whole_50, whole_95), (part_50, part_95)) = (stats(&mut whole_ms), stats(&mut part_ms));
    println!(
        "tracked face, Base layer {}x{}: whole repaint p50 {whole_50:.1} ms p95 {whole_95:.1} ms; partial p50 {part_50:.1} ms p95 {part_95:.1} ms",
        spec.size_px().0,
        spec.size_px().1
    );
    // A ceiling that holds on any machine, debug or release: the partial repaint costs well under half the whole one.
    assert!(part_50 * 2.0 < whole_50, "partial {part_50:.1} ms against whole {whole_50:.1} ms");
}
