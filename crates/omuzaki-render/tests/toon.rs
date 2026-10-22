//! Toon shading on the GPU renderer: the terminator ramp with its tinted shade, the rim, the skin treatment
//! and the hemisphere, directional and point lights, checked against an independent f64 implementation of the formulas
//! in `docs/design/render-api.md` (D10: three.js light units) and against the mock where the two should agree.
//!
//! Scenes are small orthographic stills on the software adapter with no tone curve, so a pixel is the linear colour
//! encoded to sRGB and the expected value follows from the formula alone. Each test puts several surfaces in one frame
//! (a quad per normal, per distance) so that one render covers a whole curve.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::path::{Path, PathBuf};

use omuzaki_core::glam::{DVec3, Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, Image, Instance, JointRange, Light, Material, MeshData,
    MeshId, Projection, Quality, Renderer, RendererExt, Rim, Scene, Shading, Skin, Tonemap, Toon,
};
use omuzaki_render_mock::{MockRenderer, shapes};

/// 128 x 128 pixels over a 2 m tall orthographic view: 64 pixels per metre, the origin at pixel (64, 64).
const SIZE: UVec2 = UVec2::new(128, 128);
const PIXELS_PER_METRE: f32 = 64.0;
/// 8-bit rounding in the scene format and the encode; the f64 oracle is exact, the GPU is f32 and f16.
const TOLERANCE: u8 = 2;

fn ortho() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// Exact colours (no tone curve) on a black background.
fn scene(lights: Vec<Light>) -> Scene {
    let mut s = Scene::new(ortho());
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s.lights = lights;
    s
}

/// One sample per pixel, no effects: a pixel is exactly covered or not.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

fn draw(r: &mut dyn Renderer, scene: &Scene) -> Image {
    r.render_image(SIZE, AlphaEncoding::Straight, scene, &plain()).unwrap()
}

fn rgb_at(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let [red, green, blue, _] = image.pixel(x, y).unwrap();
    [red, green, blue]
}

/// The pixel a world point on the z = 0 plane lands on.
fn pixel_of(x: f32, y: f32) -> (u32, u32) {
    ((PIXELS_PER_METRE.mul_add(x, 64.0)).floor() as u32, (PIXELS_PER_METRE.mul_add(-y, 64.0)).floor() as u32)
}

/// The world position of the centre of pixel `(x, y)` on the z = 0 plane.
fn world_of(x: u32, y: u32) -> DVec3 {
    DVec3::new((f64::from(x) + 0.5 - 64.0) / 64.0, (64.0 - f64::from(y) - 0.5) / 64.0, 0.0)
}

fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= TOLERANCE), "{what}: got {got:?}, wanted {want:?}");
}

// ---- the oracle: toon.js in f64, written from the documented formulas rather than from the shader ----

fn lin(c: Colour) -> DVec3 {
    c.to_linear().as_dvec3()
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 {
        return f64::from(x >= e0);
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// three.js `getDistanceAttenuation` with decay 2 and a cutoff distance.
fn three_attenuation(distance: f64, range: f64) -> f64 {
    (1.0 / (distance * distance).max(0.01)) * (1.0 - (distance / range).powi(4)).clamp(0.0, 1.0).powi(2)
}

fn encode(c: DVec3) -> [u8; 3] {
    let channel = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    [channel(c.x), channel(c.y), channel(c.z)]
}

/// What a toon surface of `albedo` (linear) shows at `position` with unit normal `n` seen along `view`.
fn expected(toon: &Toon, albedo: DVec3, lights: &[Light], n: DVec3, position: DVec3, view: DVec3) -> [u8; 3] {
    let shade = lin(toon.shade);
    let (shift, soft) = (f64::from(toon.shift), f64::from(toon.softness));
    let ramp = |n_dot_l: f64| shade.lerp(DVec3::ONE, smoothstep(shift - soft, shift + soft, n_dot_l));
    let mut irradiance = DVec3::ZERO;
    for light in lights {
        irradiance += match *light {
            Light::Directional { to_light, colour, intensity, .. } => {
                lin(colour) * f64::from(intensity) * ramp(n.dot(to_light.as_dvec3().normalize()))
            }
            Light::Point { position: at, colour, intensity, range } => {
                let offset = at.as_dvec3() - position;
                let distance = offset.length();
                lin(colour)
                    * f64::from(intensity)
                    * three_attenuation(distance, f64::from(range))
                    * ramp(n.dot(offset / distance.max(1e-6)))
            }
            Light::Hemisphere { sky, ground, intensity } => {
                lin(ground).lerp(lin(sky), 0.5 + 0.5 * n.y) * f64::from(intensity)
            }
            Light::Spot { position: at, direction, colour, intensity, range, cone_outer_deg, penumbra, .. } => {
                let offset = at.as_dvec3() - position;
                let distance = offset.length();
                let to_light = offset / distance.max(1e-6);
                let outer = f64::from(cone_outer_deg).to_radians();
                let cone = smoothstep(
                    outer.cos(),
                    (outer * (1.0 - f64::from(penumbra))).cos(),
                    (-to_light).dot(direction.as_dvec3().normalize()),
                );
                lin(colour)
                    * f64::from(intensity)
                    * three_attenuation(distance, f64::from(range))
                    * cone
                    * ramp(n.dot(to_light))
            }
        };
    }
    let fresnel = (1.0 - n.dot(view).clamp(0.0, 1.0)).powf(f64::from(toon.rim.power));
    let rim = lin(toon.rim.colour) * fresnel * f64::from(toon.rim.strength);
    let mut colour = albedo * irradiance / std::f64::consts::PI + lin(toon.emissive) + rim;
    if let Some(Skin { floor, hue_keep }) = toon.skin {
        let luma = DVec3::new(0.299, 0.587, 0.114);
        let keep_hue = albedo * (colour.dot(luma) / albedo.dot(luma).max(1e-3));
        colour = colour.lerp(keep_hue, f64::from(hue_keep)).max(albedo * f64::from(floor));
    }
    encode(colour)
}

// ---- scenes ----

fn toon_material(base: Colour, toon: Toon) -> Material {
    Material { shading: Shading::Toon(toon), ..Material::toon(base) }
}

fn quiet_rim(toon: Toon) -> Toon {
    Toon { rim: Rim { strength: 0.0, ..toon.rim }, ..toon }
}

/// A fan of small quads in a row, quad `i` rotated by `turns[i]` about `axis` and placed at `xs[i]`. Returns the image
/// and, for each quad, its centre pixel and its unit normal.
struct Fan {
    image: Image,
    probes: Vec<((u32, u32), DVec3)>,
}

fn fan(r: &mut dyn Renderer, material: &Material, lights: Vec<Light>, axis: Vec3, angles_deg: &[f32]) -> Fan {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.075))).unwrap();
    let material = r.create_material(material).unwrap();
    let mut s = scene(lights);
    let mut probes = Vec::new();
    let columns = 7;
    for (i, &deg) in angles_deg.iter().enumerate() {
        let (row, column) = (i / columns, i % columns);
        let x = -0.84 + 0.28 * column as f32;
        let y = 0.6 - 0.3 * row as f32;
        let rotation = Mat4::from_axis_angle(axis, deg.to_radians());
        s.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, y, 0.0)) * rotation));
        probes.push((pixel_of(x, y), (rotation.transform_vector3(Vec3::Z)).as_dvec3()));
    }
    let image = draw(r, &s);
    r.release(mesh.into()).unwrap();
    r.release(material.into()).unwrap();
    Fan { image, probes }
}

