//! Partial repaint by section on the GPU (CB-24, #1433 A5, decision 7.2): a repaint re-encodes only the sections whose
//! slice of the draw list changed, and the texture it leaves equals a full repaint. The real painter's section ranges reach
//! the rasteriser through `FaceSystem::real`. On lavapipe at probe size; without an adapter the tests print why and pass.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::any::Any;
use std::sync::{Arc, Mutex};

use common::{Gpu, device, face_texture, image, read_back};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::face_texture_desc;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{DrawList, FaceError, FaceLayerId, FaceLayerSpec, FaceView};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError, TextureDesc};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region};

const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
const MIN_PSNR_DB: f64 = 34.0;
/// The layer is about this many pixels wide: probe size.
const PROBE_WIDTH_PX: f32 = 256.0;

/// A rasteriser the test keeps a handle on, to read its counter while a system owns it.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<VelloRasterizer>>);

impl Shared {
    fn encoded(&self) -> usize {
        self.0.lock().unwrap().sections_encoded()
    }
}

impl GpuRasterizer for Shared {
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        self.0.lock().unwrap().rasterize_native(spec, list, native)
    }

    fn rasterize_native_sections(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        self.0.lock().unwrap().rasterize_native_sections(spec, list, sections, native)
    }
}

/// A paint target like the renderer's: its `native()` is the wgpu tuple.
struct Target {
    desc: TextureDesc,
    native: (wgpu::Device, wgpu::Queue, wgpu::Texture),
}

impl Target {
    fn new(gpu: &Gpu, spec: &FaceLayerSpec) -> Self {
        Self {
            desc: face_texture_desc(spec),
            native: (gpu.0.clone(), gpu.1.clone(), face_texture(&gpu.0, spec.size_px())),
        }
    }
}

impl PaintTarget for Target {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn write_rgba(&mut self, _: PixelRect, _: &[u8]) -> Result<(), RenderError> {
        Err(RenderError::invalid("write_rgba", "this target is GPU only: the painter fell back to the CPU".to_owned()))
    }

    fn native(&mut self) -> &mut dyn Any {
        &mut self.native
    }
}

/// A system around the real painter at probe density, with `gpu` registered.
fn system(gpu: &Shared) -> FaceSystem {
    let ppm = PROBE_WIDTH_PX / mock_projection().rect.width();
    let mut system = FaceSystem::real(RealPainter::new(mock_projection(), ppm), Box::new(SkiaRasterizer::new()));
    system.set_gpu_rasterizer(Some(Box::new(gpu.clone())));
    system
}

fn update(system: &mut FaceSystem, channels: &ChannelBuf) {
    system.update(channels, &mock_profile(), Seconds(0.0), FaceView::default()).unwrap();
}

fn paint_base(system: &mut FaceSystem, target: &mut Target) {
    system.layer_painter(FaceLayerId::Base).expect("Base is painted").paint(target).expect("the GPU path draws");
}

fn base_spec(system: &FaceSystem) -> FaceLayerSpec {
    system.layers().iter().find(|l| l.id == FaceLayerId::Base).copied().unwrap()
}

/// Neutral with `changes` set.
fn channels(changes: &[(Channel, f32)]) -> ChannelBuf {
    let mut buf = ChannelBuf::NEUTRAL;
    for &(channel, value) in changes {
        buf.set(channel, value);
    }
    buf
}

#[test]
fn a_mouth_only_change_encodes_only_the_mouth_and_an_unchanged_frame_encodes_nothing() {
    let Some(gpu) = device() else { return };
    let shared = Shared::default();
    let mut system = system(&shared);
    update(&mut system, &channels(&[]));
    let mut target = Target::new(&gpu, &base_spec(&system));
    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 7, "the first paint encodes every section");
    let before = read_back(&gpu, &target.native.2);

    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 0, "an unchanged frame encodes nothing");
    assert_eq!(read_back(&gpu, &target.native.2), before, "and leaves the texture as it was");

    update(&mut system, &channels(&[(Channel::JAW_OPEN, 0.6)]));
    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 1, "a mouth-only change encodes the mouth section alone");
    assert_ne!(read_back(&gpu, &target.native.2), before, "and the picture changed");
}

#[test]
fn a_new_target_texture_is_painted_whole_even_when_nothing_changed() {
    let Some(gpu) = device() else { return };
    let shared = Shared::default();
    let mut system = system(&shared);
    update(&mut system, &channels(&[(Channel::JAW_OPEN, 0.3)]));
    let spec = base_spec(&system);
    let (mut first, mut second) = (Target::new(&gpu, &spec), Target::new(&gpu, &spec));
    paint_base(&mut system, &mut first);
    paint_base(&mut system, &mut second);
    assert_eq!(shared.encoded(), 0, "no section changed");
    assert_eq!(
        read_back(&gpu, &first.native.2),
        read_back(&gpu, &second.native.2),
        "the new texture still got the picture"
    );
}

#[test]
fn twenty_four_mixed_changes_leave_the_texture_a_full_repaint_would() {
    let Some(gpu) = device() else { return };
    let shared = Shared::default();
    let mut partial = system(&shared);
    update(&mut partial, &channels(&[]));
    let spec = base_spec(&partial);
    let mut target = Target::new(&gpu, &spec);
    let size = spec.size_px();

    let steps: [&[(Channel, f32)]; 8] = [
        &[],
        &[(Channel::JAW_OPEN, 0.5)],
        &[(Channel::JAW_OPEN, 0.5), (Channel::EYE_BLINK_LEFT, 0.7)],
        &[(Channel::EYE_BLINK_LEFT, 0.7), (Channel::BLUSH, 0.8)],
        &[(Channel::BLUSH, 0.8), (Channel::MOUTH_SMILE_LEFT, 0.9), (Channel::MOUTH_SMILE_RIGHT, 0.9)],
        &[(Channel::MOUTH_SMILE_LEFT, 0.9), (Channel::MOUTH_SMILE_RIGHT, 0.9), (Channel::EYE_LOOK_OUT_LEFT, 0.8)],
        &[(Channel::EYE_LOOK_OUT_LEFT, 0.8)],
        &[],
    ];
    let (mut failures, mut identical) = (String::new(), 0);
    for frame in 0..24 {
        let buf = channels(steps[frame % steps.len()]);
        update(&mut partial, &buf);
        paint_base(&mut partial, &mut target);
        let partial_pixels = read_back(&gpu, &target.native.2);

        // The same frame from scratch: a fresh rasteriser and texture, so everything is encoded and drawn whole.
        let mut full = system(&Shared::default());
        update(&mut full, &buf);
        let mut fresh = Target::new(&gpu, &spec);
        paint_base(&mut full, &mut fresh);
        let full_pixels = read_back(&gpu, &fresh.native.2);
        identical += usize::from(partial_pixels == full_pixels);

        let report =
            compare_region(&image(size, &partial_pixels), &image(size, &full_pixels), Rect::full(size.0, size.1), RULE)
                .unwrap();
        if !report.passes(MIN_PSNR_DB) {
            failures.push_str(&format!("frame {frame}: {}\n", report.stats));
        }
    }
    eprintln!("{identical} of 24 partial repaints are byte-identical to the full repaint");
    assert!(failures.is_empty(), "partial repaints differ from full ones:\n{failures}");
}
