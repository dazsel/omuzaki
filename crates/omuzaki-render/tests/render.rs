//! End-to-end behaviour of the renderer on the software adapter: pictures, timings, targets and error handling. The
//! conformance suite (`conformance.rs`) covers the contract's own checks; these cover what is specific to the GPU
//! implementation, and compare it with the mock where the two should agree.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::{AdapterPreference, NativePaintTarget, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, ColourSpace, Filter, Image, Instance, JointRange, Mapping,
    Material, Overlay, PaintTarget, Pass, PixelRect, Projection, Quality, QualityMode, RenderError, Renderer,
    RendererExt, Sampler, Scene, TargetDesc, TextureDesc, Tonemap,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: UVec2 = UVec2::new(64, 64);

/// An orthographic camera on +Z, 2 m tall: world units are 32 pixels at 64x64 and the origin is pixel (32, 32).
fn ortho() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// Exact colours (no tone curve) on a flat background.
fn scene(background: Colour) -> Scene {
    let mut s = Scene::new(ortho());
    s.background = Background::Solid { colour: background };
    s.post.tonemap = Tonemap::None;
    s
}

/// One sample, no effects: pixels are exactly covered or not.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

fn draw(r: &mut dyn Renderer, scene: &Scene, quality: &Quality) -> Image {
    r.render_image(SIZE, AlphaEncoding::Straight, scene, quality).unwrap()
}

fn pixel(image: &Image, x: u32, y: u32) -> [u8; 4] {
    image.pixel(x, y).unwrap()
}

fn assert_close(got: [u8; 4], want: [u8; 4], tolerance: u8, what: &str) {
    assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tolerance), "{what}: got {got:?}, wanted {want:?}");
}

// ---- the picture ----

/// A scene with every feature the flat Opaque pass draws: base colour, vertex colours, a base texture, a planar
/// overlay, skinning, a mirrored transform, a double-sided sheet and an alpha mask. The mock draws unlit materials
/// exactly, so the two renderers should agree away from edges.
fn feature_scene(r: &mut dyn Renderer) -> Scene {
    let checker: Vec<u8> = (0..16)
        .flat_map(|i: u32| if (i % 4 + i / 4).is_multiple_of(2) { [230, 60, 60, 255] } else { [40, 90, 220, 255] })
        .collect();
    // The mock reads the nearest texel, so the texture asks for hard texels: the GPU would otherwise blend the checks.
    let hard = TextureDesc {
        sampler: Sampler { filter: Filter::Nearest, ..Sampler::default() },
        ..TextureDesc::colour(UVec2::new(4, 4))
    };
    let texture = r.create_texture(&hard, Some(&checker)).unwrap();

    let mut tinted = shapes::quad(Vec2::splat(0.3));
    tinted.colours = vec![[1.0, 0.5, 0.25, 1.0], [0.25, 1.0, 0.5, 1.0], [0.5, 0.25, 1.0, 1.0], [1.0; 4]];
    let tinted = r.create_mesh(&tinted).unwrap();
    let mut skinned = shapes::quad(Vec2::splat(0.2));
    skinned.joints = vec![[0, 0, 0, 0]; 4];
    skinned.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    let skinned = r.create_mesh(&skinned).unwrap();
    let cube = r.create_mesh(&shapes::cuboid(Vec3::splat(0.25))).unwrap();
    let sheet = r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();

    let mut painted = Material::unlit(Colour::rgb(200, 200, 60));
    painted.vertex_colours = true;
    let painted = r.create_material(&painted).unwrap();
    let mut textured = Material::unlit(Colour::WHITE);
    textured.base_texture = Some(texture);
    textured.overlays.push(Overlay {
        texture,
        mapping: Mapping::Planar { min: Vec2::splat(-0.1), max: Vec2::splat(0.1), fade_from: -1.0, fade_to: -1.0 },
        strength: 0.6,
    });
    let textured = r.create_material(&textured).unwrap();
    let plain_material = r.create_material(&Material::unlit(Colour::rgb(60, 200, 120))).unwrap();
    let mut two_sided = Material::unlit(Colour::rgb(240, 120, 40));
    two_sided.double_sided = true;
    two_sided.alpha = AlphaMode::Mask { cutoff: 0.5 };
    let two_sided = r.create_material(&two_sided).unwrap();

    let mut s = scene(Colour::rgb(24, 20, 36));
    let at = |x: f32, y: f32| Mat4::from_translation(Vec3::new(x, y, 0.0));
    s.instances.push(Instance::new(tinted, painted, at(-0.55, 0.5)));
    s.instances.push(Instance::new(cube, textured, at(0.5, 0.5) * Mat4::from_rotation_y(0.5)));
    let mut character = Instance::new(skinned, plain_material, Mat4::IDENTITY);
    character.skin = Some(JointRange { start: 0, len: 1 });
    s.instances.push(character);
    s.joints.push(at(-0.5, -0.5));
    s.instances.push(Instance::new(cube, plain_material, at(0.5, -0.5) * Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0))));
    s.instances.push(Instance::new(sheet, two_sided, at(0.0, 0.0) * Mat4::from_rotation_y(3.0)));
    s
}

