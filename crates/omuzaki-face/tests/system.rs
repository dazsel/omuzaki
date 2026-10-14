//! `FaceSystem` against the mock painter, the mock rasteriser and the mock renderer: a layer is rasterised and painted
//! only when its picture changed.

// A test that cannot set up its fixtures should stop with the reason.
#![allow(clippy::expect_used)]

use std::any::Any;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::face_texture_desc;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_face::{FaceSystem, FaceUpdate, GpuRasterizer, RealPainter};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter,
    FaceProfile, FaceRasterizer, FaceView, Raster,
};
use omuzaki_face_mock::{MockPainter, MockRasterizer, mock_profile, mock_projection};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError, Renderer};
use omuzaki_render_mock::MockRenderer;

fn channels(set: &[(Channel, f32)]) -> ChannelBuf {
    let mut buf = ChannelBuf::NEUTRAL;
    for &(channel, value) in set {
        buf.set(channel, value);
    }
    buf
}

fn update(system: &mut FaceSystem, profile: &FaceProfile, buf: &ChannelBuf) -> FaceUpdate {
    system.update(buf, profile, Seconds(0.0), FaceView::default()).expect("the mock painter paints any face")
}

fn changed(update: &FaceUpdate) -> Vec<FaceLayerId> {
    update.changed_layers().collect()
}

/// A rasteriser that remembers which layers it was asked for, then rasterises like the mock one.
#[derive(Clone, Default)]
struct Spy(Arc<Mutex<Vec<FaceLayerId>>>);

impl Spy {
    fn take(&self) -> Vec<FaceLayerId> {
        std::mem::take(&mut *self.0.lock().expect("spy lock"))
    }
}

impl FaceRasterizer for Spy {
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError> {
        self.0.lock().expect("spy lock").push(spec.id);
        MockRasterizer.rasterize(spec, list, px_per_m, out)
    }
}

fn mock_system(spy: &Spy) -> FaceSystem {
    FaceSystem::new(Box::new(MockPainter::default()), Box::new(spy.clone()))
}

#[test]
fn the_real_painter_draws_every_layer_once_and_then_nothing_changes_while_the_face_does_not() {
    let profile = mock_profile();
    let painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut system = FaceSystem::new(Box::new(painter), Box::new(MockRasterizer));

    let first = update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    assert_eq!(changed(&first), FaceLayerId::ALL, "the first update shows every layer");
    let moved = channels(&[(Channel::JAW_OPEN, 1.0), (Channel::EYE_BLINK_LEFT, 1.0), (Channel::BROW_INNER_UP, 1.0)]);
    update(&mut system, &profile, &moved);
    let later = update(&mut system, &profile, &moved);
    assert!(!later.any_changed(), "the sections are pure, so an unchanged face repaints identical lists");
}

#[test]
fn a_layer_is_rasterised_only_when_its_fingerprint_changes() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let mut system = mock_system(&spy);
    let neutral = ChannelBuf::NEUTRAL;

    let first = update(&mut system, &profile, &neutral);
    assert_eq!(changed(&first), [FaceLayerId::Base, FaceLayerId::Brows]);
    assert_eq!(spy.take(), [FaceLayerId::Base, FaceLayerId::Brows]);

    let same = update(&mut system, &profile, &neutral);
    assert!(!same.any_changed());
    assert!(spy.take().is_empty(), "an unchanged face rasterises nothing");

    // A blink moves the eyes, which live on the Base layer only.
    let blink = channels(&[(Channel::EYE_BLINK_LEFT, 1.0)]);
    assert_eq!(changed(&update(&mut system, &profile, &blink)), [FaceLayerId::Base]);
    assert_eq!(spy.take(), [FaceLayerId::Base]);

    // Brows are drawn on both layers.
    let brows = channels(&[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::BROW_INNER_UP, 1.0)]);
    assert_eq!(changed(&update(&mut system, &profile, &brows)), [FaceLayerId::Base, FaceLayerId::Brows]);
    assert_eq!(spy.take(), [FaceLayerId::Base, FaceLayerId::Brows]);
}

#[test]
fn the_cpu_raster_matches_the_draw_list() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let mut system = mock_system(&spy);
    assert!(system.raster(FaceLayerId::Base).is_none(), "nothing is rasterised before the first update");
    update(&mut system, &profile, &ChannelBuf::NEUTRAL);

    let spec = system.layers()[0];
    let mut expected = Raster::new(0, 0);
    MockRasterizer
        .rasterize(&spec, &system.drawing()[FaceLayerId::Base], spec.px_per_m, &mut expected)
        .expect("the mock rasteriser draws the mock list");
    assert_eq!(system.raster(FaceLayerId::Base), Some(&expected));
    assert!(system.raster(FaceLayerId::Overlay).is_none(), "the mock painter doesn't fill the overlay layer");
}

