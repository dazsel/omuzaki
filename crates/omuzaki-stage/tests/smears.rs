//! Smear frames on the stage (`scene.smears`): a body moving fast hands the renderer the pose it had `length_s` ago as
//! each skinned instance's trailing pose, carried into the model space of the frame being drawn; off, a pinned still and
//! a body that has not moved for that long draw as posed; a redraw shows the last frame's smear again; and an imported
//! avatar smears the same way along its shared palette.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;

use omuzaki_anim_api::Tick;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::scene::resolve::ResolvedScene;
use omuzaki_stage_api::Smears;

const CARTOON: Smears = Smears { amount: 1.0, length_s: 0.05, min_speed: 1.0, jag: 0.5 };

/// Metres the root moves each 60 Hz frame: 3 m/s along x.
const STEP_M: f32 = 0.05;

fn built() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn resolved(smears: Smears) -> ResolvedScene {
    let mut resolved = ResolvedScene::new();
    resolved.smears = smears;
    resolved
}

fn stage(renderer: &mut dyn Renderer, smears: Smears) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        renderer,
        built(),
        UploadOptions { face_px_per_m: 2000.0 },
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the mock character uploads");
    stage.set_scene(&resolved(smears), Mat4::IDENTITY);
    stage
}

fn moving(frame: u16) -> FrameInput {
    let x = f32::from(frame) * STEP_M;
    FrameInput { root: Mat4::from_translation(Vec3::new(x, 0.0, 0.0)), ..FrameInput::default() }
}

/// Checks every smeared instance of `scene`: its trail is as long as its skin and each trailing joint is its own joint
/// carried back by `back` metres along x (the body holds its pose, so only the root moved). Returns how many smear.
fn trails_back(scene: &Scene, back: f32) -> usize {
    let carry = Mat4::from_translation(Vec3::new(-back, 0.0, 0.0));
    let mut smeared = 0;
    for instance in &scene.instances {
        let Some(smear) = instance.smear else { continue };
        let skin = instance.skin.expect("only skinned instances smear");
        assert_eq!(smear.trail.len, skin.len);
        for k in 0..skin.len as usize {
            let (own, trail) = (scene.joints[skin.start as usize + k], scene.joints[smear.trail.start as usize + k]);
            assert!(trail.abs_diff_eq(carry * own, 1e-4), "joint {k}: {trail} against {own}");
        }
        assert!((smear.amount - 1.0).abs() < 1e-6 && (smear.jag - 0.5).abs() < 1e-6);
        assert!((smear.threshold - 0.05).abs() < 1e-6, "1 m/s for 0.05 s");
        smeared += 1;
    }
    smeared
}

#[test]
fn off_draws_every_frame_as_posed() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, Smears::OFF);
    for i in 0..10 {
        stage.tick(&mut renderer, &moving(i)).unwrap();
        assert!(stage.scene().instances.iter().all(|inst| inst.smear.is_none()));
    }
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_fast_move_trails_the_pose_from_length_s_ago() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, CARTOON);
    stage.tick(&mut renderer, &moving(0)).unwrap();
    assert!(stage.scene().instances.iter().all(|inst| inst.smear.is_none()), "no trail on the first frame");
    let skinned = stage.scene().instances.iter().filter(|inst| inst.skin.is_some()).count();
    assert!(skinned > 0, "the mock character has skinned parts");
    let joints = stage.scene().joints.len();
    for i in 1..10 {
        stage.tick(&mut renderer, &moving(i)).unwrap();
    }
    // 0.05 s at 60 Hz is three frames: 0.15 m behind.
    assert_eq!(trails_back(stage.scene(), 3.0 * STEP_M), skinned, "every skinned part smears");
    assert_eq!(stage.scene().joints.len(), 2 * joints, "the trailing joints follow the frame's own");
    // A redraw shows the same frame, smear and all.
    let ticked = stage.scene().instances.clone();
    stage.redraw(&mut renderer, &moving(9)).unwrap();
    assert_eq!(stage.scene().instances, ticked);
    assert_eq!(trails_back(stage.scene(), 3.0 * STEP_M), skinned);
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_pinned_still_and_a_resting_body_draw_as_posed() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, CARTOON);
    for i in 0..10 {
        stage.tick(&mut renderer, &moving(i)).unwrap();
    }
    let pin = stage.pin_time(Tick { index: 0, time: Seconds(0.0), dt: 1.0 / 60.0 });
    stage.tick(&mut renderer, &moving(9)).unwrap();
    assert!(stage.scene().instances.iter().all(|inst| inst.smear.is_none()), "a still has no smear");
    stage.unpin_time(&pin);
    assert!(trails_back(stage.scene(), 3.0 * STEP_M) > 0, "unpinned, the last frame's smear is back");
    // Standing still: the trail is the same pose, so the renderer has nothing to pull back.
    for _ in 0..5 {
        stage.tick(&mut renderer, &moving(9)).unwrap();
    }
    assert!(trails_back(stage.scene(), 0.0) > 0);
    stage.release(&mut renderer).unwrap();
}

#[test]
fn an_imported_avatar_smears_along_its_shared_palette() {
    let mut renderer = MockRenderer::new();
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let avatar = kit::avatar_of(&kit::as_vrm(&kit::kit()));
    let mut stage = AvatarStage::new(
        &mut renderer,
        avatar,
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the avatar stages");
    stage.set_scene(&resolved(CARTOON), Mat4::IDENTITY);
    stage.tick(&mut renderer, &moving(0)).unwrap();
    let palette = stage.scene().joints.len();
    assert!(stage.scene().instances.iter().all(|inst| inst.smear.is_none()));
    for i in 1..10 {
        stage.tick(&mut renderer, &moving(i)).unwrap();
    }
    let scene = stage.scene();
    assert_eq!(scene.joints.len(), 2 * palette);
    let smeared: Vec<_> = scene.instances.iter().filter(|inst| inst.smear.is_some()).collect();
    assert_eq!(smeared.len(), scene.instances.iter().filter(|inst| inst.skin.is_some()).count());
    for instance in smeared {
        let (smear, skin) = (instance.smear.unwrap(), instance.skin.unwrap());
        assert_eq!(smear.trail.start - skin.start, u32::try_from(palette).unwrap(), "at its run's place in the trail");
        assert_eq!(smear.trail.len, skin.len);
    }
    // The root's own joints hold their pose, so their trail is them carried 0.15 m back; the spring bones swing.
    let carry = Mat4::from_translation(Vec3::new(-3.0 * STEP_M, 0.0, 0.0));
    let held = (0..palette).filter(|&j| scene.joints[palette + j].abs_diff_eq(carry * scene.joints[j], 1e-4)).count();
    assert!(held * 2 > palette, "most joints trail by the root's move alone: {held} of {palette}");
    stage.release(&mut renderer).unwrap();
}
