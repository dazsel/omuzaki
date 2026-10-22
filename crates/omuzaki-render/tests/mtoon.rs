//! `MToon` 1.0 lighting on the GPU renderer (#592, LK-07): the shade and terminator, GI equalisation, the rim with its
//! lift and lighting mix, the matcap, the shade, shift, rim, emissive and normal textures, UV animation and blended depth
//! writes, each checked against an independent f64 implementation of three-vrm's `mtoon.frag` (and `UniVRM`'s GI
//! equalisation, which three-vrm leaves out).
//!
//! The scenes are `toon.rs`'s: small orthographic stills on the software adapter with no tone curve, so a pixel is the
//! linear colour encoded to sRGB and the expected value follows from the formulas alone.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::f64::consts::PI;

use omuzaki_core::glam::{DVec3, Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, ColourSpace, Filter, Image, Instance, Light, Material, Mtoon,
    Projection, Quality, Renderer, RendererExt, Sampler, Scene, Shading, TextureDesc, TextureId, Tonemap, Toon, Wrap,
};
use omuzaki_render_mock::shapes;

/// 128 x 128 pixels over a 2 m tall orthographic view: 64 pixels per metre, the origin at pixel (64, 64).
const SIZE: UVec2 = UVec2::new(128, 128);
const PIXELS_PER_METRE: f32 = 64.0;
/// Where the camera sits; three-vrm's view direction is toward it from every point, orthographic or not.
const EYE: Vec3 = Vec3::new(0.0, 0.0, 5.0);
/// 8-bit rounding in the scene format and the encode; the f64 oracle is exact, the GPU is f32 and f16.
const TOLERANCE: u8 = 2;

fn scene(lights: Vec<Light>) -> Scene {
    let camera =
        Camera::look_at(EYE, Vec3::ZERO, Vec3::Y, Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 });
    let mut s = Scene::new(camera);
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s.lights = lights;
    s
}

fn draw(r: &mut dyn Renderer, scene: &Scene) -> Image {
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    r.render_image(SIZE, AlphaEncoding::Straight, scene, &quality).unwrap()
}

fn rgb_at(image: &Image, (x, y): (u32, u32)) -> [u8; 3] {
    let [red, green, blue, _] = image.pixel(x, y).unwrap();
    [red, green, blue]
}

fn pixel_of(x: f32, y: f32) -> (u32, u32) {
    ((PIXELS_PER_METRE.mul_add(x, 64.0)).floor() as u32, (PIXELS_PER_METRE.mul_add(-y, 64.0)).floor() as u32)
}

fn world_of((x, y): (u32, u32)) -> DVec3 {
    DVec3::new((f64::from(x) + 0.5 - 64.0) / 64.0, (64.0 - f64::from(y) - 0.5) / 64.0, 0.0)
}

fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= TOLERANCE), "{what}: got {got:?}, wanted {want:?}");
}

fn encode(c: DVec3) -> [u8; 3] {
    let channel = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    [channel(c.x), channel(c.y), channel(c.z)]
}

fn lin(c: Colour) -> DVec3 {
    c.to_linear().as_dvec3()
}

// ---- the oracle: three-vrm's mtoon.frag in f64, written from the shader's GLSL rather than from ours ----

/// What the textures of a probe contribute, already sampled: multipliers of 1 and no matcap when absent.
#[derive(Clone, Copy)]
struct Sampled {
    shade: DVec3,
    shift: f64,
    rim: DVec3,
    emissive: DVec3,
    matcap: DVec3,
}

impl Default for Sampled {
    fn default() -> Self {
        Self { shade: DVec3::ONE, shift: 0.0, rim: DVec3::ONE, emissive: DVec3::ONE, matcap: DVec3::ZERO }
    }
}

/// three.js `getDistanceAttenuation` with decay 2 and a cutoff distance.
fn three_attenuation(distance: f64, range: f64) -> f64 {
    (1.0 / (distance * distance).max(0.01)) * (1.0 - (distance / range).powi(4)).clamp(0.0, 1.0).powi(2)
}

/// `getShading`: `linearstep(-1 + toony, 1 - toony, n_dot_l + shift)`, a step when the edges meet.
fn shading(m: &Mtoon, n_dot_l: f64, shift: f64) -> f64 {
    let toony = f64::from(m.shading_toony);
    let (low, high, x) = (-1.0 + toony, 1.0 - toony, n_dot_l + shift);
    if high <= low {
        return f64::from(x >= low);
    }
    ((x - low) / (high - low)).clamp(0.0, 1.0)
}

