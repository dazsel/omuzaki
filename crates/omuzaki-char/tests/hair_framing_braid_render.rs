//! Evidence pictures of a hairstyle of the default cap, the default framing locks and the default braid crown, built on
//! the real body and head parts of each template, front, side and back through the mock toon renderer. Written only
//! when `OMUZAKI_HAIR_PPM` names a folder (as PPM); `OMUZAKI_HAIR_TEMPLATES` (comma separated) picks the templates,
//! default `soft-sd,standard`. Taste is judged on the pictures, not on numbers.

#![allow(clippy::unwrap_used, clippy::expect_used)] // evidence helpers: a bad setup should stop with its reason

use std::collections::BTreeMap;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char_api::{
    BodyTemplateId, BraidGen, BuildOptions, BuildServices, BuiltCharacter, CapGen, CharId, CharacterBuilder,
    CharacterDef, Dynamics, FramingGen, Generator, GroupId, GuidePoint, HairGroup, HairParams, PartDef, PartId,
    PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{FaceDrawing, FaceInput, FacePainter, FaceRasterizer, FaceView, Raster};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt, Scene,
    TextureDesc, Tonemap,
};
use omuzaki_render_mock::MockRenderer;
use serde_json::{Value, json};

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The framing locks as the kit makes them; `OMUZAKI_HAIR_OLD` set draws the first defaults (metres for one head and a
/// tail of free points) instead, for a before picture.
fn framing() -> FramingGen {
    if std::env::var_os("OMUZAKI_HAIR_OLD").is_none() {
        return FramingGen::default();
    }
    FramingGen {
        edge: vec![[0.12, 0.012], [0.0, 0.08], [-0.058, 0.136], [-0.12, 0.17], [-0.2, 0.192]],
        tail: vec![GuidePoint::Head([0.2, -0.18, 0.04]), GuidePoint::Head([0.22, -0.3, 0.02])],
        outline_radius: None,
        ..FramingGen::default()
    }
}

/// The real body and head, and a hair part of the default cap, framing locks and braid crown.
fn character(template: &str) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let mut hair = HairParams { groups: BTreeMap::default(), ..HairParams::default() };
    for (id, generator) in [
        ("a-cap", Generator::Cap(CapGen::default())),
        ("b-framing", Generator::Framing(framing())),
        ("c-braid", Generator::Braid(BraidGen::default())),
    ] {
        hair.groups.insert(
            GroupId::new(id).unwrap(),
            HairGroup {
                enabled: true,
                mirror: false,
                dynamics: Dynamics::default(),
                generator,
                lock: omuzaki_char_api::LockShape::default(),
            },
        );
    }
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    def
}

fn build(def: &CharacterDef) -> BuiltCharacter {
    omuzaki_char::link();
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("the character doesn't build: {e}"))
}

/// The built character through the mock toon renderer, from `azimuth_deg` round the head, as PPM bytes.
fn picture(built: &BuiltCharacter, azimuth_deg: f32) -> Vec<u8> {
    let mut renderer = MockRenderer::new();
    let face = built.face.as_ref().expect("the head publishes a face");
    let profile = face.profile.resolve_colours(&built.palette).unwrap();
    let params = profile.mapping.apply(&ChannelBuf::NEUTRAL);
    let mut painter = MockPainter::new(face.projection);
    let mut drawing = FaceDrawing::new();
    painter
        .paint(
            &FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() },
            &mut drawing,
        )
        .unwrap();
    let mut textures = BTreeMap::new();
    for spec in painter.layers() {
        let mut raster = Raster::new(1, 1);
        MockRasterizer.rasterize(spec, &drawing[spec.id], spec.px_per_m, &mut raster).unwrap();
        let desc = TextureDesc {
            alpha: AlphaEncoding::Premultiplied,
            ..TextureDesc::colour(UVec2::new(raster.width, raster.height))
        };
        textures.insert(spec.id, renderer.create_texture(&desc, Some(&raster.pixels)).unwrap());
    }
    // Aim between the face and the chest, whatever the template's proportions: the hair's top and width set the scale.
    let (mut low, mut high) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for m in built.meshes.iter().filter(|m| m.part.as_str() == "hair") {
        for p in &m.mesh.geometry.positions {
            (low, high) = (low.min(*p), high.max(*p));
        }
    }
    let width = high.x - low.x;
    let centre = Vec3::new(0.0, high.y - 1.3 * width, 0.0);
    let eye =
        centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.0, 4.6 * width));
    let mut scene = Scene::new(Camera::look_at(
        eye,
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 34.0, near: 0.05, far: 8.0 },
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
    for built_mesh in &built.meshes {
        let spec = built.materials[&built_mesh.mesh.material].clone();
        let material = renderer
            .create_material(&spec.to_material(&|_| None, &|layer| textures.get(&layer).copied()).unwrap())
            .unwrap();
        let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
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
    ppm
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_framing_and_braid_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_HAIR_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let templates = std::env::var("OMUZAKI_HAIR_TEMPLATES").unwrap_or_else(|_| "soft-sd,standard".to_owned());
    for template in templates.split(',') {
        let built = build(&character(template));
        for (view, azimuth) in [("front", 0.0), ("side", 90.0), ("back", 180.0)] {
            std::fs::write(directory.join(format!("{template}-{view}.ppm")), picture(&built, azimuth)).unwrap();
        }
    }
}
