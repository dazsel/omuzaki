//! The face's 3D features alone, drawn by the wgpu renderer through `omuzaki_face::feature::closeup` from the front,
//! three-quarter, the side and below, in the photoreal and the anime style. Each frame must be lit and detailed, and
//! the close-up must give back every resource it made. With `OMUZAKI_FEATURE_OUT=<dir>` it saves one 2 by 2 sheet per
//! feature and style for the pull request. It runs on any adapter (lavapipe in CI); without one it prints why and
//! passes.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_wrap)]
#![allow(clippy::disallowed_methods, reason = "a test writing its own evidence pictures; not an op")]

use std::path::PathBuf;

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face::feature::closeup::{Closeup, FeatureView};
use omuzaki_face::feature::nose::NoseDesign;
use omuzaki_face::feature::{Paint, skin_mesh};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Quality, Renderer, RendererExt};

const SIZE: UVec2 = UVec2::new(768, 768);

fn renderer() -> Option<WgpuRenderer> {
    match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("no adapter, skipping: {e}");
            None
        }
    }
}

fn render(renderer: &mut WgpuRenderer, mesh: &MeshData, bounds: (Vec3, Vec3), style: f32) -> image::RgbaImage {
    let counts = |r: &WgpuRenderer| {
        let s = r.stats();
        (s.meshes, s.textures, s.materials, s.environments, s.targets)
    };
    let before = counts(renderer);
    let mut sheet = image::RgbaImage::new(SIZE.x * 2, SIZE.y * 2);
    for (i, view) in FeatureView::ALL.into_iter().enumerate() {
        let closeup = Closeup::build(renderer, mesh, bounds, style, view).unwrap();
        let quality = Quality { render_scale: 2.0, ..Quality::high() };
        let image = renderer.render_image(SIZE, AlphaEncoding::Straight, closeup.scene(), &quality).unwrap();
        closeup.release(renderer).unwrap();
        let frame = image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8).unwrap();
        let (mean, spread) = luma_spread(&frame);
        assert!(mean > 25.0 && spread > 12.0, "{view:?}: a lit, detailed frame: mean {mean}, spread {spread}");
        #[allow(clippy::cast_possible_truncation)]
        image::imageops::replace(
            &mut sheet,
            &frame,
            i64::from(SIZE.x) * (i as i64 % 2),
            i64::from(SIZE.y) * (i as i64 / 2),
        );
    }
    assert_eq!(counts(renderer), before, "the close-up gives back every resource it made");
    sheet
}

fn luma_spread(frame: &image::RgbaImage) -> (f32, f32) {
    let luma = |p: &image::Rgba<u8>| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
    let lumas: Vec<f32> = frame.pixels().map(luma).collect();
    let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
    let spread = (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt();
    (mean, spread)
}

fn save(sheet: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_FEATURE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        sheet.save(dir.join(name)).unwrap();
    }
}

fn nose_mesh(design: &NoseDesign) -> MeshData {
    let field = design.field();
    let (min, max) = design.bounds();
    let tone = design.tone.to_linear();
    let colour = |p: Vec3| tone.lerp(tone * Vec3::new(1.05, 0.86, 0.84), 0.45 * design.redness(p));
    let wet = |_: Vec3| 0.0;
    let flush = |p: Vec3| 0.4 * design.redness(p);
    skin_mesh(&field, min, max, 0.00035, &Paint { colour: &colour, wet: &wet, flush: &flush })
}

#[test]
fn the_nose_renders_from_every_side_in_both_styles() {
    let Some(mut renderer) = renderer() else { return };
    for (design, name) in [(NoseDesign::default(), "nose-real.png"), (NoseDesign::anime(), "nose-anime.png")] {
        let mesh = nose_mesh(&design);
        assert!(mesh.indices.len() > 30_000, "{name}: {} triangles", mesh.indices.len() / 3);
        let sheet = render(&mut renderer, &mesh, design.bounds(), design.style);
        save(&sheet, name);
    }
}