/// The linear colour of an `MToon` surface of lit colour `lit` and emissive factor `emissive` at `position` with unit
/// normal `n`, under `lights`.
fn expected(m: &Mtoon, lit: DVec3, emissive: DVec3, s: Sampled, lights: &[Light], n: DVec3, position: DVec3) -> DVec3 {
    let shade = lin(m.shade) * s.shade;
    let shift = f64::from(m.shading_shift) + s.shift;
    let (mut direct, mut reaching, mut toward, mut flat) = (DVec3::ZERO, DVec3::ZERO, DVec3::ZERO, DVec3::ZERO);
    for light in lights {
        let (colour, to_light) = match *light {
            Light::Directional { to_light, colour, intensity, .. } => {
                (lin(colour) * f64::from(intensity), to_light.as_dvec3().normalize())
            }
            Light::Point { position: at, colour, intensity, range } => {
                let offset = at.as_dvec3() - position;
                let distance = offset.length();
                let reach = three_attenuation(distance, f64::from(range));
                (lin(colour) * f64::from(intensity) * reach, offset / distance.max(1e-6))
            }
            Light::Hemisphere { sky, ground, intensity } => {
                let (sky, ground) = (lin(sky) * f64::from(intensity), lin(ground) * f64::from(intensity));
                toward += ground.lerp(sky, 0.5 + 0.5 * n.y);
                flat += 0.5 * (sky + ground);
                continue;
            }
            Light::Spot { .. } => panic!("the oracle has no spot lights"),
        };
        let shaded = shading(m, n.dot(to_light).clamp(-1.0, 1.0), shift);
        direct += colour * shade.lerp(lit, shaded) / PI;
        reaching += colour;
    }
    let ambient = toward.lerp(flat, f64::from(m.gi_equalisation));
    reaching += ambient;
    let rim_mix = DVec3::ONE.lerp(reaching / PI, f64::from(m.rim_lighting_mix));
    let view = (EYE.as_dvec3() - position).normalize();
    let fresnel = (1.0 - view.dot(n) + f64::from(m.rim_lift)).clamp(0.0, 1.0).max(1e-6);
    let rim = (lin(m.rim) * fresnel.powf(f64::from(m.rim_fresnel_power)) + s.matcap) * s.rim;
    direct + ambient * lit / PI + rim_mix * rim + emissive * s.emissive
}

// ---- scenes ----

fn mtoon_material(base: Colour, emissive: Colour, mtoon: Mtoon) -> Material {
    Material {
        shading: Shading::Toon(Toon { emissive, ..Toon::default() }),
        mtoon: Some(Box::new(mtoon)),
        ..Material::toon(base)
    }
}

/// Quads in rows of seven, quad `i` turned by `angles_deg[i]` about `axis`. Returns the image and each quad's centre
/// pixel and unit normal.
fn fan(r: &mut dyn Renderer, material: &Material, lights: Vec<Light>, axis: Vec3, angles_deg: &[f32]) -> Fan {
    let mut s = scene(lights);
    s.frame.time = Seconds(0.0);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.075))).unwrap();
    let id = r.create_material(material).unwrap();
    let mut probes = Vec::new();
    for (i, &deg) in angles_deg.iter().enumerate() {
        let (row, column) = (i / 7, i % 7);
        let (x, y) = (-0.84 + 0.28 * column as f32, 0.6 - 0.3 * row as f32);
        let rotation = Mat4::from_axis_angle(axis, deg.to_radians());
        s.instances.push(Instance::new(mesh, id, Mat4::from_translation(Vec3::new(x, y, 0.0)) * rotation));
        probes.push((pixel_of(x, y), rotation.transform_vector3(Vec3::Z).as_dvec3(), rotation));
    }
    let image = draw(r, &s);
    r.release(mesh.into()).unwrap();
    r.release(id.into()).unwrap();
    Fan { image, probes }
}

struct Fan {
    image: Image,
    probes: Vec<((u32, u32), DVec3, Mat4)>,
}

