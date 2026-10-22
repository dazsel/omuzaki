//! The mock's particles (#809, particles.md D3 to D5): goldens of every shape, edge, orientation and an atlas cell over
//! grey and over transparent in both alpha encodings, determinism without history, density, and no allocation.
//!
//! The goldens are blessed with `OMUZAKI_BLESS=1 cargo test -p omuzaki-render-mock --test particles`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::path::{Path, PathBuf};

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, CellPick, Colour, ColourSet, Edge, EmitterShape, Image, Light, Orientation,
    ParticleParams, ParticleShading, ParticleSystemDesc, Pass, Quality, RenderError, Renderer, RendererExt, Scene,
    Span, SpawnBatch, Sprite, SpriteShape, TextureDesc,
};
use omuzaki_render_mock::MockRenderer;

#[global_allocator]
static ALLOCATOR: omuzaki_render_mock::counting::Counting =
    omuzaki_render_mock::counting::Counting::new(env!("CARGO_PKG_NAME"));

const CELL: UVec2 = UVec2::new(192, 192);
/// Cross-platform slack for the float functions the standard library doesn't pin down.
const TOLERANCE: u8 = 2;

/// A still particle: no speed, no gravity, a long life.
fn still(size: f32, orientation: Orientation, colour: Colour) -> ParticleParams {
    ParticleParams {
        lifetime: Span::fixed(20.0),
        speed: Span::fixed(0.0),
        direction: [0.0, 1.0, 0.0],
        spread_deg: 0.0,
        gravity: [0.0; 3],
        drag: 0.0,
        wind: [0.0; 3],
        flutter: None,
        size: Span::fixed(size),
        size_curve: [1.0; 4],
        orientation,
        colours: ColourSet::one(colour),
        coverage: 1.0,
        shading: ParticleShading::Unlit,
        edge: None,
        glow: 0.0,
        floor: None,
    }
}

fn camera() -> Camera {
    Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection)
}

/// A soft sky and a warm key from the upper left, for the toon-lit cases.
fn lights() -> Vec<Light> {
    vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.6 },
        Light::Directional {
            to_light: Vec3::new(-0.6, 1.0, 1.5),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.5,
            casts_shadow: false,
        },
    ]
}

fn quality() -> Quality {
    Quality { samples: 4, ..Quality::offline() }
}

/// One golden case: a system and the time it is drawn at.
struct Case {
    desc: ParticleSystemDesc,
    time: f64,
    cells: CellPick,
}

/// A 2 x 2 atlas, premultiplied: red, green, blue and a white ring, so the cell picked is plain to see.
fn atlas_pixels() -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..32u32 {
        for x in 0..32u32 {
            let (cx, cy) = (x / 16, y / 16);
            let (lx, ly) = ((x % 16) as f32 - 7.5, (y % 16) as f32 - 7.5);
            let d = (lx * lx + ly * ly).sqrt();
            let px = match (cx, cy) {
                (0, 0) => [255, 0, 0, 255],
                (1, 0) => [0, 255, 0, 255],
                (0, 1) => [0, 0, 255, 255],
                _ if (4.0..7.0).contains(&d) => [255, 255, 255, 255],
                _ => [0, 0, 0, 0],
            };
            rgba.extend(px);
        }
    }
    rgba
}

fn cases(renderer: &mut MockRenderer) -> Vec<Case> {
    let pink = Colour::hex(0x00ff_5a7a);
    let shapes = [
        SpriteShape::Square,
        SpriteShape::rect(0.6),
        SpriteShape::Disc,
        SpriteShape::Petal,
        SpriteShape::star(5),
        SpriteShape::Heart,
    ];
    let mut out = Vec::new();
    for edge in [None, Some(Edge { shade: 0.45, width_px: 3.0 })] {
        for shape in shapes {
            let params = ParticleParams { edge, ..still(1.1, Orientation::billboard(Span::fixed(0.0)), pink) };
            out.push(Case {
                desc: ParticleSystemDesc { sprite: Sprite::Shape(shape), params },
                time: 1.0,
                cells: CellPick::All,
            });
        }
        // The streak stands upright: it follows a velocity straight up.
        let mut params = ParticleParams {
            edge,
            speed: Span::fixed(0.2),
            ..still(0.25, Orientation::velocity(4.0), Colour::hex(0x00c8_d6ec))
        };
        params.direction = [0.0, 1.0, 0.0];
        out.push(Case {
            desc: ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Streak), params },
            time: 0.0,
            cells: CellPick::All,
        });
    }
    for step in 0..3 {
        let params = ParticleParams {
            shading: ParticleShading::toon(Colour::hex(0x00b8_a8c8), 0.0, 0.1),
            ..still(1.1, Orientation::tumble(Span::fixed(45.0)), Colour::hex(0x00ff_c94d))
        };
        let desc = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::rect(0.6)), params };
        out.push(Case { desc, time: 2.0 + f64::from(step), cells: CellPick::All });
    }
    let mut desc = TextureDesc::colour(UVec2::new(32, 32));
    desc.alpha = AlphaEncoding::Premultiplied;
    desc.mips = false;
    let texture = renderer.create_texture(&desc, Some(&atlas_pixels())).unwrap();
    let params = still(1.1, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE);
    out.push(Case {
        desc: ParticleSystemDesc { sprite: Sprite::atlas(texture, 2, 2), params },
        time: 1.0,
        cells: CellPick::list(&[3]).unwrap(),
    });
    out
}

/// Every case side by side, six to a row, over `background` into a target of `alpha`.
fn sheet(background: Background, alpha: AlphaEncoding) -> Image {
    let mut renderer = MockRenderer::new();
    let cases = cases(&mut renderer);
    let columns = 6u32;
    let rows = (cases.len() as u32).div_ceil(columns);
    let size = UVec2::new(CELL.x * columns, CELL.y * rows);
    let mut sheet = vec![0u8; (size.x * size.y * 4) as usize];
    for (i, case) in cases.iter().enumerate() {
        let system = renderer.create_particle_system(&case.desc).unwrap();
        let mut scene = Scene::new(camera());
        scene.background = background;
        scene.lights = lights();
        scene.frame.time = Seconds(case.time);
        scene.particles.push(SpawnBatch { cells: case.cells, seed: 3, ..SpawnBatch::new(system, Mat4::IDENTITY) });
        let image = renderer.render_image(CELL, alpha, &scene, &quality()).unwrap();
        let (cx, cy) = (i as u32 % columns, i as u32 / columns);
        for y in 0..CELL.y {
            let from = (y * CELL.x * 4) as usize;
            let to = (((cy * CELL.y + y) * size.x + cx * CELL.x) * 4) as usize;
            sheet[to..to + (CELL.x * 4) as usize].copy_from_slice(&image.rgba8[from..from + (CELL.x * 4) as usize]);
        }
        renderer.release(system.into()).unwrap();
    }
    Image { size, alpha, rgba8: sheet }
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
