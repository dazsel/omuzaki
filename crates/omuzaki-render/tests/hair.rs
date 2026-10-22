//! The `hair` shading program on the software adapter: the registry as the renderer exposes it, the hair
//! look against an independent port of the prototype's GLSL (`parts/hair.js`), and a golden of the mock character's hair.
//!
//! The oracle scenes are one front-facing quad under a hemisphere light of intensity PI with no tone curve, so the lit
//! colour is exactly the albedo (`albedo * PI / PI`, no terminator, no rim at `N.V = 1`) and what remains is the
//! program's own maths: the strand colour from the `hair-uv` and `hair-info` attributes, then the angel ring.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_char_api::{
    BuildOptions, BuildServices, CharId, CharacterBuilder, HAIR_INFO, HAIR_PROGRAM, HAIR_UV, HairShading, MaterialName,
    RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render::{WgpuRenderer, check_shading_program};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, ExtraAttribute, Image, Instance, JointRange, Light, Material, MeshData,
    ParamValue, Projection, Quality, RenderError, Renderer, RendererExt, Scene, Shading, Tonemap, Toon,
};

const SIZE: UVec2 = UVec2::new(128, 128);

/// One GPU test at a time in this binary: software Vulkan compiles pipelines on the calling thread and racing devices
/// crashed the driver once (see `toon.rs`).
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slug(name: &str) -> Slug {
    Slug::new(name).unwrap()
}

fn hair(params: &[(&str, ParamValue)]) -> Shading {
    Shading::Program { name: slug(HAIR_PROGRAM), params: params.iter().map(|(k, v)| (slug(k), *v)).collect() }
}

// ---- the registry, as the renderer shows it ----

#[test]
fn the_renderer_lists_the_hair_program() {
    let _serial = serial();
    let renderer = support::renderer();
    let names: Vec<&str> = renderer.capabilities().shading_programs.iter().map(Slug::as_str).collect();
    assert!(names.contains(&HAIR_PROGRAM), "{names:?}");
}

#[test]
fn an_unknown_program_name_gets_a_did_you_mean_but_still_draws_as_toon() {
    let _serial = serial();
    let error = check_shading_program("hiar").unwrap_err();
    assert_eq!(error.suggestions, ["hair"]);
    assert!(error.to_string().contains("did you mean \"hair\""), "{error}");

    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&quad(false)).unwrap();
    let toon = Material::toon(Colour::hex(0x00c0_a080));
    let mut typo = toon.clone();
    typo.shading = hair_named("hiar", &[]);
    let plain = render_quad(&mut renderer, mesh, &toon);
    let fallback = render_quad(&mut renderer, mesh, &typo);
    assert_eq!(plain.rgba8, fallback.rgba8, "an unknown program draws exactly as the default toon (contract rule 5)");
}

fn hair_named(name: &str, params: &[(&str, ParamValue)]) -> Shading {
    Shading::Program { name: slug(name), params: params.iter().map(|(k, v)| (slug(k), *v)).collect() }
}

#[test]
fn parameters_are_checked_against_the_declaration_when_a_material_is_made_or_updated() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let material = |shading| Material { shading, ..Material::toon(Colour::WHITE) };
    renderer.create_material(&material(hair(&[]))).expect("every parameter has a default");
    let id = renderer.create_material(&material(hair(&[("ring-strength", ParamValue::Float(0.5))]))).unwrap();

    for (bad, needle) in [
        (hair(&[("ring-strenght", ParamValue::Float(0.5))]), "did you mean \"ring-strength\""),
        (hair(&[("ring-strength", ParamValue::Bool(true))]), "is a float, not the bool"),
        (hair(&[("ring-strength", ParamValue::Float(2.0))]), "outside 0..=1"),
        (hair(&[("root", ParamValue::Float(1.0))]), "is a colour, not the float"),
    ] {
        let Err(RenderError::Invalid { reason, .. }) = renderer.create_material(&material(bad.clone())) else {
            panic!("{bad:?} must be Invalid")
        };
        assert!(reason.contains(needle), "{reason}");
        assert!(matches!(renderer.update_material(id, &material(bad)), Err(RenderError::Invalid { .. })));
    }
}