/// Checks every quad of a fan against the oracle.
fn check(fan: &Fan, material: &Material, s: Sampled, lights: &[Light], what: &str) {
    let m = material.mtoon.as_deref().unwrap();
    let Shading::Toon(toon) = &material.shading else { panic!("an MToon material is toon") };
    for (i, &(pixel, n, _)) in fan.probes.iter().enumerate() {
        let colour = expected(m, lin(material.base), lin(toon.emissive), s, lights, n, world_of(pixel));
        assert_close(rgb_at(&fan.image, pixel), encode(colour), &format!("{what}: surface {i}, normal {n:.3}"));
    }
}

fn angles() -> Vec<f32> {
    (-7..=7).map(|i| i as f32 * 11.0).collect()
}

fn key(to_light: Vec3, intensity: f32) -> Light {
    Light::Directional { to_light, colour: Colour::WHITE, intensity, casts_shadow: false }
}

/// A texture of `w` x `h` RGBA texels, nearest and repeating, in `space`.
fn texture(r: &mut dyn Renderer, space: ColourSpace, (w, h): (u32, u32), texels: &[u8]) -> TextureId {
    let desc = TextureDesc {
        colour_space: space,
        mips: false,
        sampler: Sampler { filter: Filter::Nearest, wrap: Wrap::Repeat, ..Sampler::default() },
        ..TextureDesc::colour(UVec2::new(w, h))
    };
    r.create_texture(&desc, Some(texels)).unwrap()
}

// ---- lighting ----

#[test]
fn the_lighting_follows_three_vrm_under_every_light_kind() {
    let mut r = support::renderer();
    let mtoon = Mtoon {
        shade: Colour::hex(0x0070_4c8c),
        shading_shift: -0.1,
        shading_toony: 0.6,
        gi_equalisation: 0.5,
        rim: Colour::hex(0x0066_4d99),
        rim_fresnel_power: 3.0,
        rim_lift: 0.1,
        rim_lighting_mix: 0.5,
        ..Mtoon::default()
    };
    let material = mtoon_material(Colour::hex(0x00f3_d2c8), Colour::hex(0x0008_0410), mtoon);
    let lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00d8_e4ff), ground: Colour::hex(0x0040_3028), intensity: 0.8 },
        key(Vec3::new(1.0, 0.3, 0.4), 1.6),
        Light::Point {
            position: Vec3::new(-0.3, 0.5, 0.8),
            colour: Colour::hex(0x00ff_b070),
            intensity: 2.0,
            range: 4.0,
        },
    ];
    let f = fan(&mut r, &material, lights.clone(), Vec3::Y, &angles());
    check(&f, &material, Sampled::default(), &lights, "mixed lights");
    let (shade_side, lit_side) = (rgb_at(&f.image, f.probes[0].0), rgb_at(&f.image, f.probes[14].0));
    assert!(lit_side[0] > shade_side[0] + 40, "the lit side {lit_side:?} is far brighter than {shade_side:?}");
    // The same normals tilted up and down: the hemisphere and the point light from above.
    let f = fan(&mut r, &material, lights.clone(), Vec3::X, &angles());
    check(&f, &material, Sampled::default(), &lights, "tilted");
}

#[test]
fn toony_one_is_a_hard_step_at_the_shifted_terminator() {
    let mut r = support::renderer();
    let mtoon = Mtoon { shade: Colour::hex(0x00d7_a3a8), shading_shift: 0.3, shading_toony: 1.0, ..Mtoon::default() };
    let material = mtoon_material(Colour::WHITE, Colour::BLACK, mtoon);
    let lights = vec![key(Vec3::X, 2.0)];
    let f = fan(&mut r, &material, lights.clone(), Vec3::Y, &angles());
    check(&f, &material, Sampled::default(), &lights, "step");
    // The shift of 0.3 moves the edge to N.L = -0.3: the quad at -11 degrees (N.L = -0.19) is lit, the one at -22
    // (N.L = -0.37) is shaded.
    let (shaded, lit) = (rgb_at(&f.image, f.probes[5].0), rgb_at(&f.image, f.probes[6].0));
    assert!(lit[1] > shaded[1] + 30, "the edge falls between -22 and -11 degrees: {shaded:?} then {lit:?}");
}