#[test]
fn unlit_scenes_match_the_mock_away_from_edges() {
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    let gpu_scene = feature_scene(&mut gpu);
    let mock_scene = feature_scene(&mut mock);
    let (a, b) = (draw(&mut gpu, &gpu_scene, &plain()), draw(&mut mock, &mock_scene, &plain()));

    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let dir = PathBuf::from(dir);
        for (name, image) in [("parity-gpu.png", &a), ("parity-mock.png", &b)] {
            image::save_buffer(dir.join(name), &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8)
                .unwrap();
        }
    }
    let differing: Vec<(usize, u8)> = a
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba8.as_chunks::<4>().0)
        .enumerate()
        .map(|(i, (p, q))| (i, p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)))
        .filter(|&(_, d)| d > 3)
        .collect();
    let total = (SIZE.x * SIZE.y) as usize;
    println!("GPU vs mock: {} of {total} pixels differ by more than 3 levels", differing.len());
    // Triangle edges rasterise slightly differently (the mock snaps to 1/256 pixel); everything else must agree.
    assert!(
        differing.len() * 100 < total,
        "{} of {total} pixels differ; first few: {:?}",
        differing.len(),
        &differing[..differing.len().min(5)]
    );
    assert!(differing.iter().all(|&(_, d)| d < 128), "an edge pixel can differ, a whole region cannot");
}

#[test]
fn a_cube_renders_headless_to_a_png() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.5))).unwrap();
    let base = Colour::hex(0x00c9_b8f0);
    let material = r.create_material(&Material::unlit(base)).unwrap();
    let eye = Vec3::new(2.4, 1.8, 3.2);
    let mut s = Scene::new(Camera::look_at(eye, Vec3::ZERO, Vec3::Y, Camera::default().projection));
    s.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    s.post.tonemap = Tonemap::None;
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.4)));
    let image = r.render_image(UVec2::new(192, 192), AlphaEncoding::Straight, &s, &Quality::offline()).unwrap();

    assert_close(pixel(&image, 96, 96), [base.r, base.g, base.b, 255], 2, "the cube's flat colour");
    assert_close(pixel(&image, 2, 2), [0x18, 0x12, 0x20, 255], 2, "the backdrop");

    // The PNG survives a round trip byte for byte (in a directory of its own, so concurrent runs never share a file),
    // and `OMUZAKI_RENDER_OUT=<dir>` keeps a copy for evidence.
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("cube.png");
    image::save_buffer(&path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    assert_eq!(image::open(&path).unwrap().to_rgba8().as_raw(), &image.rgba8);
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        std::fs::copy(&path, PathBuf::from(dir).join("omuzaki-render-cube.png")).unwrap();
    }
}

#[test]
fn premultiplied_and_linear_textures_are_decoded_by_the_shader() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let render_with = |r: &mut WgpuRenderer, desc: TextureDesc, texel: [u8; 4]| {
        let texture = r.create_texture(&desc, Some(&texel.repeat(4))).unwrap();
        let mut material = Material::unlit(Colour::WHITE);
        material.base_texture = Some(texture);
        let material = r.create_material(&material).unwrap();
        let mut s = scene(Colour::BLACK);
        s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        let image = draw(r, &s, &plain());
        r.release(material.into()).unwrap();
        r.release(texture.into()).unwrap();
        pixel(&image, 32, 32)
    };
    let two = UVec2::new(2, 2);

    // Premultiplied sRGB: 128/128 is the straight colour 255, so the quad shows full red, not half.
    let premultiplied = TextureDesc { alpha: AlphaEncoding::Premultiplied, ..TextureDesc::colour(two) };
    assert_close(render_with(&mut r, premultiplied, [128, 0, 0, 128]), [255, 0, 0, 255], 2, "premultiplied sRGB red");

    // Linear data is used as stored: 128/255 linear encodes to about 188.
    let linear = TextureDesc { colour_space: ColourSpace::Linear, ..TextureDesc::colour(two) };
    assert_close(render_with(&mut r, linear, [128, 128, 128, 255]), [188, 188, 188, 255], 2, "linear grey");

    // Straight sRGB through the hardware decode: 128 stays 128 after decode and encode.
    assert_close(
        render_with(&mut r, TextureDesc::colour(two), [128, 128, 128, 255]),
        [128, 128, 128, 255],
        2,
        "sRGB grey",
    );
}