/// Checks every quad of a fan against the oracle for `base` under `toon` and `lights`.
fn check_fan(fan: &Fan, base: Colour, toon: &Toon, lights: &[Light], what: &str) {
    for (i, &((px, py), n)) in fan.probes.iter().enumerate() {
        let position = world_of(px, py);
        let want = expected(toon, lin(base), lights, n, position, DVec3::Z);
        assert_close(rgb_at(&fan.image, px, py), want, &format!("{what}: surface {i}, normal {n:.3}"));
    }
}

fn key_from_the_right(intensity: f32) -> Light {
    Light::Directional { to_light: Vec3::X, colour: Colour::WHITE, intensity, casts_shadow: false }
}

fn angles() -> Vec<f32> {
    (-7..=7).map(|i| i as f32 * 11.0).collect()
}

// ---- the ramp ----

#[test]
fn a_surface_facing_the_light_shows_the_albedo_times_intensity_over_pi() {
    let mut r = support::renderer();
    let base = Colour::rgb(255, 255, 255);
    let toon = quiet_rim(Toon::default());
    let lights =
        vec![Light::Directional { to_light: Vec3::Z, colour: Colour::WHITE, intensity: 2.0, casts_shadow: false }];
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &[0.0]);
    // Key intensity 2 on white gives 2 / pi = 0.6366 linear, the number the prototype's rigs are tuned around.
    let (x, y) = f.probes[0].0;
    assert_close(rgb_at(&f.image, x, y), encode(DVec3::splat(2.0 / std::f64::consts::PI)), "lit face");
    check_fan(&f, base, &toon, &lights, "facing");
}

#[test]
fn the_ramp_follows_the_smoothstep_around_the_terminator() {
    let mut r = support::renderer();
    let base = Colour::hex(0x00c9_b8f0);
    // A tinted shade, a shifted terminator and a wide soft edge, so the curve is easy to tell from a step or a lerp.
    let toon = quiet_rim(Toon { shade: Colour::hex(0x0088_78b8), shift: 0.1, softness: 0.3, ..Toon::default() });
    let lights = vec![key_from_the_right(2.0)];
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&f, base, &toon, &lights, "soft ramp");

    // Different pixels, so a flat or a stepped result could not pass: the first and last surfaces differ widely.
    let (first, last) =
        (rgb_at(&f.image, f.probes[0].0.0, f.probes[0].0.1), rgb_at(&f.image, f.probes[14].0.0, f.probes[14].0.1));
    assert!(last[0] > first[0] + 60, "lit side {last:?} is far brighter than the shade side {first:?}");
}

