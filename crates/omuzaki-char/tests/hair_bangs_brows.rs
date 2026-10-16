//! The brows show through every bangs preset: with `brows_through` on, the hair material composites the painted brows
//! over the fringe at the contract's 0.85 strength, on every template.
//!
//! **How it is measured.** Every material is drawn unlit and white so the pixels are exact, and the face is painted by
//! the mock painter. Four front renders of the real head and hair parts:
//! - `brows`: the head alone with the Brows layer at full strength, so a pixel that differs from white is on the brow line;
//! - `cover`: the head black and the hair white, so a white pixel has hair in front of the face;
//! - `bare`: the hair with no overlay, white;
//! - `shown`: the hair with the overlays its material carries (the Brows layer when `brows_through` is on).
//!
//! The unobscured share of the brow line is the brow's weight (how strongly each pixel is brow) with no hair in front of
//! it, plus, under hair, the change the overlay makes to the hair divided by its strength (so a full overlay counts the
//! pixel whole), over the brow's whole weight. With the overlay off the hair hides what it covers. The acceptance is
//! that 85 % of the brow line is unobscured, for every preset on all four templates.
//! `dump_the_brows_pictures_when_asked` writes the pictures for the evidence case.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel indices

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char::ops::hair::bangs::presets::PRESETS;
use omuzaki_char_api::{
    BodyTemplateId, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, Dynamics,
    FaceOverlay, Generator, GroupId, HairGroup, HairParams, MaterialSpec, PartDef, PartId, PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceRasterizer, FaceView, Raster};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt,
    Scene, Shading, TextureDesc, TextureId, Tonemap,
};
use omuzaki_render_mock::MockRenderer;
use serde_json::{Value, json};

/// Pixels along a side of a picture.
const SIZE: u32 = 256;
/// The templates the presets are tried on.
const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];
/// A brow pixel is one that differs from white by more than this (0..1, largest channel) at full strength.
const BROW_PIXEL: f32 = 0.1;
/// The overlay's strength: the contract's 0.85 for the Brows layer on hair.
const STRENGTH: f32 = 0.85;
/// The acceptance: this share of the brow line is unobscured.
const UNOBSCURED: f32 = 0.85;

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The real body, head and a hair part holding only a bangs group with `bangs`, on `template`.
fn character(template: &str, bangs: omuzaki_char_api::BangsGen, brows_through: bool) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let mut hair = HairParams { groups: std::collections::BTreeMap::default(), ..HairParams::default() };
    hair.shading.brows_through = brows_through;
    hair.groups.insert(
        GroupId::new("b-bangs").unwrap(),
        HairGroup { enabled: true, mirror: false, dynamics: Dynamics::default(), generator: Generator::Bangs(bangs) },
    );
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    def
}

fn build(def: &CharacterDef) -> BuiltCharacter {
    omuzaki_char::link();
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("the character doesn't build: {e}"))
}

/// The Brows layer, painted by the mock painter for the built character's face and rasterised.
fn brows_raster(built: &BuiltCharacter) -> (Raster, omuzaki_face_api::FaceLayerSpec) {
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
    let spec = *painter.layers().iter().find(|s| s.id == FaceLayerId::Brows).unwrap();
    let mut raster = Raster::new(1, 1);
    MockRasterizer.rasterize(&spec, &drawing[FaceLayerId::Brows], spec.px_per_m, &mut raster).unwrap();
    (raster, spec)
}

/// What a part is drawn as in one of the pictures.
#[derive(Clone)]
struct Draw {
    base: Colour,
    /// `None` draws the material's own overlays; `Some` replaces them.
    overlays: Option<Vec<FaceOverlay>>,
}