/// The mock painter with a density the test can change from outside, as a stage's quality knob would.
struct Resizable {
    inner: MockPainter,
    px_per_m: Arc<AtomicU32>,
    layers: [FaceLayerSpec; 2],
}

impl FacePainter for Resizable {
    fn layers(&self) -> &[FaceLayerSpec] {
        &self.layers
    }

    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError> {
        let base = f32::from_bits(self.px_per_m.load(Ordering::Relaxed));
        for spec in &mut self.layers {
            *spec = FaceLayerSpec::for_layer(spec.id, &mock_projection(), base);
        }
        self.inner.paint(input, out)
    }
}

#[test]
fn a_density_change_rasterises_again_without_changing_the_draw_list() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let px_per_m = Arc::new(AtomicU32::new(1000.0_f32.to_bits()));
    let inner = MockPainter::default();
    let layers = [inner.layers()[0], inner.layers()[1]];
    let painter = Resizable { inner, px_per_m: px_per_m.clone(), layers };
    let mut system = FaceSystem::new(Box::new(painter), Box::new(spy.clone()));

    update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    spy.take();
    assert!(!update(&mut system, &profile, &ChannelBuf::NEUTRAL).any_changed());

    px_per_m.store(500.0_f32.to_bits(), Ordering::Relaxed);
    let resized = update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    assert_eq!(changed(&resized), [FaceLayerId::Base, FaceLayerId::Brows]);
    assert_eq!(spy.take(), [FaceLayerId::Base, FaceLayerId::Brows]);
    let raster = system.raster(FaceLayerId::Base).expect("rasterised");
    assert_eq!((raster.width, raster.height), system.layers()[0].size_px());
}

/// Fails the first rasterisation, then behaves.
struct FailsOnce(bool);

impl FaceRasterizer for FailsOnce {
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError> {
        if !std::mem::replace(&mut self.0, true) {
            return Err(FaceError::Raster("out of luck".to_owned()));
        }
        MockRasterizer.rasterize(spec, list, px_per_m, out)
    }
}

#[test]
fn a_failed_update_leaves_every_layer_to_be_reported_next_time() {
    let profile = mock_profile();
    let mut system = FaceSystem::new(Box::new(MockPainter::default()), Box::new(FailsOnce(false)));
    let neutral = ChannelBuf::NEUTRAL;
    assert!(system.update(&neutral, &profile, Seconds(0.0), FaceView::default()).is_err());
    let retry = update(&mut system, &profile, &neutral);
    assert_eq!(changed(&retry), [FaceLayerId::Base, FaceLayerId::Brows], "nothing was lost to the failure");
}

/// A clock that advances one second every time it is read.
struct StepClock(AtomicUsize);

impl Clock for StepClock {
    #[allow(clippy::cast_precision_loss)] // a handful of reads
    fn now(&self) -> Seconds {
        Seconds(self.0.fetch_add(1, Ordering::Relaxed) as f64)
    }
}

#[test]
fn timings_come_from_the_injected_clock() {
    let profile = mock_profile();
    let mut system = FaceSystem::new(Box::new(MockPainter::default()), Box::new(MockRasterizer))
        .with_clock(Box::new(StepClock(AtomicUsize::new(0))));
    let timings = update(&mut system, &profile, &ChannelBuf::NEUTRAL).timings;
    assert_eq!((timings.map, timings.paint, timings.raster), (Seconds(1.0), Seconds(1.0), Seconds(1.0)));
    assert_eq!(timings.total(), Seconds(3.0));
}

// ------------------------------------------------------------------------------------------------ the renderer's painter

/// A paint target that records what is written and offers a native handle of its own.
struct Target {
    desc: omuzaki_render_api::TextureDesc,
    written: Option<Vec<u8>>,
    native: Native,
}

#[derive(Debug)]
struct Native;

impl Target {
    fn for_layer(spec: &FaceLayerSpec) -> Self {
        Self { desc: face_texture_desc(spec), written: None, native: Native }
    }
}

impl PaintTarget for Target {
    fn desc(&self) -> &omuzaki_render_api::TextureDesc {
        &self.desc
    }

    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        assert_eq!(rect, PixelRect::full(self.desc.size));
        assert_eq!(rgba.len(), self.desc.byte_len());
        self.written = Some(rgba.to_vec());
        Ok(())
    }

    fn native(&mut self) -> &mut dyn Any {
        &mut self.native
    }
}

