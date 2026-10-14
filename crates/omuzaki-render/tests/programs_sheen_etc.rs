//! The `sheen`, `band-highlight` and `shadow-catcher` shading programs on the software adapter, against analytic predictions on
//! a 256 px sphere.
//!
//! The sphere is black under a hemisphere light with no rim and no tone curve, so the lit colour is exactly zero and what
//! the picture shows is the program's own contribution. The camera is orthographic and looks down -Z, so a pixel's
//! view-space normal is `(x, y, sqrt(1 - x^2 - y^2))` with `x`, `y` the pixel centre in sphere radii, and `V = +Z`.
//! `OMUZAKI_RENDER_OUT=<dir>` saves the pictures the render-sheet test makes. `OMUZAKI_PROGRAM_TIMING=1` prints the timing
//! comparison for a whole character (`cargo test ... timing -- --ignored --nocapture`).

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render::{WgpuRenderer, check_shading_program};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, Image, Instance, JointRange, Light, Material, MeshData,
    ParamValue, Projection, Quality, RenderError, Renderer, RendererExt, Scene, Shading, ShadowQuality, TargetDesc,
    Tonemap, Toon,
};

const SIZE: UVec2 = UVec2::new(256, 256);
/// The orthographic view's height in metres; the unit sphere then spans `256 / 1.25 / 2 * 2 = 204.8` pixels.
const VIEW_HEIGHT: f32 = 2.5;
/// Pixels per metre.
const SCALE: f32 = 256.0 / VIEW_HEIGHT;

/// One GPU test at a time in this binary: software Vulkan compiles pipelines on the calling thread and racing devices
/// crashed the driver once (see `toon.rs`).
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slug(name: &str) -> Slug {
    Slug::new(name).unwrap()
}

fn program(name: &str, params: &[(&str, ParamValue)]) -> Shading {
    Shading::Program { name: slug(name), params: params.iter().map(|(k, v)| (slug(k), *v)).collect() }
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let (w, h) = (image.size.x, image.size.y);
        image::save_buffer(PathBuf::from(dir).join(name), &image.rgba8, w, h, image::ColorType::Rgba8).unwrap();
    }
}

// ---- the sphere ----

/// A UV sphere with smooth normals.
fn sphere(radius: f32, segments: u32, rings: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let polar = std::f32::consts::PI * ring as f32 / rings as f32;
        for segment in 0..=segments {
            let azimuth = std::f32::consts::TAU * segment as f32 / segments as f32;
            let normal = Vec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
            mesh.positions.push(normal * radius);
            mesh.normals.push(normal);
        }
    }
    let row = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b) = (ring * row + segment, (ring + 1) * row + segment);
            mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

fn camera(eye: Vec3) -> Camera {
    Camera::look_at(eye, Vec3::ZERO, Vec3::Y, Projection::Orthographic { height: VIEW_HEIGHT, near: 0.1, far: 20.0 })
}

/// Exact colours: black sphere, a hemisphere light that makes the lit colour `albedo`, no tone curve.
fn exact_scene(eye: Vec3) -> Scene {
    let mut scene = Scene::new(camera(eye));
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: std::f32::consts::PI }];
    scene
}

fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// A black toon material with no rim, so the lit colour is zero and only the program shows.
fn black(shading: Shading) -> Material {
    let mut material = Material::toon(Colour::BLACK);
    material.shading = shading;
    material
}

fn render_sphere(renderer: &mut WgpuRenderer, material: &Material, eye: Vec3) -> Image {
    let mesh = renderer.create_mesh(&sphere(1.0, 192, 96)).unwrap();
    let id = renderer.create_material(material).unwrap();
    let mut scene = exact_scene(eye);
    scene.instances.push(Instance::new(mesh, id, Mat4::IDENTITY));
    let image = renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap();
    renderer.release(id.into()).unwrap();
    renderer.release(mesh.into()).unwrap();
    image
}

