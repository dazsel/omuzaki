//! The `head` part kind through the real builder: what it builds, what it publishes, and how it sits on a body.
//!
//! The `body` kind is built elsewhere, so these tests stand a small body in for it: a part of kind
//! `test-template-body` that publishes the soft-sd template's fit, exactly what the real body publishes for the head.
//! Every number here is the template default or plainly synthetic.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp)] // exact values that were copied, not computed

use std::collections::BTreeSet;
use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::template::Template;
use omuzaki_char::template::layout::BodyLayout;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char::template::soft_sd::SoftSd;
use omuzaki_char_api::{
    BodyFit, BodyParams, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, Chin, FitFact, HeadParams, JointRef, KindProvenance, PartCategory, PartDef, PartId, PartKind,
    PartKindId, PartMeta, PartOutput, Published, Publishes, RegistryBuilder, find_kind, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{FaceLayerId, narrow};
use omuzaki_render_api::Shading;
use serde_json::{Value, json};

/// The soft-sd layout at neutral sliders.
fn default_layout() -> BodyLayout {
    let body = BodyParams::default();
    SoftSd.layout(&body.proportions, &body.shape, &body.hand)
}

/// The soft-sd template's body fit, published the way the body part publishes it.
struct TemplateBody;

impl PartKind for TemplateBody {
    type Params = omuzaki_char_api::BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-template-body", summary: "a stand-in body", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &Self::Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let layout = SoftSd.layout(&p.proportions, &p.shape, &p.hand);
        let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<FitFact>(fit)))
    }
}

omuzaki_char_api::register_part_kind!(TemplateBody);

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

/// A character of the template body and a head with `head` parameters.
fn character(head: &Value) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("test-template-body", json!({})));
    def.parts.insert(pid("head"), part("head", head.clone()));
    def
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    let id = CharId::new("t").unwrap();
    RegistryBuilder.build(&id, def, &BuildOptions::default(), &BuildServices::default())
}

fn head_joint(built: &BuiltCharacter, bone: HumanoidBone) -> Vec3 {
    let skeleton = &built.rig.skeleton;
    let mut world = omuzaki_anim_api::Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    world.local[skeleton.humanoid_bone(bone).expect("the bone exists").index()].translation
}

#[test]
fn the_kind_is_registered_as_a_project_owned_head_that_needs_the_body() {
    let reg = find_kind("head").expect("registered");
    let meta = (reg.meta)();
    assert_eq!((meta.category, meta.requires), (PartCategory::Head, &["body"][..]));
    assert_eq!((reg.provenance)(), KindProvenance::ProjectOwned);
    assert_eq!((reg.defaults)(), serde_json::to_value(HeadParams::default()).unwrap());
}

#[test]
fn it_builds_a_head_a_neck_and_two_ears_skinned_to_the_head_neck_and_chest() {
    let built = build(&character(&json!({}))).unwrap();
    let names: Vec<_> = built.meshes.iter().map(|m| m.mesh.name.as_str()).collect();
    assert_eq!(names, ["head-head", "head-neck", "head-ear-l", "head-ear-r"]);
    let materials: BTreeSet<_> = built.materials.keys().map(|m| m.as_str().to_owned()).collect();
    assert!(["head-skin-head", "head-skin-ear", "head-skin-neck"].iter().all(|m| materials.contains(*m)));
    let head = &built.meshes[0].mesh;
    assert_eq!(head.joint_bones, [JointRef::HEAD, JointRef::NECK]);
    assert_eq!(head.geometry.positions.len(), 129 * 97, "the live grid");
    assert!(head.geometry.weights.iter().any(|w| w[0] == 0.8 && w[1] > 0.19), "a chin that follows the neck");
    assert!(head.geometry.weights.iter().any(|w| w[0] == 1.0));
}

#[test]
fn the_face_material_is_flat_lit_shadowless_outlined_and_carries_the_face_overlay() {
    let built = build(&character(&json!({}))).unwrap();
    let m = &built.materials[&"head-skin-head".parse().unwrap()];
    let Shading::Toon(t) = &m.shading else { panic!("toon") };
    assert_eq!((t.shift, t.softness, t.rim.strength), (-0.28, 0.28, 0.10));
    let skin = t.skin.expect("skin treatment");
    assert_eq!((skin.floor, skin.hue_keep), (0.66, 0.55));
    assert!(!m.receives_shadows && !m.double_sided);
    assert!(m.hair_shadow.receives && !m.hair_shadow.casts, "the fringe's shadow falls on the face (#2528)");
    assert_eq!(m.outline.map(|o| o.width), Some(0.0014));
    let layers: Vec<FaceLayerId> = m.overlays.iter().map(|o| o.layer).collect();
    assert_eq!(layers, [FaceLayerId::Base, FaceLayerId::Overlay]);
    assert!(built.face.is_some(), "the build can attach the painted face");
    let ear = &built.materials[&"head-skin-ear".parse().unwrap()];
    assert!(ear.double_sided && ear.overlays.is_empty());
    let neck = &built.materials[&"head-skin-neck".parse().unwrap()];
    assert!(neck.outline.is_none(), "the neck has no outline");
    let Shading::Toon(t) = &neck.shading else { panic!("toon") };
    let skin = t.skin.expect("the neck keeps the skin's hue under coloured light, as the ears and the body do");
    assert_eq!((skin.floor, skin.hue_keep), (0.42, 0.55));
}

