//! The stage's redraw and settle on the mock character: a redraw repeats the last frame without advancing anything, and
//! settling moves the rig's spring bones as well as the runtimes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Quat, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, FrameInfo, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::face::SETTLE_STEPS;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};

const FPS: f32 = 60.0;

fn mock() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

/// A stage on `renderer` with the mock character, a front camera and a target of 48 pixels.
fn staged(renderer: &mut dyn Renderer) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(48), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    Stage::new(renderer, mock(), UploadOptions::default(), scene, target, Quality::offline(), StageClock::fixed(FPS))
        .expect("the stage starts")
}

/// A frame with the head turned, so the chain hanging from the head has something to swing from.
fn turned() -> FrameInput {
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::Head, Quat::from_rotation_z(0.6));
    FrameInput { pose, ..FrameInput::default() }
}

#[test]
fn a_redraw_repeats_the_last_frame() {
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    let target = stage.target();
    let frame = stage.tick(&mut renderer, &turned()).expect("the frame renders");
    let first = renderer.read_target(target).expect("it reads back");

    let again = stage.redraw(&mut renderer, &turned()).expect("the redraw renders");
    let second = renderer.read_target(target).expect("it reads back");
    assert_eq!(again.tick, frame.tick, "a redraw advances no tick");
    assert_eq!(first.rgba8, second.rgba8, "the same state renders the same pixels");
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn settling_moves_the_spring_bones_as_well_as_the_runtimes() {
    let mut plain_renderer = MockRenderer::new();
    let mut plain = staged(&mut plain_renderer);
    plain.tick(&mut plain_renderer, &turned()).expect("renders");
    plain.tick(&mut plain_renderer, &turned()).expect("renders");
    let unsettled = plain.skin().to_vec();

    let mut settled_renderer = MockRenderer::new();
    let mut settled = staged(&mut settled_renderer);
    settled.tick(&mut settled_renderer, &turned()).expect("renders");
    settled.settle(SETTLE_STEPS);
    settled.tick(&mut settled_renderer, &turned()).expect("renders");
    assert_ne!(settled.skin(), unsettled.as_slice(), "the settled chain is not where an unsettled one is");

    plain.release(&mut plain_renderer).expect("the stage releases");
    settled.release(&mut settled_renderer).expect("the stage releases");
}

#[test]
fn a_redraw_does_not_step_the_settled_state() {
    // Settling then redrawing leaves the skin where the settle left it: a redraw is not a tick.
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    stage.tick(&mut renderer, &turned()).expect("renders");
    stage.settle(SETTLE_STEPS);
    let before = stage.skin().to_vec();
    stage.redraw(&mut renderer, &turned()).expect("the redraw renders");
    assert_eq!(stage.skin(), before.as_slice(), "the rig is untouched by a redraw");
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn a_redraw_before_any_tick_renders_the_stage_clock_frame() {
    // The caller's scene carries a frame that no tick has made: a redraw must render at the stage's own tick, as a tick
    // would have set it, not at the caller's frame info.
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    stage.scene_mut().frame = FrameInfo { index: 7, time: Seconds(3.0) };
    stage.redraw(&mut renderer, &FrameInput::default()).expect("the redraw renders");
    assert_eq!(stage.scene().frame, FrameInfo { index: 0, time: Seconds(0.0) }, "the frame is the stage's first tick");
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn a_redraw_after_a_pin_runs_at_the_pinned_time_and_unpinning_restores_the_clock() {
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    let before = stage.tick(&mut renderer, &turned()).expect("the frame renders");
    let pinned = omuzaki_anim_api::Tick { index: 9000, time: Seconds(150.0), dt: 1.0 / FPS };
    let pin = stage.pin_time(pinned);
    // No tick in between: the redraw alone must see the pin.
    let redrawn = stage.redraw(&mut renderer, &turned()).expect("the redraw renders");
    assert_eq!(redrawn.tick, pinned, "the face and the frame are given the pinned tick");
    assert_eq!(stage.scene().frame, FrameInfo { index: pinned.index, time: pinned.time });
    stage.unpin_time(&pin);
    assert_eq!(stage.scene().frame, FrameInfo { index: before.tick.index, time: before.tick.time });
    let after = stage.redraw(&mut renderer, &turned()).expect("the redraw renders");
    assert_eq!(after.tick, before.tick, "unpinned, a redraw is back on the last frame's time");
    stage.release(&mut renderer).expect("the stage releases");
}