// ---- the look, against the prototype's maths ----

/// A quad filling most of the view, `across` in `-1..=1` along x and `along` in `0..=1` up y, with `polar` rising
/// `0.5..=1.3` up the quad so the angel ring (at 0.92) crosses it. Without `attributes` the mesh has no extras.
fn quad(attributes: bool) -> MeshData {
    let corners = [(-1.0_f32, 0.0_f32), (1.0, 0.0), (1.0, 1.0), (-1.0, 1.0)];
    let mut mesh = MeshData::default();
    let (mut uv, mut info) = (Vec::new(), Vec::new());
    for (across, along) in corners {
        mesh.positions.push(Vec3::new(across * HALF, (along * 2.0 - 1.0) * HALF, 0.0));
        mesh.normals.push(Vec3::Z);
        uv.extend([across, along]);
        info.extend([SEED, SHADE, 0.5 + 0.8 * along]);
    }
    mesh.indices = vec![0, 1, 2, 0, 2, 3];
    if attributes {
        mesh.extra = vec![
            ExtraAttribute { name: slug(HAIR_UV), components: 2, data: uv },
            ExtraAttribute { name: slug(HAIR_INFO), components: 3, data: info },
        ];
    }
    mesh
}

const HALF: f32 = 0.9;
const SEED: f32 = 0.37;
const SHADE: f32 = 0.9;

fn ortho() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// Exact colours: a hemisphere light of PI makes the lit colour the albedo, and there is no tone curve.
fn exact_scene() -> Scene {
    let mut scene = Scene::new(ortho());
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: std::f32::consts::PI }];
    scene
}

fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() }
}

fn render_quad(renderer: &mut WgpuRenderer, mesh: omuzaki_render_api::MeshId, material: &Material) -> Image {
    let material = renderer.create_material(material).unwrap();
    let mut scene = exact_scene();
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let image = renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap();
    renderer.release(material.into()).unwrap();
    image
}

fn hair_material(shading: Shading) -> Material {
    Material { base: Colour::WHITE, shading, double_sided: true, ..Material::toon(Colour::WHITE) }
}

/// The prototype's `hair.js` GLSL, line for line, in f32 (the hash multiplies `sin` by 43758, so evaluating it in f64
/// would disagree with any f32 GPU by design). Returns the sRGB bytes of the lit colour of a front-facing surface whose
/// normal faces the camera.
struct Prototype {
    root: Vec3,
    mid: Vec3,
    tip: Vec3,
    line: Vec3,
    ring: Vec3,
    ring_strength: f32,
    strand_lines: f32,
    edge_darkening: f32,
    keep_identity: f32,
}

impl Prototype {
    /// The default `HairShading` of the character kit: the parameters a hair material gets when it sets none.
    fn defaults() -> Self {
        let colour = |hex| Colour::hex(hex).to_linear();
        Self {
            root: colour(0x00c8_b09a),
            mid: colour(0x00e6_d2be),
            tip: colour(0x00dc_c4b0),
            line: colour(0x008c_685c),
            ring: colour(0x00ff_f4e8),
            ring_strength: 0.22,
            strand_lines: 0.10,
            edge_darkening: 0.14,
            keep_identity: 0.36,
        }
    }

    fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn hh(n: f32) -> f32 {
        // The prototype's constant, as written there; f32 rounds it.
        #[allow(clippy::excessive_precision)]
        let v = n.sin() * 43758.5453;
        v - v.floor()
    }

    fn hn(x: f32) -> f32 {
        let i = x.floor();
        let f = x - i;
        let f = f * f * (3.0 - 2.0 * f);
        Self::hh(i) * (1.0 - f) + Self::hh(i + 1.0) * f
    }