#[test]
fn render_scale_changes_the_internal_size_but_not_the_picture() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::rgb(200, 100, 50))).unwrap();
    let mut s = scene(Colour::rgb(10, 20, 30));
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_z(0.3)));
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();

    let mut sizes = Vec::new();
    for scale in [0.5, 1.0, 1.5, 2.0] {
        let report = r.render(target, &s, &Quality { render_scale: scale, ..plain() }).unwrap();
        sizes.push(report.render_size);
        let image = r.read_target(target).unwrap();
        assert_close(pixel(&image, 32, 32), [200, 100, 50, 255], 2, &format!("centre at scale {scale}"));
        assert_close(pixel(&image, 2, 2), [10, 20, 30, 255], 2, &format!("corner at scale {scale}"));
        if scale > 1.0 {
            let edge_pixels = image.rgba8.as_chunks::<4>().0.iter().filter(|p| p[0] != 10 && p[0] != 200).count();
            assert!(edge_pixels > 0, "supersampling at scale {scale} must blend the rotated edge");
        }
    }
    assert_eq!(sizes, [UVec2::splat(32), UVec2::splat(64), UVec2::splat(96), UVec2::splat(128)]);
}

#[test]
fn any_sample_count_works_and_falls_back_to_what_the_hardware_has() {
    let mut r = support::renderer();
    let hardware = r.capabilities().max_samples;
    assert!((1..=8).contains(&hardware));
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_z(0.5)));
    // The software adapter has 1 and 4 only: 2 falls back to 1, and a live frame's 8 and 16 to 4. An offline frame
    // reaches 8 and 16 by drawing more rounds (`tests/supersample.rs`), so its edges get more levels.
    let live = |samples| Quality { samples, mode: QualityMode::Live, ..plain() };
    let mut smoothed = Vec::new();
    let mut levels = Vec::new();
    for samples in [1, 2, 4, 8, 16] {
        let image = draw(&mut r, &s, &Quality { samples, ..plain() });
        assert_close(pixel(&image, 32, 32), [255, 255, 255, 255], 1, &format!("centre with {samples} samples"));
        let edge: Vec<u8> =
            image.rgba8.as_chunks::<4>().0.iter().map(|p| p[0]).filter(|&v| v != 0 && v != 255).collect();
        smoothed.push(edge.len());
        levels.push(edge.into_iter().collect::<std::collections::BTreeSet<u8>>().len());
    }
    assert_eq!(smoothed[0], 0, "one sample gives hard edges");
    assert!(smoothed[2] > 0, "four samples smooth the rotated edge");
    assert!(levels[4] > levels[2], "offline, 16 samples give the edge more levels than 4: {levels:?}");
    let most = draw(&mut r, &s, &live(hardware));
    assert_eq!(draw(&mut r, &s, &live(16)).rgba8, most.rgba8, "a live frame asking for more draws with the maximum");
}

// ---- exposure and tone mapping ----

/// The sRGB curve's decode of an 8-bit value, in f64.
fn srgb_to_linear(byte: u8) -> f64 {
    let c = f64::from(byte) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The sRGB curve's encode of a linear value, as a level on the 0 to 255 scale (unrounded).
fn linear_to_srgb(linear: f64) -> f64 {
    let c = linear.clamp(0.0, 1.0);
    255.0 * if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// three.js `ACESFilmicToneMapping` (Stephen Hill's RRT and ODT fit, with the `exposure / 0.6` pre-scale) followed by
/// the sRGB curve, in f64: the picture the prototype shows. Takes an 8-bit sRGB colour as `Colour::rgb` does and gives
/// the level (unrounded) each channel should reach.
fn three_js_aces(rgb: [u8; 3], exposure: f64) -> [f64; 3] {
    // `ACESInputMat` and `ACESOutputMat` from three.js, written as rows (GLSL declares them by column).
    const INPUT: [[f64; 3]; 3] =
        [[0.59719, 0.35458, 0.04823], [0.07600, 0.90834, 0.01566], [0.02840, 0.13383, 0.83777]];
    const OUTPUT: [[f64; 3]; 3] =
        [[1.60475, -0.53108, -0.07367], [-0.10208, 1.10813, -0.00605], [-0.00327, -0.07276, 1.07602]];
    let apply = |m: &[[f64; 3]; 3], v: [f64; 3]| m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2]);
    let scaled = rgb.map(|c| srgb_to_linear(c) * exposure / 0.6);
    let fitted = apply(&INPUT, scaled).map(|v| {
        let a = v * (v + 0.024_578_6) - 0.000_090_537;
        let b = v * (0.983_729 * v + 0.432_951_0) + 0.238_081;
        a / b
    });
    apply(&OUTPUT, fitted).map(linear_to_srgb)
}

