//! The contact sheets of the accessories (CB-43): every style of every kind on `standard` and `soft-sd`, seen from the
//! front and three-quarter, through the mock toon renderer. Evidence only: it renders when
//! `OMUZAKI_ACCESSORY_PPM=<directory>` is set, as one binary PPM per kind and template, and otherwise does nothing.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // pixels
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{ACC, bind_pose, character_with, scene, skinned_geometry, styles, vertices};
use omuzaki_anim_api::{ChannelBuf, Pose};
use omuzaki_char_api::{BuiltCharacter, CharacterDef, HeadSurface, PartCategory};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt, Scene,
    Tonemap,
};
use omuzaki_render_mock::MockRenderer;

/// The side of one tile, pixels.
const TILE: u32 = 256;

/// What a tile is framed on: the head for what sits on it, the accessory itself for what hangs on the body.
fn frame(on_head: bool, built: &BuiltCharacter, head: &dyn HeadSurface) -> (Vec3, f32) {
    let points = vertices(built);
    let (lo, hi) =
        points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
    let size = (hi - lo).max_element();
    if on_head {
        let r = head.radius(Vec3::Y);
        (head.centre() + Vec3::Y * (0.4 * r), (0.75 * size).max(1.75 * r))
    } else {
        ((lo + hi) / 2.0, 0.6 * size)
    }
}

/// One tile: `built` seen from `azimuth_deg`, posed by `pose` (`None` for the bind pose).
fn render_tile(built: &BuiltCharacter, pose: Option<&Pose>, target: Vec3, half: f32, azimuth_deg: f32) -> Vec<u8> {
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
        let geometry = pose.map_or_else(|| m.mesh.geometry.clone(), |pose| skinned_geometry(built, &m.mesh, pose));
        let mesh = renderer.create_mesh(&geometry).unwrap();
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

/// One sheet: a row per entry of `rows` (name, character, pose), a column per azimuth.
fn sheet(rows: &[(String, CharacterDef, bool)], on_head: bool, azimuths: &[f32]) -> Vec<u8> {
    let (w, h) = (TILE as usize * azimuths.len(), TILE as usize * rows.len());
    let mut sheet = vec![0_u8; w * h * 3];
    for (row, (name, def, driven)) in rows.iter().enumerate() {
        let s = scene(def).unwrap_or_else(|e| panic!("{name}: {e}"));
        // A driven pose: the drivers run with every channel at neutral, which folds what folds.
        let pose = driven.then(|| {
            let mut pose = bind_pose(&s.built);
            s.built.rig.driver_plan().unwrap().apply(&ChannelBuf::NEUTRAL, &mut pose);
            pose
        });
        let (target, half) = frame(on_head, &s.built, s.head.as_ref());
        for (col, azimuth) in azimuths.iter().enumerate() {
            let tile = render_tile(&s.built, pose.as_ref(), target, half, *azimuth);
            for y in 0..TILE as usize {
                let from = y * TILE as usize * 3;
                let to = ((row * TILE as usize + y) * w + col * TILE as usize) * 3;
                sheet[to..to + TILE as usize * 3].copy_from_slice(&tile[from..from + TILE as usize * 3]);
            }
        }
    }
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    ppm.extend(sheet);
    ppm
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_accessory_sheets_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_ACCESSORY_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    // Every accessory kind that is registered, found from the registry: nothing here lists them.
    let kinds: Vec<_> = omuzaki_char_api::kinds()
        .filter(|k| (k.meta)().category == PartCategory::Accessory)
        .map(|k| ((k.meta)().id, (k.meta)().requires.contains(&"head")))
        .collect();
    for (kind, on_head) in kinds {
        let azimuths: &[f32] = if on_head { &[0.0, 35.0] } else { &[35.0, 90.0, 180.0] };
        for template in ["standard", "soft-sd"] {
            let mut rows: Vec<(String, CharacterDef, bool)> = Vec::new();
            for (name, params) in styles(kind) {
                let def = character_with(template, None, &[(ACC, kind, params, None)]);
                let driven = !scene(&def).unwrap().built.rig.drivers.is_empty();
                rows.push((format!("{kind} {name} on {template}"), def.clone(), false));
                if driven {
                    rows.push((format!("{kind} {name} on {template}, driven pose"), def, true));
                }
            }
            std::fs::write(directory.join(format!("{kind}-{template}.ppm")), sheet(&rows, on_head, azimuths)).unwrap();
        }
    }
    // The glasses follow the face layout: the default, the eyes lower and closer, and the eyes higher and wider.
    for template in ["standard", "soft-sd"] {
        let rows: Vec<_> = [(-0.09, 0.088), (-0.13, 0.07), (-0.05, 0.105)]
            .into_iter()
            .map(|(eye_y, eye_spacing)| {
                let mut def = character_with(template, None, &[(ACC, "glasses", serde_json::json!({}), None)]);
                def.face.layout.eye_y = eye_y;
                def.face.layout.eye_spacing = eye_spacing;
                (format!("glasses, eyes at {eye_y} and {eye_spacing}"), def, false)
            })
            .collect();
        std::fs::write(directory.join(format!("glasses-layout-{template}.ppm")), sheet(&rows, true, &[0.0, 35.0]))
            .unwrap();
    }
}
