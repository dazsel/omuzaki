//! The `Particles` pass on the GPU (#940, particles.md D4 to D7): the contact sheet as goldens and measured against the
//! mock case by case, D5's compositing cases (coverage over transparent and grey, the far plane at `w < 1` and `w > 1`,
//! beyond it, a lens in front of and behind), determinism, and the report.
//!
//! `OMUZAKI_BLESS=1` rewrites the goldens; `OMUZAKI_RENDER_OUT=<dir>` also writes the GPU and the mock sheets.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod support;

use std::path::Path;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, ColourSet, Image, Instance, Material, Orientation,
    ParticleParams, ParticleSystemDesc, ParticleSystemId, Pass, Projection, Quality, Renderer, RendererExt, Scene,
    Span, SpawnBatch, Sprite, SpriteShape, TargetDesc,
};
use omuzaki_render_mock::particle_sheet::{self, CELL, COLUMNS, still};
use omuzaki_render_mock::{MockRenderer, shapes};

fn save(image: &Image, name: &str) {
    support::keep(name, (image.size.x, image.size.y), &image.rgba8);
}

fn matches_golden(image: &Image, name: &str) {
    save(image, &format!("gpu-{name}"));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}"));
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        image::save_buffer(&path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    }
    let golden = image::open(&path)
        .unwrap_or_else(|e| panic!("{} (bless with OMUZAKI_BLESS=1): {e}", path.display()))
        .to_rgba8();
    assert_eq!(golden.dimensions(), (image.size.x, image.size.y));
    let diff = support::Diff::of(golden.as_raw(), &image.rgba8);
    assert!(
        diff.max <= 2,
        "{name} differs from its golden by up to {} levels ({diff}; bless with OMUZAKI_BLESS=1)",
        diff.max
    );
}

/// The covered area (the sum of alpha, in pixels) and its centroid, of case `i` of a transparent sheet.
fn measure(sheet: &Image, i: u32) -> (f64, Vec2) {
    let (cx, cy) = (i % COLUMNS * CELL.x, i / COLUMNS * CELL.y);
    let (mut area, mut moment) = (0.0, Vec2::ZERO);
    for y in 0..CELL.y {
        for x in 0..CELL.x {
            let a = f64::from(sheet.colour(cx + x, cy + y).unwrap().a) / 255.0;
            area += a;
            moment += Vec2::new(x as f32 + 0.5, y as f32 + 0.5) * a as f32;
        }
    }
    (area, moment / area.max(1e-9) as f32)
}

#[test]
fn the_contact_sheet_matches_its_goldens_and_the_mock_case_by_case() {
    let mut gpu = support::renderer();
    let grey = Background::Solid { colour: Colour::hex(0x0080_8080) };
    let sheet = |r: &mut dyn Renderer, background, alpha| particle_sheet::sheet(r, background, alpha).unwrap();
    let on_grey = sheet(&mut gpu, grey, AlphaEncoding::Straight);
    let straight = sheet(&mut gpu, Background::Transparent, AlphaEncoding::Straight);
    let premultiplied = sheet(&mut gpu, Background::Transparent, AlphaEncoding::Premultiplied);
    matches_golden(&on_grey, "particles-grey.png");
    matches_golden(&straight, "particles-transparent-straight.png");
    matches_golden(&premultiplied, "particles-transparent-premultiplied.png");

    let mock = sheet(&mut MockRenderer::new(), Background::Transparent, AlphaEncoding::Straight);
    save(&mock, "mock-particles-transparent-straight.png");
    let cases = (straight.size.x / CELL.x) * (straight.size.y / CELL.y);
    for i in 0..cases {
        let ((gpu_area, gpu_centre), (mock_area, mock_centre)) = (measure(&straight, i), measure(&mock, i));
        if mock_area == 0.0 {
            assert!(gpu_area < 1.0, "case {i}: the mock draws nothing, the GPU {gpu_area:.1} pixels");
            continue;
        }
        let share = gpu_area / mock_area;
        eprintln!(
            "case {i}: area {gpu_area:.0} against {mock_area:.0} ({share:.3}), centroid {gpu_centre} against {mock_centre}"
        );
        assert!((0.97..=1.03).contains(&share), "case {i}: the GPU covers {share:.3} of the mock's area");
        assert!(gpu_centre.distance(mock_centre) < 1.0, "case {i}: centroid {gpu_centre} against {mock_centre}");
    }
}