#[test]
fn zero_softness_is_a_hard_edge_and_the_shade_is_a_tint_not_a_grey() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon { shade: Colour::hex(0x00d7_a3a8), softness: 0.0, ..Toon::default() });
    let lights = vec![key_from_the_right(2.0)];
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&f, base, &toon, &lights, "hard edge");

    let (shade_side, lit_side) = (f.probes[6].0, f.probes[8].0);
    let [r1, g1, b1] = rgb_at(&f.image, shade_side.0, shade_side.1);
    assert!(r1 > g1 + 5 && r1 > b1 + 5, "the shadow side is pink, not grey: {:?}", [r1, g1, b1]);
    let [r2, g2, b2] = rgb_at(&f.image, lit_side.0, lit_side.1);
    assert!(r2.abs_diff(g2) <= 2 && g2.abs_diff(b2) <= 2, "the lit side keeps the white light white");
}

#[test]
fn every_light_shapes_the_ramp_the_same_way() {
    let mut r = support::renderer();
    let base = Colour::hex(0x00f9_d3c5);
    let toon = quiet_rim(Toon::default());
    let lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
        key_from_the_right(1.2),
        Light::Directional {
            to_light: Vec3::new(-0.6, 0.3, 0.8),
            colour: Colour::hex(0x00ff_d9c4),
            intensity: 0.7,
            casts_shadow: false,
        },
        Light::Point {
            position: Vec3::new(-0.2, 0.4, 0.9),
            colour: Colour::hex(0x00ff_a25a),
            intensity: 3.0,
            range: 5.0,
        },
    ];
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&f, base, &toon, &lights, "mixed lights");
}

// ---- hemisphere, point lights ----

#[test]
fn the_hemisphere_blends_sky_to_ground_by_the_normals_height() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon::default());
    let lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00ff_ffff), ground: Colour::hex(0x0066_6677), intensity: 1.6 }];
    // Rotating about X tilts the normal up and down: n.y runs from about -0.98 to 0.98.
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::X, &angles());
    check_fan(&f, base, &toon, &lights, "hemisphere");
    let (up, down) = (f.probes[0].0, f.probes[14].0);
    let (up, down) = (rgb_at(&f.image, up.0, up.1), rgb_at(&f.image, down.0, down.1));
    assert!(up[0] > down[0] + 20, "a surface facing up shows the sky, one facing down the ground: {up:?} vs {down:?}");
}

#[test]
fn point_lights_use_the_three_js_distance_attenuation() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon::default());
    // Quads face +Z in a row; the light sits 0.5 m in front of the middle one, so the distance grows to either side and
    // the outermost quads are past the range of 0.8 m.
    let lights =
        vec![Light::Point { position: Vec3::new(0.0, 0.6, 0.5), colour: Colour::WHITE, intensity: 0.8, range: 0.8 }];
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &[0.0; 7]);
    check_fan(&f, base, &toon, &lights, "point falloff");
    let out_of_range = f.probes[6].0;
    assert_eq!(rgb_at(&f.image, out_of_range.0, out_of_range.1), [0, 0, 0], "no light is left at the range");
}

/// A spot light 1 m in front of a row of quads at its axis: inside the cone, across the penumbra and outside.
fn spot_over_the_fan(penumbra: f32) -> Light {
    Light::Spot {
        position: Vec3::new(0.0, 0.3, 1.0),
        direction: Vec3::new(0.0, 0.0, -2.0),
        colour: Colour::hex(0x00ff_e8c8),
        intensity: 2.5,
        range: 4.0,
        cone_outer_deg: 35.0,
        penumbra,
        casts_shadow: false,
    }
}

#[test]
fn spot_lights_are_point_lights_confined_to_a_three_js_cone() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon::default());
    for penumbra in [0.0, 0.5, 1.0] {
        let lights = vec![spot_over_the_fan(penumbra)];
        let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &[0.0; 21]);
        check_fan(&f, base, &toon, &lights, &format!("spot, penumbra {penumbra}"));
    }
    // Pixel by pixel the extremes differ: the quad on the axis is lit, the far corner is outside the 35 degree cone.
    let lights = vec![spot_over_the_fan(0.5)];
    let f = fan(&mut r, &toon_material(base, toon), lights, Vec3::Y, &[0.0; 21]);
    let (axis, corner) = (f.probes[10].0, f.probes[0].0);
    assert!(rgb_at(&f.image, axis.0, axis.1)[0] > 60, "inside the cone");
    assert_eq!(rgb_at(&f.image, corner.0, corner.1), [0, 0, 0], "outside the cone");
}

#[test]
fn spots_take_the_toon_ramp_like_the_other_lights() {
    let mut r = support::renderer();
    let base = Colour::hex(0x00f9_d3c5);
    let toon = quiet_rim(Toon { shade: Colour::hex(0x00d7_a3a8), shift: 0.1, softness: 0.2, ..Toon::default() });
    let lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.5 },
        spot_over_the_fan(1.0),
    ];
    // Tilted quads, so the spot's contribution crosses the terminator as well as its own cone. The full penumbra keeps
    // the light smooth: a tilted quad's pixel is a little off the z = 0 plane the oracle assumes, which a hard edge
    // would amplify.
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&f, base, &toon, &lights, "spot through the ramp");
}