#[test]
fn gi_equalisation_flattens_the_ambient_toward_the_mean_of_up_and_down() {
    let mut r = support::renderer();
    let lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00ff_ffff), ground: Colour::hex(0x0033_2a40), intensity: 2.0 }];
    for gi in [0.0, 0.4, 1.0] {
        let mtoon = Mtoon { gi_equalisation: gi, ..Mtoon::default() };
        let material = mtoon_material(Colour::WHITE, Colour::BLACK, mtoon);
        let f = fan(&mut r, &material, lights.clone(), Vec3::X, &angles());
        check(&f, &material, Sampled::default(), &lights, &format!("gi {gi}"));
        let (up, down) = (rgb_at(&f.image, f.probes[0].0), rgb_at(&f.image, f.probes[14].0));
        if gi >= 1.0 {
            assert_close(up, down, "fully equalised, up and down see the same light");
        } else {
            assert!(up[0] > down[0] + 10, "gi {gi}: up {up:?} is brighter than down {down:?}");
        }
    }
}

// ---- textures ----

#[test]
fn the_shade_shift_rim_and_emissive_textures_multiply_and_shift_as_the_spec_says() {
    let mut r = support::renderer();
    let shade = texture(&mut r, ColourSpace::Srgb, (1, 1), &[200, 120, 160, 255]);
    // Linear data: red 64 is a shift of 0.251 before the scale.
    let shift = texture(&mut r, ColourSpace::Linear, (1, 1), &[64, 0, 0, 255]);
    let rim = texture(&mut r, ColourSpace::Srgb, (1, 1), &[255, 180, 90, 255]);
    let emissive = texture(&mut r, ColourSpace::Srgb, (1, 1), &[90, 200, 255, 255]);
    let mtoon = Mtoon {
        shade: Colour::hex(0x0080_6090),
        shade_texture: Some(shade),
        shading_shift_texture: Some(shift),
        shading_shift_scale: -1.5,
        shading_toony: 0.7,
        rim: Colour::hex(0x00ff_ffff),
        rim_texture: Some(rim),
        rim_fresnel_power: 2.0,
        rim_lighting_mix: 0.0,
        emissive_texture: Some(emissive),
        ..Mtoon::default()
    };
    let material = mtoon_material(Colour::hex(0x00ee_ddcc), Colour::hex(0x0020_1008), mtoon);
    let lights = vec![key(Vec3::X, 2.0)];
    let f = fan(&mut r, &material, lights.clone(), Vec3::Y, &angles());
    let srgb = |c: [u8; 3]| lin(Colour::rgb(c[0], c[1], c[2]));
    let sampled = Sampled {
        shade: srgb([200, 120, 160]),
        shift: 64.0 / 255.0 * -1.5,
        rim: srgb([255, 180, 90]),
        emissive: srgb([90, 200, 255]),
        matcap: DVec3::ZERO,
    };
    check(&f, &material, sampled, &lights, "textured");
    // Without the textures the same material reads differently, so each one matters.
    let plain = Sampled::default();
    let m = material.mtoon.as_deref().unwrap();
    let (pixel, n, _) = f.probes[9];
    let without =
        encode(expected(m, lin(material.base), lin(Colour::hex(0x0020_1008)), plain, &lights, n, world_of(pixel)));
    assert_ne!(rgb_at(&f.image, pixel), without, "the textures change the surface");
    for t in [shade, shift, rim, emissive] {
        r.release(t.into()).unwrap();
    }
}

#[test]
fn the_matcap_is_read_by_the_view_space_normal_and_added_to_the_rim() {
    let mut r = support::renderer();
    // Left half red, right half blue: a normal turned toward +x reads the right half.
    let matcap = texture(&mut r, ColourSpace::Srgb, (2, 1), &[255, 0, 0, 255, 0, 0, 255, 255]);
    let mtoon = Mtoon {
        matcap: Colour::hex(0x00ff_ffff),
        matcap_texture: Some(matcap),
        rim_lighting_mix: 0.0,
        ..Mtoon::default()
    };
    let material = mtoon_material(Colour::BLACK, Colour::BLACK, mtoon);
    let turns = [-40.0, -25.0, -12.0, 12.0, 25.0, 40.0];
    let f = fan(&mut r, &material, Vec::new(), Vec3::Y, &turns);
    for (i, &(pixel, n, _)) in f.probes.iter().enumerate() {
        // three-vrm's sphere mapping; the view matrix of this camera has no rotation, so view space is world space.
        let view = (EYE.as_dvec3() - world_of(pixel)).normalize();
        let x = DVec3::new(view.z, 0.0, -view.x).normalize();
        let u = 0.5 + 0.5 * x.dot(n);
        let want = if u < 0.5 { [255, 0, 0] } else { [0, 0, 255] };
        assert_close(rgb_at(&f.image, pixel), want, &format!("matcap surface {i}, u {u:.3}"));
    }
    r.release(matcap.into()).unwrap();
}