    /// `fragDiffuse` then the lighting at `N.V = 1`, then `fragOut`.
    fn lit(&self, u: f32, v: f32, seed: f32, shade: f32, polar: f32) -> Vec3 {
        let ss = Self::smoothstep;
        let mut col = self.root.lerp(self.mid, ss(0.0, 0.25, v));
        col = col.lerp(self.tip, ss(0.55, 1.0, v));
        let w = u * 2.6 + seed * 11.0 + 0.35 * (v * 14.0 + seed * 20.0).sin();
        let (s1, s2) = (Self::hn(w * 3.0), Self::hn(w * 7.0 + 3.0));
        let lines = ss(0.62, 0.92, s1) * 0.6 + ss(0.7, 0.95, s2) * 0.4;
        col = col.lerp(self.line, lines * self.strand_lines);
        col *= 1.03 + (0.97 - 1.03) * s2;
        col *= 1.0 + ((1.0 - self.edge_darkening) - 1.0) * ss(0.55, 1.0, u.abs());
        col *= shade;
        let band = ss(0.11, 0.0, (polar - 0.92 - 0.06 * (u * 3.0 + seed * 9.0).sin()).abs());
        let teeth = ss(0.35, 0.75, Self::hn(u * 6.0 + seed * 17.0));
        let out = col + self.ring * band * teeth * 1.0 * self.ring_strength;
        let luma = out.dot(Vec3::new(0.299, 0.587, 0.114));
        out.lerp(luma * Vec3::new(1.05, 0.97, 0.90), self.keep_identity)
    }
}

fn encode(linear: Vec3) -> [u8; 3] {
    let channel = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    [channel(linear.x), channel(linear.y), channel(linear.z)]
}

/// The attributes at the centre of pixel `(x, y)`: the quad's `across`, `along` and `polar`, or `None` outside it.
fn attributes_at(x: u32, y: u32) -> Option<(f32, f32, f32)> {
    let world = Vec2::new((x as f32 + 0.5 - 64.0) / 64.0, (64.0 - y as f32 - 0.5) / 64.0);
    let (across, along) = (world.x / HALF, f32::midpoint(world.y / HALF, 1.0));
    ((-1.0..=1.0).contains(&across) && (0.0..=1.0).contains(&along)).then_some((across, along, 0.5 + 0.8 * along))
}

