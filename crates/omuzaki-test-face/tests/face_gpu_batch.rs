//! Batched layers on the GPU (#318): when the face system stages a frame's changed layers (`GpuRasterizer::stage`), the
//! frame's first paint rasterises all of them in one run of Vello's stages and each layer's own paint only resolves its
//! part. The textures it leaves equal the ones painting each layer on its own leaves, over a sequence of frames, after
//! the batch's recorder fails, and when a staged layer is painted a frame late. On lavapipe at probe size; without an
//! adapter the tests print why and pass.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Gpu, Native, device, native, read_back, submit};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::{VelloEngines, VelloRasterizer};
use omuzaki_face::{GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FacePainter, FaceView, PaintParams,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The layers are about this many pixels wide: probe size.
const PROBE_WIDTH_PX: f32 = 256.0;
/// The most a channel may differ between the batched and the lone path: the batch clips each layer to its rows, and
/// Vello composites a clip in floating point before it rounds.
const MAX_CHANNEL_DIFF: u8 = 1;

/// One frame of every layer: the specs, the drawing and the sections.
struct Frame {
    specs: Vec<FaceLayerSpec>,
    drawing: FaceDrawing,
    sections: [LayerSections; FaceLayerId::COUNT],
}

fn frame(painter: &mut RealPainter, change: fn(&mut PaintParams)) -> Frame {
    let profile = mock_profile();
    let mut params = FaceMapping::default().rest_params();
    change(&mut params);
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).unwrap();
    let sections = FaceLayerId::ALL.map(|id| painter.sections().layer(id).clone());
    Frame { specs: painter.layers().to_vec(), drawing, sections }
}

/// Every layer changes: the brows, effects on the Overlay, the eyes and mouth on the Base.
fn everything(p: &mut PaintParams) {
    p.left_brow.height = 1.0;
    p.right_brow.height = 0.6;
    p.mouth.open = 0.7;
    p.mouth.smile_left = 1.0;
    p.left_eye.close = 0.5;
    p.blush = 1.2;
    p.sweat = 1.0;
    p.effects.gloom = 1.0;
}

fn mouth_only(p: &mut PaintParams) {
    everything(p);
    p.mouth.open = 0.2;
}

fn neutral(_: &mut PaintParams) {}

/// The layers' textures, one per spec, painted by one rasteriser.
struct Layers {
    rasterizer: VelloRasterizer,
    natives: Vec<Native>,
    batched: bool,
}

impl Layers {
    fn new(gpu: &Gpu, frame: &Frame, batched: bool) -> Self {
        let natives = frame.specs.iter().map(|spec| native(gpu, spec.size_px())).collect();
        Self { rasterizer: VelloRasterizer::new(), natives, batched }
    }

    /// Stages `frame` (when batched), as `FaceSystem::update` does for the layers whose lists changed.
    fn stage(&mut self, frame: &Frame) {
        if self.batched {
            self.rasterizer.begin_frame();
            for spec in &frame.specs {
                let at = spec.id as usize;
                self.rasterizer.stage(spec, &frame.drawing[spec.id], &frame.sections[at]).unwrap();
            }
        }
    }

    /// Paints layer `at` of `frame` and submits it, as the renderer does after each paint.
    fn paint(&mut self, frame: &Frame, at: usize) {
        let spec = &frame.specs[at];
        let list = &frame.drawing[spec.id];
        let drawn = self
            .rasterizer
            .rasterize_native_sections(spec, list, &frame.sections[spec.id as usize], &mut self.natives[at])
            .unwrap();
        assert!(drawn, "the rasteriser drew the {:?} layer", spec.id);
        submit(&mut self.natives[at]);
    }

    /// Stages and paints every layer of `frame`.
    fn show(&mut self, frame: &Frame) {
        self.stage(frame);
        for at in 0..frame.specs.len() {
            self.paint(frame, at);
        }
    }

    fn pixels(&self, gpu: &Gpu, at: usize) -> Vec<u8> {
        read_back(gpu, &self.natives[at].2)
    }
}

