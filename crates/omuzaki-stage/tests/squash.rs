//! Squash and stretch on the stage (`scene.squash`): a body falling stretches, the landing squashes it and it settles
//! back, the same way every time; the volume and the ground point stay; only what is drawn deforms, not the rig; and a
//! pinned frame (a still, a sheet) shows the body as posed.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::Tick;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::scene::resolve::ResolvedScene;
use omuzaki_stage_api::Squash;

const CARTOON: Squash = Squash { stretch: 0.1, squash: 0.2, bounce_hz: 3.0, damping: 0.35, limit: 0.25 };

fn built() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn stage(renderer: &mut dyn Renderer, squash: Squash) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let mut stage = Stage::new(
        renderer,
        built(),
        options,
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the mock character uploads");
    let mut resolved = ResolvedScene::new();
    resolved.squash = squash;
    stage.set_scene(&resolved, Mat4::IDENTITY);
    stage
}

/// The first instance's scale along the up axis and across it: the root's, under the squash.
fn drawn_scale(stage: &Stage) -> (f32, f32) {
    let m = stage.scene().instances[0].transform;
    (m.y_axis.truncate().length(), m.x_axis.truncate().length())
}

/// Ticks `stage` through the first `frames` frames of [`hop_down`].
fn hop_to(renderer: &mut dyn Renderer, stage: &mut Stage, frames: usize) {
    for i in (0u16..).take(frames) {
        stage.tick(renderer, &fall_input(i)).expect("a frame");
    }
}

fn fall_input(frame: u16) -> FrameInput {
    let y = (1.0 - f32::from(frame) * (2.0 / 60.0)).max(0.0);
    FrameInput { root: Mat4::from_translation(Vec3::new(0.0, y, 0.0)), ..FrameInput::default() }
}

/// Falls from 1 m at 2 m/s for half a second, then stands on the ground for a second: the squash scale of each frame.
fn hop_down(renderer: &mut dyn Renderer, stage: &mut Stage) -> Vec<f32> {
    (0u16..90)
        .map(|i| {
            stage.tick(renderer, &fall_input(i)).expect("a frame");
            stage.squash_scale()
        })
        .collect()
}

#[test]
fn off_draws_the_body_as_posed() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, Squash::OFF);
    assert!(hop_down(&mut renderer, &mut stage).iter().all(|&s| (s - 1.0).abs() < 1e-6));
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_fall_stretches_the_landing_squashes_and_the_body_settles() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, CARTOON);
    let scales = hop_down(&mut renderer, &mut stage);
    let falling = scales[25];
    assert!(falling > 1.1, "stretched while falling: {falling}");
    let landed = scales[30..50].iter().copied().fold(f32::MAX, f32::min);
    assert!(landed < 0.95, "squashed on landing: {landed}");
    assert!((scales[89] - 1.0).abs() < 0.02, "settled: {}", scales[89]);
    let mut again = self::stage(&mut renderer, CARTOON);
    assert_eq!(hop_down(&mut renderer, &mut again), scales, "the same motion squashes the same way");
    stage.release(&mut renderer).unwrap();
    again.release(&mut renderer).unwrap();
}

#[test]
fn only_the_drawn_body_deforms_about_its_ground_point_keeping_its_volume() {
    let mut renderer = MockRenderer::new();
    let mut posed = stage(&mut renderer, Squash::OFF);
    hop_to(&mut renderer, &mut posed, 36);
    let mut stage = stage(&mut renderer, CARTOON);
    hop_to(&mut renderer, &mut stage, 36);
    let s = stage.squash_scale();
    assert!(s < 0.97, "squashed just after landing: {s}");
    let ((up, across), (posed_up, posed_across)) = (drawn_scale(&stage), drawn_scale(&posed));
    assert!((up / posed_up - s).abs() < 1e-4, "the height scales by {s}: {up} vs {posed_up}");
    assert!((across / posed_across - s.sqrt().recip()).abs() < 1e-4, "the ground plane by 1/sqrt(s)");
    let volume = (up / posed_up) * (across / posed_across).powi(2);
    assert!((volume - 1.0).abs() < 1e-4, "the volume stays: {volume}");
    // Standing on the ground at the origin, the ground point stays put.
    let origin = stage.scene().instances[0].transform.transform_point3(Vec3::ZERO);
    let posed_origin = posed.scene().instances[0].transform.transform_point3(Vec3::ZERO);
    assert!(origin.abs_diff_eq(posed_origin * Vec3::new(s.sqrt().recip(), s, s.sqrt().recip()), 1e-4));
    // The rig is not squashed: its bones are where the posed body's are.
    let hips = omuzaki_anim_api::HumanoidBone::Hips;
    assert_eq!(stage.bone_world(hips), posed.bone_world(hips));
    stage.release(&mut renderer).unwrap();
    posed.release(&mut renderer).unwrap();
}

#[test]
fn a_pinned_frame_shows_the_body_as_posed() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage(&mut renderer, CARTOON);
    hop_to(&mut renderer, &mut stage, 36);
    let squashed = stage.squash_scale();
    assert!(squashed < 0.97);
    let pin = stage.pin_time(Tick { index: 0, time: Seconds(0.0), dt: 1.0 / 60.0 });
    stage.tick(&mut renderer, &FrameInput::default()).unwrap();
    assert_eq!(stage.squash_scale(), 1.0, "a still is drawn as posed");
    let (up, across) = drawn_scale(&stage);
    assert!((up - across).abs() < 1e-5, "no squash in the still's instances: {up} {across}");
    stage.unpin_time(&pin);
    assert_eq!(stage.squash_scale(), squashed, "the squash comes back where it was");
    let (up, across) = drawn_scale(&stage);
    assert!(up < across, "and is drawn again: {up} {across}");
    stage.release(&mut renderer).unwrap();
}