#[test]
fn the_face_window_is_the_published_projection_moved_with_the_lowered_head() {
    let mut params = HeadParams::default();
    params.neck.drop = 0.03;
    let built = build(&character(&serde_json::to_value(&params).unwrap())).unwrap();
    let m = &built.materials[&"head-skin-head".parse().unwrap()];
    let window = m.overlays[0].window;
    let projection = params.face_projection();
    let centre_y = narrow(params.cranium.centre()[1] - 0.03);
    assert!((window.min.y - (projection.rect.y0 + centre_y)).abs() < 1e-6);
    assert!((window.max.x - projection.rect.x1).abs() < 1e-6);
}

#[test]
fn the_face_window_follows_the_built_chin_when_the_asked_chin_is_out_of_reach() {
    // A chin asked 50 cm below the centre: the head clamps it into its plausible space, and the window follows the chin
    // the mesh has, not the one asked for (#693).
    let mut params = HeadParams::default();
    params.outline.chin = Some(Chin { y: -0.5, ..Chin::default() });
    let built = build(&character(&serde_json::to_value(&params).unwrap())).unwrap();
    let head = built.meshes.iter().find(|m| m.mesh.name == "head-head").expect("the head mesh");
    let lowest = head.mesh.geometry.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let window = built.materials[&"head-skin-head".parse().unwrap()].overlays[0].window;
    let below = narrow(params.window.below_chin);
    assert!((window.min.y - (lowest - below)).abs() < 2e-3, "window from {} for a chin at {lowest}", window.min.y);
    let asked = narrow(params.cranium.centre()[1]) + params.face_projection().rect.y0;
    assert!(window.min.y > asked + 0.05, "the window no longer reaches the asked chin: {} vs {asked}", window.min.y);
}

#[test]
fn the_head_joint_sits_the_drop_below_the_templates() {
    let template = default_layout();
    let template_head = template.head_base().expect("the template has a head base").as_vec3();
    for drop in [0.016_f64, 0.0, -0.022] {
        let mut params = HeadParams::default();
        params.neck.drop = drop;
        let built = build(&character(&serde_json::to_value(&params).unwrap())).unwrap();
        let head = head_joint(&built, HumanoidBone::Head);
        assert!((head - (template_head - Vec3::Y * narrow(drop))).length() < 1e-5, "drop {drop}: {head:?}");
        for eye in [HumanoidBone::LeftEye, HumanoidBone::RightEye, HumanoidBone::Jaw] {
            assert!(built.rig.skeleton.humanoid_bone(eye).is_some(), "{eye:?}");
        }
    }
}

#[test]
fn the_eyes_straddle_the_face_and_sit_on_the_layout_eye_line() {
    let built = build(&character(&json!({}))).unwrap();
    let (left, right) = (head_joint(&built, HumanoidBone::LeftEye), head_joint(&built, HumanoidBone::RightEye));
    let layout = mock_character().unwrap().face.layout;
    let centre_y = HeadParams::default().cranium.centre()[1] - HeadParams::default().neck.drop;
    assert!(left.x > 0.0 && right.x < 0.0, "the character's left is +X");
    assert!((left.x + right.x).abs() < 1e-6 && (left.x - narrow(layout.eye_spacing)).abs() < 1e-5);
    assert!((left.y - narrow(centre_y + layout.eye_y)).abs() < 1e-5);
    assert!(left.z > 0.0, "in front of the head joint's plane");
}

#[test]
fn the_published_surface_projection_and_anchor_are_the_lowered_cranium() {
    let mut params = HeadParams::default();
    params.neck.drop = 0.02;
    let def = character(&serde_json::to_value(&params).unwrap());
    let built = build(&def).unwrap();
    let anchor = built.anchors.get(&(pid("head"), "top".parse().unwrap())).expect("top anchor");
    let top = narrow(params.cranium.centre()[1] + params.cranium.half_height_up - 0.02);
    assert!((anchor.position.y - top).abs() < 1e-6, "{anchor:?}");
    assert_eq!(anchor.bone, JointRef::HEAD);
}

