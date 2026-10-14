//! The scene shader against the CPU-side expectations (frame tag codec, patch bytes, premultiplied invariants).
//! These need a GPU adapter; on a machine without one they print a note and pass, as the toon-viewport tests do.

#![allow(clippy::unwrap_used)] // tests may unwrap: a panic is a clear failure

use spout_out::frame_tag::{self, FrameTag};
use spout_out::gpu;
use spout_out::scene::{self, SceneParams, SceneRenderer};

const SIZE: (u32, u32) = (1280, 720);

/// A renderer on any backend that is available, or `None` (with a note) when there is no adapter.
fn renderer() -> Option<SceneRenderer> {
    match gpu::open(wgpu::Backends::all()) {
        Ok(gpu) => SceneRenderer::new(&gpu.device, &gpu.queue, SIZE).ok(),
        Err(error) => {
            eprintln!("skipping: no GPU adapter ({error})");
            None
        }
    }
}

fn render(scene: &SceneRenderer, frame: u32, truth: bool) -> Vec<u8> {
    scene.render(&SceneParams { tag: FrameTag { frame, ticks: 0xA5A5_0000_1234_5678 }, t: 0.7, truth });
    scene.read_back().unwrap()
}

#[test]
fn the_shader_draws_the_tag_the_cpu_decodes() {
    let Some(scene) = renderer() else { return };
    let pixels = render(&scene, 4242, false);
    let read = frame_tag::decode(&pixels, SIZE.0, SIZE.1).unwrap();
    assert_eq!(read.tag, FrameTag { frame: 4242, ticks: 0xA5A5_0000_1234_5678 });
    assert!(read.header_ok && read.footer_ok);
}

#[test]
fn the_patches_hold_alpha_scaled_bytes() {
    let Some(scene) = renderer() else { return };
    let pixels = render(&scene, 1, false);
    scene::check_patches(&pixels, SIZE.0, SIZE.1).unwrap();
}

#[test]
fn colour_never_exceeds_alpha_in_the_picture_area() {
    // The defining property of premultiplied alpha. A violation means some stage forgot to multiply (or multiplied twice).
    let Some(scene) = renderer() else { return };
    let pixels = render(&scene, 9, false);
    let rows = scene.picture_rows();
    let row_bytes = SIZE.0 as usize * 4;
    let picture = &pixels[rows.start as usize * row_bytes..rows.end as usize * row_bytes];
    let worst = picture
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| px[..3].iter().map(|&c| c.saturating_sub(px[3])).max().unwrap_or(0))
        .max();
    // One level of slack for rounding each channel independently.
    assert!(worst.unwrap_or(0) <= 1, "a pixel has colour above its alpha by {worst:?} levels");
}

#[test]
fn the_picture_has_soft_edges_and_clear_space() {
    let Some(scene) = renderer() else { return };
    let pixels = render(&scene, 25, false);
    let rows = scene.picture_rows();
    let row_bytes = SIZE.0 as usize * 4;
    let picture = &pixels[rows.start as usize * row_bytes..rows.end as usize * row_bytes];
    let alphas: Vec<u8> = picture.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
    assert!(alphas.contains(&0), "no fully transparent pixels");
    assert!(alphas.contains(&255), "no fully opaque pixels");
    let partial = alphas.iter().filter(|&&a| (32..224).contains(&a)).count();
    assert!(partial > 10_000, "only {partial} partially transparent pixels: the edges are not soft");
}

#[test]
fn the_truth_render_is_opaque_and_matches_the_background_where_the_picture_is_clear() {
    let Some(scene) = renderer() else { return };
    let truth = render(&scene, 25, true);
    let rows = scene.picture_rows();
    let row_bytes = SIZE.0 as usize * 4;
    let picture = &truth[rows.start as usize * row_bytes..rows.end as usize * row_bytes];
    assert!(picture.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
    // The top-right corner of the picture area is clear of every shape.
    let at = (SIZE.0 as usize - 8) * 4;
    let [b, g, r] = [picture[at], picture[at + 1], picture[at + 2]];
    let want = scene::TRUTH_BACKGROUND;
    assert!(
        (f32::from(r) - want[0]).abs() <= 1.0
            && (f32::from(g) - want[1]).abs() <= 1.0
            && (f32::from(b) - want[2]).abs() <= 1.0
    );
}
