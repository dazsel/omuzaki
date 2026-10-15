//! Morph targets on the GPU renderer: the contract's morph conformance checks, a sparse mesh drawn at three weights,
//! and the pool's rules (a dynamic base, a released mesh).

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Instance, Material, MeshData, MeshId, MeshOptions, MeshUsage,
    MorphRange, MorphSet, Projection, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::{conformance, shapes};

#[test]
fn the_renderer_claims_morph_targets() {
    let caps = support::renderer().capabilities().clone();
    assert!(caps.morph_targets);
    assert_eq!(caps.max_morph_targets, 256);
}

#[test]
fn the_morph_conformance_checks_pass_and_leak_nothing() {
    let mut r = support::renderer();
    if let Err(failure) = conformance::run_capable(&mut r) {
        panic!("{failure}");
    }
}

/// The columns of the middle row that are lit, drawing the quad on black with orthographic pixels 1/32 m wide.
fn lit_columns(r: &mut impl Renderer, mesh: MeshId, weight: f32) -> Vec<u32> {
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    let mut instance = Instance::new(mesh, material, Mat4::IDENTITY);
    instance.morph = Some(MorphRange { start: 0, len: 1 });
    scene.instances.push(instance);
    scene.morph_weights.push(weight);
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() };
    let img = r.render_image(UVec2::splat(64), AlphaEncoding::Straight, &scene, &quality).unwrap();
    r.release(material.into()).unwrap();
    (0..64).filter(|x| img.rgba8[(32 * 64 + x) as usize * 4] > 128).collect()
}

fn slide_options(quad: &MeshData, usage: MeshUsage) -> MeshOptions {
    let deltas = vec![Vec3::new(0.5, 0.0, 0.0); quad.positions.len()];
    let set = MorphSet::from_dense(quad.positions.len(), &[(Slug::new("slide").unwrap(), &deltas, &[])], 0.0).unwrap();
    MeshOptions { usage, morphs: Some(set) }
}

/// A 0.25 m quad (pixels 24..40) whose one target slides it 0.5 m (16 pixels) to the right.
#[test]
fn one_mesh_renders_at_weights_0_half_and_1() {
    let mut r = support::renderer();
    let quad = shapes::quad(Vec2::splat(0.25));
    let mesh = r.create_mesh_with(&quad, &slide_options(&quad, MeshUsage::Static)).unwrap();
    let edges: Vec<(u32, u32)> = [0.0, 0.5, 1.0]
        .iter()
        .map(|&w| {
            let lit = lit_columns(&mut r, mesh, w);
            (*lit.first().unwrap(), *lit.last().unwrap())
        })
        .collect();
    // Edge pixels are exact at one sample: the quad is 16 pixels wide and moves 0, 8 and 16 pixels.
    assert_eq!(edges, vec![(24, 39), (32, 47), (40, 55)]);
    r.release(mesh.into()).unwrap();
}

#[test]
fn a_dynamic_meshes_morphs_follow_its_updated_vertices() {
    let mut r = support::renderer();
    let quad = shapes::quad(Vec2::splat(0.25));
    let mesh = r.create_mesh_with(&quad, &slide_options(&quad, MeshUsage::Dynamic)).unwrap();
    let before = lit_columns(&mut r, mesh, 1.0);
    // Same weights, but the base is twice as wide: the cached blend must not be reused.
    let wide: Vec<Vec3> = quad.positions.iter().map(|p| *p * Vec3::new(2.0, 1.0, 1.0)).collect();
    r.update_vertices(mesh, &wide, &quad.normals).unwrap();
    let after = lit_columns(&mut r, mesh, 1.0);
    assert_eq!((before.len(), after.len()), (16, 32), "the morphed copy follows the update");
    r.release(mesh.into()).unwrap();
}

// ---- a morphed instance is everything a mesh created at the blended geometry is ----

use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{AlphaMode, Image, Light, Outline, ShadowQuality, TargetDesc};
use omuzaki_render_mock::morph_vertices;

fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, shadows: ShadowQuality::Low, ..Quality::offline() }
}

