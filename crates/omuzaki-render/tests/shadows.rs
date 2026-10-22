//! The Shadows pass on the software adapter: where the shadow falls, that it is clean (no acne, no gap at the
//! feet), that it does not shimmer, how the live interval reuses the map, a map per casting light, and goldens of a small
//! stage.
//!
//! Most checks are *differences between renders of one scene*: the same frame with the key light removed is exactly what
//! a fully shadowed pixel must show, and the same frame with shadows off is what a lit pixel must show, so the expected
//! colours need no oracle. Pixels are chosen from the geometry (a box, a light direction) with a margin of a few pixels
//! from every edge, because the edge itself is filtered.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Pixel counts and coordinates are small; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, Image, Instance, JointRange, Light, Material, MeshData,
    MeshId, Pass, Projection, Quality, QualityMode, Renderer, RendererExt, Rim, Scene, ShadowQuality, TargetDesc,
    Tonemap, Toon,
};
use omuzaki_render_mock::shapes;

/// Software Vulkan JIT-compiles every pipeline on the calling thread; one GPU test at a time keeps it steady.
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 128 x 128 over a 2 m orthographic view: 64 pixels per metre.
const SIZE: UVec2 = UVec2::new(128, 128);
const PPM: f32 = 64.0;

/// One sample per pixel and no effects, in the given shadow quality and mode.
fn quality(shadows: ShadowQuality) -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), shadows, ..Quality::offline() }
}

fn live(shadows: ShadowQuality, interval: u8) -> Quality {
    Quality { mode: QualityMode::Live, shadow_interval: interval, ..quality(shadows) }
}

fn rgb_at(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let [red, green, blue, _] = image.pixel(x, y).unwrap();
    [red, green, blue]
}

fn close(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

/// Pixels whose colour differs by more than `tolerance` in any channel.
fn differing(a: &Image, b: &Image, tolerance: u8) -> usize {
    assert_eq!(a.size, b.size);
    a.rgba8
        .chunks(4)
        .zip(b.rgba8.chunks(4))
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > tolerance))
        .count()
}

// ---- geometry ----

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

/// A toon material with the prototype skin's pink shade and no rim, so the shadow is all that varies.
fn toon(colour: Colour) -> Material {
    let mut m = Material::toon(colour);
    m.shading = omuzaki_render_api::Shading::Toon(Toon {
        shade: Colour::hex(0x00d7_a3a8),
        rim: Rim { strength: 0.0, ..Rim::default() },
        ..Toon::default()
    });
    m
}

/// The meshes and materials of a test, on one renderer.
struct World {
    r: WgpuRenderer,
    cube: MeshId,
    ball: MeshId,
    ground: MeshId,
    cube_material: omuzaki_render_api::MaterialId,
    ground_material: omuzaki_render_api::MaterialId,
}

impl World {
    fn new() -> Self {
        let mut r = support::renderer();
        let cube = r.create_mesh(&shapes::cuboid(Vec3::splat(0.2))).unwrap();
        let ball = r.create_mesh(&sphere(0.3, 48, 24)).unwrap();
        let ground = r.create_mesh(&disc(3.0, 96)).unwrap();
        let cube_material = r.create_material(&toon(Colour::hex(0x00c9_b8f0))).unwrap();
        let ground_material = r.create_material(&toon(Colour::hex(0x00e8_e4ee))).unwrap();
        Self { r, cube, ball, ground, cube_material, ground_material }
    }

    /// The ground (which does not cast) with a cube of half-size 0.2 resting on it at `x` (so its base is at y = 0).
    fn cube_scene(&self, camera: Camera, light: Vec3, key: bool, at: Vec3) -> Scene {
        let mut s = base_scene(camera, light, key);
        s.instances.push(self.ground_instance());
        s.instances.push(Instance::new(self.cube, self.cube_material, Mat4::from_translation(at + Vec3::Y * 0.2)));
        s
    }

    fn ground_instance(&self) -> Instance {
        let mut ground = Instance::new(self.ground, self.ground_material, Mat4::IDENTITY);
        ground.casts_shadow = false;
        ground
    }

    fn image(&mut self, scene: &Scene, q: &Quality) -> Image {
        self.r.render_image(SIZE, AlphaEncoding::Straight, scene, q).unwrap()
    }
}