/// The prototype's candle rig (`viewer-candle` in the JS golden manifest): hemisphere, a flame key, a cool rim, a warm
/// fill and the candle's point light, distance 7, decay 2, intensity 4.84.
fn candle_rig() -> Vec<Light> {
    let directional = |to_light: Vec3, colour: u32, intensity: f32| Light::Directional {
        to_light,
        colour: Colour::hex(colour),
        intensity,
        casts_shadow: false,
    };
    vec![
        Light::Hemisphere { sky: Colour::hex(0x008a_7f9a), ground: Colour::hex(0x002a_1a10), intensity: 0.42 },
        directional(Vec3::new(1.65, 2.209, -0.55), 0x00ff_c28a, 1.6),
        directional(Vec3::new(-2.5, 2.2, -2.8), 0x008f_a8ff, 1.0),
        directional(Vec3::new(-2.0, 1.0, 2.5), 0x00ff_e2c4, 0.25),
        Light::Point {
            position: Vec3::new(1.45, 1.239, -1.15),
            colour: Colour::hex(0x00ff_a25a),
            intensity: 4.84,
            range: 7.0,
        },
    ]
}

#[test]
fn the_candle_rig_lights_a_skin_like_three_js_would() {
    let mut r = support::renderer();
    let base = Colour::hex(0x00f9_d3c5);
    let toon = Toon {
        shade: Colour::hex(0x00d7_a3a8),
        shift: -0.28,
        softness: 0.28,
        skin: Some(Skin { floor: 0.42, hue_keep: 0.55 }),
        ..Toon::default()
    };
    let lights = candle_rig();
    let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&f, base, &toon, &lights, "candle rig");

    // The point light alone, at a distance where the window and the inverse square both matter: 4.84 candela at 1.6 m.
    let Light::Point { colour, intensity, range, .. } = lights[4] else { unreachable!("the rig ends with the candle") };
    let in_front = Light::Point { position: Vec3::new(0.9, 0.3, 1.2), colour, intensity, range };
    let plain_toon = quiet_rim(Toon::default());
    let alone = fan(&mut r, &toon_material(Colour::WHITE, plain_toon), vec![in_front], Vec3::Y, &[0.0; 7]);
    check_fan(&alone, Colour::WHITE, &plain_toon, &[in_front], "candle alone");
}

// ---- rim, emissive, skin ----

#[test]
fn the_rim_is_a_fresnel_term_added_after_lighting() {
    let mut r = support::renderer();
    // No lights, so what is left is emissive plus rim: exactly the formula, and nothing else to explain a mismatch.
    let toon = Toon {
        rim: Rim { colour: Colour::hex(0x00ff_c8a0), power: 2.0, strength: 0.8 },
        emissive: Colour::hex(0x0010_0820),
        ..Toon::default()
    };
    let f = fan(&mut r, &toon_material(Colour::WHITE, toon), Vec::new(), Vec3::Y, &angles());
    check_fan(&f, Colour::WHITE, &toon, &[], "rim");
    let (head_on, grazing) = (f.probes[7].0, f.probes[0].0);
    let (head_on, grazing) = (rgb_at(&f.image, head_on.0, head_on.1), rgb_at(&f.image, grazing.0, grazing.1));
    assert!(grazing[0] > head_on[0] + 30, "the rim grows toward the silhouette: {head_on:?} then {grazing:?}");
}

#[test]
fn a_perspective_camera_takes_the_view_direction_from_each_pixel() {
    let mut r = support::renderer();
    let toon = Toon { rim: Rim { colour: Colour::WHITE, power: 1.0, strength: 1.0 }, ..Toon::default() };
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.1))).unwrap();
    let material = r.create_material(&toon_material(Colour::WHITE, toon)).unwrap();
    let mut s = scene(Vec::new());
    s.camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 90.0, near: 0.1, far: 20.0 },
    );
    // A quad facing +Z far to the side is seen at an angle by a camera in front of the origin.
    s.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(1.2, 0.0, 0.0))));
    let image = draw(&mut r, &s);
    // fov 90 at distance 2: the view spans +-2 m at z = 0, so x = 1.2 m is 0.6 of the way to the edge.
    let x = (64.0 + 0.6 * 64.0) as u32;
    let world_x = (f64::from(x) + 0.5 - 64.0) / 64.0 * 2.0;
    let at = DVec3::new(world_x, 0.0, 0.0);
    let want = expected(&toon, DVec3::ONE, &[], DVec3::Z, at, (DVec3::new(0.0, 0.0, 2.0) - at).normalize());
    assert_close(rgb_at(&image, x, 64), want, "rim from the pixel's own view vector");
}

#[test]
fn skin_keeps_its_floor_and_its_hue_under_coloured_light() {
    let mut r = support::renderer();
    let base = Colour::hex(0x00f9_d3c5);
    let skin =
        Toon { shade: Colour::hex(0x00d7_a3a8), skin: Some(Skin { floor: 0.42, hue_keep: 0.55 }), ..Toon::default() };
    let plain_toon = Toon { skin: None, ..skin };
    // Deep blue light from the right: without the skin treatment she turns blue, with it the skin hue comes back.
    let blue =
        Light::Directional { to_light: Vec3::X, colour: Colour::hex(0x0040_60ff), intensity: 2.0, casts_shadow: false };
    let lights = vec![blue];
    let with = fan(&mut r, &toon_material(base, skin), lights.clone(), Vec3::Y, &angles());
    check_fan(&with, base, &skin, &lights, "skin");
    let without = fan(&mut r, &toon_material(base, plain_toon), lights.clone(), Vec3::Y, &angles());
    check_fan(&without, base, &plain_toon, &lights, "no skin");

    let lit = with.probes[13].0;
    let [skin_r, _, skin_b] = rgb_at(&with.image, lit.0, lit.1);
    let [plain_r, _, plain_b] = rgb_at(&without.image, lit.0, lit.1);
    assert!(
        i32::from(skin_r) - i32::from(skin_b) > i32::from(plain_r) - i32::from(plain_b) + 30,
        "the skin keeps its red"
    );

    // In the shade (light behind the surface) skin never goes below the floor of the albedo: 0.42 of linear 0.95.
    let dark = with.probes[0].0;
    let [dark_r, ..] = rgb_at(&with.image, dark.0, dark.1);
    assert!(dark_r >= encode(DVec3::splat(0.42 * 0.95))[0].saturating_sub(4), "floored: {dark_r}");
}