#[test]
fn the_normal_map_turns_the_normal_along_the_uv_tangents() {
    let mut r = support::renderer();
    let lights = vec![key(Vec3::X, 2.0), key(Vec3::NEG_Y, 1.0)];
    // A flat map, one leaning along +u (toward +x on the quad) and one along glTF's green, -v (toward +y: the quad's v
    // runs down).
    for (texel, scale) in [([128, 128, 255], 1.0), ([255, 128, 128], 0.5), ([128, 255, 128], 1.0)] {
        let map = texture(&mut r, ColourSpace::Linear, (1, 1), &[texel[0], texel[1], texel[2], 255]);
        let mtoon = Mtoon { normal_texture: Some(map), normal_scale: scale, shading_toony: 0.5, ..Mtoon::default() };
        let material = mtoon_material(Colour::WHITE, Colour::BLACK, mtoon);
        let f = fan(&mut r, &material, lights.clone(), Vec3::Y, &[-30.0, 0.0, 30.0]);
        let m = material.mtoon.as_deref().unwrap();
        let map_n = DVec3::from_array(texel.map(|v| f64::from(v) / 255.0 * 2.0 - 1.0));
        let map_n = DVec3::new(map_n.x * f64::from(scale), map_n.y * f64::from(scale), map_n.z);
        for (i, &(pixel, n, rotation)) in f.probes.iter().enumerate() {
            let tangent = rotation.transform_vector3(Vec3::X).as_dvec3();
            let bitangent = rotation.transform_vector3(Vec3::Y).as_dvec3();
            let mapped = (tangent * map_n.x + bitangent * map_n.y + n * map_n.z).normalize();
            let want = expected(m, DVec3::ONE, DVec3::ZERO, Sampled::default(), &lights, mapped, world_of(pixel));
            assert_close(rgb_at(&f.image, pixel), encode(want), &format!("map {texel:?}: surface {i}"));
        }
        r.release(map.into()).unwrap();
    }
}

// ---- UV animation ----

/// The left and right halves of a quad drawn with a red | blue base texture under `mtoon`'s UV animation at `time`,
/// lit so a pixel shows the albedo (an ambient of pi, no rim).
fn halves(r: &mut dyn Renderer, mtoon: Mtoon, time: f64) -> ([u8; 3], [u8; 3]) {
    let atlas = texture(r, ColourSpace::Srgb, (2, 1), &[255, 0, 0, 255, 0, 0, 255, 255]);
    let mut material = mtoon_material(Colour::WHITE, Colour::BLACK, mtoon);
    material.base_texture = Some(atlas);
    let id = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let ambient = Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: std::f32::consts::PI };
    let mut s = scene(vec![ambient]);
    s.frame.time = Seconds(time);
    s.instances.push(Instance::new(mesh, id, Mat4::IDENTITY));
    let image = draw(r, &s);
    for resource in [id.into(), mesh.into(), atlas.into()] {
        r.release(resource).unwrap();
    }
    (rgb_at(&image, pixel_of(-0.2, 0.1)), rgb_at(&image, pixel_of(0.2, 0.1)))
}

#[test]
fn uv_animation_scrolls_and_turns_by_the_scene_time_where_the_mask_lets_it() {
    let mut r = support::renderer();
    let (red, blue) = ([255, 0, 0], [0, 0, 255]);
    let still = halves(&mut r, Mtoon::default(), 1.0);
    assert_eq!(still, (red, blue), "no animation: red | blue");

    let scroll = Mtoon { uv_scroll: Vec2::new(0.5, 0.0), ..Mtoon::default() };
    assert_eq!(halves(&mut r, scroll.clone(), 0.0), (red, blue), "nothing has scrolled at time 0");
    assert_eq!(halves(&mut r, scroll.clone(), 1.0), (blue, red), "half a turn of u after 1 s at 0.5 per second");
    assert_eq!(halves(&mut r, scroll.clone(), 2.0), (red, blue), "a whole one after 2 s");

    let turn = Mtoon { uv_rotation: std::f32::consts::PI, ..Mtoon::default() };
    assert_eq!(halves(&mut r, turn, 1.0), (blue, red), "half a turn about the centre after 1 s at pi per second");

    // A mask whose blue channel is 0 holds the texture still.
    let mask = texture(&mut r, ColourSpace::Linear, (1, 1), &[255, 255, 0, 255]);
    let masked = Mtoon { uv_animation_mask: Some(mask), ..scroll };
    assert_eq!(halves(&mut r, masked, 1.0), (red, blue), "the mask stops the scroll");
    r.release(mask.into()).unwrap();
}