/// Exact colours (no tone curve) on black, a dim hemisphere and the key (or only the hemisphere).
fn base_scene(camera: Camera, to_light: Vec3, key: bool) -> Scene {
    let mut s = Scene::new(camera);
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s.lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.5 }];
    if key {
        s.lights.push(Light::Directional { to_light, colour: Colour::WHITE, intensity: 2.0, casts_shadow: true });
    }
    s
}

/// Straight down from 5 m, +X to the right, -Z up the picture: pixel `(64 + 64 x, 64 + 64 z)` is the ground at `(x, z)`.
fn top_down() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 5.0, 0.0),
        Vec3::ZERO,
        Vec3::NEG_Z,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

fn pixel(x: f32, z: f32) -> (u32, u32) {
    ((PPM.mul_add(x, 64.0)).floor() as u32, (PPM.mul_add(z, 64.0)).floor() as u32)
}

/// Light from the left and above at 45 degrees: shadows fall toward +X.
const FROM_LEFT: Vec3 = Vec3::new(-1.0, 1.0, 0.0);

// ---- where the shadow falls ----

/// The cube's shadow on the ground, in both qualities: inside it the ground shows what it does without the key light,
/// outside it what it does without shadows. The shadow is the cube's footprint pushed 0.4 m along +X (its top edge is
/// 0.4 m up and the light is at 45 degrees), so it spans x in (0.2, 0.6) beyond the cube and z in (-0.2, 0.2).
#[test]
fn the_cube_shadow_falls_where_the_geometry_says_in_both_qualities() {
    let _gpu = serial();
    let mut w = World::new();
    let scene = w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO);
    let unlit = w.image(&w.cube_scene(top_down(), FROM_LEFT, false, Vec3::ZERO), &quality(ShadowQuality::Off));
    let open = w.image(&scene, &quality(ShadowQuality::Off));
    for shadows in [ShadowQuality::Low, ShadowQuality::High] {
        let shadowed = w.image(&scene, &quality(shadows));
        for (x, z) in [(0.3, 0.0), (0.4, 0.1), (0.5, -0.1), (0.4, 0.0)] {
            let (px, py) = pixel(x, z);
            assert!(
                close(rgb_at(&shadowed, px, py), rgb_at(&unlit, px, py), 2),
                "{shadows:?}: ({x}, {z}) is in shadow"
            );
        }
        for (x, z) in [(0.75, 0.0), (-0.6, 0.0), (0.4, 0.35), (0.4, -0.35), (0.0, 0.9)] {
            let (px, py) = pixel(x, z);
            assert!(close(rgb_at(&shadowed, px, py), rgb_at(&open, px, py), 2), "{shadows:?}: ({x}, {z}) is lit");
        }
        let (px, py) = pixel(0.4, 0.0);
        let (lx, ly) = pixel(0.75, 0.0);
        assert!(rgb_at(&shadowed, px, py)[0] + 40 < rgb_at(&shadowed, lx, ly)[0], "the shadow is clearly darker");
    }
}

/// A flat caster resting on the ground, such as a foot: its shadow runs the whole length the geometry says (a 10 cm slab
/// under a 45 degree light shadows 10 cm of ground), short of it by no more than the normal bias plus the filter's blur.
/// A lookup that lifted the point further would shorten the shadow and float the caster above it (peter-panning).
/// Viewed closely (2.56 px per centimetre), so that a centimetre is measurable.
#[test]
fn the_shadow_of_a_flat_caster_runs_from_its_foot_to_where_the_geometry_ends_it() {
    let _gpu = serial();
    let mut w = World::new();
    let close_up = Camera::look_at(
        Vec3::new(0.25, 5.0, 0.0),
        Vec3::new(0.25, 0.0, 0.0),
        Vec3::NEG_Z,
        Projection::Orthographic { height: 0.5, near: 0.1, far: 20.0 },
    );
    let scene_for = |w: &World, key: bool| {
        let mut s = base_scene(close_up, FROM_LEFT, key);
        s.instances.push(w.ground_instance());
        // The cube squashed to a slab 0.4 m square and 0.1 m thick, base on the ground.
        let slab = Mat4::from_scale(Vec3::new(1.0, 0.25, 1.0)) * Mat4::from_translation(Vec3::Y * 0.2);
        s.instances.push(Instance::new(w.cube, w.cube_material, slab));
        s
    };
    let unlit = w.image(&scene_for(&w, false), &quality(ShadowQuality::Off));
    let px_per_m = SIZE.x as f32 / 0.5;
    let at = |x: f32| ((x - 0.25) * px_per_m + SIZE.x as f32 / 2.0, SIZE.y as f32 / 2.0);
    for shadows in [ShadowQuality::Low, ShadowQuality::High] {
        let shadowed = w.image(&scene_for(&w, true), &quality(shadows));
        // The slab covers x up to 0.2; its shadow runs on to 0.2 + 0.1 = 0.3.
        for x in [0.2 + 0.004, 0.25, 0.3 - 0.015] {
            let (px, py) = at(x);
            assert!(
                close(rgb_at(&shadowed, px as u32, py as u32), rgb_at(&unlit, px as u32, py as u32), 2),
                "{shadows:?}: the ground at x = {x} should be shadowed"
            );
        }
        let (px, py) = at(0.3 + 0.02);
        assert!(
            !close(rgb_at(&shadowed, px as u32, py as u32), rgb_at(&unlit, px as u32, py as u32), 20),
            "{shadows:?}: the shadow must end where the geometry ends it, not run on"
        );
    }
}