/// three.js's own values for a few colours (its shader chunk evaluated outside the renderer, during the renderer's first review):
/// `(sRGB colour, exposure, levels per channel)`. They pin `three_js_aces` itself.
const THREE_JS: &[([u8; 3], f64, [f64; 3])] = &[
    ([128, 128, 128], 1.0, [140.7; 3]),
    ([128, 128, 128], 2.0, [188.4; 3]),
    ([201, 184, 240], 1.0, [205.2, 195.3, 220.8]),
    ([255, 255, 255], 1.0, [226.4; 3]),
    ([60, 200, 120], 1.0, [108.7, 204.7, 142.4]),
    ([60, 200, 120], 2.0, [173.7, 231.4, 190.8]),
    ([240, 120, 40], 1.0, [234.0, 140.3, 50.0]),
];

/// Draws `colours` as unlit 1 m squares on a grid of 16-pixel cells, 8 to a row, with the default (ACES) tone map at
/// `exposure`, and returns the centre pixel of each cell. Nothing lights the squares, so a pixel is the colour as the
/// Resolve pass alone makes it.
fn aces_cells(r: &mut dyn Renderer, colours: &[[u8; 3]], exposure: f32) -> Vec<[u8; 4]> {
    const COLUMNS: u16 = 8;
    /// Pixels per cell, and per metre of the orthographic view.
    const PITCH: u32 = 16;
    let rows = u16::try_from(colours.len()).unwrap().div_ceil(COLUMNS);
    let square = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let mut s = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: f32::from(rows), near: 0.1, far: 20.0 },
    ));
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.exposure = exposure;
    s.post.tonemap = Tonemap::Aces;
    let mut materials = Vec::new();
    for (i, &[red, green, blue]) in (0u16..).zip(colours) {
        let material = r.create_material(&Material::unlit(Colour::rgb(red, green, blue))).unwrap();
        let (column, row) = (f32::from(i % COLUMNS), f32::from(i / COLUMNS));
        let at = Vec3::new(column - f32::from(COLUMNS - 1) / 2.0, f32::from(rows - 1) / 2.0 - row, 0.0);
        s.instances.push(Instance::new(square, material, Mat4::from_translation(at)));
        materials.push(material);
    }
    let size = UVec2::new(PITCH * u32::from(COLUMNS), PITCH * u32::from(rows));
    let image = r.render_image(size, AlphaEncoding::Straight, &s, &plain()).unwrap();
    let cells = (0..u32::try_from(colours.len()).unwrap())
        .map(|i| pixel(&image, i % u32::from(COLUMNS) * PITCH + PITCH / 2, i / u32::from(COLUMNS) * PITCH + PITCH / 2))
        .collect();
    for material in materials {
        r.release(material.into()).unwrap();
    }
    r.release(square.into()).unwrap();
    cells
}