/// The oracle's picture of the quad: what the prototype's shader would show, black outside the quad.
fn oracle_image(oracle: &Prototype) -> Image {
    let mut rgba8 = Vec::with_capacity((SIZE.x * SIZE.y * 4) as usize);
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let rgb = attributes_at(x, y)
                .map_or([0, 0, 0], |(across, along, polar)| encode(oracle.lit(across, along, SEED, SHADE, polar)));
            rgba8.extend([rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    Image::new(SIZE, AlphaEncoding::Straight, rgba8).unwrap()
}

/// Whether pixel `(x, y)` is inside the quad, away from its edges (an edge pixel is half covered).
fn interior(x: u32, y: u32) -> Option<(f32, f32, f32)> {
    attributes_at(x, y).filter(|(across, along, _)| across.abs() <= 0.97 && (0.03..=0.97).contains(along))
}

/// CIE L*a*b* of an sRGB byte triple (D65).
fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
    let xyz = [
        (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47,
        0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b,
        (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83,
    ];
    let f = |t: f64| if t > 216.0 / 24_389.0 { t.cbrt() } else { (24_389.0 / 27.0 * t + 16.0) / 116.0 };
    let [fx, fy, fz] = xyz.map(f);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn delta_e(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn luma(rgb: [u8; 3]) -> f32 {
    0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2])
}

/// How far a render is from the oracle over the interior of the quad.
struct Parity {
    pixels: usize,
    max: u8,
    mean: f32,
    /// The 99th percentile of the per-channel differences.
    p99: u8,
    /// CIE76 over the pixels inside `crop` rows: mean and worst.
    delta_e: (f64, f64),
}

fn compare(image: &Image, oracle: &Prototype, crop: std::ops::RangeInclusive<u32>) -> Parity {
    let (mut diffs, mut des) = (Vec::new(), Vec::new());
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let Some((across, along, polar)) = interior(x, y) else { continue };
            let want = encode(oracle.lit(across, along, SEED, SHADE, polar));
            let got = image.pixel(x, y).unwrap();
            diffs.extend(want.iter().zip(got).map(|(w, g)| w.abs_diff(g)));
            if crop.contains(&y) {
                des.push(delta_e(want, [got[0], got[1], got[2]]));
            }
        }
    }
    diffs.sort_unstable();
    Parity {
        pixels: diffs.len() / 3,
        max: diffs.last().copied().unwrap_or(0),
        mean: diffs.iter().map(|&d| f32::from(d)).sum::<f32>() / diffs.len().max(1) as f32,
        p99: diffs[diffs.len() * 99 / 100],
        delta_e: (des.iter().sum::<f64>() / des.len().max(1) as f64, des.iter().copied().fold(0.0, f64::max)),
    }
}

/// The ring's rise in luma, row by row (mean over the quad's columns), between a picture with the ring and one without:
/// the row of the peak and the full width at half maximum, in pixels.
fn ring_band(with: &Image, without: &Image) -> (u32, u32, f32) {
    let rows: Vec<f32> = (0..SIZE.y)
        .map(|y| {
            let diffs: Vec<f32> = (0..SIZE.x)
                .filter(|&x| interior(x, y).is_some())
                .map(|x| luma(rgb_of(with, x, y)) - luma(rgb_of(without, x, y)))
                .collect();
            diffs.iter().sum::<f32>() / diffs.len().max(1) as f32
        })
        .collect();
    let peak = rows.iter().copied().fold(0.0, f32::max);
    let centre = rows.iter().position(|&r| (r - peak).abs() < f32::EPSILON).unwrap_or(0) as u32;
    let wide = rows.iter().filter(|&&r| r >= peak / 2.0).count() as u32;
    (centre, wide, peak)
}

fn rgb_of(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let [red, green, blue, _] = image.pixel(x, y).unwrap();
    [red, green, blue]
}

#[test]
fn the_hair_program_matches_the_prototypes_shader_maths() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&quad(true)).unwrap();
    let image = render_quad(&mut renderer, mesh, &hair_material(hair(&[])));
    let oracle = Prototype::defaults();
    save(&image, "hair-oracle-after.png");
    save(&oracle_image(&oracle), "hair-oracle-reference.png");
    save(&render_quad(&mut renderer, mesh, &hair_material(hair_named("unregistered", &[]))), "hair-oracle-before.png");

    // The angel ring is at polar 0.92 of the quad's 0.5..1.3: where is its band, and how wide, in the oracle and on the GPU?
    let no_ring = hair(&[("ring-strength", ParamValue::Float(0.0))]);
    let gpu_without = render_quad(&mut renderer, mesh, &hair_material(no_ring));
    let reference = Prototype { ring_strength: 0.0, ..Prototype::defaults() };
    let gpu_band = ring_band(&image, &gpu_without);
    let reference_band = ring_band(&oracle_image(&oracle), &oracle_image(&reference));
    let crop = gpu_band.0.saturating_sub(gpu_band.1)..=gpu_band.0 + gpu_band.1;
    let parity = compare(&image, &oracle, crop.clone());
    println!(
        "hair vs prototype oracle: {} px, per-channel max {}, mean {:.3}, p99 {}; ring band rows {crop:?}: dE76 mean {:.3} max {:.3}",
        parity.pixels, parity.max, parity.mean, parity.p99, parity.delta_e.0, parity.delta_e.1
    );
    println!("ring band (row of peak, FWHM px, peak luma rise): oracle {reference_band:?}, GPU {gpu_band:?}");
    assert!(parity.pixels > 12_000, "the quad covers most of the view: {}", parity.pixels);
    assert!(
        parity.mean < 0.2,
        "mean difference {:.3} levels (p99 {}, max {}, over {} px)",
        parity.mean,
        parity.p99,
        parity.max,
        parity.pixels
    );
    assert!(parity.p99 <= 1, "99th percentile {} levels", parity.p99);
    assert!(parity.max <= 2, "worst difference {} levels", parity.max);
    assert!(parity.delta_e.1 < 1.0, "a colour difference of {:.2} dE76 in the ring band is visible", parity.delta_e.1);
    assert_eq!(
        (gpu_band.0, gpu_band.1),
        (reference_band.0, reference_band.1),
        "the ring sits where the prototype's does"
    );
    assert!((gpu_band.2 - reference_band.2).abs() < 0.5, "the ring is as bright as the prototype's");
}