// ---- blended depth writes ----

/// A blended quad near the camera drawn first (order 0) and an opaque-coloured blended quad behind it drawn after
/// (order 1), overlapping in the middle. Returns the pixel in the overlap.
fn overlap(r: &mut dyn Renderer, near_writes_depth: bool) -> [u8; 3] {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();
    let blended = |base: Colour, writes: bool| Material {
        alpha: AlphaMode::Blend,
        ..mtoon_material(base, Colour::BLACK, Mtoon { blend_writes_depth: writes, ..Mtoon::default() })
    };
    let near = r.create_material(&blended(Colour::rgba(255, 0, 0, 128), near_writes_depth)).unwrap();
    let far = r.create_material(&blended(Colour::rgba(0, 0, 255, 255), false)).unwrap();
    let ambient = Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: std::f32::consts::PI };
    let mut s = scene(vec![ambient]);
    let mut front = Instance::new(mesh, near, Mat4::from_translation(Vec3::new(-0.1, 0.0, 0.5)));
    front.order = 0;
    let mut back = Instance::new(mesh, far, Mat4::from_translation(Vec3::new(0.1, 0.0, 0.0)));
    back.order = 1;
    s.instances.extend([front, back]);
    let image = draw(r, &s);
    for resource in [near.into(), far.into(), mesh.into()] {
        r.release(resource).unwrap();
    }
    rgb_at(&image, pixel_of(0.0, 0.0))
}

#[test]
fn a_blended_surface_that_writes_depth_hides_what_is_drawn_behind_it_later() {
    let mut r = support::renderer();
    let without = overlap(&mut r, false);
    let with = overlap(&mut r, true);
    // Without depth writes the blue quad drawn later covers the red one; with them it fails the depth test there and
    // the red one stays, half over the black background.
    assert!(without[2] > 200 && without[0] < 10, "the later blue quad covers: {without:?}");
    assert!(with[0] > 100 && with[2] < 10, "the near red quad hides the blue one: {with:?}");
}

// ---- evidence: five spheres to set beside three-vrm's render of the same scene ----

/// three.js `SphereGeometry(radius, 48, 24)`: the same vertices, normals, UVs and triangles, so a texture lands where
/// three-vrm puts it.
fn sphere(radius: f32) -> omuzaki_render_api::MeshData {
    let (columns, rows) = (48u32, 24u32);
    let mut mesh = omuzaki_render_api::MeshData::default();
    for iy in 0..=rows {
        let v = iy as f32 / rows as f32;
        for ix in 0..=columns {
            let u = ix as f32 / columns as f32;
            let (theta, phi) = (v * std::f32::consts::PI, u * std::f32::consts::TAU);
            let p = Vec3::new(-phi.cos() * theta.sin(), theta.cos(), phi.sin() * theta.sin());
            mesh.positions.push(p * radius);
            mesh.normals.push(p.normalize_or(Vec3::Y));
            mesh.uvs.push(Vec2::new(u, 1.0 - v));
        }
    }
    let at = |ix: u32, iy: u32| iy * (columns + 1) + ix;
    for iy in 0..rows {
        for ix in 0..columns {
            let corners = [at(ix + 1, iy), at(ix, iy), at(ix, iy + 1), at(ix + 1, iy + 1)];
            if iy != 0 {
                mesh.indices.extend([corners[0], corners[1], corners[3]]);
            }
            if iy != rows - 1 {
                mesh.indices.extend([corners[1], corners[2], corners[3]]);
            }
        }
    }
    mesh
}

/// A 64 x 64 texture from `texel(x, y)`, saved beside the render for the three-vrm page as `<name>.png`.
fn evidence_texture(
    r: &mut dyn Renderer,
    out: Option<&std::path::Path>,
    name: &str,
    space: ColourSpace,
    texel: impl Fn(u32, u32) -> [u8; 4],
) -> TextureId {
    let rgba: Vec<u8> = (0..64 * 64).flat_map(|i| texel(i % 64, i / 64)).collect();
    if let Some(dir) = out {
        image::save_buffer(dir.join(format!("{name}.png")), &rgba, 64, 64, image::ColorType::Rgba8).unwrap();
    }
    let desc = TextureDesc {
        colour_space: space,
        mips: false,
        sampler: Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, ..Sampler::default() },
        ..TextureDesc::colour(UVec2::splat(64))
    };
    r.create_texture(&desc, Some(&rgba)).unwrap()
}