#[test]
fn aces_with_exposure_matches_the_three_js_curve_on_saturated_colours() {
    // The reference is itself pinned to three.js's numbers first, so a typo in it cannot hide a bug in the shader.
    for &(rgb, exposure, want) in THREE_JS {
        let got = three_js_aces(rgb, exposure);
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 0.1), "{rgb:?} at {exposure}: {got:?} vs {want:?}");
    }

    // Six saturated hues at eight brightnesses, plus the colours three.js was evaluated on.
    let levels = [32u8, 64, 96, 128, 160, 192, 224, 255];
    let hues: [fn(u8) -> [u8; 3]; 6] =
        [|v| [v, 0, 0], |v| [0, v, 0], |v| [0, 0, v], |v| [0, v, v], |v| [v, 0, v], |v| [v, v, 0]];
    let mut colours: Vec<[u8; 3]> = hues.iter().flat_map(|hue| levels.map(hue)).collect();
    colours.extend(THREE_JS.iter().map(|&(rgb, ..)| rgb));

    let mut r = support::renderer();
    let mut worst = 0.0f64;
    for exposure in [1.0f32, 2.0] {
        let cells = aces_cells(&mut r, &colours, exposure);
        for (&rgb, cell) in colours.iter().zip(&cells) {
            let want = three_js_aces(rgb, f64::from(exposure));
            let error = (0..3).map(|c| (f64::from(cell[c]) - want[c]).abs()).fold(0.0, f64::max);
            assert!(error <= 1.0, "{rgb:?} at exposure {exposure}: GPU {cell:?}, three.js {want:?}");
            worst = worst.max(error);
        }
    }
    println!("ACES on the GPU vs three.js: worst {worst:.2} levels over {} colours at 2 exposures", colours.len());

    // The mock is the reference other tests compare against, so it draws the same curve (#630).
    let mut mock = MockRenderer::new();
    let mut worst_mock = 0.0f64;
    for exposure in [1.0f32, 2.0] {
        let cells = aces_cells(&mut mock, &colours, exposure);
        for (&rgb, cell) in colours.iter().zip(&cells) {
            let want = three_js_aces(rgb, f64::from(exposure));
            let error = (0..3).map(|c| (f64::from(cell[c]) - want[c]).abs()).fold(0.0, f64::max);
            assert!(error <= 1.0, "{rgb:?} at exposure {exposure}: mock {cell:?}, three.js {want:?}");
            worst_mock = worst_mock.max(error);
        }
    }
    println!("ACES in the mock vs three.js: worst {worst_mock:.2} levels");

    // Two values written out, so the test reads without the formula: mid grey, and a saturated green at exposure 2.
    let grey = aces_cells(&mut r, &[[128, 128, 128]], 1.0);
    assert_close(grey[0], [141, 141, 141, 255], 1, "grey 128 at exposure 1");
    let green = aces_cells(&mut r, &[[60, 200, 120]], 2.0);
    assert_close(green[0], [174, 231, 191, 255], 1, "(60, 200, 120) at exposure 2");
}

// ---- targets ----

#[test]
fn two_targets_of_different_sizes_do_not_disturb_each_other() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let (red, blue) = (
        r.create_material(&Material::unlit(Colour::rgb(255, 0, 0))).unwrap(),
        r.create_material(&Material::unlit(Colour::rgb(0, 0, 255))).unwrap(),
    );
    let small = r.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight }).unwrap();
    let large = r.create_target(&TargetDesc { size: UVec2::new(80, 48), alpha: AlphaEncoding::Straight }).unwrap();
    for _ in 0..2 {
        for (target, material) in [(small, red), (large, blue), (small, red)] {
            let mut s = scene(Colour::BLACK);
            s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
            r.render(target, &s, &plain()).unwrap();
        }
    }
    let (a, b) = (r.read_target(small).unwrap(), r.read_target(large).unwrap());
    assert_eq!((a.size, b.size), (UVec2::new(32, 32), UVec2::new(80, 48)));
    assert_close(pixel(&a, 16, 16), [255, 0, 0, 255], 1, "small target");
    assert_close(pixel(&b, 40, 24), [0, 0, 255, 255], 1, "large target");
}

#[test]
fn resizing_a_target_discards_its_frame_and_draws_at_the_new_size() {
    let mut r = support::renderer();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let s = scene(Colour::rgb(9, 99, 199));
    r.render(target, &s, &plain()).unwrap();
    let before = r.target_texture(target).unwrap();
    assert_eq!((before.width(), before.height()), (64, 64));

    r.resize_target(target, UVec2::new(40, 20)).unwrap();
    assert!(matches!(r.read_target(target), Err(RenderError::Invalid { .. })), "the old frame is gone");
    r.render(target, &s, &plain()).unwrap();
    let image = r.read_target(target).unwrap();
    assert_eq!(image.size, UVec2::new(40, 20));
    assert_close(pixel(&image, 5, 5), [9, 99, 199, 255], 1, "the new frame");
    let after = r.target_texture(target).unwrap();
    assert_eq!((after.width(), after.height(), after.format()), (40, 20, wgpu::TextureFormat::Rgba8Unorm));
}

#[test]
fn a_failed_frame_leaves_the_targets_previous_frame() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let mut s = scene(Colour::rgb(40, 40, 40));
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    r.render(target, &s, &plain()).unwrap();
    let good = r.read_target(target).unwrap();

    r.release(mesh.into()).unwrap();
    let err = r.render(target, &s, &plain()).unwrap_err();
    assert!(matches!(err, RenderError::UnknownHandle { what: "mesh" }), "{err:?}");
    assert_eq!(r.read_target(target).unwrap(), good, "the failed frame must not have touched the target");
}