#[test]
fn a_head_without_a_body_is_an_error_naming_what_is_missing() {
    let mut def = character(&json!({}));
    def.parts.remove(&pid("body"));
    let e = build(&def).unwrap_err();
    assert!(matches!(e, BuildError::Missing { needs: "a body to fit over", .. }), "{e:?}");
}

#[test]
fn a_traced_head_builds_with_a_warning_until_the_map_can_be_applied() {
    let traced = omuzaki_char_api::TracedHead {
        asset: "a".repeat(64).parse().unwrap(),
        theta_steps: 8,
        phi_steps: 16,
        mix: 1.0,
    };
    let params = HeadParams { traced: Some(traced), ..HeadParams::default() };
    let built = build(&character(&serde_json::to_value(&params).unwrap())).unwrap();
    assert!(built.report.parts.iter().flat_map(|p| &p.warnings).any(|w| w.contains("traced")), "{:?}", built.report);
}

#[test]
fn the_head_kind_orders_after_a_body_kind_whatever_the_ids() {
    // A head whose id sorts before the body's still builds after it, so the dropped joint is not overwritten.
    let mut def = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(pid("a-head"), part("head", json!({})));
    def.parts.insert(pid("z-body"), part("body", json!({})));
    assert_eq!(plan(&def).unwrap(), [pid("z-body"), pid("a-head")]);
}

#[test]
fn the_dropped_head_joint_wins_over_the_body_in_either_absorb_order() {
    let body: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(default_layout()));
    let template_head = body.joint(&JointRef::HEAD).unwrap();
    let dropped = template_head - Vec3::Y * 0.016;
    let head: Arc<dyn BodyFit> = Arc::new(omuzaki_char_api::JointFit::new([(JointRef::HEAD, dropped)]));
    assert!(!head.envelope());
    for head_first in [true, false] {
        let mut published = Published::default();
        let (b, h) = ((pid("body"), body.clone()), (pid("head"), head.clone()));
        let order = if head_first { [h, b] } else { [b, h] };
        for (part, fit) in order {
            published.absorb(&part, Publishes::default().with::<FitFact>(fit)).unwrap();
        }
        assert_eq!(published.joint(&JointRef::HEAD), Some(dropped), "head first: {head_first}");
    }
}

#[test]
fn parameters_out_of_range_are_rejected_not_built() {
    let e = build(&character(&json!({ "neck": { "drop": 5.0 } }))).unwrap_err();
    assert!(matches!(e, BuildError::Params { .. }), "{e:?}");
}

#[test]
fn the_same_inputs_build_the_same_bytes() {
    let def = character(&json!({}));
    let (a, b) = (build(&def).unwrap(), build(&def).unwrap());
    assert_eq!(a.meshes[0].mesh.geometry, b.meshes[0].mesh.geometry);
    assert_eq!(a.meshes[1].mesh.geometry, b.meshes[1].mesh.geometry);
}

#[test]
fn the_mock_hair_and_garments_build_on_the_real_head() {
    let mut def = mock_character().unwrap();
    def.parts.get_mut(&pid("head")).unwrap().kind = PartKindId::new("head").unwrap();
    // The mock hair requires the mock head by kind, so layer it after the head instead.
    def.parts.get_mut(&pid("hair")).unwrap().layer = 5;
    let built = build(&def).unwrap();
    let hair = built.meshes.iter().filter(|m| m.part.as_str() == "hair").count();
    assert!(hair > 0, "the hair is built over the published head surface");
    assert!(built.face.is_some());
}

// ------------------------------------------------------------------------------------------------ the evidence render

/// Renders the built head part (front, three-quarter and side) through the mock toon renderer when
/// `OMUZAKI_HEAD_PART_PPM=<directory>` is set, as binary PPM files. The painted face is not composited (no painter runs
/// here); the overlay is checked by the material tests. Taste is judged on the pictures, not on numbers.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_head_part_when_asked() {
    use omuzaki_core::colour::Colour;
    use omuzaki_core::glam::{Mat4, UVec2};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;

    let Some(directory) = std::env::var_os("OMUZAKI_HEAD_PART_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let built = build(&character(&json!({}))).unwrap();
    let centre = Vec3::new(0.0, 0.70, 0.0);
    for (view, azimuth_deg) in [("front", 0.0_f32), ("three-quarter", 35.0), ("side", 90.0)] {
        let mut renderer = MockRenderer::new();
        let mut scene = Scene::new(Camera::look_at(
            centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.05, 1.6)),
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
        for built_mesh in &built.meshes {
            let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
            let mut spec = built.materials[&built_mesh.mesh.material].clone();
            spec.overlays.clear();
            let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
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
