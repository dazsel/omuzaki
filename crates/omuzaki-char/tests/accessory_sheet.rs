//! The contact sheets of the accessories (CB-43): every style of every kind on `standard` and `soft-sd`, seen from the
//! front and three-quarter, through the mock toon renderer. Evidence only: it renders when
//! `OMUZAKI_ACCESSORY_PPM=<directory>` is set, as one binary PPM per kind and template, and otherwise does nothing.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // pixels
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{ACC, character_with, hairstyles, scene, styles, vertices};
use omuzaki_char_api::{BuiltCharacter, HeadSurface};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt, Scene,
    Tonemap,
};
use omuzaki_render_mock::MockRenderer;

/// The side of one tile, pixels.
const TILE: u32 = 256;

/// The kinds on the sheets and the azimuths (degrees about the vertical) each is seen from.
const KINDS: [(&str, &[f32]); 2] = [("horns", &[0.0, 35.0]), ("headphones", &[0.0, 35.0])];

/// What a tile is framed on: the head for what sits on it, the accessory itself for what hangs on the body.
fn frame(kind: &str, built: &BuiltCharacter, head: &dyn HeadSurface) -> (Vec3, f32) {
    let points = vertices(built);
    let (lo, hi) =
        points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
    let size = (hi - lo).max_element();
    if matches!(kind, "wings" | "tail") {
        ((lo + hi) / 2.0, 0.6 * size)
    } else {
        // The head and its hair, with the accessory in view.
        let r = head.radius(Vec3::Y);
        (head.centre() + Vec3::Y * (0.4 * r), (0.75 * size).max(1.75 * r))
    }
}

fn render_tile(built: &BuiltCharacter, target: Vec3, half: f32, azimuth_deg: f32) -> Vec<u8> {
    let Projection::Perspective { fov_y_deg, .. } = Camera::default().projection else {
        panic!("a perspective camera")
    };
    let distance = half / (fov_y_deg.to_radians() / 2.0).tan();
    let mut renderer = MockRenderer::new();
    let eye = target + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::Z * distance);
    let mut scene = Scene::new(Camera::look_at(eye, target, Vec3::Y, Camera::default().projection));
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
    for m in &built.meshes {
        let mesh = renderer.create_mesh(&m.mesh.geometry).unwrap();
        let mut spec = built.materials[&m.mesh.material].clone();
        if m.part != ACC.parse().unwrap() {
            // The body, head and hair are context: plain toon, because their textures and programs are not uploaded here.
            let colour =
                if m.part.as_str() == "hair" { Colour::rgb(0x6a, 0x4c, 0x5a) } else { Colour::rgb(0xf0, 0xcd, 0xbe) };
            spec = omuzaki_char_api::MaterialSpec::toon(colour);
        }
        spec.overlays.clear();
        spec.base_texture = None;
        let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    }
    let quality = Quality { samples: 4, ..Quality::offline() };
    let image = renderer.render_image(UVec2::splat(TILE), AlphaEncoding::Straight, &scene, &quality).unwrap();
    let mut rgb = Vec::new();
    for y in 0..image.size.y {
        for x in 0..image.size.x {
            let [r, g, b, _] = image.pixel(x, y).unwrap();
            rgb.extend([r, g, b]);
        }
    }
    rgb
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_accessory_sheets_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_ACCESSORY_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let hair = hairstyles().into_iter().next().unwrap().1;
    for (kind, azimuths) in KINDS {
        for template in ["standard", "soft-sd"] {
            let list = styles(kind);
            let (w, h) = (TILE as usize * azimuths.len(), TILE as usize * list.len());
            let mut sheet = vec![0_u8; w * h * 3];
            for (row, (name, params)) in list.iter().enumerate() {
                let def = character_with(template, Some(&hair), &[(ACC, kind, params.clone(), None)]);
                let s = scene(&def).unwrap_or_else(|e| panic!("{kind} {name} on {template}: {e}"));
                let (target, half) = frame(kind, &s.built, s.head.as_ref());
                for (col, azimuth) in azimuths.iter().enumerate() {
                    let tile = render_tile(&s.built, target, half, *azimuth);
                    for y in 0..TILE as usize {
                        let from = y * TILE as usize * 3;
                        let to = ((row * TILE as usize + y) * w + col * TILE as usize) * 3;
                        sheet[to..to + TILE as usize * 3].copy_from_slice(&tile[from..from + TILE as usize * 3]);
                    }
                }
            }
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            ppm.extend(sheet);
            std::fs::write(directory.join(format!("{kind}-{template}.ppm")), ppm).unwrap();
        }
    }
}
