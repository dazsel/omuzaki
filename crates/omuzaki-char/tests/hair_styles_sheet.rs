//! The hair-styles evidence sheet (`crates/omuzaki-char/evidence/hair-styles.toml`): every hair generator in a whole
//! hairstyle on the real body, head and hair parts, through the mock toon renderer with the face painted, so a reviewer
//! sees each style as a character wears it. One picture per style and template, front, side and back side by side.
//!
//! The styles are the default style, the default with each bangs preset, and the default with its curtain swapped for a
//! ponytail, twintails, a low and a high bun, or framing locks with a braid crown. Each is built on `soft-sd` and
//! `standard`, and the test also checks that every one of them builds there within the hair budget, so the sheet can't
//! silently drop one.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel indices

use std::collections::BTreeMap;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char::hair::r#gen::{bun, tail};
use omuzaki_char::ops::hair::bangs::presets::PRESETS;
use omuzaki_char_api::{
    BodyTemplateId, BraidGen, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    Dynamics, FramingGen, Generator, GroupId, HAIR_PROGRAM, HairGroup, HairParams, HairShading, PartDef, PartId,
    PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{ColourRef, FaceDrawing, FaceInput, FacePainter, FaceRasterizer, FaceView, Raster};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt, Scene,
    Shading, TextureDesc, Tonemap,
};
use omuzaki_render_mock::MockRenderer;
use serde_json::{Value, json};

/// Pixels along a side of one view.
const VIEW: u32 = 384;
/// The templates the sheet is drawn on.
const TEMPLATES: [&str; 2] = ["soft-sd", "standard"];
/// The views, as turns round the character in degrees.
const VIEWS: [(&str, f32); 3] = [("front", 0.0), ("side", 90.0), ("back", 180.0)];

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

fn group(generator: Generator, mirror: bool) -> HairGroup {
    HairGroup {
        enabled: true,
        mirror,
        dynamics: Dynamics::default(),
        generator,
        lock: omuzaki_char_api::LockShape::default(),
    }
}

fn id(name: &str) -> GroupId {
    GroupId::new(name).unwrap()
}

/// The default style with its curtain (the long back hair) replaced by `groups`.
fn without_curtain(groups: impl IntoIterator<Item = (&'static str, HairGroup)>) -> HairParams {
    let mut hair = HairParams::default();
    hair.groups.retain(|_, g| !matches!(g.generator, Generator::Curtain(_)));
    hair.groups.extend(groups.into_iter().map(|(name, g)| (id(name), g)));
    hair
}

/// Every style on the sheet, by name.
fn styles() -> Vec<(String, HairParams)> {
    let mut styles = vec![("default".to_owned(), HairParams::default())];
    for preset in PRESETS {
        let mut hair = HairParams::default();
        for g in hair.groups.values_mut() {
            if matches!(g.generator, Generator::Bangs(_)) {
                g.generator = Generator::Bangs((preset.make)());
            }
        }
        styles.push((format!("bangs-{}", preset.id), hair));
    }
    styles
        .push(("ponytail".to_owned(), without_curtain([("d-tail", group(Generator::Tail(tail::ponytail()), false))])));
    styles
        .push(("twintails".to_owned(), without_curtain([("d-tail", group(Generator::Tail(tail::twintails()), true))])));
    styles.push(("low-bun".to_owned(), without_curtain([("d-bun", group(Generator::Bun(bun::low_bun()), false))])));
    styles.push(("high-bun".to_owned(), without_curtain([("d-bun", group(Generator::Bun(bun::high_bun()), false))])));
    styles.push((
        "braid-crown".to_owned(),
        without_curtain([
            ("d-framing", group(Generator::Framing(FramingGen::default()), true)),
            ("e-braid", group(Generator::Braid(BraidGen::default()), false)),
        ]),
    ));
    styles
}

/// The real body, head and `hair` on `template`, the hair coloured [`brown`].
fn character(template: &str, hair: &HairParams) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts = BTreeMap::new();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let mut hair = hair.clone();
    brown(&mut hair.shading);
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    def
}

/// The sheet's hair colour: a mid brown, so the clumps, outlines and the tie read against the skin (the default hair
/// is pale).
const BROWN: (u8, u8, u8) = (0x6b, 0x4a, 0x3a);

