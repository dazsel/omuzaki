//! The mock's particles (#809, particles.md D3 to D5): goldens of every shape, edge, orientation and an atlas cell over
//! grey and over transparent in both alpha encodings, determinism without history, density, and no allocation.
//!
//! The goldens are blessed with `OMUZAKI_BLESS=1 cargo test -p omuzaki-render-mock --test particles`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::path::{Path, PathBuf};

use omuzaki_core::glam::{Mat4, UVec2};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Background, CellPick, Colour, ColourSet, EmitterShape, Image, Orientation, ParticleParams,
    ParticleSystemDesc, Pass, Quality, RenderError, Renderer, RendererExt, Scene, Span, SpawnBatch, Sprite,
    SpriteShape, TextureDesc,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_render_mock::particle_sheet::{self, camera, quality, still};

#[global_allocator]
static ALLOCATOR: omuzaki_render_mock::counting::Counting =
    omuzaki_render_mock::counting::Counting::new(env!("CARGO_PKG_NAME"));

/// Cross-platform slack for the float functions the standard library doesn't pin down.
const TOLERANCE: u8 = 2;

fn sheet(background: Background, alpha: AlphaEncoding) -> Image {
    particle_sheet::sheet(&mut MockRenderer::new(), background, alpha).unwrap()
}

fn save_png(image: &Image, path: &Path) {
    image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
}

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/particles").join(name)
}

fn matches_golden(image: &Image, name: &str) {
    let path = golden(name);
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_png(image, &path);
    }
    let want = image::open(&path).unwrap().to_rgba8();
    assert_eq!((want.width(), want.height()), (image.size.x, image.size.y));
    let worst = want.as_raw().iter().zip(&image.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    if worst > TOLERANCE {
        let actual = std::env::temp_dir().join(format!("omuzaki-render-mock-{name}"));
        save_png(image, &actual);
        panic!(
            "{name} differs from its golden by up to {worst} levels; see {}, or bless with OMUZAKI_BLESS=1",
            actual.display()
        );
    }
}

#[test]
fn every_shape_edge_orientation_and_atlas_cell_matches_its_golden() {
    let grey = Background::Solid { colour: Colour::hex(0x0080_8080) };
    matches_golden(&sheet(grey, AlphaEncoding::Straight), "grey.png");
    matches_golden(&sheet(Background::Transparent, AlphaEncoding::Straight), "transparent-straight.png");
    matches_golden(&sheet(Background::Transparent, AlphaEncoding::Premultiplied), "transparent-premultiplied.png");
}

/// A 200-particle confetti burst from the origin at time `t`.
fn burst(renderer: &mut MockRenderer, density: f32, times: &[f64]) -> Vec<Image> {
    let params = ParticleParams {
        speed: Span::new(0.8, 1.4),
        spread_deg: 180.0,
        gravity: [0.0, -1.0, 0.0],
        drag: 1.0,
        lifetime: Span::new(3.0, 4.0),
        colours: ColourSet::new(&[Colour::hex(0x00ff_5a7a), Colour::hex(0x005a_d1ff), Colour::WHITE]).unwrap(),
        ..still(0.04, Orientation::tumble(Span::new(200.0, 600.0)), Colour::WHITE)
    };
    let desc = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::rect(0.6)), params };
    let system = renderer.create_particle_system(&desc).unwrap();
    let images = times
        .iter()
        .map(|&t| {
            let mut scene = Scene::new(camera());
            scene.background = Background::Transparent;
            scene.frame.time = Seconds(t);
            scene.particles.push(SpawnBatch {
                count: 200,
                seed: 11,
                shape: EmitterShape::sphere(0.05),
                ..SpawnBatch::new(system, Mat4::IDENTITY)
            });
            let q = Quality { particle_density: density, ..quality() };
            renderer.render_image(UVec2::new(96, 96), AlphaEncoding::Premultiplied, &scene, &q).unwrap()
        })
        .collect();
    renderer.release(system.into()).unwrap();
    images
}

#[test]
fn a_burst_renders_the_same_twice_and_any_frame_stands_alone() {
    let mut renderer = MockRenderer::new();
    let twice = burst(&mut renderer, 1.0, &[1.0, 1.0]);
    assert_eq!(twice[0].rgba8, twice[1].rgba8);
    let direct = burst(&mut MockRenderer::new(), 1.0, &[2.0]);
    let after = burst(&mut MockRenderer::new(), 1.0, &[0.0, 1.0, 2.0]);
    assert_eq!(direct[0].rgba8, after[2].rgba8, "t = 2 doesn't depend on the frames before it");
}

fn coverage(image: &Image) -> u64 {
    image.rgba8.as_chunks::<4>().0.iter().map(|p| u64::from(p[3])).sum()
}