/// No acne: a sphere under lights at several angles. Where the light reaches it, the picture must match the one without
/// shadows (no bands at a grazing angle); on its far side it must match the one without the key light (no speckle).
#[test]
fn lit_surfaces_show_no_acne_even_at_grazing_angles() {
    let _gpu = serial();
    let mut w = World::new();
    // Seen from the side, from +Z at the height of the ball's centre: the sphere's surface point at pixel (px, py) has the
    // unit normal (x, y, sqrt(r^2 - x^2 - y^2)) / r.
    let side = Camera::look_at(
        Vec3::new(0.0, 0.3, 5.0),
        Vec3::new(0.0, 0.3, 0.0),
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let normal_at = |px: u32, py: u32| {
        let (x, y) = ((px as f32 + 0.5 - 64.0) / PPM, (64.0 - py as f32 - 0.5) / PPM);
        let r2 = x * x + y * y;
        (r2 < 0.3 * 0.3 * 0.9).then(|| Vec3::new(x, y, (0.09 - r2).sqrt()) / 0.3)
    };
    for to_light in [Vec3::new(-1.0, 1.0, 0.0), Vec3::new(-1.0, 0.25, 0.3), Vec3::new(0.3, 3.0, 1.0)] {
        let mut scene = base_scene(side, to_light, true);
        scene.instances.push(w.ground_instance());
        scene.instances.push(Instance::new(w.ball, w.cube_material, Mat4::from_translation(Vec3::new(0.0, 0.3, 0.0))));
        let mut dark = scene.clone();
        dark.lights.pop();
        let hemisphere_only = w.image(&dark, &quality(ShadowQuality::Off));
        let open = w.image(&scene, &quality(ShadowQuality::Off));
        let facing = to_light.normalize();
        for shadows in [ShadowQuality::Low, ShadowQuality::High] {
            let shadowed = w.image(&scene, &quality(shadows));
            // Where the surface faces the light by more than 0.35 (the ramp is fully lit above 0.12) the picture matches
            // the one without shadows; where it turns from the light by more than 0.4 it is in its own shadow and matches
            // the one without the key light. Nearer the terminator the depth bias, as in the prototype, lets the light
            // through up to a normal of -0.24, where the ramp has already gone to the shade colour.
            let (mut lit, mut lit_wrong, mut away, mut away_wrong) = (0, 0, 0, 0);
            for py in 0..SIZE.y {
                for px in 0..SIZE.x {
                    let Some(n) = normal_at(px, py) else { continue };
                    let facing_light = n.dot(facing);
                    if facing_light > 0.35 {
                        lit += 1;
                        lit_wrong += usize::from(!close(rgb_at(&shadowed, px, py), rgb_at(&open, px, py), 2));
                    } else if facing_light < -0.4 {
                        away += 1;
                        away_wrong +=
                            usize::from(!close(rgb_at(&shadowed, px, py), rgb_at(&hemisphere_only, px, py), 2));
                    }
                }
            }
            assert!(lit > 100 && away > 100, "test setup: {lit} lit and {away} dark sphere pixels under {to_light}");
            assert_eq!(lit_wrong, 0, "{shadows:?}, light {to_light}: {lit_wrong} of {lit} lit pixels carry acne");
            assert_eq!(away_wrong, 0, "{shadows:?}, light {to_light}: {away_wrong} of {away} dark pixels carry acne");
        }
    }
}

/// A material that does not receive shadows (faces stay clean under hair shadows) is not darkened.
#[test]
fn a_material_that_does_not_receive_shadows_stays_lit() {
    let _gpu = serial();
    let mut w = World::new();
    let mut clean = toon(Colour::hex(0x00e8_e4ee));
    clean.receives_shadows = false;
    w.ground_material = w.r.create_material(&clean).unwrap();
    let scene = w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO);
    let (a, b) = (w.image(&scene, &quality(ShadowQuality::High)), w.image(&scene, &quality(ShadowQuality::Off)));
    assert_eq!(differing(&a, &b, 1), 0);
}

