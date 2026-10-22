//! Partial repaint by section on the GPU (CB-24, #1433 A5, decision 7.2): a repaint re-encodes only the sections whose
//! slice of the draw list changed, and the texture it leaves equals a full repaint. The real painter's section ranges reach
//! the rasteriser through `FaceSystem::real`. On lavapipe at probe size; without an adapter the tests print why and pass.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::any::Any;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use common::{Gpu, Native, device, gpu_pixels, image, native, read_native, two_devices};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::face_texture_desc;
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{
    Cap, Cmd, Colour, DrawList, FaceError, FaceLayerId, FaceLayerSpec, FaceRasterizer, FaceRect, FaceView, Join, Paint,
    Raster,
};
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

    fn device_lost(&mut self) {
        self.0.lock().unwrap().device_lost();
    }

    fn last_damage(&self) -> Option<PixelRect> {
        self.0.lock().unwrap().last_damage()
    }

    fn paint_failed(&mut self, layer: FaceLayerId) {
        self.0.lock().unwrap().paint_failed(layer);
    }
}

/// A paint target like the renderer's: its `native()` is the wgpu tuple, and it keeps what the painter marked.
struct Target {
    desc: TextureDesc,
    native: Native,
    damage: Vec<PixelRect>,
}

impl Target {
    fn new(gpu: &Gpu, spec: &FaceLayerSpec) -> Self {
        Self { desc: face_texture_desc(spec), native: native(gpu, spec.size_px()), damage: Vec::new() }
    }

    /// The texture's pixels once what the painter recorded has run.
    fn pixels(&mut self) -> Vec<u8> {
        read_native(&mut self.native)
    }