#[test]
fn every_parameter_reaches_the_shader() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&quad(true)).unwrap();
    let base = render_quad(&mut renderer, mesh, &hair_material(hair(&[])));
    let colour = |hex| ParamValue::Colour(Colour::hex(hex));
    let mut moved = |what: &str, params: &[(&str, ParamValue)]| {
        let image = render_quad(&mut renderer, mesh, &hair_material(hair(params)));
        assert_ne!(image.rgba8, base.rgba8, "setting {what} changed nothing");
    };
    moved("root", &[("root", colour(0x0020_4080))]);
    moved("mid", &[("mid", colour(0x0020_4080))]);
    moved("tip", &[("tip", colour(0x0020_4080))]);
    moved("line", &[("line", colour(0x0000_00ff)), ("strand-lines", ParamValue::Float(1.0))]);
    moved("ring", &[("ring", colour(0x0000_ff00)), ("ring-strength", ParamValue::Float(1.0))]);
    moved("ring-strength", &[("ring-strength", ParamValue::Float(1.0))]);
    moved("strand-lines", &[("strand-lines", ParamValue::Float(1.0))]);
    moved("edge-darkening", &[("edge-darkening", ParamValue::Float(0.9))]);
    moved("keep-identity", &[("keep-identity", ParamValue::Float(1.0))]);
}

#[test]
fn a_mesh_without_the_hair_attributes_draws_with_zeros_not_black() {
    let _serial = serial();
    let mut renderer = support::renderer();
    let bare = renderer.create_mesh(&quad(false)).unwrap();
    let image = render_quad(&mut renderer, bare, &hair_material(hair(&[])));
    // Every vertex reads across 0, along 0, seed 0, shade unset (1) and polar 0.
    let want = encode(Prototype::defaults().lit(0.0, 0.0, 0.0, 1.0, 0.0));
    let got = image.pixel(64, 64).unwrap();
    assert!(want.iter().zip(got).all(|(w, g)| w.abs_diff(g) <= 2), "expected {want:?}, drew {got:?}");
    assert!(got[0] > 100, "not black: {got:?}");

    // An attribute with the wrong number of components is not the one the program reads: also zeros.
    let mut wrong = quad(true);
    wrong.extra[0].components = 1;
    wrong.extra[0].data.truncate(4);
    let wrong = renderer.create_mesh(&wrong).unwrap();
    let again = render_quad(&mut renderer, wrong, &hair_material(hair(&[])));
    let partial = again.pixel(64, 64).unwrap();
    assert!(partial[0] > 100, "{partial:?}");
}