// ---- casters that are not plain ----

fn translucent_quad(w: &mut World, alpha: AlphaMode, opacity: u8) -> (MeshId, omuzaki_render_api::MaterialId) {
    let quad = w.r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();
    let mut m = toon(Colour::rgba(255, 255, 255, opacity));
    m.alpha = alpha;
    m.double_sided = true;
    (quad, w.r.create_material(&m).unwrap())
}

/// A masked caster casts only where its texels survive the cutoff; a blended one where it is mostly there.
#[test]
fn masked_and_blended_casters_cast_by_coverage() {
    let _gpu = serial();
    let mut w = World::new();
    for (alpha, opacity, casts) in [
        (AlphaMode::Mask { cutoff: 0.5 }, 230, true),
        (AlphaMode::Mask { cutoff: 0.5 }, 40, false),
        (AlphaMode::Blend, 230, true),
        (AlphaMode::Blend, 40, false),
    ] {
        let (quad, material) = translucent_quad(&mut w, alpha, opacity);
        let mut scene = base_scene(top_down(), FROM_LEFT, true);
        scene.instances.push(w.ground_instance());
        // A pane standing 0.5 m up, tilted flat so it faces the light: it shadows the ground 0.5 m toward +X.
        scene.instances.push(Instance::new(
            quad,
            material,
            Mat4::from_translation(Vec3::new(0.0, 0.5, 0.0)) * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        ));
        let shadowed = w.image(&scene, &quality(ShadowQuality::High));
        let open = w.image(&scene, &quality(ShadowQuality::Off));
        let (px, py) = pixel(0.75, 0.0);
        let dark = !close(rgb_at(&shadowed, px, py), rgb_at(&open, px, py), 4);
        assert_eq!(dark, casts, "{alpha:?} at opacity {opacity}");
    }
}

/// A skinned caster is bounded by where its joints put it: a cube lifted 1.5 m sideways by its joint still has its shadow
/// inside the fitted map.
#[test]
fn a_posed_skinned_caster_still_gets_its_shadow() {
    let _gpu = serial();
    let mut w = World::new();
    let mut cube = shapes::cuboid(Vec3::splat(0.2));
    cube.joints = vec![[0; 4]; cube.positions.len()];
    cube.weights = vec![[1.0, 0.0, 0.0, 0.0]; cube.positions.len()];
    let skinned = w.r.create_mesh(&cube).unwrap();
    let mut scene = base_scene(top_down(), FROM_LEFT, true);
    scene.joints = vec![Mat4::from_translation(Vec3::new(-0.7, 0.2, 0.0))];
    scene.instances.push(w.ground_instance());
    let mut instance = Instance::new(skinned, w.cube_material, Mat4::IDENTITY);
    instance.skin = Some(JointRange { start: 0, len: 1 });
    scene.instances.push(instance);
    let shadowed = w.image(&scene, &quality(ShadowQuality::High));
    let open = w.image(&scene, &quality(ShadowQuality::Off));
    // The cube now rests at x = -0.7, so its shadow spans x in (-0.5, -0.1).
    let (px, py) = pixel(-0.3, 0.0);
    assert!(!close(rgb_at(&shadowed, px, py), rgb_at(&open, px, py), 20), "no shadow where the posed cube casts one");
}

// ---- stability ----