fn square(r: &mut dyn Renderer, colour: Colour, coverage: f32, size: f32) -> ParticleSystemId {
    let params = ParticleParams { coverage, ..still(size, Orientation::billboard(Span::fixed(0.0)), colour) };
    r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Square), params }).unwrap()
}

fn render(r: &mut dyn Renderer, scene: &Scene, alpha: AlphaEncoding) -> Image {
    r.render_image(UVec2::new(32, 32), alpha, scene, &Quality { samples: 4, ..Quality::high() }).unwrap()
}

fn at(scene_camera: Camera, background: Background, system: ParticleSystemId, depth: f32) -> Scene {
    let mut scene = Scene::new(scene_camera);
    scene.background = background;
    scene.post.tonemap = omuzaki_render_api::Tonemap::None;
    scene.frame.time = Seconds(1.0);
    scene.particles.push(SpawnBatch::new(system, Mat4::from_translation(Vec3::new(0.0, 0.0, -depth))));
    scene
}

fn eye(far: f32) -> Camera {
    Camera::look_at(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y, Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far })
}

/// D5: coverage over a transparent target gives `(c/2, 1/2)` at 0.5 (four samples), `(c, 1)` at 1 and nothing at 0, in
/// both encodings and the same as the mock; over grey it mixes half and half.
#[test]
fn coverage_composites_premultiplied_without_fringes() {
    let colour = Colour::rgb(240, 120, 40);
    for (coverage, want_alpha) in [(1.0, 255u8), (0.5, 128), (0.0, 0)] {
        for encoding in [AlphaEncoding::Straight, AlphaEncoding::Premultiplied] {
            let mut pictures = Vec::new();
            for r in [&mut support::renderer() as &mut dyn Renderer, &mut MockRenderer::new()] {
                let id = square(r, colour, coverage, 2.0);
                let picture = render(r, &at(eye(100.0), Background::Transparent, id, 1.0), encoding);
                r.release(id.into()).unwrap();
                pictures.push(picture.colour(16, 16).unwrap());
            }
            let (gpu, mock) = (pictures[0], pictures[1]);
            assert!(gpu.a.abs_diff(want_alpha) <= 1, "coverage {coverage} ({encoding:?}): alpha {}", gpu.a);
            for (g, m) in [(gpu.r, mock.r), (gpu.g, mock.g), (gpu.b, mock.b), (gpu.a, mock.a)] {
                assert!(g.abs_diff(m) <= 3, "coverage {coverage} ({encoding:?}): GPU {gpu}, mock {mock}");
            }
            if encoding == AlphaEncoding::Straight && coverage > 0.0 {
                assert!(close(gpu, Colour::rgba(colour.r, colour.g, colour.b, want_alpha)), "no fringe: {gpu}");
            }
        }
    }
    let mut r = support::renderer();
    let id = square(&mut r, Colour::WHITE, 0.5, 2.0);
    let grey = Background::Solid { colour: Colour::rgb(0, 0, 0) };
    let picture = render(&mut r, &at(eye(100.0), grey, id, 1.0), AlphaEncoding::Straight);
    // Half white over black: 0.5 in linear light.
    let got = picture.colour(16, 16).unwrap();
    assert!(got.r.abs_diff(188) <= 2 && got.a == 255, "half white over black is {got}");
}

fn close(a: Colour, b: Colour) -> bool {
    [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)].iter().all(|&(x, y)| x.abs_diff(y) <= 3)
}