/// Tone 12 (the deepest) and tone 01 (the lightest) of the curated skin range (neutral undertone), as a head part
/// builds them: the skin colour, `SkinSlots::toon_shade()` as the shade multiplier, the rim slot and the tone's params.
/// Using the shade colour itself as the multiplier would multiply a deep skin by a dark colour and crush it to black.
const SKIN_TONES: [(&str, u32, u32, u32, f32, f32); 3] = [
    ("tone-01", 0x00fd_d2bd, 0x00f7_d7d9, 0x00ff_e6d6, 0.32, 0.72),
    ("tone-06", 0x00b9_866b, 0x00e7_c1bc, 0x00e6_b493, 0.40, 0.68),
    ("tone-12", 0x005c_3d30, 0x00d4_a9a0, 0x00a9_8875, 0.62, 0.58),
];

#[test]
fn deep_skin_tones_shade_with_toon_shade_and_never_crush_to_black() {
    let mut r = support::renderer();
    // The head's look in the prototype: terminator at -0.28, soft 0.28, rim 0.10; the studio key from the right.
    let lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
        key_from_the_right(1.7),
    ];
    for (id, skin, shade, rim, floor, hue_keep) in SKIN_TONES {
        let base = Colour::hex(skin);
        let toon = Toon {
            shade: Colour::hex(shade),
            shift: -0.28,
            softness: 0.28,
            rim: Rim { colour: Colour::hex(rim), power: 3.0, strength: 0.10 },
            emissive: Colour::BLACK,
            skin: Some(Skin { floor, hue_keep }),
        };
        let f = fan(&mut r, &toon_material(base, toon), lights.clone(), Vec3::Y, &angles());
        check_fan(&f, base, &toon, &lights, id);

        // The shadow side keeps the skin's colour (the floor), and the lit side is brighter than the shade by a visible
        // step, on every tone, the deepest included.
        let luma = |p: [u8; 3]| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
        let (dark, lit) = (f.probes[0].0, f.probes[14].0);
        let (dark, lit) = (rgb_at(&f.image, dark.0, dark.1), rgb_at(&f.image, lit.0, lit.1));
        assert!(luma(dark) > 12.0, "{id}: the shadow side is not black: {dark:?}");
        assert!(luma(lit) > luma(dark), "{id}: lit {lit:?} reads brighter than shade {dark:?}");
    }
}

// ---- the sphere ----

fn uv_sphere(radius: f32, rings: u32, segments: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let (sin_t, cos_t) = (ring as f32 / rings as f32 * std::f32::consts::PI).sin_cos();
        for segment in 0..=segments {
            let (sin_p, cos_p) = (segment as f32 / segments as f32 * std::f32::consts::TAU).sin_cos();
            let n = Vec3::new(sin_t * cos_p, cos_t, sin_t * sin_p);
            mesh.positions.push(n * radius);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(segment as f32 / segments as f32, ring as f32 / rings as f32));
        }
    }
    let stride = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let a = ring * stride + segment;
            mesh.indices.extend([a, a + 1, a + stride, a + 1, a + stride + 1, a + stride]);
        }
    }
    mesh
}

#[test]
fn a_sphere_shows_the_ramp_pixel_by_pixel() {
    let mut renderer = support::renderer();
    let base = Colour::hex(0x00f9_d3c5);
    let toon = Toon {
        shade: Colour::hex(0x00d7_a3a8),
        shift: -0.28,
        softness: 0.28,
        skin: Some(Skin { floor: 0.42, hue_keep: 0.55 }),
        ..Toon::default()
    };
    let lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
        Light::Directional {
            to_light: Vec3::new(0.5, 0.6, 0.62),
            colour: Colour::hex(0x00ff_f6ec),
            intensity: 1.7,
            casts_shadow: false,
        },
    ];
    let radius = 0.8_f32;
    let mesh = renderer.create_mesh(&uv_sphere(radius, 96, 192)).unwrap();
    let material = renderer.create_material(&toon_material(base, toon)).unwrap();
    let mut frame = scene(lights.clone());
    frame.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let image = draw(&mut renderer, &frame);

    let (mut compared, mut worst, mut sum) = (0_u32, 0_u8, 0_u64);
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let point = world_of(x, y);
            let radius = f64::from(radius);
            let squared = (point.x * point.x + point.y * point.y) / (radius * radius);
            if squared > 0.9 * 0.9 {
                continue; // the silhouette and the polygons' own error live out there
            }
            let normal =
                DVec3::new(point.x, point.y, (radius * radius - point.x * point.x - point.y * point.y).sqrt()) / radius;
            let surface = DVec3::new(point.x, point.y, normal.z * radius);
            let want = expected(&toon, lin(base), &lights, normal, surface, DVec3::Z);
            let got = rgb_at(&image, x, y);
            let error = got.iter().zip(want).map(|(g, w)| g.abs_diff(w)).max().unwrap_or(0);
            worst = worst.max(error);
            sum += u64::from(error);
            compared += 1;
        }
    }
    let mean = sum as f64 / f64::from(compared);
    println!("sphere vs oracle: {compared} pixels, mean {mean:.3}, worst {worst} levels");
    // The mesh has 18k triangles, so interpolated normals follow the exact ones to a fraction of a level except where
    // the soft edge is steepest.
    assert!(worst <= 3 && mean < 0.3, "worst {worst}, mean {mean}");
}