/// Sub-texel camera moves change no shadow pixel: the map depends on the light and the casters, not the camera, so moving
/// the camera by whole pixels shifts the picture, shadow included, exactly.
#[test]
fn moving_the_camera_does_not_move_a_shadow_edge() {
    let _gpu = serial();
    let mut w = World::new();
    for shadows in [ShadowQuality::Low, ShadowQuality::High] {
        let before = w.image(&w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO), &quality(shadows));
        for shift in [1u32, 2, 5] {
            let moved = Camera::look_at(
                Vec3::new(shift as f32 / PPM, 5.0, 0.0),
                Vec3::new(shift as f32 / PPM, 0.0, 0.0),
                Vec3::NEG_Z,
                Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
            );
            let after = w.image(&w.cube_scene(moved, FROM_LEFT, true, Vec3::ZERO), &quality(shadows));
            // Moving the camera right by `shift` pixels moves the picture left by as many. Compare the columns that
            // stay in view; the picture's edge is not part of the check.
            let mut off = 0;
            for py in 0..SIZE.y {
                for px in 0..SIZE.x - shift {
                    if !close(rgb_at(&before, px + shift, py), rgb_at(&after, px, py), 2) {
                        off += 1;
                    }
                }
            }
            assert!(off <= 4, "{shadows:?}: moving the camera {shift} px changed {off} pixels");
        }
    }
}

/// A caster that drifts by a fraction of a texel leaves the shadow of everything else where it was: the window is snapped
/// to the texel grid, so a bounding box that grows a little does not slide the whole map.
#[test]
fn a_drifting_caster_does_not_shimmer_the_shadow_of_a_still_one() {
    let _gpu = serial();
    let mut w = World::new();
    let texel_world = 0.0028; // about a texel of the 2048 map around a 1 m window; the drift stays well inside one
    for shadows in [ShadowQuality::Low, ShadowQuality::High] {
        let scene_with = |w: &World, drift: f32| {
            let mut s = w.cube_scene(top_down(), FROM_LEFT, true, Vec3::new(0.0, 0.0, 0.0));
            // A second cube far to the side whose position the test moves by a fraction of a texel.
            s.instances.push(Instance::new(
                w.cube,
                w.cube_material,
                Mat4::from_translation(Vec3::new(-0.6 + drift, 0.2, 0.5)),
            ));
            s
        };
        let still = w.image(&scene_with(&w, 0.0), &quality(shadows));
        for drift in [texel_world * 0.1, texel_world * 0.25] {
            let moved = w.image(&scene_with(&w, drift), &quality(shadows));
            // Only the drifting cube's own shadow (and the cube) may change: the left half of the picture holds the
            // still cube's shadow at x in (0.2, 0.6), z in (-0.2, 0.2), which is the right half, z above 0.
            let mut off = 0;
            for py in 0..SIZE.y / 2 + 8 {
                for px in 64..SIZE.x {
                    if !close(rgb_at(&still, px, py), rgb_at(&moved, px, py), 2) {
                        off += 1;
                    }
                }
            }
            assert_eq!(off, 0, "{shadows:?}: a drift of {drift} m moved {off} pixels of the still cube's shadow");
        }
    }
}

// ---- the live interval ----

fn ran(report: &omuzaki_render_api::FrameReport) -> bool {
    report.cpu.get(Pass::Shadows).is_some()
}

/// Live mode redraws every `shadow_interval`-th frame: the frames between report the pass as skipped and keep sampling
/// the map of the last redraw (a cube that moved is still shadowed where it was); offline mode redraws every frame.
#[test]
fn the_live_interval_redraws_every_nth_frame_and_the_others_reuse_the_map() {
    let _gpu = serial();
    let mut w = World::new();
    let target = w.r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let q = live(ShadowQuality::High, 3);
    let at = |w: &World, x: f32| w.cube_scene(top_down(), FROM_LEFT, true, Vec3::new(x, 0.0, 0.0));
    let mut sequence = Vec::new();
    let mut images = Vec::new();
    for x in [0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5] {
        let report = w.r.render(target, &at(&w, x), &q).unwrap();
        sequence.push(ran(&report));
        images.push(w.r.read_target(target).unwrap());
    }
    assert_eq!(
        sequence,
        [true, false, false, true, false, false, true],
        "every third frame redraws, the first at once"
    );

    // Frame 1: the cube is at x = 0.5 now, but the map still holds it at x = 0. Ground at x = 0.9 is in the shadow the
    // new position would cast (0.5 + 0.2 .. 0.5 + 0.6) and not in the old one (0.2 .. 0.6).
    let (px, py) = pixel(0.9, 0.0);
    let open = w.image(&at(&w, 0.5), &quality(ShadowQuality::Off));
    let unlit =
        w.image(&w.cube_scene(top_down(), FROM_LEFT, false, Vec3::new(0.5, 0.0, 0.0)), &quality(ShadowQuality::Off));
    // The spot at x = 0.28 is left of the moved cube (0.3 .. 0.7) and inside the old shadow.
    let (sx, sy) = pixel(0.28, 0.0);
    assert!(close(rgb_at(&images[1], sx, sy), rgb_at(&unlit, sx, sy), 2), "the stale map still shadows the old place");
    assert!(close(rgb_at(&images[1], px, py), rgb_at(&open, px, py), 2), "and does not yet shadow the new one");
    // Frame 3 redrew: the new shadow is there.
    assert!(close(rgb_at(&images[3], px, py), rgb_at(&unlit, px, py), 2), "the redraw catches up");

    // Offline: every frame draws.
    for _ in 0..3 {
        let report = w.r.render(target, &at(&w, 0.0), &quality(ShadowQuality::High)).unwrap();
        assert!(ran(&report), "offline frames are frame-exact");
    }
}