    /// The rectangles painters reported changing since the last call.
    fn take_damage(&mut self) -> Vec<PixelRect> {
        std::mem::take(&mut self.damage)
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

    fn mark_damage(&mut self, rect: PixelRect) {
        self.damage.push(rect);
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

/// Paints the Base layer into `target` and submits what the painter recorded, as the renderer does after each paint.
fn paint_base(system: &mut FaceSystem, target: &mut Target) {
    system.layer_painter(FaceLayerId::Base).expect("Base is painted").paint(target).expect("the GPU path draws");
    common::submit(&mut target.native);
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
    let spec = base_spec(&system);
    let (width, height) = spec.size_px();
    let whole = PixelRect { x: 0, y: 0, width, height };
    let mut target = Target::new(&gpu, &spec);
    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 7, "the first paint encodes every section");
    assert_eq!(target.take_damage(), vec![whole], "and reports the whole layer as changed");
    let before = target.pixels();

    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 0, "an unchanged frame encodes nothing");
    assert_eq!(
        target.take_damage(),
        vec![PixelRect { x: 0, y: 0, width: 0, height: 0 }],
        "and reports nothing changed"
    );
    assert_eq!(target.pixels(), before, "and leaves the texture as it was");

    update(&mut system, &channels(&[(Channel::JAW_OPEN, 0.6)]));
    paint_base(&mut system, &mut target);
    assert_eq!(shared.encoded(), 1, "a mouth-only change encodes the mouth section alone");
    let after = target.pixels();
    let changed = changed_pixels(&before, &after, width).expect("the picture changed");
    let damage = target.take_damage();
    assert_eq!(damage.len(), 1, "one report per paint: {damage:?}");
    let rect = damage[0];
    assert!(rect.width * rect.height < width * height / 4, "a mouth-sized rectangle, not the layer: {rect:?}");
    assert!(
        contains(rect, changed),
        "the reported rectangle {rect:?} holds every pixel that changed ({changed:?}), so the mips over it are enough"
    );
}

/// The smallest rectangle holding every pixel that differs between two `width`-wide pictures; `None` if none does.
fn changed_pixels(before: &[u8], after: &[u8], width: u32) -> Option<PixelRect> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (at, (a, b)) in before.chunks(4).zip(after.chunks(4)).enumerate() {
        if a != b {
            let at = u32::try_from(at).unwrap();
            let (x, y) = (at % width, at / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    (x1 > x0).then(|| PixelRect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
}

/// Whether `outer` holds all of `inner`.
fn contains(outer: PixelRect, inner: PixelRect) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.width >= inner.x + inner.width
        && outer.y + outer.height >= inner.y + inner.height
}

/// Paints the Base layer of `system` into `target` and drops what the painter recorded, as the renderer does when the
/// paint fails after the painter returned, then tells the system so, as the stage does.
fn paint_base_and_drop(system: &mut FaceSystem, target: &mut Target, gpu: &Gpu) {
    system.layer_painter(FaceLayerId::Base).expect("Base is painted").paint(target).expect("the GPU path draws");
    target.native.3 = gpu.0.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    system.gpu_paint_failed(FaceLayerId::Base);
}

#[test]
fn after_a_paint_the_renderer_dropped_the_next_one_redraws_the_whole_layer() {
    let Some(gpu) = device() else { return };
    let run = |report_failure: bool| {
        let shared = Shared::default();
        let mut live = system(&shared);
        update(&mut live, &channels(&[]));
        let spec = base_spec(&live);
        let mut target = Target::new(&gpu, &spec);
        paint_base(&mut live, &mut target);
        // A blink the renderer never draws: its commands are dropped.
        update(&mut live, &channels(&[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 1.0)]));
        if report_failure {
            paint_base_and_drop(&mut live, &mut target, &gpu);
        } else {
            live.layer_painter(FaceLayerId::Base).unwrap().paint(&mut target).unwrap();
            target.native.3 = gpu.0.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        }
        // Then only the mouth moves.
        let last = [(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 1.0), (Channel::JAW_OPEN, 0.6)];
        update(&mut live, &channels(&last));
        target.take_damage();
        paint_base(&mut live, &mut target);
        let damage = target.take_damage();
        // The same frame from scratch.
        let mut fresh_system = system(&Shared::default());
        update(&mut fresh_system, &channels(&last));
        let mut fresh = Target::new(&gpu, &spec);
        paint_base(&mut fresh_system, &mut fresh);
        (target.pixels() == fresh.pixels(), damage, spec.size_px())
    };
    let (matches, damage, (width, height)) = run(true);
    assert!(matches, "after the dropped paint the texture shows what a fresh repaint shows");
    assert_eq!(damage, vec![PixelRect { x: 0, y: 0, width, height }], "because the next paint redrew the whole layer");
    let (matches_unreported, _, _) = run(false);
    assert!(
        !matches_unreported,
        "without the report the blink would never reach the texture, or this test aims at nothing"
    );
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
    assert_eq!(first.pixels(), second.pixels(), "the new texture still got the picture");
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
        let partial_pixels = target.pixels();

        // The same frame from scratch: a fresh rasteriser and texture, so everything is encoded and drawn whole.
        let mut full = system(&Shared::default());
        update(&mut full, &buf);
        let mut fresh = Target::new(&gpu, &spec);
        paint_base(&mut full, &mut fresh);
        let full_pixels = fresh.pixels();
        identical += usize::from(partial_pixels == full_pixels);

        let report =
            compare_region(&image(size, &partial_pixels), &image(size, &full_pixels), Rect::full(size.0, size.1), RULE)
                .unwrap();
        if !report.passes(MIN_PSNR_DB) {
            let _ = writeln!(failures, "frame {frame}: {}", report.stats);
        }
    }
    eprintln!("{identical} of 24 partial repaints are byte-identical to the full repaint");
    assert!(failures.is_empty(), "partial repaints differ from full ones:\n{failures}");
}

/// A 64 x 64 px layer and a list of one filled ellipse, `broken` ending in a clip that is never popped and a transform
/// that overflows (so the encode fails with the clip open).
fn small_layer(broken: bool) -> (FaceLayerSpec, DrawList) {
    let spec = FaceLayerSpec::for_rect(FaceRect { x0: -0.5, x1: 0.5, y0: -0.5, y1: 0.5 }, FaceLayerId::Base, 64.0);
    let mut list = DrawList::new();
    let ellipse = list.path().ellipse(Vec2::ZERO, 0.3, 0.2).finish();
    list.push(Cmd::Fill { path: ellipse, paint: Paint::Solid(Colour::rgba(200, 40, 60, 255)), opacity: 1.0 });
    if broken {
        let clip = list.path().ellipse(Vec2::ZERO, 0.1, 0.1).finish();
        list.push(Cmd::PushClip(clip));
        list.push(Cmd::PushTransform(Affine2::from_scale(Vec2::splat(f32::INFINITY))));
    }
    (spec, list)
}

#[test]
fn a_failed_encode_does_not_leave_the_old_picture_cached_over_a_half_built_scene() {
    let Some(gpu) = device() else { return };
    let (spec, good) = small_layer(false);
    let (_, broken) = small_layer(true);
    let mut native = native(&gpu, spec.size_px());
    let mut rasterizer = VelloRasterizer::new();

    assert!(rasterizer.rasterize_native(&spec, &good, &mut native).unwrap(), "the first frame draws");
    common::submit(&mut native);
    assert!(rasterizer.rasterize_native(&spec, &broken, &mut native).is_err(), "the broken frame fails to encode");
    assert!(rasterizer.rasterize_native(&spec, &good, &mut native).unwrap(), "the old content comes back");
    assert_eq!(rasterizer.sections_encoded(), 1, "so its section is encoded again, not taken from the failed one");

    let fresh = gpu_pixels(&gpu, &mut VelloRasterizer::new(), &spec, &good);
    assert_eq!(read_native(&mut native), fresh, "and the texture shows what a fresh repaint shows");
    assert!(fresh.chunks(4).any(|p| p[3] != 0), "the picture has ink, so equality is not two blank textures");
}

#[test]
fn a_different_device_gets_its_own_renderer_and_textures() {
    let Some((first, second)) = two_devices() else { return };
    let (spec, list) = small_layer(false);
    let mut rasterizer = VelloRasterizer::new();
    let on_first = gpu_pixels(&first, &mut rasterizer, &spec, &list);

    // The same rasteriser, the same layer size, another device: as after the renderer rebuilt its device.
    let on_second = gpu_pixels(&second, &mut rasterizer, &spec, &list);
    assert_eq!(on_second, on_first, "the second device draws the same picture, without a validation error");
    assert_eq!(
        on_second,
        gpu_pixels(&second, &mut VelloRasterizer::new(), &spec, &list),
        "as a fresh rasteriser would"
    );
}

#[test]
fn a_non_finite_opacity_is_an_error_on_the_gpu_path_as_on_the_cpu_path() {
    let Some(gpu) = device() else { return };
    let spec = FaceLayerSpec::for_rect(FaceRect { x0: -0.5, x1: 0.5, y0: -0.5, y1: 0.5 }, FaceLayerId::Base, 64.0);
    let mut list = DrawList::new();
    let ellipse = list.path().ellipse(Vec2::ZERO, 0.3, 0.2).finish();
    list.push(Cmd::Fill { path: ellipse, paint: Paint::Solid(Colour::rgba(200, 40, 60, 255)), opacity: f32::NAN });
    let mut native = native(&gpu, spec.size_px());
    let error = VelloRasterizer::new().rasterize_native(&spec, &list, &mut native).unwrap_err();
    assert!(matches!(error, FaceError::Raster(_)), "{error:?}");
    assert!(
        SkiaRasterizer::new().rasterize(&spec, &list, spec.px_per_m, &mut Raster::new(1, 1)).is_err(),
        "the CPU raster rejects the same list"
    );
}

#[test]
fn a_sheared_stroke_leaves_no_stale_ink_when_it_goes_away() {
    let Some(gpu) = device() else { return };
    let spec = FaceLayerSpec::for_rect(FaceRect { x0: -0.5, x1: 0.5, y0: -0.5, y1: 0.5 }, FaceLayerId::Base, 256.0);
    // A wide round-capped dash under a shear whose rows are longer than its columns: the stroke reaches further
    // sideways than the longest column says.
    let mut stroked = DrawList::new();
    let dash = stroked.path().move_to(Vec2::new(-0.01, 0.0)).line_to(Vec2::new(0.01, 0.0)).finish();
    stroked.push(Cmd::PushTransform(Affine2::from_cols(Vec2::new(1.0, 0.0), Vec2::new(1.0, 0.5), Vec2::ZERO)));
    stroked.push(Cmd::Stroke {
        path: dash,
        paint: Paint::Solid(Colour::rgba(200, 40, 60, 255)),
        width: 0.1,
        cap: Cap::Round,
        join: Join::Round,
        opacity: 1.0,
    });
    stroked.push(Cmd::PopTransform);
    let gone = DrawList::new();

    let mut native = native(&gpu, spec.size_px());
    let mut rasterizer = VelloRasterizer::new();
    assert!(rasterizer.rasterize_native(&spec, &stroked, &mut native).unwrap());
    let ink = read_native(&mut native).chunks(4).filter(|p| p[3] != 0).count();
    assert!(ink > 0, "the dash has ink to go stale");
    assert!(rasterizer.rasterize_native(&spec, &gone, &mut native).unwrap());
    let left: usize = read_native(&mut native).chunks(4).filter(|p| p[3] != 0).count();
    assert_eq!(left, 0, "{left} pixels of the old dash are left after the repaint");
}

#[test]
fn a_device_of_another_instance_gets_its_own_state_after_device_lost() {
    // Two independent instances: wgpu numbers resources per instance, so the first device of each is the same number
    // and the rasteriser can't tell them apart by comparing them. A rebuilt renderer is exactly this.
    let (Some(first), Some(second)) = (device(), device()) else { return };
    let (spec, list) = small_layer(false);
    let mut rasterizer = VelloRasterizer::new();
    let on_first = gpu_pixels(&first, &mut rasterizer, &spec, &list);

    rasterizer.device_lost();
    let on_second = gpu_pixels(&second, &mut rasterizer, &spec, &list);
    assert_eq!(on_second, on_first, "the second instance's device draws the same picture, without a validation error");
    assert_eq!(
        on_second,
        gpu_pixels(&second, &mut VelloRasterizer::new(), &spec, &list),
        "as a fresh rasteriser would"
    );
}

#[test]
fn device_lost_also_forgets_a_device_vello_was_rejected_on() {
    let Some(adapter) = common::adapter() else { return };
    let weak = wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..wgpu::DeviceDescriptor::default()
    };
    let Ok(weak) = pollster::block_on(adapter.request_device(&weak)) else {
        eprintln!("skipped: the adapter has no downlevel device");
        return;
    };
    let Some(strong) = device() else { return };
    let (spec, list) = small_layer(false);
    let mut rasterizer = VelloRasterizer::new();
    let mut on_weak = native(&weak, spec.size_px());
    assert!(!rasterizer.rasterize_native(&spec, &list, &mut on_weak).unwrap(), "the weak device was declined");

    // The capable device is the first of another instance, so it may compare equal to the weak one.
    rasterizer.device_lost();
    let drawn = gpu_pixels(&strong, &mut rasterizer, &spec, &list);
    assert!(drawn.chunks(4).any(|p| p[3] != 0), "the capable device draws");
}
