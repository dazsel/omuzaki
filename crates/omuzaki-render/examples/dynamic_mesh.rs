//! The per-frame cost of a dynamic mesh: `cargo run --release --example dynamic_mesh -- [--vertices 20000]
//! [--frames 2000] [--target 64] [--software]`. Leave `--software` off on a machine with a GPU to measure the real one.
//!
//! Updates a rippling sheet of the given size every frame and prints the CPU time (mean, median, 95th percentile,
//! worst) of three things:
//! - `update_vertices` alone;
//! - the whole dynamic frame: the update plus the `render` call that draws the outlined instance next, which does the
//!   work the update defers (welding, smoothing, the outline upload and the bounds refit);
//! - the same `render` call on unchanged geometry, which defers nothing.
//!
//! The dynamic frame minus the unchanged render is the incremental cost of moving the mesh; the budget is 0.2 ms of it
//! for 20 000 vertices. The two renders are timed on alternate frames so that drift and load hit both alike.
//!
//! `--render-to DIR` also renders the sheet flat and rippling, for the pull request: `before.png` and `after.png`.

// A benchmark's statistics use small counts in float maths; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "../tests/support/grid.rs"]
mod grid;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Image, Instance, Light, Material, MeshOptions, MeshUsage, Outline, Quality,
    Renderer, RendererExt, Scene, TargetDesc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut vertices = 20_000u32;
    let mut frames = 2000usize;
    let mut software = false;
    let mut target_side = 64u32;
    let mut render_to: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--vertices" => vertices = args.next().ok_or("--vertices needs a number")?.parse()?,
            "--frames" => frames = args.next().ok_or("--frames needs a number")?.parse()?,
            "--software" => software = true,
            "--target" => target_side = args.next().ok_or("--target needs a side in pixels")?.parse()?,
            "--render-to" => render_to = Some(args.next().ok_or("--render-to needs a directory")?.into()),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let adapter = if software { AdapterPreference::Software } else { AdapterPreference::Auto };
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter })?;
    println!("backend: {}", renderer.capabilities().backend);

    // A sheet of about `vertices` vertices, twice as wide as tall.
    let rows = (f64::from(vertices) / 2.0).sqrt().round().max(2.0) as u32;
    let cols = (vertices / rows).max(2);
    let mesh = grid::grid(cols, rows);
    let flat = mesh.positions.clone();
    let id = renderer.create_mesh_with(&mesh, &MeshOptions { usage: MeshUsage::Dynamic, morphs: None })?;
    println!("{cols} x {rows} = {} vertices", mesh.positions.len());
    let (mut positions, mut normals) = (mesh.positions.clone(), mesh.normals.clone());
    // The picture before any update: the sheet as it was created, flat.
    let material = renderer.create_material(&outlined_material())?;
    let before = render_to.as_ref().map(|_| draw(&mut renderer, id, material)).transpose()?;

    // A small target: the GPU's share is not what is measured, the CPU's is (encoding, uploads, the deferred refresh).
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::splat(target_side), alpha: AlphaEncoding::Straight })?;
    let mut scene = frame_scene(id, material);
    let quality = Quality::live();
    let warmup = 100usize;
    let (mut update_only, mut dynamic_frame, mut unchanged_frame) =
        (Vec::with_capacity(frames), Vec::with_capacity(frames), Vec::with_capacity(frames));
    for i in 0..frames + warmup {
        // 1. `update_vertices` alone. Its deferred work stays pending for the next draw.
        grid::wave_into(&flat, i as f32 * 0.05, &mut positions, &mut normals);
        let start = Instant::now();
        renderer.update_vertices(id, &positions, &normals)?;
        let update = start.elapsed();
        // 2. The draw that follows: the refresh runs inside `render`. Update plus draw is the whole dynamic frame.
        scene.frame.index = i as u64 * 2;
        let start = Instant::now();
        renderer.render(target, &scene, &quality)?;
        let dynamic = update + start.elapsed();
        // Each timed call starts on an idle queue, so none of them is charged for the GPU work of an earlier one.
        renderer.wait_idle()?;
        // 3. The same draw with nothing changed since the last one.
        scene.frame.index = i as u64 * 2 + 1;
        let start = Instant::now();
        renderer.render(target, &scene, &quality)?;
        let unchanged = start.elapsed();
        renderer.wait_idle()?;
        if i >= warmup {
            update_only.push(update);
            dynamic_frame.push(dynamic);
            unchanged_frame.push(unchanged);
        }
    }
    // The incremental cost, frame by frame: the dynamic frame less its neighbour on unchanged geometry (floored at
    // zero: a neighbour can be the slower one by noise).
    let mut incremental: Vec<Duration> =
        dynamic_frame.iter().zip(&unchanged_frame).map(|(d, u)| d.saturating_sub(*u)).collect();
    report("update_vertices alone", &mut update_only);
    report("render, unchanged geometry", &mut unchanged_frame);
    report("update + outlined render", &mut dynamic_frame);
    report("incremental (frame - unchanged)", &mut incremental);

    if let Some(dir) = render_to {
        std::fs::create_dir_all(&dir)?;
        grid::wave_into(&flat, 1.3, &mut positions, &mut normals);
        renderer.update_vertices(id, &positions, &normals)?;
        let after = draw(&mut renderer, id, material)?;
        for (name, image) in [("before.png", before.ok_or("no before picture")?), ("after.png", after)] {
            image::save_buffer(dir.join(name), &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8)?;
        }
        println!("renders in {}", dir.display());
    }
    Ok(())
}

fn outlined_material() -> Material {
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.double_sided = true;
    material.outline = Some(Outline { colour: Colour::BLACK, width: 0.015 });
    material
}

fn draw(
    r: &mut WgpuRenderer,
    mesh: omuzaki_render_api::MeshId,
    material: omuzaki_render_api::MaterialId,
) -> Result<Image, Box<dyn std::error::Error>> {
    Ok(r.render_image(UVec2::new(640, 400), AlphaEncoding::Straight, &frame_scene(mesh, material), &Quality::high())?)
}

/// One outlined instance of `mesh` under one light.
fn frame_scene(mesh: omuzaki_render_api::MeshId, material: omuzaki_render_api::MaterialId) -> Scene {
    let camera = Camera::look_at(Vec3::new(0.9, 0.7, 3.2), Vec3::ZERO, Vec3::Y, Camera::default().projection);
    let mut scene = Scene::new(camera);
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.6, 1.0).normalize(),
        colour: Colour::WHITE,
        intensity: 1.0,
        casts_shadow: false,
    });
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    scene
}

fn report(label: &str, times: &mut [Duration]) {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let mean = times.iter().map(|d| ms(*d)).sum::<f64>() / times.len() as f64;
    let at = |q: f64| ms(times[((times.len() - 1) as f64 * q) as usize]);
    println!(
        "{label}: mean {mean:.3} ms, median {:.3} ms, p95 {:.3} ms, worst {:.3} ms ({} samples)",
        at(0.5),
        at(0.95),
        ms(times[times.len() - 1]),
        times.len()
    );
}