/// The view-space normal of the sphere at the centre of pixel `(x, y)` (a fractional `x` is allowed), or `None` outside it.
fn normal_at(x: f32, y: f32) -> Option<Vec3> {
    let (sx, sy) = ((x - 128.0) / SCALE, (128.0 - y) / SCALE);
    let z2 = 1.0 - sx * sx - sy * sy;
    (z2 > 0.0).then(|| Vec3::new(sx, sy, z2.sqrt()))
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn decode(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

// ---- the registry and the parameters ----

#[test]
fn the_renderer_lists_the_programs_and_checks_their_parameters_at_create_material() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let names: Vec<&str> = renderer.capabilities().shading_programs.iter().map(Slug::as_str).collect();
    for name in ["sheen", "band-highlight", "shadow-catcher"] {
        assert!(names.contains(&name), "{names:?}");
        assert!(check_shading_program(name).is_ok());
    }
    renderer.create_material(&black(program("sheen", &[]))).expect("every parameter has a default");
    renderer.create_material(&black(program("band-highlight", &[]))).expect("every parameter has a default");
    renderer.create_material(&black(program("shadow-catcher", &[]))).expect("every parameter has a default");

    let float = ParamValue::Float;
    let cases = [
        (program("sheen", &[("edge0", float(1.5))]), "outside 0..=1"),
        (program("sheen", &[("edge1", float(-0.1))]), "outside 0..=1"),
        (program("sheen", &[("colour", float(1.0))]), "is a colour, not the float"),
        (program("sheen", &[("colur", ParamValue::Colour(Colour::WHITE))]), "did you mean \"colour\""),
        (program("band-highlight", &[("strength", float(1.1))]), "outside 0..=1"),
        (program("shadow-catcher", &[("opacity", float(1.5))]), "outside 0..=1"),
        (program("shadow-catcher", &[("opacity", ParamValue::Bool(true))]), "is a float, not the bool"),
        (program("band-highlight", &[("dir", ParamValue::Vec3(Vec3::new(0.0, 2.0, 0.0)))]), "outside -1..=1"),
        (program("band-highlight", &[("dir", float(1.0))]), "is a vec3, not the float"),
    ];
    for (bad, needle) in cases {
        let Err(RenderError::Invalid { reason, .. }) = renderer.create_material(&black(bad.clone())) else {
            panic!("{bad:?} must be Invalid")
        };
        assert!(reason.contains(needle), "{reason}");
    }
}

#[test]
fn a_misspelt_program_still_draws_as_plain_toon() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let mut toon = black(Shading::Toon(Toon::default()));
    toon.base = Colour::hex(0x00c0_a080);
    let mut typo = toon.clone();
    typo.shading = program("sheeen", &[]);
    let eye = Vec3::new(0.0, 0.0, 5.0);
    assert_eq!(render_sphere(&mut renderer, &toon, eye).rgba8, render_sphere(&mut renderer, &typo, eye).rgba8);
}

// ---- sheen ----

#[test]
fn sheen_adds_exactly_its_colour_where_the_surface_faces_the_camera() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let colour = Colour::hex(0x0040_6080);
    let image =
        render_sphere(&mut renderer, &black(program("sheen", &[("colour", ParamValue::Colour(colour))])), camera_eye());
    save(&image, "sheen-sphere.png");
    for (x, y) in [(127, 127), (128, 128), (127, 128)] {
        let got = image.pixel(x, y).unwrap();
        for (got, want) in got.iter().zip([0x40_u8, 0x60, 0x80]) {
            assert!(got.abs_diff(want) <= 1, "pixel ({x}, {y}) is {got:?}, want about {want:#x}");
        }
    }
}

#[test]
fn sheen_follows_the_prototype_ramp_over_the_whole_sphere() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let colour = Colour::hex(0x00c0_a080);
    let params = [
        ("colour", ParamValue::Colour(colour)),
        ("edge0", ParamValue::Float(0.30)),
        ("edge1", ParamValue::Float(0.80)),
    ];
    let image = render_sphere(&mut renderer, &black(program("sheen", &params)), camera_eye());
    let linear = colour.to_linear().to_array();
    let (mut worst, mut at, mut checked) = (0.0_f32, (0, 0), 0_u32);
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            // Stay off the limb, where one pixel spans a large step in the normal.
            let Some(n) = normal_at(x as f32 + 0.5, y as f32 + 0.5).filter(|n| n.z > 0.5) else { continue };
            let ramp = smoothstep(0.30, 0.80, n.z);
            let got = image.pixel(x, y).unwrap();
            // Linear light, because 8-bit sRGB is so steep near black that one level is a large step in the ramp.
            let err = (0..3).map(|c| (decode(got[c]) - linear[c] * ramp).abs()).fold(0.0, f32::max);
            if err > worst {
                (worst, at) = (err, (x, y));
            }
            checked += 1;
        }
    }
    assert!(checked > 15_000, "{checked}");
    assert!(
        worst < 0.01,
        "the ramp differs from its analytic form by {worst} (linear) at {at:?} over {checked} pixels"
    );
}