// ---- surfaces and transforms ----

#[test]
fn double_sided_backs_are_lit_by_their_own_flipped_normal() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon { shade: Colour::hex(0x0060_6090), softness: 0.0, ..Toon::default() });
    let lights =
        vec![Light::Directional { to_light: Vec3::Z, colour: Colour::WHITE, intensity: 2.0, casts_shadow: false }];
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();
    let mut sheet = toon_material(base, toon);
    sheet.double_sided = true;
    let (sheet, one_sided) =
        (r.create_material(&sheet).unwrap(), r.create_material(&toon_material(base, toon)).unwrap());

    // The quad turned around faces -Z; the camera sees its back.
    let mut s = scene(lights.clone());
    s.instances.push(Instance::new(
        mesh,
        sheet,
        Mat4::from_translation(Vec3::new(-0.5, 0.0, 0.0)) * Mat4::from_rotation_y(std::f32::consts::PI),
    ));
    s.instances.push(Instance::new(
        mesh,
        one_sided,
        Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0)) * Mat4::from_rotation_y(std::f32::consts::PI),
    ));
    let image = draw(&mut r, &s);

    let (px, py) = pixel_of(-0.5, 0.0);
    // Lit as a face toward the viewer (+Z), which faces the light: full brightness, not the shade.
    assert_close(
        rgb_at(&image, px, py),
        expected(&toon, DVec3::ONE, &lights, DVec3::Z, world_of(px, py), DVec3::Z),
        "back of a sheet",
    );
    let (px, py) = pixel_of(0.5, 0.0);
    assert_eq!(rgb_at(&image, px, py), [0, 0, 0], "a one-sided surface seen from behind is culled");
}

#[test]
fn skinned_normals_follow_the_joints() {
    let mut r = support::renderer();
    let base = Colour::WHITE;
    let toon = quiet_rim(Toon { shade: Colour::hex(0x0060_6090), softness: 0.0, ..Toon::default() });
    let lights = vec![key_from_the_right(2.0)];
    let mut quad = shapes::quad(Vec2::splat(0.3));
    quad.joints = vec![[0, 0, 0, 0]; 4];
    quad.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    let mesh = r.create_mesh(&quad).unwrap();
    let material = r.create_material(&toon_material(base, toon)).unwrap();

    let angle = 50.0_f32.to_radians();
    let mut s = scene(lights.clone());
    s.joints.push(Mat4::from_rotation_y(-angle)); // turns the quad away from the light
    s.joints.push(Mat4::from_rotation_y(angle)); // and toward it
    for (i, x) in [-0.5_f32, 0.5].into_iter().enumerate() {
        let mut inst = Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, 0.0, 0.0)));
        inst.skin = Some(JointRange { start: u32::try_from(i).unwrap(), len: 1 });
        s.instances.push(inst);
    }
    let image = draw(&mut r, &s);
    for (x, deg) in [(-0.5_f32, -50.0_f32), (0.5, 50.0)] {
        let (px, py) = pixel_of(x, 0.0);
        let n = Mat4::from_rotation_y(deg.to_radians()).transform_vector3(Vec3::Z).as_dvec3();
        assert_close(
            rgb_at(&image, px, py),
            expected(&toon, DVec3::ONE, &lights, n, world_of(px, py), DVec3::Z),
            "skinned",
        );
    }
    let (a, b) = (pixel_of(-0.5, 0.0), pixel_of(0.5, 0.0));
    assert!(rgb_at(&image, b.0, b.1)[0] > rgb_at(&image, a.0, a.1)[0] + 40, "the joint decides which way it faces");
}

#[test]
fn vertex_colours_tint_the_albedo_before_lighting() {
    let mut r = support::renderer();
    let toon = quiet_rim(Toon::default());
    let lights =
        vec![Light::Directional { to_light: Vec3::Z, colour: Colour::WHITE, intensity: 2.0, casts_shadow: false }];
    let mut quad = shapes::quad(Vec2::splat(0.3));
    quad.colours = vec![[0.5, 0.25, 1.0, 1.0]; 4];
    let mesh = r.create_mesh(&quad).unwrap();
    let mut material = toon_material(Colour::hex(0x00c9_b8f0), toon);
    material.vertex_colours = true;
    let material = r.create_material(&material).unwrap();
    let mut s = scene(lights.clone());
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let image = draw(&mut r, &s);
    let albedo = lin(Colour::hex(0x00c9_b8f0)) * DVec3::new(0.5, 0.25, 1.0);
    assert_close(
        rgb_at(&image, 64, 64),
        expected(&toon, albedo, &lights, DVec3::Z, world_of(64, 64), DVec3::Z),
        "vertex colours",
    );
}