/// How many pixels differ by more than `tolerance` in some channel.
fn differing(a: &Image, b: &Image, tolerance: u8) -> usize {
    assert_eq!(a.size, b.size);
    a.rgba8
        .chunks(4)
        .zip(b.rgba8.chunks(4))
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > tolerance))
        .count()
}

/// A one-target set from dense deltas, and `mesh` baked at `weight` (the geometry a morphed instance should draw).
fn target_and_baked(mesh: &MeshData, positions: &[Vec3], normals: &[Vec3], weight: f32) -> (MorphSet, MeshData) {
    let set =
        MorphSet::from_dense(mesh.positions.len(), &[(Slug::new("t").unwrap(), positions, normals)], 0.0).unwrap();
    let (p, n) = morph_vertices(&mesh.positions, &mesh.normals, &set, &[weight]).unwrap();
    (set, MeshData { positions: p, normals: n, ..mesh.clone() })
}

/// Renders `instances` in a scene built by `scene`, with the morph weight `weight` for target 0.
fn draw(r: &mut WgpuRenderer, mut scene: Scene, instances: Vec<Instance>, weight: f32) -> Image {
    scene.instances = instances;
    scene.morph_weights = vec![weight];
    scene.background = Background::Solid { colour: Colour::WHITE };
    scene.post.tonemap = Tonemap::None;
    r.render_image(UVec2::splat(128), AlphaEncoding::Straight, &scene, &plain()).unwrap()
}

fn morphing(mut instance: Instance) -> Instance {
    instance.morph = Some(MorphRange { start: 0, len: 1 });
    instance
}