fn camera_eye() -> Vec3 {
    Vec3::new(0.0, 0.0, 5.0)
}

// ---- band-highlight ----

/// The band's blend factor at pixel `(x, y)` of a render with a white highlight on black: the mix weight, i.e.
/// `smoothstep * strength`.
fn factor(image: &Image, x: u32, y: u32) -> f32 {
    decode(image.pixel(x, y).unwrap()[0])
}

#[test]
fn the_band_is_centred_where_dot_n_dir_is_088() {
    let _serial = serial();
    let mut renderer = support::renderer();
    // A direction along the x axis of the screen, so a pixel row crosses the band and the prediction is one-dimensional.
    let dir = Vec3::new(0.6, 0.0, 0.8);
    let params = [("dir", ParamValue::Vec3(dir)), ("colour", ParamValue::Colour(Colour::WHITE))];
    let image = render_sphere(&mut renderer, &black(program("band-highlight", &params)), camera_eye());
    save(&image, "band-highlight-sphere.png");

    // The row of pixel centres just above the equator. The band's middle is where the weight is half its maximum.
    let row = 127_u32;
    let y = row as f32 + 0.5;
    let dot_at = |x: f32| normal_at(x, y).map_or(f32::NEG_INFINITY, |n| n.dot(dir));
    // Analytic: dot(N, dir) = 0.88 on each side of the peak (bisection on the monotone halves).
    let peak = 128.0 + 0.6 * SCALE;
    let root = |mut inside: f32, mut outside: f32| {
        for _ in 0..60 {
            let mid = f32::midpoint(inside, outside);
            if dot_at(mid) > 0.88 { inside = mid } else { outside = mid }
        }
        inside
    };
    let want = [root(peak, 128.0), root(peak, 128.0 + SCALE * 0.99)];

    // Measured: the sub-pixel position where the weight crosses half of its strength (0.6 / 2), rising then falling.
    let half = 0.6 / 2.0;
    let mut crossings = Vec::new();
    for x in 0..SIZE.x - 1 {
        let (a, b) = (factor(&image, x, row), factor(&image, x + 1, row));
        if (a - half) * (b - half) < 0.0 {
            crossings.push(x as f32 + 0.5 + (half - a) / (b - a));
        }
    }
    assert_eq!(crossings.len(), 2, "the row crosses the band twice: {crossings:?}");
    for (got, want) in crossings.iter().zip(want) {
        assert!((got - want).abs() <= 1.0, "band edge at x = {got}, analytic {want}");
    }
    // At full strength the colour mixes in by `strength` = 0.6 at the peak.
    let top = factor(&image, peak.round() as u32, row);
    assert!((top - 0.6).abs() < 0.02, "peak weight {top}");
}

#[test]
fn the_band_rides_with_the_camera_not_the_world() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let params = [("colour", ParamValue::Colour(Colour::WHITE))];
    let material = black(program("band-highlight", &params));
    let front = render_sphere(&mut renderer, &material, camera_eye());
    // A camera on the +X axis sees the same sphere from another side. A sphere looks the same from everywhere, so a
    // highlight fixed in the world would have moved and one fixed in view space has not.
    let side = render_sphere(&mut renderer, &material, Vec3::new(5.0, 0.0, 0.0));
    let differ = front.rgba8.chunks(4).zip(side.rgba8.chunks(4)).filter(|(a, b)| a[0].abs_diff(b[0]) > 12).count();
    assert!(differ < 300, "{differ} pixels differ between two camera positions");
    assert!(front.rgba8.chunks(4).filter(|p| p[0] > 40).count() > 200, "the highlight is visible");
}

#[test]
fn the_default_band_sits_up_and_left_of_the_eye() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let params = [("colour", ParamValue::Colour(Colour::WHITE))];
    let image = render_sphere(&mut renderer, &black(program("band-highlight", &params)), camera_eye());
    // The plateau (weight at its maximum, `dot >= 0.90`) against the same set predicted analytically.
    let dir = Vec3::new(-0.32, 0.58, 0.75).normalize();
    let (mut seen, mut want) = (((0.0_f32, 0.0_f32), 0.0_f32), ((0.0_f32, 0.0_f32), 0.0_f32));
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            if factor(&image, x, y) > 0.59 {
                seen = (((seen.0).0 + x as f32, (seen.0).1 + y as f32), seen.1 + 1.0);
            }
            if normal_at(x as f32 + 0.5, y as f32 + 0.5).is_some_and(|n| n.dot(dir) >= 0.90) {
                want = (((want.0).0 + x as f32, (want.0).1 + y as f32), want.1 + 1.0);
            }
        }
    }
    assert!(seen.1 > 100.0 && want.1 > 100.0, "plateau sizes {} and {}", seen.1, want.1);
    let centre = |s: ((f32, f32), f32)| ((s.0).0 / s.1, (s.0).1 / s.1);
    let (a, b) = (centre(seen), centre(want));
    assert!((a.0 - b.0).abs() <= 1.0 && (a.1 - b.1).abs() <= 1.0, "plateau centre {a:?}, analytic {b:?}");
    assert!(a.0 < 128.0 && a.1 < 128.0, "up and to the left of the eye: {a:?}");
    assert!(seen.1 / want.1 > 0.95 && seen.1 / want.1 < 1.05, "plateau area {} vs {}", seen.1, want.1);
}

