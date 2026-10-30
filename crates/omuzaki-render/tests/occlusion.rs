//! Ambient occlusion and contact shadows (`Scene::occlusion`, #2507) on the GPU. The surfaces are unlit white, so a
//! pixel's colour is exactly how open the pass found it:
//!
//! - a faceted sphere (smooth normals over flat triangles, as every mesh is) is convex, and a floor seen at a slant is
//!   flat, so contact shadows must leave both alone: a march that finds the surface itself as a blocker bands into
//!   stripes (the eye's front view showed them on the lid);
//! - a box on a floor darkens the floor along its base (ambient occlusion), and a wall casts a contact shadow on the
//!   floor on its side away from the light, but not on the floor past its end.
//!
//! `OMUZAKI_RENDER_OUT=<dir>` saves the frames. Runs on any adapter (lavapipe in CI); without one it prints why and
//! passes.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]
#![allow(clippy::disallowed_methods, reason = "a test writing its own evidence pictures; not an op")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3, Vec4};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Image, Instance, Light, Material, MeshData, Occlusion, Projection, Quality,
    Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::shapes;

const SIZE: UVec2 = UVec2::new(160, 160);

/// One GPU test at a time in this binary: software Vulkan compiles pipelines on the calling thread.
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

fn renderer() -> Option<WgpuRenderer> {
    match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("no adapter, skipping: {e}");
            None
        }
    }
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let (w, h) = (image.size.x, image.size.y);
        image::save_buffer(PathBuf::from(dir).join(name), &image.rgba8, w, h, image::ColorType::Rgba8).unwrap();
    }
}

/// One sample, no post effects, no tone curve: an unlit white pixel is exactly the occlusion factor.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() }
}

/// The light the contact shadows march toward: up and to the right.
const TO_LIGHT: Vec3 = Vec3::new(1.0, 1.0, 0.0);

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 1.4, 2.4),
        Vec3::new(0.0, 0.2, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    )
}

/// A UV sphere of `radius` with smooth normals over flat facets: `around` segments round, `rings` from pole to pole.
fn sphere(radius: f32, around: u32, rings: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for r in 0..=rings {
        let theta = std::f32::consts::PI * r as f32 / rings as f32;
        for a in 0..=around {
            let phi = std::f32::consts::TAU * a as f32 / around as f32;
            let n = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            mesh.positions.push(n * radius);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(a as f32 / around as f32, r as f32 / rings as f32));
        }
    }
    let at = |r: u32, a: u32| r * (around + 1) + a;
    for r in 0..rings {
        for a in 0..around {
            let (p, q, s, t) = (at(r, a), at(r, a + 1), at(r + 1, a + 1), at(r + 1, a));
            mesh.indices.extend_from_slice(&[p, q, s, p, s, t]);
        }
    }
    mesh
}

/// Renders unlit white `meshes` (each with its transform) under the shadow-casting light, with `occlusion`.
fn render(r: &mut WgpuRenderer, meshes: &[(MeshData, Mat4)], occlusion: Option<Occlusion>) -> Image {
    render_from(r, camera(), TO_LIGHT, meshes, occlusion)
}

/// [`render`] from `camera`, with the light toward `to_light`.
fn render_from(
    r: &mut WgpuRenderer,
    camera: Camera,
    to_light: Vec3,
    meshes: &[(MeshData, Mat4)],
    occlusion: Option<Occlusion>,
) -> Image {
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.lights = vec![Light::Directional { to_light, colour: Colour::WHITE, intensity: 1.0, casts_shadow: true }];
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    for (mesh, transform) in meshes {
        let mesh = r.create_mesh(mesh).unwrap();
        scene.instances.push(Instance::new(mesh, material, *transform));
    }
    scene.occlusion = occlusion;
    r.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap()
}

/// Where world point `p` lands in the picture, in pixels.
fn pixel_of(p: Vec3) -> (u32, u32) {
    let camera = camera();
    let clip = camera.projection_matrix(SIZE.x as f32 / SIZE.y as f32) * camera.view() * Vec4::from((p, 1.0));
    let ndc = clip.truncate() / clip.w;
    let x = f32::midpoint(ndc.x, 1.0) * SIZE.x as f32;
    let y = (1.0 - ndc.y) * 0.5 * SIZE.y as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a point inside the picture")]
    (x as u32, y as u32)
}

fn red(image: &Image, (x, y): (u32, u32)) -> u8 {
    image.rgba8[((y * image.size.x + x) * 4) as usize]
}

/// The mean red level of the pixels within 2 of `at`.
fn around(image: &Image, at: (u32, u32)) -> f32 {
    let mut sum = 0.0;
    for dy in 0..5 {
        for dx in 0..5 {
            sum += f32::from(red(image, (at.0 + dx - 2, at.1 + dy - 2)));
        }
    }
    sum / 25.0
}