#[test]
fn hair_takes_its_toon_base_from_the_program() {
    let _serial = serial();
    let mut renderer = support::renderer();
    // One light from the side, so half of the sphere-like quad is in shade: the `shade` parameter tints it.
    let mesh = renderer.create_mesh(&quad(true)).unwrap();
    let mut shaded = exact_scene();
    shaded.lights = vec![Light::Directional {
        to_light: Vec3::new(0.0, 0.0, -1.0),
        colour: Colour::WHITE,
        intensity: std::f32::consts::PI,
        casts_shadow: false,
    }];
    let picture = |renderer: &mut WgpuRenderer, shade: u32| {
        let material = renderer
            .create_material(&hair_material(hair(&[("shade", ParamValue::Colour(Colour::hex(shade)))])))
            .unwrap();
        let mut scene = shaded.clone();
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap()
    };
    let (cool, warm) = (picture(&mut renderer, 0x0040_4080), picture(&mut renderer, 0x0080_4040));
    let (a, b) = (cool.pixel(64, 64).unwrap(), warm.pixel(64, 64).unwrap());
    assert!(a[2] > a[0] && b[0] > b[2], "the shadow side follows the shade parameter: {a:?} / {b:?}");
}

// ---- the mock character ----

/// The head the mock character builds: its centre, from the `mock-head` mesh bounds (y 0.60 to 0.97, x and z about
/// 0.19 either side).
const HEAD_CENTRE: Vec3 = Vec3::new(0.0, 0.78, -0.02);
const HEAD_RADIUS: f32 = 0.19;