// ---- shadow-catcher ----

/// A flat disc in the y = 0 plane facing up.
fn disc(radius: f32, segments: u32) -> MeshData {
    let mut mesh = MeshData::default();
    mesh.positions.push(Vec3::ZERO);
    for i in 0..=segments {
        let a = std::f32::consts::TAU * i as f32 / segments as f32;
        mesh.positions.push(Vec3::new(a.cos() * radius, 0.0, a.sin() * radius));
    }
    mesh.normals = vec![Vec3::Y; mesh.positions.len()];
    for i in 1..=segments {
        mesh.indices.extend([0, i + 1, i]);
    }
    mesh
}

const GROUND: Colour = Colour::rgb(0x80, 0x80, 0x80);

/// The stage's ground disc (radius 0.8) under a small sphere 0.3 m up, lit from 45 degrees toward +X so the shadow falls
/// at x = -0.3. `catcher` draws the disc with the program (blended, receiving, not casting); a caster-less scene
/// (`casting = false`) has no shadow at all. The camera looks straight down: +X is right and +Z is down the picture.
fn catcher_world(renderer: &mut WgpuRenderer, catcher: Option<f32>, casting: bool, eye: Vec3, up: Vec3) -> Scene {
    let ball = renderer.create_mesh(&sphere(0.15, 48, 24)).unwrap();
    let ground = renderer.create_mesh(&disc(0.8, 48)).unwrap();
    let mut lit = Material::toon(Colour::hex(0x00c0_5040));
    lit.double_sided = true;
    let ball_material = renderer.create_material(&lit).unwrap();
    let mut scene = Scene::new(Camera::look_at(
        eye,
        Vec3::ZERO,
        up,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: GROUND };
    scene.post.tonemap = Tonemap::None;
    scene.lights = vec![Light::Directional {
        to_light: Vec3::new(1.0, 1.0, 0.0).normalize(),
        colour: Colour::WHITE,
        intensity: 2.0,
        casts_shadow: true,
    }];
    let mut ball_instance = Instance::new(ball, ball_material, Mat4::from_translation(Vec3::new(0.0, 0.3, 0.0)));
    ball_instance.casts_shadow = casting;
    scene.instances.push(ball_instance);
    if let Some(opacity) = catcher {
        let mut material = black(program("shadow-catcher", &[("opacity", ParamValue::Float(opacity))]));
        material.alpha = AlphaMode::Blend;
        material.double_sided = true;
        let id = renderer.create_material(&material).unwrap();
        let mut instance = Instance::new(ground, id, Mat4::IDENTITY);
        // The shadow map is fitted to the casters: a casting ground would widen the fit.
        instance.casts_shadow = false;
        scene.instances.push(instance);
    }
    scene
}

fn with_shadows(shadows: ShadowQuality) -> Quality {
    Quality { shadows, ..plain() }
}

fn catcher_scene(renderer: &mut WgpuRenderer, catcher: Option<f32>, casting: bool, eye: Vec3, up: Vec3) -> Image {
    let scene = catcher_world(renderer, catcher, casting, eye, up);
    renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &with_shadows(ShadowQuality::High)).unwrap()
}

/// Looking straight down with +X right: the pixel of world `(x, z)` on the ground.
fn ground_pixel(x: f32, z: f32) -> (u32, u32) {
    ((128.0 + x * 128.0) as u32, (128.0 + z * 128.0) as u32)
}

fn top_down() -> (Vec3, Vec3) {
    (Vec3::new(0.0, 5.0, 0.0), -Vec3::Z)
}

