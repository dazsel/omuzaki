//! Landmarks measured on the template characters and the mock: their order, the head found by category, the errors
//! for a character that can't be measured, and a repeat measure that changes nothing.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "scene/templates.rs"]
mod templates;

use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::scene::landmarks::{Bounds, Landmarks};
use omuzaki_stage::{StageError, UploadOptions};

fn mock() -> BuiltCharacter {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

#[test]
fn templates_measure_in_order() {
    for id in templates::TEMPLATES {
        let built = templates::build(id);
        let l = Landmarks::measure(&built).unwrap();
        let (chest, eyes) = (l.chest.expect("a chest"), l.eyes.expect("eyes"));
        let (chin, crown) = (l.chin().expect("a chin"), l.crown().expect("a crown"));
        let hands = l.hands.expect("hands");
        eprintln!(
            "{id}: bottom {:.3} chest {chest:.3} chin {chin:.3} eyes {eyes:.3} crown {crown:.3} top {:.3} \
             hands.max.y {:.3} height {:.3}",
            l.bottom(),
            l.top(),
            hands.max.y,
            l.height_m
        );
        assert!(l.bottom() < chest && chest < chin && chin < eyes && eyes < crown && crown <= l.top(), "{id}");
        let height_m = l.height_m;
        assert!(
            ((l.top() - l.bottom() - height_m) / height_m).abs() < 0.05,
            "{id}: no hair, so the box is the stature"
        );
        assert!(hands.max.y < chest, "{id}: the hands hang below the chest");
        assert_eq!(height_m, built.stature.expect("a stature").height_m, "{id}");
        let bust = l.bust.expect("a bust");
        assert!(bust.min.y >= chest && (bust.max.y - l.top()).abs() < 1e-6, "{id}: the bust runs from the chest up");
    }
}

#[test]
fn the_mock_character_has_a_head_and_a_body() {
    let built = mock();
    assert!(built.meshes.iter().any(|m| m.mesh.name == "head"), "the mock's head mesh is named 'head'");
    let l = Landmarks::measure(&built).unwrap();
    let head = l.head.expect("mock-head is found by its category");
    assert!(head.height() > 0.0 && head.max.y <= l.top() && head.min.y > l.bottom());
    assert_eq!(l.height_m, l.body.height(), "the mock has no stature");
}

#[test]
fn a_character_stands_on_its_soles_and_a_higher_heel_raises_it() {
    let bare = Landmarks::measure(&templates::build("standard")).unwrap();
    assert!(bare.lift < 0.002, "the bare body stands on the floor line: lifted {} m", bare.lift);
    let shod = |heel: f64| {
        let built = templates::build_dressed(
            "standard",
            &["body", "head"],
            &[("boots", serde_json::json!({"heel": {"height": heel}}))],
        );
        let l = Landmarks::measure(&built).unwrap();
        assert!(l.bottom().abs() < 1e-6, "the lowest point is on the floor: {}", l.bottom());
        assert!((l.lift - omuzaki_stage::frame::stand_lift(&built)).abs() < 1e-6, "framing and the stage agree");
        l
    };
    let (low, high) = (shod(0.02), shod(0.06));
    eprintln!("lift: bare {:.4}, 20 mm heel {:.4}, 60 mm heel {:.4}", bare.lift, low.lift, high.lift);
    assert!(low.lift > 0.02, "a boot's sole and heel raise the character: {}", low.lift);
    assert!(((high.lift - low.lift) - 0.04).abs() < 0.002, "40 mm more heel: {} m higher", high.lift - low.lift);
    let (eyes_low, eyes_high) = (low.eyes.unwrap(), high.eyes.unwrap());
    assert!(((eyes_high - eyes_low) - (high.lift - low.lift)).abs() < 1e-5, "the eyes rise with the heel");
    assert!(((eyes_low - bare.eyes.unwrap()) - low.lift).abs() < 1e-4, "the boots move nothing else");
}

#[test]
fn measure_reads_the_templates_as_their_charts_say() {
    for (id, heads) in [("standard", 6.0), ("tall", 7.5)] {
        let built = templates::build(id);
        let body = omuzaki_stage::body::Body::Kit(Box::new(built));
        let m = omuzaki_stage::ops::measure::measure(&CharId::new(id).unwrap(), &body).unwrap();
        let l = &m.lengths;
        eprintln!("{id}: {:.3} m, {:.2} heads, {l:?}", m.height, m.heads_tall.unwrap());
        assert!((m.heads_tall.unwrap() - heads).abs() < 0.3, "{id}: {} heads", m.heads_tall.unwrap());
        assert!(m.joints["hips"][1] > m.joints["leftLowerLeg"][1] && m.joints["head"][1] > m.joints["neck"][1]);
        let (shoulders, hips) = (l.shoulder_width.unwrap(), l.hip_width.unwrap());
        assert!(shoulders > hips && l.leg.unwrap() > l.arm.unwrap() && l.torso.unwrap() > l.neck.unwrap(), "{id}");
        assert!((m.eyes.unwrap() - m.joints["leftEye"][1]).abs() < 0.002, "{id}: the eye line is the eye joints'");
    }
}

#[test]
fn a_character_without_a_head_part_has_no_head() {
    let built = templates::build_parts("standard", &["body"]);
    let l = Landmarks::measure(&built).unwrap();
    assert!(l.head.is_none() && l.crown().is_none() && l.chin().is_none());
    assert!(l.body.height() > 0.0);
}

#[test]
fn no_vertex_is_a_framing_error() {
    let template = templates::build("standard");
    let built = BuiltCharacter::new(template.id.clone(), template.def.clone(), template.rig.clone());
    assert!(matches!(Landmarks::measure(&built), Err(StageError::Framing { .. })));
}

#[test]
fn a_non_finite_position_or_stature_is_a_framing_error() {
    let mut built = templates::build("standard");
    built.stature.as_mut().unwrap().height_m = f32::NAN;
    assert!(matches!(Landmarks::measure(&built), Err(StageError::Framing { .. })));

    let mut built = templates::build("standard");
    built.meshes[0].mesh.geometry.positions[0] = Vec3::new(0.0, f32::INFINITY, 0.0);
    let Err(StageError::Framing { reason }) = Landmarks::measure(&built) else { panic!("expected a framing error") };
    assert!(reason.contains("vertex 0"), "{reason}");
}

#[test]
fn measuring_twice_gives_the_same_landmarks() {
    let built = templates::build("mini");
    assert_eq!(Landmarks::measure(&built).unwrap(), Landmarks::measure(&built).unwrap());
}

#[test]
fn width_follows_the_camera_yaw() {
    let b = Bounds { min: Vec3::new(-1.0, 0.0, -0.5), max: Vec3::new(1.0, 2.0, 0.5) };
    assert_eq!(b.height(), 2.0);
    assert_eq!(b.centre(), Vec3::new(0.0, 1.0, 0.0));
    assert!((b.width_at(0.0) - 2.0).abs() < 1e-6, "from the front it is the x extent");
    assert!((b.width_at(90.0) - 1.0).abs() < 1e-6, "from the side it is the z extent");
    assert!((b.width_at(-90.0) - 1.0).abs() < 1e-6, "left and right see the same width");
}

#[test]
fn a_zero_or_negative_height_is_a_framing_error() {
    for height_m in [0.0, -1.66] {
        let mut built = templates::build("standard");
        built.stature.as_mut().unwrap().height_m = height_m;
        let Err(StageError::Framing { reason }) = Landmarks::measure(&built) else {
            panic!("{height_m}: expected a framing error")
        };
        assert!(reason.contains("above zero"), "{reason}");
    }
}

#[test]
fn a_flat_body_without_a_stature_is_a_framing_error() {
    let mut built = templates::build("standard");
    built.stature = None;
    for mesh in &mut built.meshes {
        mesh.mesh.geometry.positions.iter_mut().for_each(|p| p.y = 0.5);
    }
    assert!(matches!(Landmarks::measure(&built), Err(StageError::Framing { .. })));
}

/// The oracle: the vertices the stage itself draws when nothing animates it, in the base pose (decision 0014 §4: the
/// character's bind pose). The landmarks must measure the character where the stage shows it.
#[test]
fn the_body_and_the_hands_are_measured_where_the_stage_draws_them() {
    for id in templates::TEMPLATES {
        let built = templates::build(id);
        let l = Landmarks::measure(&built).unwrap();
        let mut input = FrameInput::default();
        seed_base_pose(&built.rig, &mut input.pose);
        let mut renderer = MockRenderer::new();
        let target = renderer
            .create_target(&TargetDesc { size: UVec2::splat(16), alpha: AlphaEncoding::Straight })
            .expect("the target is made");
        let mut stage = Stage::new(
            &mut renderer,
            built,
            UploadOptions::default(),
            Scene::new(Camera::default()),
            target,
            Quality::offline(),
            StageClock::fixed(30.0),
        )
        .expect("the stage starts");
        stage.tick(&mut renderer, &input).expect("a frame renders");
        let drawn = stage.drawn_vertices();
        stage.release(&mut renderer).expect("the stage is released");
        let min = drawn.iter().fold(Vec3::splat(f32::INFINITY), |m, p| m.min(*p));
        let max = drawn.iter().fold(Vec3::splat(f32::NEG_INFINITY), |m, p| m.max(*p));
        assert!(
            l.body.min.abs_diff_eq(min, 1e-3) && l.body.max.abs_diff_eq(max, 1e-3),
            "{id}: {:?} vs {min} {max}",
            l.body
        );
    }
}
