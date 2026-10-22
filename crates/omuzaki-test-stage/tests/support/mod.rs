//! What the face-on-the-GPU tests and the face bench share (#2074): the wgpu renderer, a stage of the mock character
//! with its face framed, rigs that paint on the GPU or the CPU, a GPU rasteriser that counts its draws, and the
//! expressions that change the face every frame.

// Shared by several test targets and a bench, each of which uses part of it; test helpers may panic.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::RgbaImage;
use omuzaki_anim_api::Channel;
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, JointRef, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{DrawList, FaceError, FaceLayerId, FaceLayerSpec};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Image, Light, PixelRect, Projection, Quality, Renderer, Scene, TargetDesc,
    Tonemap,
};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};

/// The face texture density of the test stages: the mock head's Base layer is 256 pixels wide, the probe scale lavapipe
/// draws in a test (the live default, `DEFAULT_PX_PER_M`, makes it 1,440).
pub const PROBE_PX_PER_M: f32 = 640.0;

/// The headless wgpu renderer on the software adapter (lavapipe in CI; `WGPU_ADAPTER_NAME` picks another), or `None`
/// after printing why there is none.
pub fn renderer() -> Option<WgpuRenderer> {
    match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(renderer) => Some(renderer),
        Err(e) => {
            eprintln!("skipped: no adapter for the wgpu renderer ({e}); install mesa-vulkan-drivers for lavapipe");
            None
        }
    }
}

/// The renderer on the machine's best GPU (what the app opens), for the bench.
pub fn hardware_renderer() -> Result<WgpuRenderer, String> {
    WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Auto }).map_err(|e| e.to_string())
}

/// A GPU rasteriser that counts the paints it drew, so a test can tell the GPU path from the CPU fallback.
pub struct Counted {
    inner: VelloRasterizer,
    drawn: Arc<AtomicUsize>,
}

impl Counted {
    /// `VelloRasterizer::new` (which draws on software adapters too), counting into `drawn`.
    pub fn new(drawn: &Arc<AtomicUsize>) -> Self {
        Self::wrapping(VelloRasterizer::new(), drawn)
    }

    /// `inner`, counting into `drawn`.
    pub fn wrapping(inner: VelloRasterizer, drawn: &Arc<AtomicUsize>) -> Self {
        Self { inner, drawn: Arc::clone(drawn) }
    }

    fn count(&self, drawn: Result<bool, FaceError>) -> Result<bool, FaceError> {
        if matches!(drawn, Ok(true)) {
            self.drawn.fetch_add(1, Ordering::Relaxed);
        }
        drawn
    }
}

impl GpuRasterizer for Counted {
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let drawn = self.inner.rasterize_native(spec, list, native);
        self.count(drawn)
    }

    fn rasterize_native_sections(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let drawn = self.inner.rasterize_native_sections(spec, list, sections, native);
        self.count(drawn)
    }

    fn begin_frame(&mut self) {
        self.inner.begin_frame();
    }

    fn stage(&mut self, spec: &FaceLayerSpec, list: &DrawList, sections: &LayerSections) -> Result<(), FaceError> {
        self.inner.stage(spec, list, sections)
    }

    fn device_lost(&mut self) {
        self.inner.device_lost();
    }

    fn last_damage(&self) -> Option<PixelRect> {
        self.inner.last_damage()
    }

    fn paint_failed(&mut self, layer: FaceLayerId) {
        self.inner.paint_failed(layer);
    }

    fn repaint_needed(&mut self, layer: FaceLayerId) -> bool {
        self.inner.repaint_needed(layer)
    }
}

/// The production rig's face systems (the real painter, the CPU rasteriser as the fallback) with no GPU rasteriser:
/// every layer is the CPU raster written with `write_rgba`.
pub fn cpu_rig() -> FaceRig {
    FaceRig::new(Box::new(|projection, px_per_m| {
        FaceSystem::real(RealPainter::new(*projection, px_per_m), Box::new(SkiaRasterizer::new()))
    }))
    .with_executor(Box::new(InlineExecutor))
}

/// The production rig with `gpu` instead of its hardware-only rasteriser, so it draws on lavapipe too.
pub fn gpu_rig(gpu: Box<dyn GpuRasterizer>) -> FaceRig {
    FaceRig::standard().with_gpu_rasterizer(gpu).with_executor(Box::new(InlineExecutor))
}

/// A lit scene in front of a plain background.
fn scene() -> Scene {
    let mut scene = Scene::new(Camera::default());
    scene.background = Background::Solid { colour: Colour::hex(0x00f0_e8e0) };
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00ff_ffff), ground: Colour::hex(0x0080_7060), intensity: 1.2 }];
    scene.post.tonemap = Tonemap::None;
    scene
}

/// What the test stages render with: no multisampling or post effects, so the face is all that differs.
pub fn quality() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// A camera in front of the head, framing the face window.
fn face_camera(stage: &Stage) -> Camera {
    let built = stage.built();
    let face = built.face.as_ref().expect("the mock character has a face");
    let height = face.projection.rect.y1 - face.projection.rect.y0;
    let bone = built.rig.bone(&JointRef::HEAD).expect("a head bone").index();
    let bind = built.rig.inverse_bind()[bone].inverse().w_axis.truncate();
    let head = stage.skin().get(bone).map_or(bind, |skin| skin.transform_point3(bind));
    Camera::look_at(
        head + Vec3::new(0.0, 0.0, 3.0),
        head,
        Vec3::Y,
        Projection::Orthographic { height: height * 1.4, near: 0.1, far: 10.0 },
    )
}

/// A stage of the mock character on `renderer`, `size` pixels square, with `rig`, the face textures at `px_per_m` and
/// the face filling the frame.
pub fn face_stage(renderer: &mut dyn Renderer, rig: FaceRig, size: u32, px_per_m: f32) -> Stage {
    // The part kinds the mock character is built from register themselves in the crates the stage links.
    omuzaki_stage::link();
    let def = mock_character().expect("the mock character parses");
    let built = RegistryBuilder
        .build(&CharId::new("mock").expect("an id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds");
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(size), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        renderer,
        built,
        UploadOptions { face_px_per_m: px_per_m },
        scene(),
        target,
        quality(),
        StageClock::fixed(30.0),
    )
    .expect("the stage starts");
    stage.set_face(renderer, rig).expect("the face binds");
    stage.scene_mut().camera = face_camera(&stage);
    stage
}

/// Frame `n` of a face that changes every frame: talking (the jaw), blinking, glancing and blushing on different
/// periods, so the mouth always changes and the other sections often do.
pub fn expression(n: u32) -> FrameInput {
    let wave = |period: u32| {
        let phase = f32::from(u16::try_from(n % period).unwrap_or(0)) / f32::from(u16::try_from(period).unwrap_or(1));
        0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
    };
    let mut input = FrameInput::default();
    input.channels.set(Channel::JAW_OPEN, 0.6 * wave(6));
    input.channels.set(Channel::EYE_BLINK_LEFT, wave(37));
    input.channels.set(Channel::EYE_BLINK_RIGHT, wave(37));
    input.channels.set(Channel::EYE_LOOK_OUT_LEFT, 0.7 * wave(23));
    input.channels.set(Channel::MOUTH_SMILE_LEFT, 0.8 * wave(17));
    input.channels.set(Channel::BLUSH, wave(29));
    input
}

/// A rendered frame as an image the region comparison takes.
pub fn rgba(image: &Image) -> RgbaImage {
    RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).expect("an RGBA8 image")
}