#[test]
fn the_catcher_shows_nothing_unshadowed_and_a_quarter_black_in_full_shadow() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let (eye, up) = top_down();
    let with = catcher_scene(&mut renderer, Some(0.25), true, eye, up);
    // The same scene with no disc at all, and with a disc but a ball that casts nothing: both show the bare background
    // and the ball, so the catcher draws nothing where nothing is shadowed.
    let without = catcher_scene(&mut renderer, None, true, eye, up);
    let unshadowed = catcher_scene(&mut renderer, Some(0.25), false, eye, up);
    let bare = catcher_scene(&mut renderer, None, false, eye, up);
    save(&with, "shadow-catcher-top-down.png");
    assert_eq!(unshadowed.rgba8, bare.rgba8, "with no shadow the catcher changes no pixel");

    // Full shadow at the middle of the shadow (x = -0.3, the ball's width of 0.3 m is all inside it).
    let (x, y) = ground_pixel(-0.3, 0.0);
    let (got, background) = (with.pixel(x, y).unwrap(), without.pixel(x, y).unwrap());
    assert_eq!(background[..3], [0x80, 0x80, 0x80], "the background there is the plain backdrop");
    for (c, byte) in got.iter().take(3).enumerate() {
        let want = decode(0x80) * 0.75;
        let have = decode(*byte);
        assert!((have - want).abs() < 0.004, "channel {c}: {have} linear, want {want} (25% black over the grey)");
    }
    // A lit pixel of the disc, far from the shadow, is the backdrop untouched.
    let (x, y) = ground_pixel(0.5, 0.3);
    assert_eq!(with.pixel(x, y), without.pixel(x, y));
}

/// The shadow map keeps its last matrix and texture when sampling is switched off, so a catcher that ignored the switch
/// would keep drawing the old shadow on a reused target.
#[test]
fn the_catcher_forgets_the_shadow_when_shadows_go_off_or_the_caster_goes() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let (eye, up) = top_down();
    let mut scene = catcher_world(&mut renderer, Some(0.25), true, eye, up);
    let target = renderer.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let (x, y) = ground_pixel(-0.3, 0.0);
    let shown = |renderer: &mut WgpuRenderer, scene: &Scene, shadows| {
        renderer.render(target, scene, &with_shadows(shadows)).unwrap();
        renderer.read_target(target).unwrap().pixel(x, y).unwrap()
    };
    let shadowed = shown(&mut renderer, &scene, ShadowQuality::High);
    assert!(decode(shadowed[0]) < decode(0x80) * 0.8, "the shadow shows first: {shadowed:?}");
    let backdrop = [0x80, 0x80, 0x80, 0xff];
    assert_eq!(shown(&mut renderer, &scene, ShadowQuality::Off), backdrop, "shadows off: no stale shadow");
    assert_eq!(shown(&mut renderer, &scene, ShadowQuality::High), shadowed, "and it comes back when they are on");
    scene.instances[0].casts_shadow = false;
    assert_eq!(shown(&mut renderer, &scene, ShadowQuality::High), backdrop, "caster removed: no stale shadow");
}

#[test]
fn the_catchers_opacity_scales_the_shadow() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let (eye, up) = top_down();
    let (x, y) = ground_pixel(-0.3, 0.0);
    for opacity in [0.0_f32, 0.5, 1.0] {
        let image = catcher_scene(&mut renderer, Some(opacity), true, eye, up);
        let have = decode(image.pixel(x, y).unwrap()[0]);
        let want = decode(0x80) * (1.0 - opacity);
        assert!((have - want).abs() < 0.004, "opacity {opacity}: {have}, want {want}");
    }
}

// ---- the render sheet and the timing, on the mock character ----

const EYE: Vec3 = Vec3::new(0.0, 1.45, 1.6);
const TARGET: Vec3 = Vec3::new(0.0, 0.8, 0.0);
const CHARACTER_SIZE: UVec2 = UVec2::new(256, 256);

/// A lit sphere on grey with the program, for the render sheet.
fn lit_sphere(renderer: &mut WgpuRenderer, shading: Shading) -> Image {
    let mesh = renderer.create_mesh(&sphere(1.0, 96, 48)).unwrap();
    let mut material = Material::toon(Colour::hex(0x00a0_5a50));
    material.shading = shading;
    let id = renderer.create_material(&material).unwrap();
    let mut scene = lit_scene(camera(camera_eye()));
    scene.instances.push(Instance::new(mesh, id, Mat4::IDENTITY));
    renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap()
}