/// Asserts that every layer of `a` and `b` holds the same picture, within [`MAX_CHANNEL_DIFF`], and that the face
/// drew something (or the comparison would prove nothing).
fn assert_same(gpu: &Gpu, a: &Layers, b: &Layers, what: &str) {
    let mut painted = 0;
    for at in 0..a.natives.len() {
        let (pa, pb) = (a.pixels(gpu, at), b.pixels(gpu, at));
        let worst = pa.iter().zip(&pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        painted += pa.iter().skip(3).step_by(4).filter(|alpha| **alpha > 0).count();
        assert!(worst <= MAX_CHANNEL_DIFF, "{what}: layer {at} differs by up to {worst} per channel");
    }
    assert!(painted > 0, "{what}: every layer is blank, so the comparison proves nothing");
}

fn painter() -> RealPainter {
    RealPainter::new(mock_projection(), PROBE_WIDTH_PX / mock_projection().rect.width())
}

#[test]
fn batched_frames_leave_the_same_textures_as_lone_paints() {
    let Some(gpu) = device() else { return };
    let mut painter = painter();
    let frames = [neutral, everything, mouth_only, neutral].map(|change| frame(&mut painter, change));
    let mut batched = Layers::new(&gpu, &frames[0], true);
    let mut alone = Layers::new(&gpu, &frames[0], false);
    for (step, frame) in frames.iter().enumerate() {
        batched.show(frame);
        alone.show(frame);
        assert_same(&gpu, &batched, &alone, &format!("frame {step}"));
        if step == 1 {
            // The first frame paints every layer whole (nothing was painted yet); this one changes all three.
            assert_eq!(batched.rasterizer.layers_batched(), frames[1].specs.len(), "every changed layer in one batch");
        }
        if step == 2 {
            assert_eq!(batched.rasterizer.layers_batched(), 0, "one changed layer is drawn on its own");
        }
    }
    assert_eq!(alone.rasterizer.layers_batched(), 0, "nothing staged, nothing batched");
}

#[test]
fn a_dropped_batch_leaves_the_other_layers_drawn_on_their_own() {
    let Some(gpu) = device() else { return };
    let mut painter = painter();
    let frames = [neutral, everything, mouth_only].map(|change| frame(&mut painter, change));
    let mut batched = Layers::new(&gpu, &frames[0], true);
    let mut alone = Layers::new(&gpu, &frames[0], false);
    batched.show(&frames[0]);
    alone.show(&frames[0]);

    // The renderer refuses the recorder's paint: its commands, the batch's raster with them, are dropped unsubmitted.
    batched.stage(&frames[1]);
    let spec = frames[1].specs[0];
    batched
        .rasterizer
        .rasterize_native_sections(
            &spec,
            &frames[1].drawing[spec.id],
            &frames[1].sections[spec.id as usize],
            &mut batched.natives[0],
        )
        .unwrap();
    batched.natives[0].3 = gpu.0.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    batched.rasterizer.paint_failed(spec.id);
    for at in 1..frames[1].specs.len() {
        batched.paint(&frames[1], at);
    }
    alone.show(&frames[1]);
    for at in 1..frames[1].specs.len() {
        assert_eq!(batched.pixels(&gpu, at), alone.pixels(&gpu, at), "layer {at} after the recorder failed");
    }

    // The next frame repaints the failed layer whole.
    batched.show(&frames[2]);
    alone.show(&frames[2]);
    assert_same(&gpu, &batched, &alone, "the frame after the failure");
}

#[test]
fn a_staged_layer_painted_a_frame_late_shows_its_staged_picture() {
    let Some(gpu) = device() else { return };
    let mut painter = painter();
    let frames = [neutral, everything].map(|change| frame(&mut painter, change));
    let mut batched = Layers::new(&gpu, &frames[0], true);
    let mut alone = Layers::new(&gpu, &frames[0], false);
    batched.show(&frames[0]);
    alone.show(&frames[0]);

    // Every layer is staged, but the last one's paint does not come this frame.
    batched.stage(&frames[1]);
    let last = frames[1].specs.len() - 1;
    for at in 0..last {
        batched.paint(&frames[1], at);
    }
    // Next frame nothing changed, so nothing is staged; the late layer is painted now.
    batched.rasterizer.begin_frame();
    batched.paint(&frames[1], last);
    alone.show(&frames[1]);
    assert_same(&gpu, &batched, &alone, "the late layer");
}

#[test]
fn faces_on_one_device_share_one_engine_and_each_draws_its_own() {
    let Some(gpu) = device() else { return };
    let mut painter = painter();
    let frames = [neutral, everything, mouth_only].map(|change| frame(&mut painter, change));
    // Two characters on stage sharing one group, painting different frames, interleaved as the stage updates them.
    let engines = VelloEngines::new();
    let shared = |frame: &Frame| {
        let mut layers = Layers::new(&gpu, frame, true);
        layers.rasterizer = VelloRasterizer::new().sharing(&engines);
        layers
    };
    let (mut first, mut second) = (shared(&frames[0]), shared(&frames[0]));
    // The same two with engines of their own.
    let mut first_alone = Layers::new(&gpu, &frames[0], true);
    let mut second_alone = Layers::new(&gpu, &frames[0], true);
    for step in 0..frames.len() {
        let (a, b) = (&frames[step], &frames[(step + 1) % frames.len()]);
        first.stage(a);
        second.stage(b);
        for at in 0..a.specs.len() {
            first.paint(a, at);
            second.paint(b, at);
        }
        first_alone.show(a);
        second_alone.show(b);
        assert_same(&gpu, &first, &first_alone, &format!("the first face, frame {step}"));
        assert_same(&gpu, &second, &second_alone, &format!("the second face, frame {step}"));
    }
    assert!(first.rasterizer.shares_engine_with(&second.rasterizer), "one engine for the device");
    assert!(!first_alone.rasterizer.shares_engine_with(&second_alone.rasterizer), "no group, own engine");
    assert!(!first.rasterizer.shares_engine_with(&first_alone.rasterizer));
}