/// D6: the cleared depth never matches. A zero-coverage particle at the far plane leaves the target untouched with
/// `w < 1` there (a far plane at 0.8 m, beyond the near fade) and with `w > 1` (the default 100 m); a covering one at
/// the far plane still draws; one beyond it draws nothing and never hides nearer geometry.
#[test]
fn the_far_plane_and_the_depth_clamp() {
    let mut r = support::renderer();
    for far in [0.8, 100.0] {
        let hidden = square(&mut r, Colour::WHITE, 0.0, far);
        let picture =
            render(&mut r, &at(eye(far), Background::Transparent, hidden, far * 0.9999), AlphaEncoding::Straight);
        assert!(picture.rgba8.iter().all(|&v| v == 0), "far {far}: a zero-coverage particle wrote something");
        let shown = square(&mut r, Colour::WHITE, 1.0, far);
        let picture =
            render(&mut r, &at(eye(far), Background::Transparent, shown, far * 0.9999), AlphaEncoding::Straight);
        assert_eq!(picture.colour(16, 16).unwrap(), Colour::WHITE, "far {far}: a particle at the far plane draws");
        let beyond = render(&mut r, &at(eye(far), Background::Transparent, shown, far * 1.01), AlphaEncoding::Straight);
        assert!(beyond.rgba8.iter().all(|&v| v == 0), "far {far}: a particle beyond the far plane drew");

        // Nearer geometry: a green card at half the far distance, a particle beyond the far plane behind it.
        let mesh = r.create_mesh(&shapes::quad(Vec2::splat(far))).unwrap();
        let material = r.create_material(&Material::unlit(Colour::rgb(0, 255, 0))).unwrap();
        let mut scene = at(eye(far), Background::Transparent, shown, far * 1.01);
        scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.0, 0.0, -far * 0.5))));
        let picture = render(&mut r, &scene, AlphaEncoding::Straight);
        assert_eq!(picture.colour(16, 16).unwrap(), Colour::rgb(0, 255, 0), "far {far}: the card stays in front");
        for id in [hidden, shown] {
            r.release(id.into()).unwrap();
        }
        r.release(material.into()).unwrap();
        r.release(mesh.into()).unwrap();
    }
}

/// D4: a translucent lens in front of a particle tints it; behind it, the particle hides it. No sorting either way.
#[test]
fn a_lens_in_front_tints_and_a_lens_behind_is_hidden() {
    let mut r = support::renderer();
    let particle = square(&mut r, Colour::rgb(0, 0, 255), 1.0, 0.5);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(1.0))).unwrap();
    let mut glass = Material::unlit(Colour::rgba(255, 0, 0, 128));
    glass.alpha = AlphaMode::Blend;
    let glass = r.create_material(&glass).unwrap();
    let mut colours = Vec::new();
    for lens_depth in [0.5, 2.0] {
        let mut scene = at(eye(100.0), Background::Solid { colour: Colour::BLACK }, particle, 1.0);
        scene.instances.push(Instance::new(mesh, glass, Mat4::from_translation(Vec3::new(0.0, 0.0, -lens_depth))));
        colours.push(render(&mut r, &scene, AlphaEncoding::Straight).colour(16, 16).unwrap());
    }
    let (front, behind) = (colours[0], colours[1]);
    assert!(front.r > 100 && front.b > 100, "a lens in front tints the particle: {front}");
    assert!(behind.r == 0 && behind.b > 250, "a lens behind is hidden by the particle: {behind}");
    for id in [particle.into(), glass.into(), mesh.into()] {
        r.release(id).unwrap();
    }
}

/// D7: a frame is a function of its time. Rendering it twice gives the same bytes, and any frame stands alone.
#[test]
fn a_burst_is_deterministic_and_has_no_history() {
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
    let frame = |r: &mut dyn Renderer, t: f64| {
        let id = r.create_particle_system(&desc).unwrap();
        let mut scene = Scene::new(particle_sheet::camera());
        scene.background = Background::Transparent;
        scene.frame.time = Seconds(t);
        scene.particles.push(SpawnBatch { count: 500, seed: 11, ..SpawnBatch::new(id, Mat4::IDENTITY) });
        let image =
            r.render_image(UVec2::new(96, 96), AlphaEncoding::Premultiplied, &scene, &particle_sheet::quality());
        r.release(id.into()).unwrap();
        image.unwrap()
    };
    let mut r = support::renderer();
    let (a, b) = (frame(&mut r, 1.0), frame(&mut r, 1.0));
    assert_eq!(a.rgba8, b.rgba8, "the same frame twice");
    let mut fresh = support::renderer();
    let direct = frame(&mut fresh, 2.0);
    let _ = frame(&mut r, 0.3);
    assert_eq!(frame(&mut r, 2.0).rgba8, direct.rgba8, "t = 2 doesn't depend on the frames before it");
    assert!(a.rgba8.as_chunks::<4>().0.iter().any(|p| p[3] > 0), "the burst is visible");
}