#[test]
fn masked_pixels_below_the_cutoff_are_discarded_and_the_rest_are_lit() {
    let mut r = support::renderer();
    let toon = quiet_rim(Toon::default());
    let lights =
        vec![Light::Directional { to_light: Vec3::Z, colour: Colour::WHITE, intensity: 2.0, casts_shadow: false }];
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.2))).unwrap();
    let mut s = scene(lights.clone());
    for (x, alpha) in [(-0.5_f32, 60_u8), (0.5, 220)] {
        let mut material = toon_material(Colour::rgba(255, 200, 150, alpha), toon);
        material.alpha = AlphaMode::Mask { cutoff: 0.5 };
        let material = r.create_material(&material).unwrap();
        s.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, 0.0, 0.0))));
    }
    let image = draw(&mut r, &s);
    let (px, py) = pixel_of(-0.5, 0.0);
    assert_eq!(rgb_at(&image, px, py), [0, 0, 0], "alpha 60 is below the cutoff");
    let (px, py) = pixel_of(0.5, 0.0);
    let want = expected(&toon, lin(Colour::rgb(255, 200, 150)), &lights, DVec3::Z, world_of(px, py), DVec3::Z);
    assert_close(rgb_at(&image, px, py), want, "alpha 220 stays and is lit (and opaque)");
    assert_eq!(image.pixel(px, py).unwrap()[3], 255);
}

#[test]
fn unlit_materials_ignore_every_light() {
    let mut r = support::renderer();
    let lights =
        vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: 3.0 }, key_from_the_right(5.0)];
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::rgb(40, 90, 200))).unwrap();
    let mut s = scene(lights);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let image = draw(&mut r, &s);
    assert_close(rgb_at(&image, 64, 64), [40, 90, 200], "flat colour");
}

#[test]
fn eight_lights_are_all_counted() {
    let mut r = support::renderer();
    let toon = quiet_rim(Toon::default());
    let lights: Vec<Light> = (0..8u8)
        .map(|i| Light::Point {
            position: Vec3::new(0.0, 0.0, 0.7 + 0.05 * f32::from(i)),
            colour: Colour::hex(0x00ff_e0c0),
            intensity: 0.1,
            range: 6.0,
        })
        .collect();
    let f = fan(&mut r, &toon_material(Colour::WHITE, toon), lights.clone(), Vec3::Y, &[0.0, 30.0, -30.0]);
    check_fan(&f, Colour::WHITE, &toon, &lights, "eight lights");
}

// ---- the mock and the contract's golden cube ----

/// The mock's own golden scene: a lavender cube under a warm key, a cool fill and a hemisphere, no outline (that is the
/// Outlines pass's).
fn cube_scene(r: &mut dyn Renderer, mesh: MeshId) -> Scene {
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.shading = Shading::Toon(Toon { shade: Colour::hex(0x0088_78b8), ..Toon::default() });
    let material = r.create_material(&material).unwrap();
    let mut s =
        Scene::new(Camera::look_at(Vec3::new(2.4, 1.8, 3.2), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    s.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    // Exact lit colours: the mock's tone curve is a different fit of ACES from the one the GPU renderer shares with
    // three.js (see `the_cube_matches_the_mocks_golden_inside_its_faces`), so the two are compared before tone mapping.
    s.post.tonemap = Tonemap::None;
    s.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.5 },
        Light::Directional {
            to_light: Vec3::new(-0.8, 1.6, 2.2),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.0,
            casts_shadow: false,
        },
        Light::Directional {
            to_light: Vec3::new(1.8, 0.6, 1.5),
            colour: Colour::hex(0x00ff_d9c4),
            intensity: 0.55,
            casts_shadow: false,
        },
    ];
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.4)));
    s
}

/// The pixels whose 5 x 5 neighbourhood is one flat colour in `reference`: the inside of the faces, away from edges.
fn flat_pixels(reference: &Image) -> Vec<(u32, u32)> {
    let at = |x: u32, y: u32| reference.pixel(x, y).unwrap();
    let mut flat = Vec::new();
    for y in 2..reference.size.y - 2 {
        for x in 2..reference.size.x - 2 {
            let centre = at(x, y);
            let same = (y - 2..=y + 2)
                .all(|j| (x - 2..=x + 2).all(|i| at(i, j).iter().zip(centre).all(|(a, b)| a.abs_diff(b) <= 2)));
            if same {
                flat.push((x, y));
            }
        }
    }
    flat
}