/// A crown of hair over the mock head, which the mock itself does not build (it skips the `cap` generator): 56 ribbon
/// clumps from the parting down past the ears, carrying `hair-uv` and `hair-info` exactly as `omuzaki-char-mock`'s
/// ribbons do (`across` -1..1, `along` 0..1; seed, shade, and the polar angle from the top of the head, as
/// `HAIR_INFO` and the prototype's `acos(dir.y)` define it). It exists so the angel ring, which sits on the crown, can be seen on the mock character.
fn crown() -> MeshData {
    const CLUMPS: u32 = 56;
    const POINTS: u32 = 10;
    let mut mesh = MeshData::default();
    let (mut uv, mut info) = (Vec::new(), Vec::new());
    for k in 0..CLUMPS {
        let phi = std::f32::consts::TAU * k as f32 / CLUMPS as f32;
        let seed = (k as f32 * 0.618_034).fract();
        let shade = 0.82 + 0.18 * (k % 3) as f32 / 2.0;
        let base = mesh.positions.len() as u32;
        for i in 0..POINTS {
            let along = i as f32 / (POINTS - 1) as f32;
            // From just off the crown to below the ears, swaying a little so neighbours do not line up.
            let theta = 0.1 + along * 1.65;
            let phi = phi + 0.07 * (along * 5.0 + seed * 6.0).sin();
            let outward = Vec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
            let at = HEAD_CENTRE + outward * (HEAD_RADIUS * 1.07);
            let tangent = Vec3::new(theta.cos() * phi.sin(), -theta.sin(), theta.cos() * phi.cos());
            let side = tangent.cross(outward).normalize() * (0.02 * (1.0 - 0.6 * along));
            let polar = (at - HEAD_CENTRE).normalize().y.acos();
            for (across, p) in [(-1.0, at - side), (1.0, at + side)] {
                mesh.positions.push(p);
                mesh.normals.push(outward);
                mesh.uvs.push(Vec2::new(across, along));
                uv.extend([across, along]);
                info.extend([seed, shade, polar]);
            }
        }
        for i in 0..POINTS - 1 {
            let a = base + 2 * i;
            // Counter-clockwise seen from outside, so the shader's normal (flipped on back faces) stays outward.
            mesh.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }
    mesh.extra = vec![
        ExtraAttribute { name: slug(HAIR_UV), components: 2, data: uv },
        ExtraAttribute { name: slug(HAIR_INFO), components: 3, data: info },
    ];
    mesh
}

const MOCK_EYE: Vec3 = Vec3::new(0.0, 1.45, 1.35);
const MOCK_TARGET: Vec3 = Vec3::new(0.0, 0.7, 0.0);
const MOCK_SIZE: UVec2 = UVec2::new(256, 256);

/// The mock character (head, hair, torso and garments in the rest pose), lit and framed from the front and above.
/// `program` false draws the hair as plain toon: what any renderer without the program shows.
fn mock_head_scene(renderer: &mut WgpuRenderer, hair: Hair) -> Scene {
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut scene = Scene::new(Camera::look_at(
        MOCK_EYE,
        MOCK_TARGET,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
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
    let mut materials = BTreeMap::new();
    for part in &built.meshes {
        let name = part.mesh.material.clone();
        if !materials.contains_key(&name) {
            let mut spec = built.materials[&name].clone();
            spec.overlays.clear();
            spec.base_texture = None;
            // Outlines are the Outlines pass's (tests/outlines.rs); a hull over one-sided ribbons would hide the hair shading.
            spec.outline = None;
            if name.as_str() == "mock-hair" {
                match hair {
                    Hair::Toon => spec.shading = Shading::Toon(Toon::default()),
                    Hair::Program { ring_strength: Some(r) } => {
                        if let Shading::Program { params, .. } = &mut spec.shading {
                            params.insert(slug("ring-strength"), ParamValue::Float(r));
                        }
                    }
                    Hair::Program { ring_strength: None } => {}
                }
            }
            let material = spec.to_material(&|_| None, &|_| None).unwrap();
            materials.insert(name.clone(), renderer.create_material(&material).unwrap());
        }
        let mesh = renderer.create_mesh(&part.mesh.geometry).unwrap();
        let mut instance = Instance::new(mesh, materials[&name], Mat4::IDENTITY);
        if !part.mesh.joint_bones.is_empty() {
            // The rest pose: every joint at identity leaves the bind-space vertices where the part built them.
            let start = u32::try_from(scene.joints.len()).unwrap();
            let len = u32::try_from(part.mesh.joint_bones.len()).unwrap();
            scene.joints.extend(std::iter::repeat_n(Mat4::IDENTITY, part.mesh.joint_bones.len()));
            instance.skin = Some(JointRange { start, len });
        }
        scene.instances.push(instance);
    }
    let crown = renderer.create_mesh(&crown()).unwrap();
    let hair = materials[&MaterialName::new("mock-hair").unwrap()];
    scene.instances.push(Instance::new(crown, hair, Mat4::IDENTITY));
    scene
}

/// How the mock's hair is drawn: as plain toon (what a renderer without the program shows), or with the program, whose
/// ring strength a test may override.
#[derive(Copy, Clone)]
enum Hair {
    Toon,
    Program { ring_strength: Option<f32> },
}

fn mock_head(hair: Hair) -> Image {
    let mut renderer = support::renderer();
    let scene = mock_head_scene(&mut renderer, hair);
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    renderer.render_image(MOCK_SIZE, AlphaEncoding::Straight, &scene, &quality).unwrap()
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        image::save_buffer(
            PathBuf::from(dir).join(name),
            &image.rgba8,
            image.size.x,
            image.size.y,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

#[test]
fn the_mock_characters_hair_matches_its_golden() {
    let _serial = serial();
    let (before, after) = (mock_head(Hair::Toon), mock_head(Hair::Program { ring_strength: None }));
    save(&before, "mock-hair-before.png");
    save(&after, "mock-hair-after.png");
    assert_ne!(before.rgba8, after.rgba8, "the program must change how the hair looks");

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/mock-hair.png");
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        image::save_buffer(&path, &after.rgba8, after.size.x, after.size.y, image::ColorType::Rgba8).unwrap();
    }
    let golden = image::open(&path).expect("tests/golden/mock-hair.png (bless with OMUZAKI_BLESS=1)").to_rgba8();
    assert_eq!(golden.dimensions(), (after.size.x, after.size.y));
    let diff = support::Diff::of(golden.as_raw(), &after.rgba8);
    assert!(
        diff.max <= 2,
        "the mock hair differs from its golden by up to {} levels ({diff}; bless with OMUZAKI_BLESS=1)",
        diff.max
    );
}

#[test]
fn the_character_kits_default_hair_shading_is_the_programs_default() {
    // `HairShading::default()` and the program's declared defaults are the same numbers: a material that sets nothing
    // looks like the kit's default hair, and the oracle above is the kit's default.
    let _serial = serial();
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&quad(true)).unwrap();
    let palette = omuzaki_face_api::Palette::default();
    let kit = HairShading::default().shading(&palette).unwrap();
    let from_kit = render_quad(&mut renderer, mesh, &hair_material(kit));
    let from_defaults = render_quad(&mut renderer, mesh, &hair_material(hair(&[])));
    assert_eq!(from_kit.rgba8, from_defaults.rgba8);
}

/// The pixel row of the crown point at polar angle `polar` from the top, straight in front of the head (+Z side).
fn crown_row(polar: f32) -> f32 {
    let camera = Camera::look_at(
        MOCK_EYE,
        MOCK_TARGET,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    );
    let point = HEAD_CENTRE + Vec3::new(0.0, polar.cos(), polar.sin()) * (HEAD_RADIUS * 1.07);
    let clip = camera.projection_matrix(1.0) * camera.view() * point.extend(1.0);
    (1.0 - clip.y / clip.w) / 2.0 * MOCK_SIZE.y as f32
}

#[test]
fn the_angel_ring_lands_on_the_crown_where_the_prototype_puts_it() {
    // The ring is at 0.92 rad from the top of the head (`acos(dir.y)` in hair.js). On hair-shaped geometry (the crown
    // of ribbon clumps, with the strand coordinates real hair kinds provide) seen from the front and above, with the
    // tone curve on, the ring-only difference (default ring minus `ring-strength` 0) must be one band, centred on the
    // projected row of polar 0.92 straight ahead, a few rows wide, and several sRGB levels brighter.
    let _serial = serial();
    let (with, without) =
        (mock_head(Hair::Program { ring_strength: None }), mock_head(Hair::Program { ring_strength: Some(0.0) }));
    save(&with, "crown-ring-with.png");
    save(&without, "crown-ring-without.png");
    let centre = i64::from(MOCK_SIZE.x / 2);
    let rows: Vec<f32> = (0..MOCK_SIZE.y)
        .map(|y| {
            let diffs: Vec<f32> = (centre - 10..=centre + 10)
                .map(|x| luma(rgb_of(&with, x as u32, y)) - luma(rgb_of(&without, x as u32, y)))
                .collect();
            diffs.iter().sum::<f32>() / diffs.len() as f32
        })
        .collect();
    let peak = rows.iter().copied().fold(0.0, f32::max);
    let peak_row = rows.iter().position(|&r| (r - peak).abs() < f32::EPSILON).unwrap() as f32;
    let fwhm = rows.iter().filter(|&&r| r >= peak / 2.0).count();
    let expected = crown_row(0.92);
    println!(
        "crown ring: peak row {peak_row} (projected polar 0.92: {expected:.1}), FWHM {fwhm} rows, rise {peak:.2} luma"
    );
    // The prototype's ring: +8.3 sRGB levels over its base after ACES, about 16 rows wide at 720p with the head 1/3 of the
    // frame; here the head fills about 60% of a 256-pixel frame and the strip is averaged over 21 columns of teeth.
    assert!(
        (peak_row - expected).abs() <= 4.0,
        "the ring peaks at row {peak_row}, polar 0.92 projects to {expected:.1}"
    );
    assert!((4..=24).contains(&fwhm), "the ring is {fwhm} rows wide");
    assert!((3.0..=20.0).contains(&peak), "the ring raises luma by {peak:.2} levels (prototype: 8.3)");
}