/// Five `MToon` spheres under a key light and a hemisphere, at time 0.8 s: the shade and rim; a hard, texture-shifted
/// terminator; the matcap; a normal map with an emissive texture; a scrolling, turning base texture. With
/// `OMUZAKI_RENDER_OUT=<dir>` it saves `mtoon-omuzaki.png` (as the spec says), `mtoon-omuzaki-gi0.png` (GI
/// equalisation off, as three-vrm draws) and the textures. The three-vrm page on the `evidence/render-live` branch
/// (`592/three-vrm-page.html`) draws the same scene from the same numbers and textures.
#[test]
#[allow(clippy::too_many_lines, reason = "one scene, five materials, written out so they read like the three-vrm page")]
fn mtoon_spheres_evidence() {
    let out = std::env::var_os("OMUZAKI_RENDER_OUT").map(std::path::PathBuf::from);
    let out = out.as_deref();
    let mut r = support::renderer();
    let stripes = evidence_texture(&mut r, out, "stripes", ColourSpace::Linear, |_, y| {
        if (y / 8).is_multiple_of(2) { [255, 0, 0, 255] } else { [0, 0, 0, 255] }
    });
    let matcap = evidence_texture(&mut r, out, "matcap", ColourSpace::Srgb, |x, y| {
        let d = Vec2::new(x as f32 / 63.0 - 0.35, y as f32 / 63.0 - 0.3).length();
        let c = (1.0 - d / 0.7).clamp(0.0, 1.0);
        [(30.0 + 225.0 * c) as u8, (30.0 + 170.0 * c) as u8, (40.0 + 100.0 * c) as u8, 255]
    });
    let bumps = evidence_texture(&mut r, out, "bumps", ColourSpace::Linear, |x, y| {
        let nx = 0.5 * (std::f32::consts::TAU * x as f32 / 16.0).sin();
        let ny = 0.5 * (std::f32::consts::TAU * y as f32 / 16.0).cos();
        let nz = (1.0 - nx * nx - ny * ny).sqrt();
        let byte = |v: f32| ((v * 0.5 + 0.5) * 255.0).round() as u8;
        [byte(nx), byte(ny), byte(nz), 255]
    });
    let checker = |a: [u8; 4], b: [u8; 4]| move |x: u32, y: u32| if (x / 8 + y / 8).is_multiple_of(2) { a } else { b };
    let base =
        evidence_texture(&mut r, out, "checker", ColourSpace::Srgb, checker([230, 230, 240, 255], [60, 90, 200, 255]));
    let glow = evidence_texture(&mut r, out, "glow", ColourSpace::Srgb, checker([255, 160, 40, 255], [0, 0, 0, 255]));

    let spheres = [
        (
            Colour::rgb(240, 200, 185),
            Colour::BLACK,
            None,
            Mtoon {
                shade: Colour::rgb(150, 100, 140),
                rim: Colour::rgb(170, 190, 255),
                rim_fresnel_power: 4.0,
                rim_lift: 0.1,
                ..Mtoon::default()
            },
        ),
        (
            Colour::rgb(170, 210, 240),
            Colour::BLACK,
            None,
            Mtoon {
                shade: Colour::rgb(80, 110, 170),
                shading_toony: 0.95,
                shading_shift: 0.1,
                shading_shift_texture: Some(stripes),
                shading_shift_scale: -0.6,
                ..Mtoon::default()
            },
        ),
        (
            Colour::rgb(140, 140, 150),
            Colour::BLACK,
            None,
            Mtoon { matcap_texture: Some(matcap), rim_lighting_mix: 0.5, ..Mtoon::default() },
        ),
        (
            Colour::rgb(200, 220, 200),
            Colour::rgb(120, 60, 0),
            None,
            Mtoon {
                shade: Colour::rgb(90, 120, 100),
                normal_texture: Some(bumps),
                emissive_texture: Some(glow),
                ..Mtoon::default()
            },
        ),
        (
            Colour::WHITE,
            Colour::BLACK,
            Some(base),
            Mtoon {
                shade: Colour::rgb(140, 140, 170),
                uv_scroll: Vec2::new(0.25, 0.0),
                uv_rotation: 0.5,
                ..Mtoon::default()
            },
        ),
    ];
    let camera =
        Camera::look_at(EYE, Vec3::ZERO, Vec3::Y, Projection::Orthographic { height: 2.4, near: 0.1, far: 20.0 });
    let mut s = Scene::new(camera);
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s.frame.time = Seconds(0.8);
    s.lights = vec![
        Light::Directional {
            to_light: Vec3::new(1.0, 1.0, 1.0),
            colour: Colour::rgb(255, 250, 240),
            intensity: 2.5,
            casts_shadow: false,
        },
        Light::Hemisphere { sky: Colour::rgb(200, 210, 235), ground: Colour::rgb(110, 95, 85), intensity: 1.0 },
    ];
    let mesh = r.create_mesh(&sphere(0.6)).unwrap();
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    // As the spec says (GI equalisation 0.9 by default), and with it off, as three-vrm draws it.
    let mut images = Vec::new();
    for (gi, name) in [(None, "mtoon-omuzaki.png"), (Some(0.0), "mtoon-omuzaki-gi0.png")] {
        let mut scene = s.clone();
        let mut materials = Vec::new();
        for (i, (lit, emissive, texture, mtoon)) in spheres.iter().cloned().enumerate() {
            let gi_equalisation = gi.unwrap_or(mtoon.gi_equalisation);
            let mut material = mtoon_material(lit, emissive, Mtoon { gi_equalisation, ..mtoon });
            material.base_texture = texture;
            let id = r.create_material(&material).unwrap();
            let x = -2.8 + 1.4 * i as f32;
            scene.instances.push(Instance::new(mesh, id, Mat4::from_translation(Vec3::new(x, 0.0, 0.0))));
            materials.push(id);
        }
        let drawn = r.render_image(UVec2::new(768, 256), AlphaEncoding::Straight, &scene, &quality).unwrap();
        if let Some(dir) = out {
            image::save_buffer(dir.join(name), &drawn.rgba8, 768, 256, image::ColorType::Rgba8).unwrap();
        }
        for id in materials {
            r.release(id.into()).unwrap();
        }
        images.push(drawn);
    }
    for resource in [mesh.into(), stripes.into(), matcap.into(), bumps.into(), base.into(), glow.into()] {
        r.release(resource).unwrap();
    }
    compare_with_three_vrm(&images[1]);
}

