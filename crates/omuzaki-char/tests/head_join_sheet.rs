//! The eye-line join of every head preset, close up: the evidence for the soft waist the join leaves (#798).
//!
//! The scene is the one `tests/head_part.rs` renders, on the same stand-in body. Each preset's head is built on its own
//! and framed at its join, `outline.top`: the camera is centred on that height and sized from the built head's width, so
//! no view is tuned by hand.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::sync::Arc;

use omuzaki_char::head::presets::ShapeProfiles;
use omuzaki_char::template::Template;
use omuzaki_char::template::layout::BodyLayout;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char::template::soft_sd::SoftSd;
use omuzaki_char_api::{
    BodyFit, BodyParams, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, FitFact, HeadParams, PartCategory, PartDef, PartKind, PartKindId, PartMeta, PartOutput, Publishes,
    RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::narrow;
use serde_json::{Value, json};

/// The soft-sd template's body fit, published the way the body part publishes it (as in `tests/head_part.rs`).
struct TemplateBody;

impl PartKind for TemplateBody {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-template-body", summary: "a stand-in body", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &Self::Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let layout: BodyLayout = SoftSd.layout(&p.proportions, &p.shape, &p.hand);
        let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<FitFact>(fit)))
    }
}

omuzaki_char_api::register_part_kind!(TemplateBody);

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// A character of the template body and a head with `head` parameters.
fn character(head: &Value) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert("body".parse().unwrap(), part("test-template-body", json!({})));
    def.parts.insert("head".parse().unwrap(), part("head", head.clone()));
    def
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    let id = CharId::new("t").unwrap();
    RegistryBuilder.build(&id, def, &BuildOptions::default(), &BuildServices::default())
}

/// The frame's half-size is this many head widths at the join. The frame spans 1.2 head widths, so the join's silhouette
/// sits inside it with background beyond it in the front view.
const HALF_FRAME_PER_WIDTH: f32 = 0.6;

/// The half-height of the slab of the built head that gives the head's width at the join.
const JOIN_SLAB: f32 = 0.005;

/// The views: a name and the azimuth of the camera around the join, in degrees.
const VIEWS: [(&str, f32); 3] = [("front", 0.0), ("three-quarter", 35.0), ("side", 90.0)];

/// Renders the eye-line join of each of the five presets (front, three-quarter and side) through the mock toon renderer
/// when `OMUZAKI_HEAD_JOIN_PPM=<directory>` is set, as binary PPM files named `<preset>-<view>.ppm`. The painted face is
/// not composited (no painter runs here); the join is judged on the pictures, not on numbers.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_head_join_when_asked() {
    use omuzaki_core::colour::Colour;
    use omuzaki_core::glam::{Mat4, UVec2};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Projection, Quality, Renderer, RendererExt, Scene,
        Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;

    let Some(directory) = std::env::var_os("OMUZAKI_HEAD_JOIN_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let Projection::Perspective { fov_y_deg, .. } = Camera::default().projection else {
        panic!("a perspective camera")
    };
    for id in ShapeProfiles::list().unwrap().iter().map(|p| p.id.as_str()) {
        let profile = ShapeProfiles::get(id).unwrap();
        let params = HeadParams { outline: profile.profile.outline, ..HeadParams::default() };
        let built = build(&character(&serde_json::to_value(&params).unwrap())).unwrap();
        // The head's local origin is its cranium centre, lowered by the neck drop (as `tests/head_part.rs` places it).
        let join = Vec3::new(0.0, narrow(params.cranium.centre[1] - params.neck.drop + params.outline.top), 0.0);
        let head = &built.meshes[0].mesh.geometry.positions;
        // The head's width at the join, measured on the built mesh in a thin slab at the join height.
        let (lo, hi) = head
            .iter()
            .filter(|p| (p.y - join.y).abs() < JOIN_SLAB)
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
        let half = HALF_FRAME_PER_WIDTH * (hi - lo);
        let distance = half / (fov_y_deg.to_radians() / 2.0).tan();
        for (view, azimuth_deg) in VIEWS {
            let mut renderer = MockRenderer::new();
            let eye = join + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::Z * distance);
            let mut scene = Scene::new(Camera::look_at(eye, join, Vec3::Y, Camera::default().projection));
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
                let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
                let mut spec = built.materials[&built_mesh.mesh.material].clone();
                spec.overlays.clear();
                let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
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
            std::fs::write(directory.join(format!("{id}-{view}.ppm")), ppm).unwrap();
        }
    }
}
