//! The `fabric` shading program (#2489) on the software adapter: a close-up cloth ball under a key, a fill and a rim light.
//! Every parameter changes the picture, each the way it is meant to (the sheen lights the edge, a lower roughness makes
//! a sharper highlight, anisotropy streaks it, the weave shows close up and fades far away, the fold colour tints the
//! shade side), and the presets read as different cloths. `OMUZAKI_RENDER_OUT=<dir>` keeps the renders.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render::{WgpuRenderer, check_shading_program};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Light, Material, MeshData, ParamValue, Projection,
    Quality, Renderer, RendererExt, Scene, Shading, Tonemap,
};

const SIZE: UVec2 = UVec2::new(256, 256);
/// A ball 4 cm across filling most of the frame: 5120 pixels per metre, so a cotton thread (900 per metre) is about 6
/// pixels wide, as a close-up of a sleeve shows it.
const RADIUS: f32 = 0.02;
const VIEW_HEIGHT: f32 = 0.05;

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

fn slug(name: &str) -> Slug {
    Slug::new(name).unwrap()
}

fn fabric(params: &[(&str, ParamValue)]) -> Shading {
    Shading::Program { name: slug("fabric"), params: params.iter().map(|(k, v)| (slug(k), *v)).collect() }
}

/// The cloth preset `name`: the program `fabric-<name>` with its own defaults.
fn preset(name: &str) -> Shading {
    Shading::Program { name: slug(&format!("fabric-{name}")), params: omuzaki_render_api::ProgramParams::new() }
}

const PRESETS: [&str; 6] = ["cotton", "satin", "knit", "leather", "denim", "velvet"];

/// A key from the upper left front, a dim cool fill and a rim from behind on the right.
fn scene(view_height: f32) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: view_height, near: 0.01, far: 10.0 },
    ));
    scene.background = Background::Solid { colour: Colour::hex(0x0010_1014) };
    scene.post.tonemap = Tonemap::None;
    scene.lights = vec![
        Light::Directional {
            to_light: Vec3::new(-0.6, 0.5, 0.65).normalize(),
            colour: Colour::WHITE,
            intensity: 2.6,
            casts_shadow: false,
        },
        Light::Hemisphere { sky: Colour::hex(0x0060_7090), ground: Colour::hex(0x0030_2a28), intensity: 0.8 },
        Light::Directional {
            to_light: Vec3::new(0.8, 0.2, -0.55).normalize(),
            colour: Colour::hex(0x00ff_e8d0),
            intensity: 2.0,
            casts_shadow: false,
        },
    ];
    scene
}

fn render(renderer: &mut WgpuRenderer, shading: &Shading, view_height: f32) -> Image {
    let mesh = renderer.create_mesh(&sphere(RADIUS, 160, 80)).unwrap();
    let mut material = Material::toon(Colour::hex(0x00b0_4a5a));
    material.shading = shading.clone();
    let id = renderer.create_material(&material).unwrap();
    let mut scene = scene(view_height);
    scene.instances.push(Instance::new(mesh, id, Mat4::IDENTITY));
    let quality = Quality { samples: 4, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    let image = renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &quality).unwrap();
    renderer.release(id.into()).unwrap();
    renderer.release(mesh.into()).unwrap();
    image
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (image.size.x, image.size.y);
        image::save_buffer(PathBuf::from(dir).join(name), &image.rgba8, w, h, image::ColorType::Rgba8).unwrap();
    }
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

/// The view's pixels on the ball whose normal faces the camera by at least `min` and at most `max` (`n.z`).
fn band(min: f32, max: f32) -> impl Iterator<Item = (u32, u32)> {
    let pixels_per_metre = SIZE.y as f32 / VIEW_HEIGHT;
    (0..SIZE.y).flat_map(move |y| {
        (0..SIZE.x).filter_map(move |x| {
            let sx = (x as f32 + 0.5 - 128.0) / pixels_per_metre / RADIUS;
            let sy = (128.0 - y as f32 - 0.5) / pixels_per_metre / RADIUS;
            let z2 = 1.0 - sx * sx - sy * sy;
            (z2 > 0.0 && (min..=max).contains(&z2.sqrt())).then_some((x, y))
        })
    })
}

fn mean(image: &Image, pixels: impl Iterator<Item = (u32, u32)>) -> f32 {
    let (mut sum, mut count) = (0.0, 0.0);
    for (x, y) in pixels {
        sum += luma(image.pixel(x, y).unwrap());
        count += 1.0;
    }
    sum / count
}

