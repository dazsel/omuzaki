//! [`FaceSystem`]: channels in, changed face layers out.

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_face_api::{
    FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceProfile, FaceRasterizer, FaceView,
    Raster,
};

use crate::layer::{FaceLayerPainter, GpuRasterizer};
use crate::painter::{LayerSections, RealPainter};

/// The painter a system owns. The real one is kept by value so its section ranges reach the GPU rasteriser (Q2 of #1433);
/// any other (the mock) goes through the contract only.
enum Painter {
    Real(Box<RealPainter>),
    Other(Box<dyn FacePainter>),
}

impl Painter {
    fn layers(&self) -> &[FaceLayerSpec] {
        match self {
            Self::Real(painter) => painter.layers(),
            Self::Other(painter) => painter.layers(),
        }
    }

    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError> {
        match self {
            Self::Real(painter) => painter.paint(input, out),
            Self::Other(painter) => painter.paint(input, out),
        }
    }

    /// Where each section sits in `layer`'s list, if the painter records it.
    fn sections(&self, layer: FaceLayerId) -> Option<&LayerSections> {
        match self {
            Self::Real(painter) => Some(painter.sections().layer(layer)),
            Self::Other(_) => None,
        }
    }
}

/// What one layer's texture last showed, and the CPU raster of the current picture.
pub(crate) struct LayerSlot {
    /// The fingerprint and spec of the picture last reported as changed; `None` until the first one.
    shown: Option<(u64, FaceLayerSpec)>,
    /// The CPU raster of the current draw list, valid unless `stale`.
    pub(crate) raster: Raster,
    /// The raster lags the draw list: a GPU rasteriser is registered (it draws at paint time and the CPU raster is only
    /// the fallback), or the last CPU rasterisation failed.
    pub(crate) stale: bool,
}

impl LayerSlot {
    fn new() -> Self {
        Self { shown: None, raster: Raster::new(0, 0), stale: true }
    }
}

/// Where the time of one [`FaceSystem::update`] went, from the system's [`Clock`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FaceTimings {
    /// Mapping the channels to paint parameters.
    pub map: Seconds,
    /// The painter filling the draw lists.
    pub paint: Seconds,
    /// Fingerprinting, and CPU-rasterising the changed layers (zero with a GPU rasteriser: that draws at paint time).
    pub raster: Seconds,
}

impl FaceTimings {
    /// The whole update.
    pub fn total(&self) -> Seconds {
        Seconds(self.map.0 + self.paint.0 + self.raster.0)
    }
}

/// What one [`FaceSystem::update`] did. `Copy` and fixed-size: reading it allocates nothing.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FaceUpdate {
    changed: [bool; FaceLayerId::COUNT],
    /// Where the time went.
    pub timings: FaceTimings,
}

impl FaceUpdate {
    /// True if `layer`'s picture changed, so its texture needs painting.
    pub fn is_changed(&self, layer: FaceLayerId) -> bool {
        self.changed[layer as usize]
    }

    /// The layers whose picture changed, in [`FaceLayerId::ALL`] order.
    pub fn changed_layers(&self) -> impl Iterator<Item = FaceLayerId> + '_ {
        FaceLayerId::ALL.into_iter().filter(|&layer| self.is_changed(layer))
    }

    /// True if any layer changed.
    pub fn any_changed(&self) -> bool {
        self.changed.contains(&true)
    }
}

/// The per-frame face: owns the painter, the rasterisers, the draw lists, and for every layer the fingerprint it last
/// showed and its CPU raster. Nothing in [`update`](Self::update) allocates once the painter's lists have warmed up.
pub struct FaceSystem {
    painter: Painter,
    cpu: Box<dyn FaceRasterizer>,
    gpu: Option<Box<dyn GpuRasterizer>>,
    clock: Box<dyn Clock>,
    drawing: FaceDrawing,
    /// One per entry of `painter.layers()`, in the same order.
    slots: Vec<LayerSlot>,
}

impl FaceSystem {
    /// A system painting with `painter` and rasterising on the CPU with `cpu`, timed by the real clock.
    pub fn new(painter: Box<dyn FacePainter>, cpu: Box<dyn FaceRasterizer>) -> Self {
        Self::with(Painter::Other(painter), cpu)
    }

    /// A system around the real painter, which it keeps by value so that a registered GPU rasteriser gets each section's
    /// command range and repaints only the sections that changed.
    pub fn real(painter: RealPainter, cpu: Box<dyn FaceRasterizer>) -> Self {
        Self::with(Painter::Real(Box::new(painter)), cpu)
    }

    fn with(painter: Painter, cpu: Box<dyn FaceRasterizer>) -> Self {
        Self {
            painter,
            cpu,
            gpu: None,
            clock: Box::new(SystemClock::new()),
            drawing: FaceDrawing::new(),
            slots: Vec::new(),
        }
    }