#[test]
fn the_report_counts_slots_and_times_the_pass() {
    let mut r = support::renderer();
    let id = square(&mut r, Colour::WHITE, 1.0, 0.1);
    let target = r.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight }).unwrap();
    let mut scene = at(eye(100.0), Background::Transparent, id, 1.0);
    scene.particles[0].count = 300;
    scene.particles.push(SpawnBatch { count: 0, ..scene.particles[0] });
    let report = r.render(target, &scene, &Quality::live()).unwrap();
    assert_eq!(report.particles, 300);
    assert!(report.cpu.get(Pass::Particles).is_some(), "the pass ran and is timed");
    scene.particles.clear();
    let report = r.render(target, &scene, &Quality::live()).unwrap();
    assert_eq!(report.particles, 0);
    assert!(report.cpu.get(Pass::Particles).is_none(), "no batches, no pass");
    r.release(id.into()).unwrap();
    r.release(target.into()).unwrap();
}

/// The design's §3 confetti, and rain and snow as the weather draws them: (name, system, batch, time).
fn presets(r: &mut dyn Renderer) -> Vec<(&'static str, ParticleSystemId, SpawnBatch, f64)> {
    let confetti: ParticleParams = serde_json::from_value(serde_json::json!({
        "lifetime": [3.5, 5.0], "speed": [3.0, 5.0], "direction": [0.0, 1.0, 0.0], "spread_deg": 35.0,
        "gravity": [0.0, -3.0, 0.0], "drag": 3.0, "wind": [0.0, 0.0, 0.0],
        "flutter": { "amplitude_m": 0.05, "frequency_hz": 1.5 }, "size": [0.012, 0.018],
        "size_curve": [0.4, 1.0, 1.0, 0.0], "orientation": { "kind": "tumble", "rate_deg": [360.0, 900.0] },
        "colours": ["#ff5a7a", "#ffc94d", "#5ad1ff", "#9b7bff", "#6ee7a0", "#ffffff"], "coverage": 1.0,
        "shading": { "kind": "toon", "shade": "#b8a8c8", "shift": 0.0, "softness": 0.1 }, "glow": 0.0, "floor": 0.0
    }))
    .unwrap();
    let rain: ParticleParams = serde_json::from_value(serde_json::json!({
        "lifetime": [0.55, 0.75], "speed": [6.5, 7.5], "direction": [0.05, -1.0, 0.0], "spread_deg": 2.0,
        "gravity": [0.0, 0.0, 0.0], "drag": 0.0, "wind": [0.0, 0.0, 0.0], "size": [0.005, 0.007],
        "size_curve": [1.0, 1.0, 1.0, 1.0], "orientation": { "kind": "velocity", "stretch": 0.035 },
        "colours": ["#c8d6ec"], "coverage": 0.55, "shading": { "kind": "unlit" }, "glow": 0.0, "floor": 0.0
    }))
    .unwrap();
    let snow: ParticleParams = serde_json::from_value(serde_json::json!({
        "lifetime": [6.0, 8.0], "speed": [0.1, 0.3], "direction": [0.0, -1.0, 0.0], "spread_deg": 30.0,
        "gravity": [0.0, -0.3, 0.0], "drag": 1.0, "wind": [0.1, 0.0, 0.0],
        "flutter": { "amplitude_m": 0.15, "frequency_hz": 0.5 }, "size": [0.012, 0.02],
        "size_curve": [0.6, 1.0, 1.0, 0.6], "orientation": { "kind": "billboard", "spin_deg": [-60.0, 60.0] },
        "colours": ["#f4f8ff", "#e6eefc"], "coverage": 0.9, "shading": { "kind": "unlit" }, "floor": 0.0
    }))
    .unwrap();
    let mut system = |sprite, params| r.create_particle_system(&ParticleSystemDesc { sprite, params }).unwrap();
    let confetti = system(Sprite::Shape(SpriteShape::rect(0.6)), confetti);
    let rain = system(Sprite::Shape(SpriteShape::Streak), rain);
    let snow = system(Sprite::Shape(SpriteShape::Disc), snow);
    let burst = SpawnBatch {
        shape: omuzaki_render_api::EmitterShape::sphere(0.05),
        count: 150,
        seed: 21,
        ..SpawnBatch::new(confetti, Mat4::from_translation(Vec3::new(0.0, 1.3, 0.15)))
    };
    // A ceiling at 2.6 m over a 3.7 m x 5.2 m area: rain at intensity 0.6 (1,830 births a second) and a steady snow.
    let ceiling = |system, interval: f32, count| SpawnBatch {
        shape: omuzaki_render_api::EmitterShape::rect(Vec2::new(1.85, 2.6)),
        start: Seconds(4.0 - f64::from(interval) * f64::from(count)),
        interval,
        count,
        seed: 5,
        ..SpawnBatch::new(system, Mat4::from_translation(Vec3::new(0.0, 2.6, -1.0)))
    };
    vec![
        ("confetti at 0.1 s", confetti, burst, 0.1),
        ("confetti at 0.6 s", confetti, burst, 0.6),
        ("confetti at 2.0 s", confetti, burst, 2.0),
        ("rain at 4 s", rain, ceiling(rain, 1.0 / 1830.0, 1464), 4.0),
        ("snow at 4 s", snow, ceiling(snow, 1.0 / 500.0, 4000), 4.0),
    ]
}