/// How much the picture changes from one pixel to the next over the ball's middle: the thread pattern's contrast.
fn texture(image: &Image) -> f32 {
    texture_at(image, 1)
}

/// How much the picture changes from a pixel to the one `lag` to its right over the ball's middle: the contrast of a
/// pattern about `2 * lag` pixels across (a knit's loops) as well as of a fine one.
fn texture_at(image: &Image, lag: u32) -> f32 {
    let (mut sum, mut count) = (0.0, 0.0);
    for (x, y) in band(0.6, 1.0) {
        if x + lag < SIZE.x {
            sum += (luma(image.pixel(x + lag, y).unwrap()) - luma(image.pixel(x, y).unwrap())).abs();
            count += 1.0;
        }
    }
    sum / count
}

/// The largest per-channel difference between two pictures.
fn difference(a: &Image, b: &Image) -> u8 {
    a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn every_parameter_changes_the_cloth() {
    let mut r = support::renderer();
    assert!(check_shading_program("fabric").is_ok());
    // The fold at full strength, so a change of its colour shows.
    let base = render(&mut r, &fabric(&[("fold-strength", ParamValue::Float(1.0))]), VIEW_HEIGHT);
    save(&base, "fabric-default.png");
    let f = ParamValue::Float;
    let colour = |hex| ParamValue::Colour(Colour::hex(hex));
    for (name, value) in [
        ("weave", f(2.0)),
        ("weave-scale", f(120.0)),
        ("weave-depth", f(1.0)),
        ("roughness", f(0.3)),
        ("specular", f(0.15)),
        ("anisotropy", f(1.0)),
        ("wrap", f(0.9)),
        ("sheen", f(1.5)),
        ("sheen-colour", colour(0x0040_80ff)),
        ("sheen-roughness", f(0.2)),
        ("fold-colour", colour(0x0000_40ff)),
        ("fold-strength", f(0.0)),
        ("fold-softness", f(0.05)),
        ("style", f(1.0)),
    ] {
        let mut params = vec![("fold-strength", f(1.0))];
        params.retain(|(n, _)| *n != name);
        params.push((name, value));
        let changed = render(&mut r, &fabric(&params), VIEW_HEIGHT);
        let d = difference(&base, &changed);
        assert!(d >= 8, "{name} changes the picture: at most {d} levels");
    }
}

#[test]
fn each_parameter_does_what_it_says() {
    let mut r = support::renderer();
    let f = ParamValue::Float;
    let at = |r: &mut WgpuRenderer, params: &[(&str, ParamValue)]| render(r, &fabric(params), VIEW_HEIGHT);

    // The sheen lights the edge (grazing) far more than the middle.
    let (none, fuzzy) = (at(&mut r, &[("sheen", f(0.0))]), at(&mut r, &[("sheen", f(2.0))]));
    let edge = mean(&fuzzy, band(0.05, 0.3)) - mean(&none, band(0.05, 0.3));
    let middle = mean(&fuzzy, band(0.9, 1.0)) - mean(&none, band(0.9, 1.0));
    assert!(edge > 2.0 * middle.max(0.5), "sheen: +{edge} at the edge, +{middle} in the middle");

    // A smoother, more reflective cloth has a brighter highlight.
    let peak = |image: &Image| band(0.0, 1.0).map(|(x, y)| luma(image.pixel(x, y).unwrap())).fold(0.0, f32::max);
    let matte = at(&mut r, &[("roughness", f(0.9)), ("specular", f(0.05)), ("weave-depth", f(0.0))]);
    let glossy = at(&mut r, &[("roughness", f(0.25)), ("specular", f(0.05)), ("weave-depth", f(0.0))]);
    save(&glossy, "fabric-glossy.png");
    assert!(peak(&glossy) > peak(&matte) + 20.0, "roughness: peak {} glossy, {} matte", peak(&glossy), peak(&matte));

    // Anisotropy streaks the highlight across the warp (the warp runs down the ball, so the streak runs across it).
    let bright = |image: &Image| {
        // The key light's highlight, on the left half (the rim light catches the right edge).
        let left = || band(0.0, 1.0).filter(|&(x, _)| x < 128);
        let top = left().map(|(x, y)| luma(image.pixel(x, y).unwrap())).fold(0.0, f32::max) - 25.0;
        let lit: Vec<(u32, u32)> = left().filter(|&(x, y)| luma(image.pixel(x, y).unwrap()) >= top).collect();
        let span =
            |pick: fn(&(u32, u32)) -> u32| lit.iter().map(pick).max().unwrap() - lit.iter().map(pick).min().unwrap();
        (span(|p| p.0), span(|p| p.1))
    };
    let satin =
        at(&mut r, &[("roughness", f(0.25)), ("specular", f(0.05)), ("anisotropy", f(1.0)), ("weave-depth", f(0.0))]);
    save(&satin, "fabric-anisotropic.png");
    let ((gx, gy), (sx, sy)) = (bright(&glossy), bright(&satin));
    assert!(
        sx as f32 / sy.max(1) as f32 > 1.5 * (gx as f32 / gy.max(1) as f32),
        "anisotropy: the highlight is {gx}x{gy} px round, {sx}x{sy} px streaked"
    );

    // The weave shows close up, more with depth, and fades where a pixel covers several threads.
    let flat = texture(&at(&mut r, &[("weave-depth", f(0.0))]));
    let woven = texture(&at(&mut r, &[("weave-depth", f(1.0))]));
    assert!(woven > flat + 2.0, "weave-depth: {flat} flat, {woven} woven");
    let fine = texture(&at(&mut r, &[("weave-depth", f(1.0)), ("weave-scale", f(5000.0))]));
    assert!(fine < flat + 1.0, "a weave finer than the pixels fades: {fine} against {flat} flat");

    // The fold colour tints the shade side: a blue fold on a red cloth turns its unlit side blue.
    let blue_of = |image: &Image| {
        let pixels: Vec<(u32, u32)> = band(0.3, 1.0).filter(|&(x, y)| x > 150 && y > 150).collect();
        let n = pixels.len() as f32;
        let b: f32 = pixels.iter().map(|&(x, y)| f32::from(image.pixel(x, y).unwrap()[2])).sum::<f32>() / n;
        let red: f32 = pixels.iter().map(|&(x, y)| f32::from(image.pixel(x, y).unwrap()[0])).sum::<f32>() / n;
        b / red.max(1.0)
    };
    let blue = ParamValue::Colour(Colour::hex(0x0030_60ff));
    let plain = at(&mut r, &[("fold-strength", f(0.0))]);
    let folded = at(&mut r, &[("fold-strength", f(1.0)), ("fold-colour", blue)]);
    save(&folded, "fabric-fold.png");
    assert!(
        blue_of(&folded) > 1.3 * blue_of(&plain),
        "fold: blue/red {} plain, {} folded",
        blue_of(&plain),
        blue_of(&folded)
    );
}

#[test]
fn the_presets_read_as_different_cloths() {
    let mut r = support::renderer();
    let listed: Vec<String> = r.capabilities().shading_programs.iter().map(|s| s.as_str().to_owned()).collect();
    for name in PRESETS {
        assert!(listed.contains(&format!("fabric-{name}")), "{listed:?}");
    }
    let renders: Vec<(&str, Image)> = PRESETS.iter().map(|&p| (p, render(&mut r, &preset(p), VIEW_HEIGHT))).collect();
    assert_eq!(render(&mut r, &fabric(&[]), VIEW_HEIGHT).rgba8, renders[0].1.rgba8, "plain `fabric` is cotton");
    for (name, image) in &renders {
        save(image, &format!("fabric-{name}.png"));
    }
    let get = |name: &str| &renders.iter().find(|(n, _)| *n == name).unwrap().1;
    for (i, (a, image_a)) in renders.iter().enumerate() {
        for (b, image_b) in &renders[i + 1..] {
            assert!(difference(image_a, image_b) >= 20, "{a} and {b} differ");
        }
    }
    // Woven, knitted and grained cloths show their pattern close up where satin is smooth; satin is the glossiest, velvet
    // glows at its edge.
    for name in ["cotton", "knit", "leather", "denim"] {
        let (pattern, smooth) = (texture_at(get(name), 4), texture_at(get("satin"), 4));
        assert!(pattern > smooth + 1.0, "{name} shows its pattern ({pattern}) more than smooth satin ({smooth})");
    }
    let peak = |image: &Image| band(0.0, 1.0).map(|(x, y)| luma(image.pixel(x, y).unwrap())).fold(0.0, f32::max);
    assert!(peak(get("satin")) > peak(get("cotton")) + 20.0, "satin shines");
    assert!(peak(get("leather")) > peak(get("cotton")), "leather has a gloss");
    let edge_over_middle = |image: &Image| mean(image, band(0.05, 0.3)) / mean(image, band(0.9, 1.0)).max(1.0);
    assert!(edge_over_middle(get("velvet")) > edge_over_middle(get("cotton")), "velvet's pile glows at the edge");
}