#[test]
fn gpu_memory_estimates_follow_what_is_created_and_return_to_zero() {
    let mut r = support::renderer();
    assert_eq!(r.stats().gpu_bytes, 0, "a fresh renderer holds no resources");
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::ONE)).unwrap();
    // 24 vertices of 72 bytes, each with a 12 byte smooth normal beside it (for outlines), and 36 indices.
    let mesh_bytes = 24 * (72 + 12) + 36 * 4;
    assert_eq!(r.stats().gpu_bytes, mesh_bytes, "vertices, smooth normals and indices");
    let texture = r.create_texture(&TextureDesc::colour(UVec2::splat(64)), None).unwrap();
    let with_texture = r.stats().gpu_bytes;
    assert_eq!(with_texture, mesh_bytes + 64 * 64 * 4 * 4 / 3, "a mip-mapped texture is a third larger");
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let before_frame = r.stats().gpu_bytes;
    assert_eq!(before_frame, with_texture + 64 * 64 * 4, "the output texture");
    r.render(target, &scene(Colour::BLACK), &Quality { samples: 4, ..plain() }).unwrap();
    assert!(r.stats().gpu_bytes > before_frame + 64 * 64 * 8 * 4, "the first frame adds the multisampled attachments");

    for resource in [mesh.into(), texture.into(), target.into()] {
        r.release(resource).unwrap();
    }
    assert_eq!(r.stats().gpu_bytes, 0);
}

// ---- timings ----

#[test]
fn reports_have_real_cpu_times_and_gpu_times_that_arrive_later() {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.7)));

    let mut last = None;
    for frame in 0..20 {
        s.frame.index = frame;
        let report = r.render(target, &s, &plain()).unwrap();
        assert_eq!(report.frame, frame);
        for pass in [Pass::Opaque, Pass::Resolve] {
            let cpu = report.cpu.get(pass).unwrap_or_else(|| panic!("{} must run", pass.name()));
            assert!(cpu.0 > 0.0 && cpu.0 < 1.0, "{} recorded in {} s", pass.name(), cpu.0);
        }
        for pass in
            [Pass::PaintTextures, Pass::Shadows, Pass::Outlines, Pass::Transparent, Pass::Bloom, Pass::DepthOfField]
        {
            assert_eq!(report.cpu.get(pass), None, "{} has no work", pass.name());
        }
        assert_eq!((report.draw_calls, report.triangles), (2, 12), "one cube draw plus the resolve triangle");
        r.wait_idle().unwrap();
        last = Some(report);
    }
    let report = last.unwrap();
    if !r.capabilities().gpu_timings {
        // The WebGPU profile has no timestamps between passes (browsers expose none): the report says so honestly.
        assert!(report.gpu.is_none(), "no timestamps, so no GPU times");
        return;
    }
    let gpu = report.gpu.expect("after twenty frames and a wait, some GPU timings must have arrived");
    assert!(gpu.frame <= report.frame, "timings describe a frame that has been submitted");
    for pass in [Pass::Opaque, Pass::Resolve] {
        assert!(gpu.passes.get(pass).is_some_and(|t| t.0 > 0.0), "{} should have taken GPU time", pass.name());
    }
    assert_eq!(gpu.passes.get(Pass::Bloom), None);
    println!("GPU time per pass on {}: {:?}", r.capabilities().backend, gpu.passes.iter().collect::<Vec<_>>());
}

#[test]
fn gpu_timings_keep_following_the_frames_when_the_frame_index_goes_backwards() {
    let mut r = support::renderer();
    if !r.capabilities().gpu_timings {
        return; // The WebGPU profile has no timestamps between passes; there is nothing to follow.
    }
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.7)));

    // A loop or a scrub: the timeline runs to 1014 and jumps back to 0. Measurements are ranked by submission, not by
    // the frame index, so the newest one keeps being reported; its label is the frame it measured.
    let mut behind = Vec::new();
    for (submission, frame) in (0..).zip((1000..1015).chain(0..15)) {
        s.frame.index = frame;
        let report = r.render(target, &s, &plain()).unwrap();
        r.wait_idle().unwrap();
        // Give the jump a few frames to show; a report's label is the frame it measured, a frame or two earlier.
        if submission >= 15 + 3 {
            let gpu = report.gpu.expect("timings must keep arriving after the jump");
            behind.push(frame.checked_sub(gpu.frame));
        }
    }
    assert_eq!(behind.len(), 12);
    assert!(
        behind.iter().all(|b| b.is_some_and(|b| b <= 2)),
        "frames between each report and the timings it carries after the jump: {behind:?}"
    );
}