fn lit_scene(camera: Camera) -> Scene {
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::hex(0x0086_8b99) };
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
        Light::Directional {
            to_light: Vec3::new(0.4, 0.8, 0.6).normalize(),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.0,
            casts_shadow: false,
        },
    ];
    scene.post.tonemap = Tonemap::None;
    scene
}

/// The mock character with `shading` on every material but the hair (the hair cap has no strand attributes).
struct Character {
    renderer: WgpuRenderer,
    scene: Scene,
}

fn character(shading: Option<&Shading>) -> Character {
    let mut renderer = support::renderer();
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut scene = lit_scene(Camera::look_at(
        EYE,
        TARGET,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    ));
    let mut materials = BTreeMap::new();
    for part in &built.meshes {
        let name = part.mesh.material.clone();
        if !materials.contains_key(&name) {
            let mut spec = built.materials[&name].clone();
            spec.overlays.clear();
            spec.base_texture = None;
            spec.outline = None;
            if let (Some(shading), false) = (shading, name.as_str() == "mock-hair") {
                spec.shading = shading.clone();
            }
            let material = spec.to_material(&|_| None, &|_| None).unwrap();
            materials.insert(name.clone(), renderer.create_material(&material).unwrap());
        }
        let mesh = renderer.create_mesh(&part.mesh.geometry).unwrap();
        let mut instance = Instance::new(mesh, materials[&name], Mat4::IDENTITY);
        if !part.mesh.joint_bones.is_empty() {
            let start = u32::try_from(scene.joints.len()).unwrap();
            let len = u32::try_from(part.mesh.joint_bones.len()).unwrap();
            scene.joints.extend(std::iter::repeat_n(Mat4::IDENTITY, part.mesh.joint_bones.len()));
            instance.skin = Some(JointRange { start, len });
        }
        scene.instances.push(instance);
    }
    Character { renderer, scene }
}

impl Character {
    fn render(&mut self) -> Image {
        self.renderer.render_image(CHARACTER_SIZE, AlphaEncoding::Straight, &self.scene, &plain()).unwrap()
    }
}

/// A rim-free toon in the character's materials would hide the highlight's effect on the shade; the sheet uses the
/// materials as the kit built them.
fn sheet_programs() -> [(&'static str, Option<Shading>); 3] {
    [
        ("toon", None),
        ("sheen", Some(program("sheen", &[("colour", ParamValue::Colour(Colour::hex(0x0080_6458)))]))),
        ("band-highlight", Some(program("band-highlight", &[]))),
    ]
}

#[test]
fn the_programs_draw_on_a_sphere_and_on_the_mock_character() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let mut plain_character = None;
    for (name, shading) in sheet_programs() {
        let sphere = lit_sphere(&mut renderer, shading.clone().unwrap_or_else(|| Shading::Toon(Toon::default())));
        let figure = character(shading.as_ref()).render();
        save(&sphere, &format!("sphere-{name}.png"));
        save(&figure, &format!("character-{name}.png"));
        match &plain_character {
            None => plain_character = Some(figure),
            Some(base) => assert_ne!(base.rgba8, figure.rgba8, "{name} must change the character"),
        }
    }
}

#[test]
fn the_shadow_catcher_row_of_the_sheet() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let oblique = (Vec3::new(1.6, 2.2, 2.4), Vec3::Y);
    let shadowed = catcher_scene(&mut renderer, Some(0.25), true, oblique.0, oblique.1);
    let bare = catcher_scene(&mut renderer, None, true, oblique.0, oblique.1);
    save(&shadowed, "shadow-catcher-ground-disc.png");
    save(&bare, "shadow-catcher-no-disc.png");
    assert_ne!(shadowed.rgba8, bare.rgba8, "the shadow shows on the ground disc");
}

#[test]
#[ignore = "timing; run by hand with --ignored --nocapture"]
fn timing_of_a_character_with_and_without_the_programs() {
    let _serial = serial();
    let mut baseline = character(None);
    let mut both = character(Some(&program("sheen", &[])));
    let mut band = character(Some(&program("band-highlight", &[])));
    let median = |c: &mut Character| {
        let mut times: Vec<f64> = (0..40)
            .map(|_| {
                let start = Instant::now();
                c.render();
                start.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    };
    let _ = (median(&mut baseline), median(&mut both), median(&mut band));
    let (a, b, c) = (median(&mut baseline), median(&mut both), median(&mut band));
    println!("median ms per frame (render + readback, lavapipe): toon {a:.3}, sheen {b:.3}, band-highlight {c:.3}");
    println!("delta: sheen {:+.3} ms, band-highlight {:+.3} ms", b - a, c - a);
}