/// A turned light redraws at once even between live redraws, so a rig switch never shows the old shadows.
#[test]
fn a_new_light_direction_redraws_at_once() {
    let _gpu = serial();
    let mut w = World::new();
    let target = w.r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let q = live(ShadowQuality::Low, 8);
    let first = w.r.render(target, &w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO), &q).unwrap();
    let second = w.r.render(target, &w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO), &q).unwrap();
    let turned = w.r.render(target, &w.cube_scene(top_down(), Vec3::new(1.0, 1.0, 0.0), true, Vec3::ZERO), &q).unwrap();
    assert_eq!([ran(&first), ran(&second), ran(&turned)], [true, false, true]);
}

/// Off, no casting light and nothing that casts all skip the pass, and a frame after such a gap redraws.
#[test]
fn the_pass_is_skipped_without_shadows_a_caster_or_a_casting_light() {
    let _gpu = serial();
    let mut w = World::new();
    let target = w.r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let scene = w.cube_scene(top_down(), FROM_LEFT, true, Vec3::ZERO);
    assert!(ran(&w.r.render(target, &scene, &quality(ShadowQuality::Low)).unwrap()));
    assert!(!ran(&w.r.render(target, &scene, &quality(ShadowQuality::Off)).unwrap()), "shadows off");
    let no_key = w.cube_scene(top_down(), FROM_LEFT, false, Vec3::ZERO);
    assert!(!ran(&w.r.render(target, &no_key, &quality(ShadowQuality::High)).unwrap()), "no casting light");
    let mut none_cast = scene.clone();
    for instance in &mut none_cast.instances {
        instance.casts_shadow = false;
    }
    assert!(!ran(&w.r.render(target, &none_cast, &quality(ShadowQuality::High)).unwrap()), "no caster");
    // After the gap the map is drawn again at once, even in live mode with a long interval.
    let after = w.r.render(target, &scene, &live(ShadowQuality::High, 8)).unwrap();
    assert!(ran(&after));
    // Off really switches the lookup off: the picture with shadows off equals the one rendered without a casting light
    // wherever the cube's shadow would have been.
    let off = w.r.render(target, &scene, &quality(ShadowQuality::Off)).unwrap();
    let off_image = w.r.read_target(target).unwrap();
    let open = w.image(&scene, &quality(ShadowQuality::Off));
    assert!(!ran(&off));
    assert_eq!(differing(&off_image, &open, 0), 0);
}

// ---- several casting lights ----

/// Light from the right and above at 45 degrees: shadows fall toward -X.
const FROM_RIGHT: Vec3 = Vec3::new(1.0, 1.0, 0.0);

/// `cube_scene` with a second casting key from the right (`second`), or with only one of the two keys.
fn two_keys(w: &World, left: bool, right: bool) -> Scene {
    let mut s = w.cube_scene(top_down(), FROM_LEFT, left, Vec3::ZERO);
    if right {
        s.lights.push(Light::Directional {
            to_light: FROM_RIGHT,
            colour: Colour::WHITE,
            intensity: 2.0,
            casts_shadow: true,
        });
    }
    s
}