#[test]
fn painting_shows_up_as_a_pass_and_the_painters_cpu_time_is_charged_to_it() {
    let mut r = support::renderer();
    let texture = r.create_texture(&TextureDesc::colour(UVec2::new(256, 256)).dynamic(), None).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let s = scene(Colour::BLACK);
    r.update_texture(texture, PixelRect::full(UVec2::new(256, 256)), &vec![200; 256 * 256 * 4]).unwrap();
    let report = r.render(target, &s, &plain()).unwrap();
    assert!(report.cpu.get(Pass::PaintTextures).is_some_and(|t| t.0 > 0.0));
    assert_eq!(r.render(target, &s, &plain()).unwrap().cpu.get(Pass::PaintTextures), None);
}

// ---- painters ----

#[test]
fn gpu_painters_get_the_documented_native_tuple_and_their_work_is_ordered_before_the_next_frame() {
    let mut r = support::renderer();
    let size = UVec2::new(4, 4);
    let face = r.create_texture(&TextureDesc::colour(size).dynamic(), None).unwrap();
    let mut material = Material::unlit(Colour::BLACK);
    material.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));

    let mut downcast_worked = false;
    r.paint_texture(face, &mut |target: &mut dyn PaintTarget| {
        let Some((_device, _queue, texture, encoder)) = target.native().downcast_mut::<NativePaintTarget>() else {
            return target.write_rgba(PixelRect::full(size), &[0; 64]);
        };
        downcast_worked = true;
        // A GPU painter records into the renderer's encoder and never submits; clearing level 0 is the smallest
        // stand-in for its commands.
        clear_level_zero(texture, encoder, wgpu::Color { r: 1.0, g: 0.0, b: 1.0, a: 1.0 });
        Ok(())
    })
    .unwrap();
    assert!(downcast_worked, "native() must hand a GPU painter (Device, Queue, Texture, CommandEncoder)");
    let image = draw(&mut r, &s, &plain());
    assert_close(pixel(&image, 32, 32), [255, 0, 255, 255], 2, "the GPU-painted magenta");
}

/// Records a clear of `texture`'s level 0 to `colour` into `encoder`: a GPU painter's work in one pass.
fn clear_level_zero(texture: &wgpu::Texture, encoder: &mut wgpu::CommandEncoder, colour: wgpu::Color) {
    let level_zero =
        texture.create_view(&wgpu::TextureViewDescriptor { mip_level_count: Some(1), ..Default::default() });
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("a GPU painter"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &level_zero,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(colour), store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    }));
}