#[test]
fn a_caster_morphed_out_of_its_base_bounds_still_casts_its_shadow() {
    let mut r = support::renderer();
    let cube = shapes::cuboid(Vec3::splat(0.2));
    let slide = vec![Vec3::new(1.2, 0.0, 0.0); cube.positions.len()];
    let (set, baked) = target_and_baked(&cube, &slide, &[], 1.0);
    let morph = r.create_mesh_with(&cube, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    let baked = r.create_mesh(&baked).unwrap();
    let ground = {
        let corners = |y: f32| [(-3.0, -3.0), (3.0, -3.0), (3.0, 3.0), (-3.0, 3.0)].map(|(x, z)| Vec3::new(x, y, z));
        let disc = MeshData {
            positions: corners(0.0).to_vec(),
            normals: vec![Vec3::Y; 4],
            indices: vec![0, 2, 1, 0, 3, 2],
            ..MeshData::default()
        };
        r.create_mesh(&disc).unwrap()
    };
    let material = r.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let floor = r.create_material(&Material::toon(Colour::hex(0x00e8_e4ee))).unwrap();
    let scene = || {
        let mut s = Scene::new(Camera::look_at(
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::ZERO,
            Vec3::NEG_Z,
            Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
        ));
        s.lights = vec![Light::Directional {
            to_light: Vec3::new(-0.3, 1.0, 0.0),
            colour: Colour::WHITE,
            intensity: 2.0,
            casts_shadow: true,
        }];
        s
    };
    let at = Mat4::from_translation(Vec3::new(-0.9, 0.2, 0.0));
    let ground_instance = || {
        let mut g = Instance::new(ground, floor, Mat4::IDENTITY);
        g.casts_shadow = false;
        g
    };
    let morphed = draw(&mut r, scene(), vec![ground_instance(), morphing(Instance::new(morph, material, at))], 1.0);
    let want = draw(&mut r, scene(), vec![ground_instance(), Instance::new(baked, material, at)], 0.0);
    let no_cube = draw(&mut r, scene(), vec![ground_instance()], 0.0);
    assert!(differing(&want, &no_cube, 4) > 200, "the baked cube and its shadow must show");
    assert_eq!(differing(&morphed, &want, 2), 0, "the morphed cube casts the shadow of the cube at the blended place");
}

#[test]
fn a_morph_that_swaps_the_depth_of_two_blended_quads_swaps_their_compositing_order() {
    let mut r = support::renderer();
    let quad = shapes::quad(Vec2::splat(0.4));
    let forward = vec![Vec3::new(0.0, 0.0, 1.0); quad.positions.len()];
    let (set, baked) = target_and_baked(&quad, &forward, &[], 1.0);
    let morph = r.create_mesh_with(&quad, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    let baked = r.create_mesh(&baked).unwrap();
    let other = r.create_mesh(&quad).unwrap();
    let blended = |r: &mut WgpuRenderer, colour| {
        let mut m = Material::unlit(colour);
        m.alpha = AlphaMode::Blend;
        r.create_material(&m).unwrap()
    };
    let (red, green) = (blended(&mut r, Colour::rgba(255, 0, 0, 128)), blended(&mut r, Colour::rgba(0, 255, 0, 128)));
    let scene =
        || Scene::new(Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let green_at = || Instance::new(other, green, Mat4::from_translation(Vec3::new(0.1, 0.0, 0.5)));
    // Red starts behind green (z 0 against 0.5); the target brings it to z 1, in front.
    let morphed = draw(&mut r, scene(), vec![morphing(Instance::new(morph, red, Mat4::IDENTITY)), green_at()], 1.0);
    let want = draw(&mut r, scene(), vec![Instance::new(baked, red, Mat4::IDENTITY), green_at()], 0.0);
    let behind = draw(&mut r, scene(), vec![morphing(Instance::new(morph, red, Mat4::IDENTITY)), green_at()], 0.0);
    assert!(differing(&want, &behind, 4) > 100, "the two orders must look different");
    assert_eq!(differing(&morphed, &want, 2), 0, "the morphed quad composites in front, as the baked one does");
}

#[test]
fn an_outlined_morph_that_changes_normals_across_seams_outlines_like_the_blended_mesh() {
    let mut r = support::renderer();
    let cube = shapes::cuboid(Vec3::splat(0.4));
    // Every copy of a corner moves alike (the seams stay closed) while every normal tilts.
    let stretch: Vec<Vec3> = cube.positions.iter().map(|p| Vec3::new(p.x * 0.5, 0.0, 0.0)).collect();
    let tilt = vec![Vec3::new(0.0, 0.6, 0.3); cube.positions.len()];
    let (set, baked) = target_and_baked(&cube, &stretch, &tilt, 1.0);
    let morph = r.create_mesh_with(&cube, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    let baked = r.create_mesh(&baked).unwrap();
    let mut outlined = Material::unlit(Colour::rgb(200, 200, 255));
    outlined.outline = Some(Outline { colour: Colour::BLACK, width: 0.06 });
    let outlined = r.create_material(&outlined).unwrap();
    let scene = || {
        let mut s =
            Scene::new(Camera::look_at(Vec3::new(1.5, 1.2, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
        s.lights.clear();
        s
    };
    let morphed = draw(&mut r, scene(), vec![morphing(Instance::new(morph, outlined, Mat4::IDENTITY))], 1.0);
    let want = draw(&mut r, scene(), vec![Instance::new(baked, outlined, Mat4::IDENTITY)], 0.0);
    let base_hull = draw(&mut r, scene(), vec![morphing(Instance::new(morph, outlined, Mat4::IDENTITY))], 0.0);
    assert!(differing(&want, &base_hull, 4) > 100, "the blended cube must look unlike the base one");
    assert_eq!(differing(&morphed, &want, 2), 0, "the hull follows the blended normals");
}

/// Writes the morph pictures as binary PPM files when `OMUZAKI_MORPH_PPM=<directory>` is set (the `morph` case of
/// `evidence.toml`): a quad that slides at weights 0, 0.5 and 1, and an outlined, shadow-casting cube that stretches and
/// tilts its normals, at weights 0, 0.5 and 1.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_morph_pictures_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_MORPH_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let save = |name: &str, image: &Image| {
        let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
        for pixel in image.rgba8.chunks(4) {
            ppm.extend(&pixel[..3]);
        }
        std::fs::write(directory.join(name), ppm).unwrap();
    };
    let mut r = support::renderer();

    let quad = shapes::quad(Vec2::splat(0.25));
    let quad_mesh = r.create_mesh_with(&quad, &slide_options(&quad, MeshUsage::Static)).unwrap();
    let paint = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let ortho = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    for weight in [0.0, 0.5, 1.0] {
        let image =
            draw(&mut r, Scene::new(ortho), vec![morphing(Instance::new(quad_mesh, paint, Mat4::IDENTITY))], weight);
        save(&format!("quad-{weight}.ppm"), &image);
    }

    let cube = shapes::cuboid(Vec3::splat(0.3));
    let stretch: Vec<Vec3> = cube.positions.iter().map(|p| Vec3::new(p.x * 1.2, 0.0, 0.0)).collect();
    let tilt = vec![Vec3::new(0.0, 0.5, 0.3); cube.positions.len()];
    let (set, _) = target_and_baked(&cube, &stretch, &tilt, 1.0);
    let cube_mesh = r.create_mesh_with(&cube, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    let mut outlined = Material::toon(Colour::hex(0x00c9_b8f0));
    outlined.outline = Some(Outline { colour: Colour::BLACK, width: 0.03 });
    let outlined = r.create_material(&outlined).unwrap();
    let floor_mesh = {
        let mut m = shapes::quad(Vec2::splat(2.0));
        m.positions = m.positions.iter().map(|p| Vec3::new(p.x, 0.0, -p.y)).collect();
        m.normals = vec![Vec3::Y; m.positions.len()];
        r.create_mesh(&m).unwrap()
    };
    let ground = r.create_material(&Material::toon(Colour::hex(0x00e8_e4ee))).unwrap();
    for weight in [0.0, 0.5, 1.0] {
        let mut scene = Scene::new(Camera::look_at(
            Vec3::new(1.2, 1.6, 2.6),
            Vec3::new(0.0, 0.2, 0.0),
            Vec3::Y,
            Camera::default().projection,
        ));
        scene.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.5 },
            Light::Directional {
                to_light: Vec3::new(-0.6, 1.0, 0.4),
                colour: Colour::WHITE,
                intensity: 2.0,
                casts_shadow: true,
            },
        ];
        let mut floor = Instance::new(floor_mesh, ground, Mat4::IDENTITY);
        floor.casts_shadow = false;
        let body = morphing(Instance::new(cube_mesh, outlined, Mat4::from_translation(Vec3::new(0.0, 0.3, 0.0))));
        save(&format!("outlined-{weight}.ppm"), &draw(&mut r, scene, vec![floor, body], weight));
    }
}

#[test]
fn releasing_a_morphed_mesh_gives_back_the_pooled_buffers() {
    let mut r = support::renderer();
    let empty = Scene::new(Camera::default());
    let quality = plain();
    let target = r.create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight }).unwrap();
    r.render(target, &empty, &quality).unwrap();
    let before = r.stats().gpu_bytes;

    let cube = shapes::cuboid(Vec3::splat(0.3));
    let mesh = r.create_mesh_with(&cube, &slide_options(&cube, MeshUsage::Static)).unwrap();
    let mut outlined = Material::unlit(Colour::WHITE);
    outlined.outline = Some(Outline { colour: Colour::BLACK, width: 0.02 });
    let material = r.create_material(&outlined).unwrap();
    let mut scene = Scene::new(Camera::default());
    scene.instances.push(morphing(Instance::new(mesh, material, Mat4::IDENTITY)));
    scene.morph_weights.push(0.5);
    r.render(target, &scene, &quality).unwrap();
    let with = r.stats().gpu_bytes;
    // Shape and outline-direction streams, 24 and 12 bytes a vertex, on top of the mesh and material themselves.
    let pooled = (cube.positions.len() * (24 + 12)) as u64;
    assert!(with >= before + pooled, "the pool's buffers are counted: {before} then {with}");

    r.release(mesh.into()).unwrap();
    r.release(material.into()).unwrap();
    r.render(target, &empty, &quality).unwrap();
    assert_eq!(r.stats().gpu_bytes, before, "releasing the mesh frees the pooled buffers with it");
}