#[test]
fn the_layer_painter_writes_the_cpu_raster_into_the_texture() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let mut system = mock_system(&spy);
    assert!(system.layer_painter(FaceLayerId::Base).is_none(), "no painter before the first update");
    update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    spy.take();

    let spec = system.layers()[0];
    let mut target = Target::for_layer(&spec);
    system.layer_painter(FaceLayerId::Base).expect("Base is painted").paint(&mut target).expect("sizes match");
    let expected = system.raster(FaceLayerId::Base).expect("rasterised").pixels.clone();
    assert_eq!(target.written, Some(expected));
    assert!(spy.take().is_empty(), "painting a texture doesn't rasterise again");
    assert!(system.layer_painter(FaceLayerId::Overlay).is_none(), "the mock painter has no overlay layer");
}

#[test]
fn the_layer_painter_refuses_a_texture_of_the_wrong_size() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let mut system = mock_system(&spy);
    update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    let mut wrong = Target::for_layer(&system.layers()[1]); // the half-density brows texture
    let outcome = system.layer_painter(FaceLayerId::Base).expect("Base is painted").paint(&mut wrong);
    assert!(matches!(outcome, Err(RenderError::Invalid { what: "face layer", .. })), "{outcome:?}");
    assert!(wrong.written.is_none());
}

#[test]
fn the_layers_reach_the_mock_renderer_through_paint_texture() {
    let (profile, spy) = (mock_profile(), Spy::default());
    let mut system = mock_system(&spy);
    let mut renderer = MockRenderer::new();
    let textures: Vec<_> = system
        .layers()
        .iter()
        .map(|spec| renderer.create_texture(&face_texture_desc(spec), None).expect("a valid texture"))
        .collect();

    let first = update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    for (spec, &texture) in system.layers().to_vec().iter().zip(&textures) {
        if first.is_changed(spec.id) {
            let mut painter = system.layer_painter(spec.id).expect("painted layer");
            renderer.paint_texture(texture, &mut painter).expect("the mock renderer accepts the face layer");
        }
    }
}

/// A GPU rasteriser that draws into `Native` targets (counting) and declines everything else.
#[derive(Clone, Default)]
struct Gpu(Arc<AtomicUsize>);

impl GpuRasterizer for Gpu {
    fn rasterize_native(&mut self, _: &FaceLayerSpec, _: &DrawList, native: &mut dyn Any) -> Result<bool, FaceError> {
        let known = native.downcast_mut::<Native>().is_some();
        if known {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        Ok(known)
    }
}

#[test]
fn a_gpu_rasteriser_draws_layers_and_the_cpu_raster_is_skipped() {
    let (profile, spy, gpu) = (mock_profile(), Spy::default(), Gpu::default());
    let mut system = mock_system(&spy);
    system.set_gpu_rasterizer(Some(Box::new(gpu.clone())));

    let first = update(&mut system, &profile, &ChannelBuf::NEUTRAL);
    assert_eq!(changed(&first), [FaceLayerId::Base, FaceLayerId::Brows], "layers still report as changed");
    assert!(spy.take().is_empty(), "with a GPU rasteriser the CPU raster is not made up front");
    assert!(system.raster(FaceLayerId::Base).is_none());

    let mut target = Target::for_layer(&system.layers()[0]);
    system.layer_painter(FaceLayerId::Base).expect("painted layer").paint(&mut target).expect("the GPU path draws");
    assert_eq!(gpu.0.load(Ordering::Relaxed), 1);
    assert!(target.written.is_none(), "nothing was written from the CPU");
    assert!(spy.take().is_empty(), "the GPU path never touches the CPU rasteriser");
}

#[test]
fn a_gpu_rasteriser_that_does_not_know_the_target_falls_back_to_the_cpu_raster() {
    let (profile, spy, gpu) = (mock_profile(), Spy::default(), Gpu::default());
    let mut system = mock_system(&spy);
    system.set_gpu_rasterizer(Some(Box::new(gpu.clone())));
    update(&mut system, &profile, &ChannelBuf::NEUTRAL);

    // The mock renderer's native handle is its own marker type, which `Gpu` declines.
    let mut renderer = MockRenderer::new();
    let spec = system.layers()[0];
    let texture = renderer.create_texture(&face_texture_desc(&spec), None).expect("a valid texture");
    let mut painter = system.layer_painter(FaceLayerId::Base).expect("painted layer");
    renderer.paint_texture(texture, &mut painter).expect("falls back to the CPU raster");
    assert_eq!(gpu.0.load(Ordering::Relaxed), 0);
    assert_eq!(spy.take(), [FaceLayerId::Base], "the fallback rasterises the layer on demand");
    assert!(system.raster(FaceLayerId::Base).is_some(), "and keeps the raster for next time");
}