#[test]
fn a_gpu_painter_that_fails_has_its_commands_dropped_and_the_old_pixels_stay() {
    let mut r = support::renderer();
    let size = UVec2::new(4, 4);
    let face = r.create_texture(&TextureDesc::colour(size).dynamic(), None).unwrap();
    let mut material = Material::unlit(Colour::BLACK);
    material.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    r.update_texture(face, PixelRect::full(size), &[0, 0, 255, 255].repeat(16)).unwrap();

    let result = r.paint_texture(face, &mut |target: &mut dyn PaintTarget| {
        let Some((_, _, texture, encoder)) = target.native().downcast_mut::<NativePaintTarget>() else {
            return Ok(());
        };
        clear_level_zero(texture, encoder, wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        Err(RenderError::Backend("the painter gave up after recording".into()))
    });
    assert!(result.is_err());
    let image = draw(&mut r, &s, &plain());
    assert_close(pixel(&image, 32, 32), [0, 0, 255, 255], 2, "the recorded red was never submitted");
}

#[test]
fn a_gpu_painters_commands_are_timed_under_paint_textures_and_a_failed_ones_are_not() {
    let mut r = support::renderer();
    if !r.capabilities().gpu_timings {
        println!("skipped: the device has no timestamp queries inside encoders");
        return;
    }
    let size = UVec2::new(512, 512);
    let face = r.create_texture(&TextureDesc::colour(size).dynamic(), None).unwrap();
    let mut material = Material::unlit(Colour::BLACK);
    material.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let target = r.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let pixels = vec![128; 512 * 512 * 4];
    // Even frames: a GPU painter's 64 clears of the whole texture. Odd frames: a GPU painter that records clears and
    // then fails, and a `write_rgba` of the whole texture, so both frames rebuild the same mips. The failed paint's
    // timestamp pair is the one the even frame's paint wrote last: counted, it would make the odd frame as long.
    let (mut native, mut written) = (Vec::new(), Vec::new());
    for frame in 0..24_u64 {
        let painter_fails = frame % 2 == 1;
        let result = r.paint_texture(face, &mut |target: &mut dyn PaintTarget| {
            let Some((_, _, texture, encoder)) = target.native().downcast_mut::<NativePaintTarget>() else {
                return Err(RenderError::Backend("no native target".into()));
            };
            for clear in 0..64 {
                let shade = f64::from(clear % 2);
                clear_level_zero(texture, encoder, wgpu::Color { r: shade, g: 0.5, b: 0.0, a: 1.0 });
            }
            if painter_fails { Err(RenderError::Backend("the painter gave up".into())) } else { Ok(()) }
        });
        assert_eq!(result.is_err(), painter_fails);
        if painter_fails {
            r.update_texture(face, PixelRect::full(size), &pixels).unwrap();
        }
        s.frame.index = frame;
        let report = r.render(target, &s, &plain()).unwrap();
        r.wait_idle().unwrap();
        if let Some(timings) = report.gpu
            && let Some(t) = timings.passes.get(Pass::PaintTextures)
        {
            (if timings.frame % 2 == 0 { &mut native } else { &mut written }).push(t.0);
        }
    }
    // The median of each kind of frame: one frame's timing on a shared CI runner can be off by half. Counted, the
    // failed pair would make the odd frames at least as long as the even ones, so 1.5 times still tells the two apart.
    let median = |mut times: Vec<f64>| {
        times.sort_by(f64::total_cmp);
        times.get(times.len() / 2).copied()
    };
    let (native, written) =
        (median(native).expect("an even frame was timed"), median(written).expect("an odd frame was timed"));
    println!("PaintTextures: {native:.6} s with the GPU painter's clears, {written:.6} s with write_rgba (medians)");
    assert!(
        native > 1.5 * written,
        "the painter's 64 clears are timed ({native} s), and the failed painter's pair is not ({written} s for the mips)"
    );
}

#[test]
fn a_painter_that_fails_after_writing_leaves_the_old_pixels() {
    let mut r = support::renderer();
    let size = UVec2::new(2, 2);
    let face = r.create_texture(&TextureDesc::colour(size).dynamic(), None).unwrap();
    let mut material = Material::unlit(Colour::BLACK);
    material.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));

    r.update_texture(face, PixelRect::full(size), &[0, 0, 255, 255].repeat(4)).unwrap();
    let result = r.paint_texture(face, &mut |t: &mut dyn PaintTarget| {
        t.write_rgba(PixelRect::full(size), &[255, 0, 0, 255].repeat(4))?;
        Err(RenderError::Backend("gave up".into()))
    });
    assert!(matches!(result, Err(RenderError::Backend(_))));
    assert_close(pixel(&draw(&mut r, &s, &plain()), 32, 32), [0, 0, 255, 255], 1, "still blue");
}

#[test]
fn painting_an_empty_rectangle_is_not_an_error() {
    let mut r = support::renderer();
    let face = r.create_texture(&TextureDesc::colour(UVec2::splat(4)).dynamic(), None).unwrap();
    r.update_texture(face, PixelRect { x: 1, y: 1, width: 0, height: 2 }, &[]).unwrap();
    r.update_texture(face, PixelRect { x: 4, y: 4, width: 0, height: 0 }, &[]).unwrap();
}

// ---- setup ----

#[test]
fn a_renderer_can_be_built_on_a_device_the_caller_already_has() {
    // What the egui app does: it owns the adapter, device and queue, and hands them over.
    // The product's backends (`support::instance`). The CPU adapter is lavapipe on Linux and WARP on Windows.
    let instance = support::instance();
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let adapter = adapters
        .into_iter()
        .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
        .expect("a software adapter (lavapipe on Linux, WARP on Windows)");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

    let mut r = WgpuRenderer::from_device(&adapter, device, queue).unwrap();
    let name = adapter.get_info().name;
    assert!(r.capabilities().backend.contains(&name), "{} should name {name}", r.capabilities().backend);
    assert!(!r.capabilities().gpu_timings, "this device was created without timestamp features");
    let s = scene(Colour::rgb(1, 2, 3));
    let image = draw(&mut r, &s, &plain());
    assert_close(pixel(&image, 10, 10), [1, 2, 3, 255], 1, "a frame on a borrowed device");
}

#[test]
fn the_software_adapter_is_the_default_for_tests() {
    let r = WgpuRenderer::headless(&omuzaki_render::HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
    assert!(r.capabilities().backend.starts_with("wgpu "), "{}", r.capabilities().backend);
    assert!(r.capabilities().supports(Pass::Opaque) && r.capabilities().supports(Pass::Resolve));
    let listed = &r.capabilities().passes;
    assert!(listed.is_sorted(), "only implemented passes are listed, in frame order: {listed:?}");
    assert_eq!(r.capabilities().max_texture_size.min(4096), 4096, "at least the guaranteed size");
    assert!(!r.capabilities().bit_exact);
}
