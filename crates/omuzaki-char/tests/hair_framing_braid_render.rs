//! Evidence pictures of the framing locks and the braid crown on a synthetic head, written only when
//! `OMUZAKI_HAIR_PPM` names a folder (front, side and back, as PPM). Taste is judged on the pictures, not on numbers.

#![allow(clippy::unwrap_used, clippy::expect_used)] // evidence helpers: a bad setup should stop with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // grid indices are small

use omuzaki_char::hair::compile::{HairGenCtx, UnitStream};
use omuzaki_char::hair::r#gen::{braid, framing};
use omuzaki_char::hair::tube::{OutwardFrame, TubeLook, TubeMesh, polar_angles};
use omuzaki_char_api::hair::FramingLocks;
use omuzaki_char_api::{BraidGen, Dynamics, FramingGen, GroupId, HairClump, HeadSurface, Wave};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::Palette;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Material, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::MockRenderer;

/// An ellipsoid head with its centre at `(0, 1.6, 0)`.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.6, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.2, 0.22, 0.2)).length()
    }
}

struct Mid;

impl UnitStream for Mid {
    fn next_unit(&mut self) -> f64 {
        0.5
    }
}

/// A latitude-longitude ellipsoid for the head.
fn head_mesh() -> MeshData {
    let (rings, segs) = (48_u32, 64_u32);
    let mut mesh = MeshData::default();
    for i in 0..=rings {
        let th = i as f32 / rings as f32 * std::f32::consts::PI;
        for j in 0..=segs {
            let ph = j as f32 / segs as f32 * std::f32::consts::TAU;
            let dir = Vec3::new(th.sin() * ph.sin(), th.cos(), th.sin() * ph.cos());
            mesh.positions.push(Egg.centre() + dir * Egg.radius(dir));
            mesh.normals.push(dir);
        }
    }
    for i in 0..rings {
        for j in 0..segs {
            let a = i * (segs + 1) + j;
            let b = a + segs + 1;
            mesh.indices.extend([a, b, a + 1, b, b + 1, a + 1]);
        }
    }
    mesh
}

fn tubes(clumps: &[HairClump]) -> MeshData {
    let frame = OutwardFrame { head: Egg.centre(), body_axis: Vec2::new(0.0, -0.03) };
    let mut mesh = TubeMesh::new();
    for clump in clumps {
        let polar = polar_angles(&clump.rest, frame.head);
        let look = TubeLook { seed: 0.3, shade: clump.shade, polar: &polar, tint: None, flip: 1.0 };
        mesh.push(&clump.rest, &clump.width, &clump.thickness, |p| frame.rest(p), &look).unwrap();
    }
    mesh.finish().unwrap()
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_framing_and_braid_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_HAIR_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let (palette, id, dynamics) = (Palette::default(), GroupId::new("hair").unwrap(), Dynamics::default());
    let mut stream = Mid;
    let mut ctx = HairGenCtx::new(&Egg, &palette, omuzaki_char_api::Quality::Live, &id, &dynamics, &mut stream);
    let framing_gen = FramingGen {
        count: 5,
        wave: Wave { amplitude: 0.016, ..Wave::default() },
        locks: Some(FramingLocks::new()),
        ..FramingGen::default()
    };
    let mut clumps = framing::compile(&framing_gen, &mut ctx).clumps;
    clumps.extend(braid::compile(&BraidGen::default(), &mut ctx).clumps);
    let hair = tubes(&clumps);
    let centre = Vec3::new(0.0, 1.45, 0.0);
    for (view, azimuth_deg) in [("front", 0.0_f32), ("side", 90.0), ("back", 180.0)] {
        let mut renderer = MockRenderer::new();
        let mut scene = Scene::new(Camera::look_at(
            centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.1, 1.4)),
            centre,
            Vec3::Y,
            Camera::default().projection,
        ));
        scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
        scene.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
            Light::Directional {
                to_light: Vec3::new(1.6, 3.0, 2.4),
                colour: Colour::hex(0x00ff_f6ec),
                intensity: 1.7,
                casts_shadow: false,
            },
        ];
        scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
        for (geometry, colour) in [(head_mesh(), 0x00e8_c4a8), (hair.clone(), 0x0048_2a1c)] {
            let mesh = renderer.create_mesh(&geometry).unwrap();
            let mut material = Material::toon(Colour::hex(colour));
            material.double_sided = true;
            let material = renderer.create_material(&material).unwrap();
            scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        }
        let quality = Quality { samples: 4, ..Quality::offline() };
        let image = renderer.render_image(UVec2::splat(512), AlphaEncoding::Straight, &scene, &quality).unwrap();
        let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
        for y in 0..image.size.y {
            for x in 0..image.size.x {
                let [r, g, b, _] = image.pixel(x, y).unwrap();
                ppm.extend([r, g, b]);
            }
        }
        std::fs::write(directory.join(format!("{view}.ppm")), ppm).unwrap();
    }
}