/// One preset's picture.
const PRESET: UVec2 = UVec2::new(320, 320);

/// The presets side by side over `background`, from 3.2 m back: the whole confetti arc, from the launch at 1.3 m to
/// its peak a metre above, is in frame.
fn preset_sheet(r: &mut dyn Renderer, background: Background) -> Image {
    let cases = presets(r);
    let width = PRESET.x * u32::try_from(cases.len()).unwrap();
    let mut sheet = vec![0u8; (width * PRESET.y * 4) as usize];
    let row = (PRESET.x * 4) as usize;
    let camera =
        Camera::look_at(Vec3::new(0.0, 1.5, 3.2), Vec3::new(0.0, 1.5, 0.0), Vec3::Y, Camera::default().projection);
    for (i, (_, _, batch, time)) in cases.iter().enumerate() {
        let mut scene = Scene::new(camera);
        scene.background = background;
        scene.lights = particle_sheet::lights();
        scene.post.tonemap = omuzaki_render_api::Tonemap::None;
        scene.frame.time = Seconds(*time);
        scene.particles.push(*batch);
        let image = r.render_image(PRESET, AlphaEncoding::Straight, &scene, &Quality::offline()).unwrap();
        for y in 0..PRESET.y as usize {
            let to = (y * width as usize + i * PRESET.x as usize) * 4;
            sheet[to..to + row].copy_from_slice(&image.rgba8[y * row..(y + 1) * row]);
        }
    }
    let mut released = Vec::new();
    for (_, system, _, _) in cases {
        if !released.contains(&system) {
            r.release(system.into()).unwrap();
            released.push(system);
        }
    }
    Image { size: UVec2::new(width, PRESET.y), alpha: AlphaEncoding::Straight, rgba8: sheet }
}

/// #940's goldens: the §3 confetti at 0.1, 0.6 and 2.0 s, rain and snow at 4 s, over transparent and over grey (the
/// offline preset). The transparent render composited over grey in linear light agrees with the grey render within
/// two levels: coverage leaves nothing for an encoding to get wrong.
#[test]
fn the_presets_match_their_goldens_and_composite_like_the_grey_render() {
    let mut r = support::renderer();
    let grey = Colour::hex(0x0080_8080);
    let over_grey = preset_sheet(&mut r, Background::Solid { colour: grey });
    let transparent = preset_sheet(&mut r, Background::Transparent);
    matches_golden(&over_grey, "particles-presets-grey.png");
    matches_golden(&transparent, "particles-presets-transparent.png");
    let behind = grey.to_linear();
    let mut worst = 0u8;
    for (t, g) in transparent.rgba8.as_chunks::<4>().0.iter().zip(over_grey.rgba8.as_chunks::<4>().0) {
        let a = f32::from(t[3]) / 255.0;
        let mixed = Colour::rgb(t[0], t[1], t[2]).to_linear() * a + behind * (1.0 - a);
        let c = Colour::from_linear(mixed, 1.0);
        worst = worst.max(c.r.abs_diff(g[0])).max(c.g.abs_diff(g[1])).max(c.b.abs_diff(g[2]));
    }
    assert!(worst <= 2, "the transparent render over grey differs from the grey render by up to {worst} levels");
    let covered = transparent.rgba8.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
    assert!(covered > 2_000, "the presets are visible: {covered} pixels");
}