#[test]
fn contact_shadows_leave_a_faceted_convex_surface_alone() {
    let _gpu = serial();
    let Some(mut r) = renderer() else { return };
    let ball = [(sphere(0.6, 24, 12), Mat4::from_translation(Vec3::new(0.0, 0.3, 0.0)))];
    // A reach far below a pixel turns the ambient occlusion off, so only the contact shadows act.
    let contact = Occlusion::new(0.0001, 0.9).with_contact(0.25);
    let off = render(&mut r, &ball, None);
    let on = render(&mut r, &ball, Some(contact));
    save(&off, "occlusion-sphere-off.png");
    save(&on, "occlusion-sphere-contact.png");
    let covered: Vec<usize> = (0..off.rgba8.len() / 4).filter(|i| off.rgba8[i * 4] > 128).collect();
    assert!(covered.len() > 4000, "the sphere covers {} pixels", covered.len());
    let darkened = covered.iter().filter(|i| u16::from(on.rgba8[*i * 4]) + 12 < u16::from(off.rgba8[*i * 4])).count();
    let share = darkened as f32 / covered.len() as f32;
    assert!(share < 0.02, "{share:.3} of a convex surface is shadowed by its own facets");
}

#[test]
fn contact_shadows_leave_a_flat_floor_seen_at_a_slant_alone() {
    let _gpu = serial();
    let Some(mut r) = renderer() else { return };
    // A low camera and a low light: each pixel's depth changes steeply across it, so a march that compares against the
    // depth at the pixel it lands in, without asking whether anything rises above the floor, finds the floor itself.
    let low = Camera::look_at(
        Vec3::new(0.0, 0.25, 2.0),
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    );
    let floor = [(shapes::quad(Vec2::splat(4.0)), Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2))];
    let light = Vec3::new(0.2, 0.25, -1.0);
    let contact = Occlusion::new(0.0001, 0.9).with_contact(0.3);
    let off = render_from(&mut r, low, light, &floor, None);
    let on = render_from(&mut r, low, light, &floor, Some(contact));
    save(&on, "occlusion-floor-contact.png");
    let covered: Vec<usize> = (0..off.rgba8.len() / 4).filter(|i| off.rgba8[i * 4] > 128).collect();
    assert!(covered.len() > 4000, "the floor covers {} pixels", covered.len());
    let darkened = covered.iter().filter(|i| u16::from(on.rgba8[*i * 4]) + 12 < u16::from(off.rgba8[*i * 4])).count();
    let share = darkened as f32 / covered.len() as f32;
    assert!(share < 0.01, "{share:.3} of an open floor is shadowed by itself");
}

#[test]
fn a_box_darkens_the_floor_at_its_base_and_a_wall_casts_a_contact_shadow_away_from_the_light() {
    let _gpu = serial();
    let Some(mut r) = renderer() else { return };
    // Contact shadows: a thin wall running away from the camera, right of centre, so the camera sees its left face and
    // the floor there. Thin, because a contact shadow counts a blocker only as thick as half its reach.
    let wall = Vec3::new(0.03, 0.25, 0.3);
    let at = Vec3::new(0.4, wall.y, 0.0);
    let floor = shapes::quad(Vec2::splat(3.0));
    let flat = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let scene = [(floor, flat), (shapes::cuboid(wall), Mat4::from_translation(at))];
    let off = render(&mut r, &scene, None);
    let contact = render(&mut r, &scene, Some(Occlusion::new(0.0001, 0.9).with_contact(0.3)));
    save(&off, "occlusion-wall-off.png");
    save(&contact, "occlusion-wall-contact.png");

    // Ambient occlusion along the base of a box facing the camera: the floor just in front of it, and far from it.
    let half = 0.25;
    let boxed = [
        (shapes::quad(Vec2::splat(3.0)), flat),
        (shapes::cuboid(Vec3::splat(half)), Mat4::from_translation(Vec3::new(0.0, half, 0.0))),
    ];
    let bare = render(&mut r, &boxed, None);
    let ambient = render(&mut r, &boxed, Some(Occlusion::new(0.2, 0.9)));
    save(&ambient, "occlusion-box-ambient.png");
    let at_base = pixel_of(Vec3::new(0.0, 0.0, half + 0.04));
    let far = pixel_of(Vec3::new(-0.6, 0.0, 0.8));
    assert!((around(&bare, at_base) - around(&bare, far)).abs() < 2.0, "without the pass the floor is even");
    let (near_ao, far_ao) = (around(&ambient, at_base), around(&ambient, far));
    assert!(near_ao < 0.95 * far_ao, "the floor at the base is darker: {near_ao} against {far_ao}");
    assert!(far_ao > 0.95 * around(&bare, far), "open floor stays open: {far_ao}");

    let left_face = at.x - wall.x;
    // The light is up and to the right, over the wall: the floor at its left is in its contact shadow, and the floor
    // as far left of it but past its end, where nothing stands between it and the light, is lit.
    let shade = pixel_of(Vec3::new(left_face - 0.05, 0.0, 0.0));
    let open = pixel_of(Vec3::new(left_face - 0.05, 0.0, wall.z + 0.25));
    let (shadowed, lit) = (around(&contact, shade), around(&contact, open));
    assert!(shadowed < 0.8 * around(&off, shade), "the floor beside the wall is in shadow: {shadowed}");
    assert!(lit > 0.95 * around(&off, open), "the floor past its end is lit: {lit}");
}