#[test]
fn density_thins_the_same_particles() {
    let mut renderer = MockRenderer::new();
    let full = coverage(&burst(&mut renderer, 1.0, &[0.5])[0]);
    let half = coverage(&burst(&mut renderer, 0.5, &[0.5])[0]);
    let none = coverage(&burst(&mut renderer, 0.0, &[0.5])[0]);
    assert!(full > 0);
    assert_eq!(none, 0, "density 0 draws nothing");
    let share = half as f64 / full as f64;
    assert!((0.35..=0.65).contains(&share), "density 0.5 covers {share:.2} of density 1");
}

#[test]
fn the_report_counts_slots_and_the_pass_runs_only_with_batches() {
    let mut renderer = MockRenderer::new();
    assert!(renderer.capabilities().supports(Pass::Particles));
    let desc = ParticleSystemDesc {
        sprite: Sprite::Shape(SpriteShape::Disc),
        params: still(0.1, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE),
    };
    let system = renderer.create_particle_system(&desc).unwrap();
    assert_eq!(renderer.stats().particle_systems, 1);
    let target = renderer
        .create_target(&omuzaki_render_api::TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .unwrap();
    let mut scene = Scene::new(camera());
    let report = renderer.render(target, &scene, &quality()).unwrap();
    assert_eq!((report.particles, report.cpu.get(Pass::Particles)), (0, None));
    scene.particles.push(SpawnBatch { count: 30, ..SpawnBatch::new(system, Mat4::IDENTITY) });
    scene.particles.push(SpawnBatch { count: 12, ..SpawnBatch::new(system, Mat4::IDENTITY) });
    let report = renderer.render(target, &scene, &quality()).unwrap();
    assert_eq!(report.particles, 42);
    assert!(report.cpu.get(Pass::Particles).is_some());
    renderer.release(system.into()).unwrap();
    assert!(matches!(
        renderer.render(target, &scene, &quality()),
        Err(RenderError::UnknownHandle { what: "particle system" })
    ));
    assert_eq!(renderer.stats().particle_systems, 0);
}

#[test]
fn an_atlas_texture_in_use_and_a_cell_outside_it_are_refused() {
    let mut renderer = MockRenderer::new();
    let texture = renderer.create_texture(&TextureDesc::colour(UVec2::new(4, 4)), None).unwrap();
    let desc = ParticleSystemDesc {
        sprite: Sprite::atlas(texture, 2, 2),
        params: still(0.1, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE),
    };
    let system = renderer.create_particle_system(&desc).unwrap();
    assert!(matches!(renderer.release(texture.into()), Err(RenderError::InUse { by: "particle system", .. })));
    let mut scene = Scene::new(camera());
    scene
        .particles
        .push(SpawnBatch { cells: CellPick::list(&[4]).unwrap(), ..SpawnBatch::new(system, Mat4::IDENTITY) });
    let err = renderer.render_image(UVec2::new(8, 8), AlphaEncoding::Straight, &scene, &quality()).unwrap_err();
    assert!(err.to_string().contains("outside its system's atlas"), "{err}");
    renderer.release(system.into()).unwrap();
    renderer.release(texture.into()).unwrap();
    let dead = ParticleSystemDesc { sprite: Sprite::atlas(texture, 2, 2), ..desc };
    assert!(matches!(renderer.create_particle_system(&dead), Err(RenderError::UnknownHandle { what: "texture" })));
}

#[test]
fn five_thousand_particles_add_no_allocation_to_a_frame() {
    let mut renderer = MockRenderer::new();
    let params = ParticleParams {
        speed: Span::new(0.5, 1.0),
        spread_deg: 180.0,
        lifetime: Span::fixed(10.0),
        ..still(0.02, Orientation::billboard(Span::new(-90.0, 90.0)), Colour::WHITE)
    };
    let desc = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params };
    let system = renderer.create_particle_system(&desc).unwrap();
    let target = renderer
        .create_target(&omuzaki_render_api::TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight })
        .unwrap();
    let mut empty = Scene::new(camera());
    empty.frame.time = Seconds(1.0);
    let mut full = empty.clone();
    full.particles.extend((0..50).map(|i| SpawnBatch {
        count: 100,
        seed: i,
        ..SpawnBatch::new(system, Mat4::IDENTITY)
    }));
    let q = Quality::live();
    // Warm up both, then count one frame of each: the particles must add nothing.
    renderer.render(target, &empty, &q).unwrap();
    renderer.render(target, &full, &q).unwrap();
    let count = |scene: &Scene, renderer: &mut MockRenderer| -> usize {
        omuzaki_render_mock::counting::measure(|| {
            renderer.render(target, scene, &q).unwrap();
        })
        .values()
        .sum()
    };
    let (without, with) = (count(&empty, &mut renderer), count(&full, &mut renderer));
    assert_eq!(with, without, "5,000 particles added {} allocations to a frame", with.abs_diff(without));
}