/// The picture of the head and hair parts, drawn unlit as `head` and `hair` say (`None`: left out).
#[allow(clippy::needless_pass_by_value)] // the draws are small, built per picture
fn picture(
    built: &BuiltCharacter,
    brows: &(Raster, omuzaki_face_api::FaceLayerSpec),
    background: Colour,
    head: Option<Draw>,
    hair: Option<Draw>,
) -> Vec<[f32; 3]> {
    let mut renderer = MockRenderer::new();
    let (raster, _) = brows;
    let desc = TextureDesc {
        alpha: AlphaEncoding::Premultiplied,
        ..TextureDesc::colour(UVec2::new(raster.width, raster.height))
    };
    let texture: TextureId = renderer.create_texture(&desc, Some(&raster.pixels)).unwrap();
    let (head_part, hair_part) = (PartId::new("head").unwrap(), PartId::new("hair").unwrap());
    // Aim at the middle of the face window (the brows' neighbourhood), whatever the template's stature.
    let window =
        built.materials.values().flat_map(|m| m.overlays.iter()).next().expect("the head carries an overlay").window;
    let centre = ((window.min + window.max) * 0.5).extend(0.0);
    let mut scene = Scene::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, 1.0),
        centre,
        Vec3::Y,
        // Orthographic, so a pixel looks along the layer's own planar projection and the brow does not shift
        // with the fringe's distance in front of the face.
        Projection::Orthographic { height: 0.6, near: 0.05, far: 4.0 },
    ));
    scene.background = Background::Solid { colour: background };
    scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
    for built_mesh in &built.meshes {
        let draw = if built_mesh.part == head_part {
            head.clone()
        } else if built_mesh.part == hair_part {
            hair.clone()
        } else {
            None
        };
        let Some(draw) = draw else { continue };
        let spec: MaterialSpec = built.materials[&built_mesh.mesh.material].clone();
        let flat = MaterialSpec {
            base: draw.base,
            base_texture: None,
            shading: Shading::Unlit,
            overlays: draw.overlays.unwrap_or(spec.overlays),
            alpha: AlphaMode::Opaque,
            vertex_colours: false,
            outline: None,
            ..spec
        };
        let material = renderer.create_material(&flat.to_material(&|_| None, &|_| Some(texture)).unwrap()).unwrap();
        let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    }
    let quality = Quality { samples: 1, ..Quality::offline() };
    let image = renderer.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, &scene, &quality).unwrap();
    (0..SIZE * SIZE)
        .map(|i| {
            let [r, g, b, _] = image.pixel(i % SIZE, i / SIZE).unwrap();
            [r, g, b].map(|c| f32::from(c) / 255.0)
        })
        .collect()
}

/// How much the brow line is covered and shown in one built character.
#[derive(Debug, Default, Clone, Copy)]
struct Measure {
    brow_pixels: usize,
    under_hair: usize,
    /// The share of the brow line (weighted by how strongly each pixel is brow) that is unobscured.
    unobscured: f32,
    /// The brow that comes through under hair, as a share of what the brow would add to the bare hair: the overlay's
    /// strength where it is on, 0 where it is off.
    mean_through: f32,
}