    /// Times updates with `clock` (a `FakeClock` makes timings deterministic).
    #[must_use]
    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Registers a GPU rasteriser, or removes it with `None`. While one is registered, [`update`](Self::update) skips
    /// the CPU raster of changed layers and [`FaceLayerPainter`] draws them on the GPU, rasterising on the CPU only as
    /// the fallback.
    pub fn set_gpu_rasterizer(&mut self, gpu: Option<Box<dyn GpuRasterizer>>) {
        self.gpu = gpu;
    }

    /// Takes the registered GPU rasteriser out, leaving none: the stage moves it from a face system it replaces to the
    /// new one, so the rasteriser's pipelines are compiled once, not once per system.
    pub fn take_gpu_rasterizer(&mut self) -> Option<Box<dyn GpuRasterizer>> {
        self.gpu.take()
    }

    /// Tells the registered GPU rasteriser (if any) that the renderer's device was lost or rebuilt, so it drops what it
    /// made on the old one ([`GpuRasterizer::device_lost`]). The stage calls it when it rebuilds the renderer.
    pub fn gpu_device_lost(&mut self) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.device_lost();
        }
    }

    /// Tells the registered GPU rasteriser (if any) that the renderer refused the last paint of `layer`
    /// ([`GpuRasterizer::paint_failed`]), so the next paint redraws it whole. The stage calls it when
    /// `Renderer::paint_texture` fails.
    pub fn gpu_paint_failed(&mut self, layer: FaceLayerId) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.paint_failed(layer);
        }
    }

    /// The layers the painter fills and where they sit; the stage creates one texture per layer a material uses.
    pub fn layers(&self) -> &[FaceLayerSpec] {
        self.painter.layers()
    }

    /// The draw lists of the last update.
    pub fn drawing(&self) -> &FaceDrawing {
        &self.drawing
    }

    /// The CPU raster of `layer` as of the last update, or `None` if the layer isn't painted, hasn't been updated yet, or
    /// is only drawn on the GPU.
    pub fn raster(&self, layer: FaceLayerId) -> Option<&Raster> {
        let at = self.painter.layers().iter().position(|spec| spec.id == layer)?;
        self.slots.get(at).filter(|slot| !slot.stale).map(|slot| &slot.raster)
    }

    /// One frame: maps `channels` through `profile.mapping` once, paints, and rasterises the layers whose draw list
    /// changed since the last update (or whose [`FaceLayerSpec`] did: a density change). `profile` must have its
    /// palette slots resolved (`FaceProfile::resolve_colours`).
    ///
    /// # Errors
    /// What the painter or the CPU rasteriser returns. Nothing is then reported as changed, so the next successful
    /// update reports every layer the failed one touched.
    pub fn update(
        &mut self,
        channels: &ChannelBuf,
        profile: &FaceProfile,
        time: Seconds,
        view: FaceView,
    ) -> Result<FaceUpdate, FaceError> {
        let t0 = self.clock.now();
        let params = profile.mapping.apply(channels);
        let t1 = self.clock.now();
        self.painter.paint(&FaceInput { profile, params: &params, time, view }, &mut self.drawing)?;
        let t2 = self.clock.now();

        let fingerprints = self.drawing.fingerprints();
        let layers = self.painter.layers();
        if self.slots.len() != layers.len() {
            self.slots.resize_with(layers.len(), LayerSlot::new);
        }
        let mut update = FaceUpdate::default();
        for (slot, spec) in self.slots.iter_mut().zip(layers) {
            if slot.shown == Some((fingerprints[spec.id as usize], *spec)) {
                continue;
            }
            update.changed[spec.id as usize] = true;
            slot.stale = true;
            if self.gpu.is_none() {
                self.cpu.rasterize(spec, &self.drawing[spec.id], spec.px_per_m, &mut slot.raster)?;
                slot.stale = false;
            }
        }
        // Recorded only now that every layer succeeded: a failure above leaves all of them to be reported next time.
        for (slot, spec) in self.slots.iter_mut().zip(layers) {
            if update.changed[spec.id as usize] {
                slot.shown = Some((fingerprints[spec.id as usize], *spec));
            }
        }
        let t3 = self.clock.now();
        update.timings =
            FaceTimings { map: Seconds(t1.0 - t0.0), paint: Seconds(t2.0 - t1.0), raster: Seconds(t3.0 - t2.0) };
        Ok(update)
    }

    /// The renderer's `Painter` for `layer`, to hand to `Renderer::paint_texture` when
    /// [`FaceUpdate::is_changed`] says so. `None` if the painter doesn't fill `layer` or [`update`](Self::update)
    /// hasn't run yet.
    pub fn layer_painter(&mut self, layer: FaceLayerId) -> Option<FaceLayerPainter<'_>> {
        let at = self.painter.layers().iter().position(|spec| spec.id == layer)?;
        let spec = *self.painter.layers().get(at)?;
        let slot = self.slots.get_mut(at)?;
        let gpu = self.gpu.as_mut().map(|gpu| &mut **gpu as &mut dyn GpuRasterizer);
        let sections = self.painter.sections(layer);
        Some(FaceLayerPainter::new(spec, &self.drawing[layer], sections, slot, self.cpu.as_mut(), gpu))
    }
}