/// The hair's colours set round [`BROWN`].
fn brown(shading: &mut HairShading) {
    shading.root = ColourRef::rgb(0x4a, 0x30, 0x26);
    shading.mid = ColourRef::rgb(BROWN.0, BROWN.1, BROWN.2);
    shading.tip = ColourRef::rgb(0x80, 0x5c, 0x48);
    shading.shade = ColourRef::rgb(0x3a, 0x26, 0x20);
    shading.line = ColourRef::rgb(0x2c, 0x1c, 0x16);
    shading.outline = ColourRef::rgb(0x30, 0x1e, 0x18);
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, String> {
    omuzaki_char::link();
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .map_err(|e| e.to_string())
}

/// The face's painted layers, as textures of `renderer`.
fn face_textures(
    renderer: &mut MockRenderer,
    built: &BuiltCharacter,
) -> BTreeMap<omuzaki_face_api::FaceLayerId, omuzaki_render_api::TextureId> {
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
    textures
}

/// Where the camera looks and how much it shows, from the painted face's window on the head (`centre`, and its height):
/// head and shoulders whatever the template's size, wide enough for twintails falling past the shoulders.
fn framing(built: &BuiltCharacter) -> (Vec3, f32) {
    let window = built.materials.values().flat_map(|m| m.overlays.iter()).next().expect("the face's overlay").window;
    let height = window.max.y - window.min.y;
    let centre = ((window.min + window.max) * 0.5).extend(0.0);
    (centre - Vec3::new(0.0, 0.45 * height, 0.0), 3.4 * height)
}

/// The three views of `built` side by side, as RGB rows.
fn sheet_row(built: &BuiltCharacter) -> Vec<[u8; 3]> {
    let mut renderer = MockRenderer::new();
    let textures = face_textures(&mut renderer, built);
    let (centre, shown) = framing(built);
    let fov_y_deg = 30.0_f32;
    let distance = 0.5 * shown / (0.5 * fov_y_deg).to_radians().tan();
    let mut instances = Vec::new();
    for built_mesh in &built.meshes {
        let mut spec = built.materials[&built_mesh.mesh.material].clone();
        // The mock renderer draws a shading program as a plain toon of the base colour, and the hair program's base is
        // white (its colours are program parameters), so the sheet gives the hair its mid colour.
        if matches!(&spec.shading, Shading::Program { name, .. } if name.as_str() == HAIR_PROGRAM) {
            spec.base = Colour::rgb(BROWN.0, BROWN.1, BROWN.2);
        }
        let material = renderer
            .create_material(&spec.to_material(&|_| None, &|layer| textures.get(&layer).copied()).unwrap())
            .unwrap();
        let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
        instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    }
    let mut row = vec![[0_u8; 3]; (VIEW * VIEW) as usize * VIEWS.len()];
    for (k, (_, azimuth)) in VIEWS.iter().enumerate() {
        let eye = centre + Mat4::from_rotation_y(azimuth.to_radians()).transform_vector3(Vec3::new(0.0, 0.0, distance));
        let mut scene = Scene::new(Camera::look_at(
            eye,
            centre,
            Vec3::Y,
            Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
        ));
        scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
        scene.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
            Light::Directional {
                to_light: Mat4::from_rotation_y(azimuth.to_radians()).transform_vector3(Vec3::new(1.6, 3.0, 2.4)),
                colour: Colour::hex(0x00ff_f6ec),
                intensity: 1.7,
                casts_shadow: false,
            },
        ];
        scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
        scene.instances.clone_from(&instances);
        let quality = Quality { samples: 4, ..Quality::offline() };
        let image = renderer.render_image(UVec2::splat(VIEW), AlphaEncoding::Straight, &scene, &quality).unwrap();
        for y in 0..VIEW {
            for x in 0..VIEW {
                let [r, g, b, _] = image.pixel(x, y).unwrap();
                row[(y * VIEW * VIEWS.len() as u32 + k as u32 * VIEW + x) as usize] = [r, g, b];
            }
        }
    }
    row
}

/// The most triangles a whole hairstyle may have at the default (Live) quality. The spec names no hair budget, so this
/// is the parity target's own: the prototype's hair in its game variant is 39 968 triangles (cap shell 9 216, static
/// locks 18 400, simulated locks 3 552, curtain 8 800), and no kit style may cost more than the hair it ports.
const HAIR_TRIANGLES: usize = 39_968;

/// The hair part's triangles in `built`.
fn hair_triangles(built: &BuiltCharacter) -> usize {
    built.meshes.iter().filter(|m| m.part.as_str() == "hair").map(|m| m.mesh.geometry.indices.len() / 3).sum()
}

#[test]
fn every_style_on_the_sheet_builds_on_its_templates_within_the_hair_budget() {
    let mut failed = Vec::new();
    for template in TEMPLATES {
        for (name, hair) in styles() {
            match build(&character(template, &hair)) {
                Err(e) => failed.push(format!("{template} {name}: {e}")),
                Ok(built) => {
                    let triangles = hair_triangles(&built);
                    eprintln!("{template} {name}: {triangles} hair triangles");
                    if triangles > HAIR_TRIANGLES {
                        failed.push(format!("{template} {name}: {triangles} triangles, over {HAIR_TRIANGLES}"));
                    }
                }
            }
        }
    }
    assert!(failed.is_empty(), "styles that don't build or cost too much: {failed:#?}");
}

/// Writes `<template>-<style>.ppm` (front, side and back side by side) into `OMUZAKI_HAIR_STYLES_PPM` when it is set.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_hair_styles_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_HAIR_STYLES_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for template in TEMPLATES {
        for (name, hair) in styles() {
            let built = build(&character(template, &hair)).unwrap();
            let row = sheet_row(&built);
            let mut ppm = format!("P6\n{} {VIEW}\n255\n", VIEW * VIEWS.len() as u32).into_bytes();
            ppm.extend(row.iter().flatten());
            std::fs::write(directory.join(format!("{template}-{name}.ppm")), ppm).unwrap();
        }
    }
}
