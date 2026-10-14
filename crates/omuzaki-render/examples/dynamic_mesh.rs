//! The cost of `update_vertices`: `cargo run --release --example dynamic_mesh -- [--vertices 20000] [--frames 2000]
//! [--software]`. Leave `--software` off on a machine with a GPU to measure the real one.
//!
//! Updates a rippling sheet of the given size every frame and prints the CPU time of the `update_vertices` call alone
//! (mean, median, 95th percentile, worst). The outline directions are not part of it: they are recomputed when an
//! outlined instance next draws the mesh. The budget is 0.2 ms for 20 000 vertices.
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
    Renderer, RendererExt, Scene,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut vertices = 20_000u32;
    let mut frames = 2000usize;
    let mut software = false;
    let mut render_to: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--vertices" => vertices = args.next().ok_or("--vertices needs a number")?.parse()?,
            "--frames" => frames = args.next().ok_or("--frames needs a number")?.parse()?,
            "--software" => software = true,
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

    let mut times = Vec::with_capacity(frames);
    for i in 0..frames + 100 {
        grid::wave_into(&flat, i as f32 * 0.05, &mut positions, &mut normals);
        let start = Instant::now();
        renderer.update_vertices(id, &positions, &normals)?;
        let spent = start.elapsed();
        if i >= 100 {
            times.push(spent);
        }
        // The queue must not run away from the CPU: submit the queued writes and wait now and then, outside the timed
        // call (`wait_idle` submits first, which also frees the staging memory the writes took).
        if i % 50 == 49 {
            renderer.wait_idle()?;
        }
    }
    report("update_vertices", &mut times);

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
    let camera = Camera::look_at(Vec3::new(0.9, 0.7, 3.2), Vec3::ZERO, Vec3::Y, Camera::default().projection);
    let mut scene = Scene::new(camera);
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.6, 1.0).normalize(),
        colour: Colour::WHITE,
        intensity: 1.0,
        casts_shadow: false,
    });
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    Ok(r.render_image(UVec2::new(640, 400), AlphaEncoding::Straight, &scene, &Quality::high())?)
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