impl Measure {
    fn share(&self) -> f32 {
        self.unobscured
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// The Brows overlay the hair material of `template` carries when the brows show through: where the layer lands on this
/// template's head, at the contract's strength.
fn brows_overlay(template: &str) -> FaceOverlay {
    let built = build(&character(template, (PRESETS[4].make)(), true));
    built
        .materials
        .values()
        .find_map(|m| m.overlays.iter().find(|o| o.layer == FaceLayerId::Brows).copied())
        .expect("the hair material carries the Brows overlay when brows_through is on")
}

fn measure(built: &BuiltCharacter, overlay: FaceOverlay) -> (Measure, [Vec<[f32; 3]>; 4]) {
    let brows = brows_raster(built);
    let white = Draw { base: Colour::WHITE, overlays: None };
    let none = Draw { base: Colour::WHITE, overlays: Some(Vec::new()) };
    let full = FaceOverlay { strength: 1.0, ..overlay };
    let m = picture(built, &brows, Colour::WHITE, Some(Draw { base: Colour::WHITE, overlays: Some(vec![full]) }), None);
    let cover = picture(
        built,
        &brows,
        Colour::BLACK,
        Some(Draw { base: Colour::BLACK, overlays: Some(Vec::new()) }),
        Some(none.clone()),
    );
    let bare = picture(built, &brows, Colour::BLACK, Some(white.clone()), Some(none));
    let shown = picture(built, &brows, Colour::BLACK, Some(white.clone()), Some(white));
    let mut out = Measure::default();
    let (mut weight, mut seen, mut through_sum, mut covered_weight) = (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
    for i in 0..m.len() {
        let w = distance(m[i], [1.0; 3]);
        if w / 3.0 <= BROW_PIXEL {
            continue;
        }
        out.brow_pixels += 1;
        weight += w;
        if cover[i][0] > 0.5 {
            // Under hair: the brow that comes through is the overlay's change to the hair, scaled back by its strength.
            out.under_hair += 1;
            covered_weight += w;
            let change = distance(bare[i], shown[i]);
            through_sum += change;
            seen += (change / STRENGTH).min(w);
        } else {
            seen += w;
        }
    }
    out.unobscured = seen / weight.max(1e-6);
    out.mean_through = through_sum / covered_weight.max(1e-6);
    (out, [m, cover, bare, shown])
}

#[test]
fn the_brows_show_through_every_preset_on_every_template() {
    let mut any_under_hair = false;
    for template in TEMPLATES {
        for preset in PRESETS {
            let bangs = (preset.make)();
            let overlay = brows_overlay(template);
            let (on, _) = measure(&build(&character(template, bangs.clone(), true)), overlay);
            let (off, _) = measure(&build(&character(template, bangs, false)), overlay);
            println!(
                "{template:8} {:11} brow {:5} px, under hair {:5}, through {:.2}, unobscured {:.3} (brows_through off: {:.3})",
                preset.id,
                on.brow_pixels,
                on.under_hair,
                on.mean_through,
                on.share(),
                off.share()
            );
            assert!(on.brow_pixels > 0, "{template} {}: the brow line is not in the picture", preset.id);
            assert!(
                on.share() >= UNOBSCURED,
                "{template} {}: only {:.3} of the brow line is unobscured",
                preset.id,
                on.share()
            );
            // Without the overlay the same fringe hides what it covers, so the measure can tell the two apart.
            assert!(
                off.share() <= on.share(),
                "{template} {}: the overlay cannot make the brows less visible",
                preset.id
            );
            any_under_hair |= on.under_hair > 0;
            if on.under_hair > 0 {
                assert!(
                    off.share() + 0.01 < on.share(),
                    "{template} {}: hair covers the brows, so switching the overlay off must show",
                    preset.id
                );
            }
        }
    }
    assert!(any_under_hair, "no preset put hair over the brows, so the measure proved nothing");
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn dump_the_brows_pictures_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_BANGS_BROWS_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for template in TEMPLATES {
        let overlay = brows_overlay(template);
        for preset in PRESETS {
            let built = build(&character(template, (preset.make)(), true));
            let (_, [m, cover, bare, shown]) = measure(&built, overlay);
            for (name, pixels) in [("brows", m), ("cover", cover), ("bare", bare), ("shown", shown)] {
                let mut ppm = format!("P6\n{SIZE} {SIZE}\n255\n").into_bytes();
                for p in pixels {
                    ppm.extend(p.map(|c| (c * 255.0).round() as u8));
                }
                std::fs::write(directory.join(format!("{template}-{}-{name}.ppm", preset.id)), ppm).unwrap();
            }
        }
    }
}

/// The built character as the mock toon renderer draws it (materials, outlines and the painted Base and Brows layers),
/// from `azimuth_deg` round the face.
fn toon_picture(built: &BuiltCharacter, azimuth_deg: f32) -> Vec<u8> {
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
    let mut textures = std::collections::BTreeMap::new();
    for spec in painter.layers() {
        let mut raster = Raster::new(1, 1);
        MockRasterizer.rasterize(spec, &drawing[spec.id], spec.px_per_m, &mut raster).unwrap();
        let desc = TextureDesc {
            alpha: AlphaEncoding::Premultiplied,
            ..TextureDesc::colour(UVec2::new(raster.width, raster.height))
        };
        textures.insert(spec.id, renderer.create_texture(&desc, Some(&raster.pixels)).unwrap());
    }
    let window = built.materials.values().flat_map(|m| m.overlays.iter()).next().expect("an overlay").window;
    let centre = ((window.min + window.max) * 0.5).extend(0.0) + Vec3::new(0.0, 0.03, 0.0);
    let eye = centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.0, 1.0));
    let mut scene = Scene::new(Camera::look_at(
        eye,
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 22.0, near: 0.05, far: 8.0 },
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
    for built_mesh in built.meshes.iter().filter(|m| matches!(m.part.as_str(), "head" | "hair")) {
        let spec = built.materials[&built_mesh.mesh.material].clone();
        let material = renderer
            .create_material(&spec.to_material(&|_| None, &|layer| textures.get(&layer).copied()).unwrap())
            .unwrap();
        let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    }
    let quality = Quality { samples: 4, ..Quality::offline() };
    let image = renderer.render_image(UVec2::splat(384), AlphaEncoding::Straight, &scene, &quality).unwrap();
    let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
    for y in 0..image.size.y {
        for x in 0..image.size.x {
            let [r, g, b, _] = image.pixel(x, y).unwrap();
            ppm.extend([r, g, b]);
        }
    }
    ppm
}

/// The fringe through the mock toon renderer, front, side and back, for each preset on soft-sd and standard: the pictures
/// the reviewer looks at.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_bangs_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_BANGS_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for template in ["soft-sd", "standard"] {
        for preset in PRESETS {
            let built = build(&character(template, (preset.make)(), true));
            for (view, azimuth) in [("front", 0.0), ("side", 90.0), ("back", 180.0)] {
                std::fs::write(
                    directory.join(format!("{template}-{}-{view}.ppm", preset.id)),
                    toon_picture(&built, azimuth),
                )
                .unwrap();
            }
        }
    }
}