/// Checks the spheres drawn with GI equalisation off against three-vrm's render of the same scene
/// (`tests/golden/mtoon-three-vrm.png`, drawn by the page `mtoon_spheres_evidence` names). Inside each
/// sphere, away from its silhouette (where the two rasterisers cover pixels differently), the mean difference stays
/// under 1 level of 255 and the 99th percentile within 3.
fn compare_with_three_vrm(image: &Image) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/mtoon-three-vrm.png");
    let golden = image::open(&path).expect("the three-vrm reference").to_rgb8();
    assert_eq!(golden.dimensions(), (image.size.x, image.size.y));
    let ours = |x: u32, y: u32| rgb_at(image, (x, y));
    let theirs = |x: u32, y: u32| golden.get_pixel(x, y).0;
    let covered = |x: u32, y: u32| ours(x, y) != [0, 0, 0] && theirs(x, y) != [0, 0, 0];
    for sphere in 0..5u32 {
        let centre = (384.0 + (-2.8 + 1.4 * sphere as f32) * 256.0 / 2.4) as u32;
        let mut differences = Vec::new();
        for y in 1..image.size.y - 1 {
            for x in centre.saturating_sub(70).max(1)..(centre + 70).min(image.size.x - 1) {
                let inside = (y - 1..=y + 1).all(|v| (x - 1..=x + 1).all(|u| covered(u, v)));
                if inside {
                    let (a, b) = (ours(x, y), theirs(x, y));
                    differences.push((0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0));
                }
            }
        }
        assert!(differences.len() > 10_000, "sphere {sphere} covers {} pixels", differences.len());
        differences.sort_unstable();
        let mean = differences.iter().map(|&d| f64::from(d)).sum::<f64>() / differences.len() as f64;
        let p99 = differences[differences.len() * 99 / 100];
        assert!(mean < 1.0 && p99 <= 3, "sphere {sphere}: mean difference {mean:.2}, 99th percentile {p99}");
    }
}
