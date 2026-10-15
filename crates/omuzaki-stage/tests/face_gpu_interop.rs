//! GPU interop (#151, CB-24): the face painter draws into `omuzaki-render`'s dynamic texture through `PaintTarget::native`
//! (`omuzaki_render::NativePaintTarget`, `(Device, Queue, Texture)`), and a quad that overlays the texture looks like the
//! one that overlays the texture the CPU raster filled through `write_rgba`: same pixels within tolerance (per channel 8, at
//! most 1 % of pixels over it, PSNR 34 dB or more, A3's rule).
//!
//! It fails loudly if the convention drifts: the GPU rasteriser is wrapped to count the paints it drew, and the test asserts
//! it drew the layer instead of declining to the CPU (which would make the two quads identical for the wrong reason). The
//! stage is where the face and the renderer may meet (`cargo xtask lint-deps`). With no adapter the test prints why and
//! passes. Run it with `cargo test -p omuzaki-stage --features wgpu --test face_gpu_interop`.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char_api::face_texture_desc;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, DrawList, FaceError, FaceLayerId, FaceLayerSpec, FaceView};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Mapping, Material, Overlay, Projection, Quality,
    Renderer, RendererExt, Scene, TextureId, Tonemap,
};
use omuzaki_render_mock::shapes;

const MAX_DELTA: u8 = 8;
const MAX_BAD_FRACTION: f64 = 0.01;
const MIN_PSNR_DB: f64 = 34.0;
/// The layer is about this many pixels wide: probe size.
const PROBE_WIDTH_PX: f32 = 256.0;

/// `VelloRasterizer`, counting the paints it drew and the ones it declined.
struct Counting {
    inner: VelloRasterizer,
    drawn: Arc<AtomicUsize>,
    declined: Arc<AtomicUsize>,
}

impl GpuRasterizer for Counting {
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let drawn = self.inner.rasterize_native(spec, list, native)?;
        let counter = if drawn { &self.drawn } else { &self.declined };
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(drawn)
    }
}

/// A face system for the synthetic face at probe density, updated once, and the Base layer's spec.
fn system(gpu: Option<Counting>) -> (FaceSystem, FaceLayerSpec) {
    let ppm = PROBE_WIDTH_PX / mock_projection().rect.width();
    let mut system =
        FaceSystem::new(Box::new(RealPainter::new(mock_projection(), ppm)), Box::new(SkiaRasterizer::new()));
    if let Some(gpu) = gpu {
        system.set_gpu_rasterizer(Some(Box::new(gpu)));
    }
    system.update(&ChannelBuf::NEUTRAL, &mock_profile(), Seconds(0.0), FaceView::default()).unwrap();
    let spec = system.layers().iter().find(|l| l.id == FaceLayerId::Base).copied().unwrap();
    let _ = DEFAULT_PX_PER_M;
    (system, spec)
}

fn page() -> Scene {
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::WHITE };
    scene.post.tonemap = Tonemap::None;
    scene
}

/// An unlit white quad with `texture` as its overlay, centred at `x`.
fn quad_with(renderer: &mut dyn Renderer, texture: TextureId, x: f32) -> Instance {
    let mut material = Material::unlit(Colour::WHITE);
    material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    let material = renderer.create_material(&material).unwrap();
    let mesh = renderer.create_mesh(&shapes::quad(Vec2::splat(0.9))).unwrap();
    Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, 0.0, 0.0)))
}

/// The two halves of `image`, as `(left, right)` straight RGBA8 of equal size.
fn halves(image: &Image) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (image.size.x as usize, image.size.y as usize);
    let half = w / 2 * 4;
    let rows = |from: usize| -> Vec<u8> {
        (0..h).flat_map(|y| image.rgba8[y * w * 4 + from..y * w * 4 + from + half].to_vec()).collect()
    };
    (rows(0), rows(half))
}

/// Pixels whose largest channel difference exceeds `MAX_DELTA`, as a fraction, and the PSNR over all channels.
fn compare(a: &[u8], b: &[u8]) -> (f64, Option<f64>) {
    let pixels = a.len() / 4;
    let bad = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > MAX_DELTA))
        .count();
    let squares: f64 = a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y)).powi(2)).sum();
    let mse = squares / a.len() as f64;
    (bad as f64 / pixels as f64, (mse > 0.0).then(|| 10.0 * (255.0_f64 * 255.0 / mse).log10()))
}

#[test]
fn the_face_drawn_through_native_matches_the_write_rgba_path() {
    let mut renderer = match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(renderer) => renderer,
        Err(e) => {
            eprintln!("skipped: no software adapter ({e}); install mesa-vulkan-drivers for lavapipe");
            return;
        }
    };
    let (drawn, declined) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let counting = Counting { inner: VelloRasterizer::new(), drawn: drawn.clone(), declined: declined.clone() };
    let ((mut on_gpu, spec), (mut on_cpu, _)) = (system(Some(counting)), system(None));
    let desc = face_texture_desc(&spec);

    let gpu_texture = renderer.create_texture(&desc, None).unwrap();
    let cpu_texture = renderer.create_texture(&desc, None).unwrap();
    renderer.paint_texture(gpu_texture, &mut on_gpu.layer_painter(FaceLayerId::Base).unwrap()).unwrap();
    renderer.paint_texture(cpu_texture, &mut on_cpu.layer_painter(FaceLayerId::Base).unwrap()).unwrap();
    assert_eq!(
        (drawn.load(Ordering::Relaxed), declined.load(Ordering::Relaxed)),
        (1, 0),
        "the GPU rasteriser must draw through `native()`, not decline to the CPU: the native convention drifted"
    );

    let mut scene = page();
    let (left, right) = (quad_with(&mut renderer, gpu_texture, -1.0), quad_with(&mut renderer, cpu_texture, 1.0));
    scene.instances.extend([left, right]);
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() };
    let image = renderer.render_image(UVec2::new(1024, 512), AlphaEncoding::Straight, &scene, &quality).unwrap();
    let (gpu_half, cpu_half) = halves(&image);
    assert!(cpu_half.chunks_exact(4).any(|p| p[..3] != [255, 255, 255]), "the CPU quad shows nothing over the page");

    let (bad, psnr) = compare(&gpu_half, &cpu_half);
    eprintln!(
        "face through native vs write_rgba: bad fraction {:.3} %, PSNR {psnr:?} dB (lavapipe, probe size)",
        bad * 100.0
    );
    assert!(bad <= MAX_BAD_FRACTION, "{:.3} % of pixels differ by more than {MAX_DELTA}", bad * 100.0);
    assert!(psnr.is_none_or(|db| db >= MIN_PSNR_DB), "PSNR {psnr:?} dB is below {MIN_PSNR_DB}");
}