/// Two casting keys each get a map at `High`: the ground the left key's shadow covers is lit by the right key alone, and
/// the other way round. At `Low` only the first casting light has a map, so the second key lights through the cube.
#[test]
fn two_casting_keys_cast_two_shadows_and_low_keeps_the_first() {
    let _gpu = serial();
    let mut w = World::new();
    let both = two_keys(&w, true, true);
    let off = quality(ShadowQuality::Off);
    let (open, right_only, left_only) =
        (w.image(&both, &off), w.image(&two_keys(&w, false, true), &off), w.image(&two_keys(&w, true, false), &off));
    let high = w.image(&both, &quality(ShadowQuality::High));
    let low = w.image(&both, &quality(ShadowQuality::Low));
    for z in [-0.1, 0.0, 0.1] {
        // The left key's shadow falls at x in (0.2, 0.6), the right key's at x in (-0.6, -0.2).
        let (px, py) = pixel(0.4, z);
        let (nx, ny) = pixel(-0.4, z);
        assert!(close(rgb_at(&high, px, py), rgb_at(&right_only, px, py), 2), "the left key's shadow at (0.4, {z})");
        assert!(close(rgb_at(&high, nx, ny), rgb_at(&left_only, nx, ny), 2), "the right key's shadow at (-0.4, {z})");
        assert!(close(rgb_at(&low, px, py), rgb_at(&right_only, px, py), 2), "Low still shadows the first key");
        assert!(close(rgb_at(&low, nx, ny), rgb_at(&open, nx, ny), 2), "and lights the second through the cube");
    }
    for (x, z) in [(0.75, 0.0), (-0.75, 0.0), (0.0, 0.9)] {
        let (px, py) = pixel(x, z);
        assert!(close(rgb_at(&high, px, py), rgb_at(&open, px, py), 2), "({x}, {z}) is lit by both");
    }
}

/// Each map keeps its own live interval and redraws alone when its light turns; a rig that gains a casting light draws
/// the new map at once, and one that loses it stops sampling it.
#[test]
fn each_map_keeps_its_own_interval_and_a_new_casting_light_is_drawn_at_once() {
    let _gpu = serial();
    let mut w = World::new();
    let target = w.r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    // A long interval, so every redraw below is one a new or turned light asked for.
    let q = live(ShadowQuality::High, 8);
    let render = |w: &mut World, scene: &Scene| ran(&w.r.render(target, scene, &q).unwrap());
    let one = two_keys(&w, true, false);
    let both = two_keys(&w, true, true);
    let mut turned = both.clone();
    if let Some(Light::Directional { to_light, .. }) = turned.lights.last_mut() {
        *to_light = Vec3::new(1.0, 1.0, 0.5);
    }
    let sequence = [
        render(&mut w, &one),
        render(&mut w, &one),
        render(&mut w, &both),
        render(&mut w, &both),
        render(&mut w, &turned),
        render(&mut w, &turned),
    ];
    assert_eq!(sequence, [true, false, true, false, true, false], "first, interval, new light, interval, turned");
    // Back to one casting light: the second map is switched off, so the ground behind the cube on the left is lit by
    // the right key's light again only because that key is gone; the picture equals a fresh one-key render.
    render(&mut w, &one);
    let after = w.r.read_target(target).unwrap();
    let fresh = w.image(&one, &quality(ShadowQuality::High));
    let (nx, ny) = pixel(-0.4, 0.0);
    assert!(close(rgb_at(&after, nx, ny), rgb_at(&fresh, nx, ny), 2), "no stale second shadow");
}

// ---- the golden stage ----

/// The character stand-in of the evidence: feet, legs, torso, an arm and a head on a ground disc, lit by the prototype's
/// key (the prototype: position (-0.8, 1.6, 2.2), intensity 2) from the front-left.
fn stage(w: &mut World) -> Scene {
    let skin = w.r.create_material(&toon(Colour::hex(0x00f9_d3c5))).unwrap();
    let top = w.r.create_material(&toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let boxes = [
        ([0.10, 0.06, 0.26], [0.11, 0.03, 0.04], skin),
        ([0.10, 0.06, 0.26], [-0.11, 0.03, 0.04], skin),
        ([0.09, 0.78, 0.09], [0.11, 0.45, 0.0], skin),
        ([0.09, 0.78, 0.09], [-0.11, 0.45, 0.0], skin),
        ([0.34, 0.50, 0.20], [0.0, 1.09, 0.0], top),
        ([0.07, 0.50, 0.07], [0.24, 1.05, 0.0], skin),
    ];
    let camera = Camera::look_at(
        Vec3::new(0.4, 1.2, 4.6),
        Vec3::new(0.0, 0.75, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 40.0 },
    );
    let mut s = base_scene(camera, Vec3::new(-0.8, 1.6, 2.2), true);
    for (dims, at, material) in boxes {
        let mesh = w.r.create_mesh(&shapes::cuboid(Vec3::from(dims) * 0.5)).unwrap();
        s.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::from(at))));
    }
    let head = w.r.create_mesh(&sphere(0.12, 48, 24)).unwrap();
    s.instances.push(Instance::new(head, skin, Mat4::from_translation(Vec3::new(0.0, 1.52, 0.0))));
    s.instances.push(w.ground_instance());
    s
}