fn max_difference(a: &Image, b: &Image, pixels: &[(u32, u32)]) -> u8 {
    pixels
        .iter()
        .map(|&(x, y)| {
            a.pixel(x, y).unwrap().iter().zip(b.pixel(x, y).unwrap()).map(|(p, q)| p.abs_diff(q)).max().unwrap()
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn the_cube_matches_the_mocks_golden_inside_its_faces() {
    const CUBE: UVec2 = UVec2::new(192, 192);
    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../omuzaki-render-mock/tests/golden/cube.png");
    let golden = image::open(&golden_path).unwrap().to_rgba8();
    let golden = Image::new(CUBE, AlphaEncoding::Straight, golden.into_raw()).unwrap();

    let mut gpu = support::renderer();
    let mesh = gpu.create_mesh(&shapes::cuboid(Vec3::splat(0.5))).unwrap();
    // The golden is drawn with the default tone map, which the mock and the GPU now share (three.js's ACES, #630).
    let mut scene = cube_scene(&mut gpu, mesh);
    scene.post.tonemap = Tonemap::Aces;
    let image = gpu.render_image(CUBE, AlphaEncoding::Straight, &scene, &Quality::offline()).unwrap();
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let image_path = PathBuf::from(dir).join("toon-cube-gpu.png");
        image::save_buffer(image_path, &image.rgba8, CUBE.x, CUBE.y, image::ColorType::Rgba8).unwrap();
    }

    // The golden has the mock's outline, and the mock lights per triangle (its rim is taken at the centroid): away from
    // the edges the faces are flat colours that the GPU's per-pixel rim differs from by a few levels at most.
    let inside = flat_pixels(&golden);
    assert!(inside.len() > 5000, "the faces cover many flat pixels: {}", inside.len());
    let worst = max_difference(&golden, &image, &inside);
    println!("cube vs golden: {} flat pixels, worst difference {worst} levels", inside.len());
    assert!(worst <= 2, "the GPU cube differs from the mock's golden by up to {worst} levels");
}

#[test]
fn the_gpu_and_the_mock_agree_on_mirrored_and_rotated_cubes() {
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    let render_on = |r: &mut dyn Renderer| {
        let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.5))).unwrap();
        let mut s = cube_scene(r, mesh);
        // A second, mirrored cube beside the first: its lit faces must be the outside ones.
        let material = s.instances[0].material;
        s.instances[0].transform = Mat4::from_translation(Vec3::new(-0.6, 0.0, 0.0))
            * Mat4::from_rotation_y(0.4)
            * Mat4::from_scale(Vec3::splat(0.55));
        s.instances.push(Instance::new(
            mesh,
            material,
            Mat4::from_translation(Vec3::new(0.6, 0.0, 0.0))
                * Mat4::from_rotation_y(0.4)
                * Mat4::from_scale(Vec3::new(-0.55, 0.55, 0.55)),
        ));
        s.camera = Camera::look_at(Vec3::new(0.0, 0.6, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection);
        r.render_image(UVec2::new(256, 256), AlphaEncoding::Straight, &s, &Quality { samples: 1, ..Quality::offline() })
            .unwrap()
    };
    let (a, b) = (render_on(&mut gpu), render_on(&mut mock));
    let inside = flat_pixels(&b);
    assert!(inside.len() > 3000, "{}", inside.len());
    let worst = max_difference(&a, &b, &inside);
    println!("two cubes, GPU vs mock: {} flat pixels, worst {worst}", inside.len());
    assert!(worst <= 2, "mirrored and rotated cubes differ from the mock by {worst} levels");
}

/// The point light's direction term, GPU against the mock. A quad tilted about X (its normal has N.L of about 0.3 to the
/// light) with a non-white shade, so the terminator ramp is mid-way: the mock's ramp reads `n . offset` at each
/// triangle's centroid, and a flipped sign would show as the shade where the surface is lit. The light is 20 m away so
/// its direction barely changes across the quad, which keeps the mock's flat triangle and the GPU's per-pixel light
/// within the usual tolerance.
#[test]
fn a_point_light_shades_a_tilted_quad_the_same_on_the_gpu_and_the_mock() {
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    let toon = quiet_rim(Toon { shade: Colour::hex(0x0088_78b8), shift: 0.3, softness: 0.2, ..Toon::default() });
    let material = toon_material(Colour::hex(0x00c9_b8f0), toon);
    let light = Light::Point {
        position: Vec3::new(0.0, 4.0, 20.0),
        colour: Colour::hex(0x00ff_d9c4),
        intensity: 800.0,
        range: 40.0,
    };
    let render_on = |r: &mut dyn Renderer| {
        let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.15))).unwrap();
        let material = r.create_material(&material).unwrap();
        let mut s = scene(vec![light]);
        s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_x(60f32.to_radians())));
        draw(r, &s)
    };
    let (a, b) = (render_on(&mut gpu), render_on(&mut mock));
    let inside = flat_pixels(&b);
    assert!(inside.len() > 100, "the tilted quad covers flat pixels: {}", inside.len());
    let worst = max_difference(&a, &b, &inside);
    let lit = rgb_at(&b, 64, 64);
    println!(
        "tilted quad under a point light, GPU vs mock: {} flat pixels, worst {worst}, centre {lit:?}",
        inside.len()
    );
    assert!(worst <= 2, "the point light's shading differs between the GPU and the mock by {worst} levels");
}

#[test]
fn rendering_a_lit_frame_twice_gives_identical_bytes() {
    let mut r: WgpuRenderer = support::renderer();
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.5))).unwrap();
    let s = cube_scene(&mut r, mesh);
    let quality = Quality::offline();
    let a = r.render_image(UVec2::new(96, 96), AlphaEncoding::Straight, &s, &quality).unwrap();
    let b = r.render_image(UVec2::new(96, 96), AlphaEncoding::Straight, &s, &quality).unwrap();
    assert_eq!(a, b);
}
