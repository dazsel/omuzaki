//! The 3D eye close up, drawn by the wgpu renderer: the eyeball with its refractive cornea (`eye` program), the lids,
//! the skin round the eye and the lashes (`skin` program) and the occlusion shell, built from one design by
//! `omuzaki_face::eye3d::closeup` (as `render.eye` builds it), lit by a key light with shadows, a soft fill and a studio
//! environment with a softbox (the catchlight).
//!
//! The test renders the macro and the front views, checks each frame is lit and detailed and that the close-up gives
//! back every resource it made, and, with `OMUZAKI_EYE_OUT=<dir>`, saves `eye-macro.png` and `eye-front.png` for the
//! pull request. It runs on any adapter (lavapipe in CI); without one it prints why and passes.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::many_single_char_names)]
#![allow(clippy::disallowed_methods, reason = "a test writing its own evidence pictures; not an op")]

use std::path::PathBuf;

use omuzaki_core::glam::UVec2;
use omuzaki_face::eye3d::closeup::{Closeup, View};
use omuzaki_face::eye3d::{self, EyeDesign};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Quality, Renderer, RendererExt};

/// The frame's size (the reference close-up's proportions).
const SIZE: UVec2 = UVec2::new(1024, 744);

fn renderer() -> Option<WgpuRenderer> {
    match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("no adapter, skipping: {e}");
            None
        }
    }
}

/// Renders the close-up of `design` from `view` and returns the frame, through the same [`Closeup`] `render.eye` uses.
fn render(renderer: &mut WgpuRenderer, design: &EyeDesign, view: View) -> image::RgbaImage {
    if let Some(dir) = std::env::var_os("OMUZAKI_EYE_OUT").map(PathBuf::from) {
        let atlas = eye3d::textures::atlas(design, 2048);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_raw(atlas.width, atlas.height, atlas.rgba)
            .unwrap()
            .save(dir.join("eye-atlas.png"))
            .unwrap();
    }
    let counts = |r: &WgpuRenderer| {
        let s = r.stats();
        (s.meshes, s.textures, s.materials, s.environments, s.targets)
    };
    let before = counts(renderer);
    let closeup = Closeup::build(renderer, design, view, 2048).unwrap();
    let quality = Quality { render_scale: 2.0, ..Quality::high() };
    let image = renderer.render_image(SIZE, AlphaEncoding::Straight, closeup.scene(), &quality).unwrap();
    closeup.release(renderer).unwrap();
    assert_eq!(counts(renderer), before, "the close-up gives back every resource it made");
    image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8).unwrap()
}

/// The frame's mean luma and its spread.
fn luma_spread(frame: &image::RgbaImage) -> (f32, f32) {
    let luma = |p: &image::Rgba<u8>| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
    let lumas: Vec<f32> = frame.pixels().map(luma).collect();
    let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
    let spread = (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt();
    (mean, spread)
}

#[test]
fn the_eye_renders_at_macro_framing_as_an_eye() {
    let Some(mut renderer) = renderer() else { return };
    // Looking up lifts the upper lid, as in the reference close-up.
    let mut design = EyeDesign::default();
    design.aperture.upper = 0.006;
    for (view, name) in [(View::Macro, "eye-macro.png"), (View::Front, "eye-front.png")] {
        let frame = render(&mut renderer, &design, view);
        if let Some(dir) = std::env::var_os("OMUZAKI_EYE_OUT").map(PathBuf::from) {
            std::fs::create_dir_all(&dir).unwrap();
            frame.save(dir.join(name)).unwrap();
        }
        // Something drew over most of the frame, and it isn't flat.
        let (mean, spread) = luma_spread(&frame);
        assert!(mean > 30.0 && spread > 15.0, "{name}: a lit, detailed frame: mean {mean}, spread {spread}");
    }
}

#[test]
fn the_anime_eye_renders_as_a_drawn_eye() {
    let Some(mut renderer) = renderer() else { return };
    let design = EyeDesign::anime();
    for (view, name) in [(View::Front, "eye-anime-front.png"), (View::Macro, "eye-anime-macro.png")] {
        let frame = render(&mut renderer, &design, view);
        if let Some(dir) = std::env::var_os("OMUZAKI_EYE_OUT").map(PathBuf::from) {
            std::fs::create_dir_all(&dir).unwrap();
            frame.save(dir.join(name)).unwrap();
        }
        let (mean, spread) = luma_spread(&frame);
        assert!(mean > 30.0 && spread > 15.0, "{name}: a lit, detailed frame: mean {mean}, spread {spread}");
    }
}