const STAGE_SIZE: UVec2 = UVec2::new(256, 256);

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Compares `image` with `tests/golden/<name>.png` (writes it instead when `OMUZAKI_BLESS=1`): at most `budget` pixels may
/// differ by more than 3 levels. A deliberate change of the look blesses the new picture in the same commit.
fn check_golden(name: &str, image: &Image, budget: usize) {
    let path = golden_dir().join(format!("{name}.png"));
    if std::env::var("OMUZAKI_BLESS").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(golden_dir()).unwrap();
        image::save_buffer(&path, &image.rgba8, image.size.x, image.size.y, image::ExtendedColorType::Rgba8).unwrap();
        return;
    }
    let golden = image::open(&path)
        .unwrap_or_else(|e| panic!("golden {}: {e} (bless with OMUZAKI_BLESS=1)", path.display()))
        .to_rgba8();
    let golden = Image::new(image.size, AlphaEncoding::Straight, golden.into_raw()).unwrap();
    let off = differing(image, &golden, 3);
    assert!(
        off <= budget,
        "{name}: {off} pixels differ from the golden (budget {budget}); bless with OMUZAKI_BLESS=1 if intended"
    );
}

#[test]
fn the_stage_matches_its_goldens() {
    let _gpu = serial();
    let mut w = World::new();
    let scene = stage(&mut w);
    for (name, q) in [
        ("stage_live_low", live(ShadowQuality::Low, 1)),
        ("stage_offline_high", quality(ShadowQuality::High)),
        ("stage_off", quality(ShadowQuality::Off)),
    ] {
        let image = w.r.render_image(STAGE_SIZE, AlphaEncoding::Straight, &scene, &q).unwrap();
        check_golden(name, &image, 60);
    }
    let image =
        w.r.render_image(STAGE_SIZE, AlphaEncoding::Straight, &two_key_stage(&scene), &quality(ShadowQuality::High));
    check_golden("stage_two_keys_high", &image.unwrap(), 60);
}

/// The stage with a second casting key from the front-right, dimmer than the first, so the figure casts two shadows.
fn two_key_stage(stage: &Scene) -> Scene {
    let mut s = stage.clone();
    s.lights.push(Light::Directional {
        to_light: Vec3::new(1.4, 1.8, 1.2),
        colour: Colour::hex(0x00ff_e2c8),
        intensity: 1.2,
        casts_shadow: true,
    });
    s
}

/// With `OMUZAKI_SHADOW_EVIDENCE=<dir>` the stage is also written there, at 512 px, once per quality, for the contact sheet.
#[test]
fn write_the_evidence_pictures_when_asked() {
    let Ok(dir) = std::env::var("OMUZAKI_SHADOW_EVIDENCE") else { return };
    let _gpu = serial();
    let mut w = World::new();
    let scene = stage(&mut w);
    for (name, q) in [
        ("rust_off", quality(ShadowQuality::Off)),
        ("rust_low", quality(ShadowQuality::Low)),
        ("rust_high", quality(ShadowQuality::High)),
        ("rust_two_keys_low", quality(ShadowQuality::Low)),
        ("rust_two_keys_high", quality(ShadowQuality::High)),
    ] {
        let shown = if name.contains("two_keys") { two_key_stage(&scene) } else { scene.clone() };
        let image = w.r.render_image(UVec2::splat(512), AlphaEncoding::Straight, &shown, &q).unwrap();
        let path = PathBuf::from(&dir).join(format!("{name}.png"));
        image::save_buffer(path, &image.rgba8, 512, 512, image::ExtendedColorType::Rgba8).unwrap();
    }
}
