//! An imported avatar on the stage (spec v3 §27 EX-12, `docs/design/byom.md`): the bring-your-own-model path from a
//! file to a rendered frame, with no third-party model. The kit's mock character is exported as a VRM 1.0 file, read
//! back and prepared as an [`ImportedAvatar`], exactly as `io.import {as: avatar}` does with a person's file, and then
//! staged: skinned by its own inverse binds, its expressions turned into morph weights, posed by the rig. On the GPU
//! (`--features wgpu`) the imported copy renders with the kit character's silhouette.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)]

use omuzaki_anim_api::{BoneId, Channel, HumanoidBone, HumanoidPose};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::expression::ExprIx;
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec3};
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::stages::{AnyStage, FrameLoop};

#[path = "avatar/kit.rs"]
mod kit;

use kit::{as_vrm, avatar_of, kit, with_morph};

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.9, 4.0),
        Vec3::new(0.0, 0.9, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    )
}

fn stage_on(renderer: &mut dyn Renderer, avatar: ImportedAvatar, size: UVec2) -> (AvatarStage, TargetId) {
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).expect("a target");
    let stage =
        AvatarStage::new(renderer, avatar, Scene::new(camera()), target, Quality::offline(), StageClock::fixed(60.0))
            .expect("the avatar stages");
    (stage, target)
}

fn input_at(pose: HumanoidPose) -> FrameInput {
    FrameInput { pose, ..FrameInput::default() }
}

/// The expression with a morph bind, and a channel that drives it.
fn driven_morph(avatar: &ImportedAvatar) -> (ExprIx, Channel) {
    let face = avatar.face();
    face.drivers
        .iter()
        .find(|(_, e)| face.expressions.get(e.index()).is_some_and(|x| !x.morph.is_empty()))
        .map(|(channel, e)| (e, channel))
        .expect("a channel drives the morph expression")
}

/// The bone behind each entry of the scene's joint palette, mesh after mesh, as the stage fills it.
fn palette_bones(avatar: &ImportedAvatar) -> Vec<BoneId> {
    let mut order: Vec<_> = avatar.meshes.iter().collect();
    order.sort_by_key(|m| m.mesh.render_order);
    order
        .iter()
        .flat_map(|m| &m.mesh.joint_bones)
        .map(|joint| match joint {
            JointRef::Humanoid(bone) => avatar.skeleton.humanoid_bone(*bone).expect("a mapped bone"),
            JointRef::Named(name) => avatar.skeleton.find(name.as_str()).expect("a named bone"),
        })
        .collect()
}

/// True if `bone` is a spring bone or hangs from one: the springs move it on their own.
fn on_a_spring(avatar: &ImportedAvatar, bone: BoneId) -> bool {
    avatar.springs.chains.iter().flat_map(|c| &c.joints).any(|j| avatar.skeleton.is_ancestor_or_self(j.bone, bone))
}

#[test]
fn at_its_bind_pose_every_joint_of_the_imported_kit_is_the_identity() {
    let avatar = avatar_of(&as_vrm(&kit()));
    let base = avatar.bind;
    let bones = palette_bones(&avatar);
    let mut renderer = MockRenderer::new();
    let (mut stage, target) = stage_on(&mut renderer, avatar, UVec2::new(32, 32));
    stage.tick(&mut renderer, &input_at(base)).expect("a frame");
    let scene = stage.scene();
    assert_eq!(scene.instances.len(), stage.avatar().meshes.len(), "one instance per mesh");
    assert_eq!(scene.joints.len(), bones.len(), "one palette entry per joint of every mesh");
    // The meshes are stored in their bind pose: each bone's model matrix at bind undoes its inverse bind exactly. The
    // spring bones are the exception: on the first step they settle out of the head's collider, as they would in any
    // VRM runtime.
    let mut checked = 0;
    for (i, (joint, bone)) in scene.joints.iter().zip(&bones).enumerate() {
        if !on_a_spring(stage.avatar(), *bone) {
            assert!(joint.abs_diff_eq(Mat4::IDENTITY, 1e-4), "joint {i} at bind is {joint}");
            checked += 1;
        }
    }
    assert!(checked > bones.len() / 2, "most joints are the body's: {checked} of {}", bones.len());
    stage.release(&mut renderer).expect("released");
    renderer.release(target.into()).expect("target released");
}

#[test]
fn a_turned_head_moves_only_the_joints_of_the_head() {
    let avatar = avatar_of(&as_vrm(&kit()));
    let head = avatar.skeleton.humanoid_bone(HumanoidBone::Head).expect("a head");
    let bones = palette_bones(&avatar);
    let base = avatar.bind;
    let mut renderer = MockRenderer::new();
    let (mut stage, target) = stage_on(&mut renderer, avatar, UVec2::new(32, 32));
    stage.tick(&mut renderer, &input_at(base)).expect("a frame at rest");
    let rest = stage.scene().joints.clone();
    let mut turned = base;
    turned.set(HumanoidBone::Head, Quat::from_rotation_y(0.5) * base.get(HumanoidBone::Head));
    stage.tick(&mut renderer, &input_at(turned)).expect("a frame");
    let skeleton = &stage.avatar().skeleton;
    for (i, ((now, before), bone)) in stage.scene().joints.iter().zip(&rest).zip(&bones).enumerate() {
        let moved = !now.abs_diff_eq(*before, 1e-4);
        assert_eq!(moved, skeleton.is_ancestor_or_self(head, *bone), "joint {i} ({bone:?}) moved: {moved}");
    }
    stage.release(&mut renderer).expect("released");
    renderer.release(target.into()).expect("target released");
}

#[test]
fn a_driven_channel_reaches_the_scene_as_its_morph_weight() {
    let avatar = avatar_of(&with_morph(as_vrm(&kit())));
    let (expression, channel) = driven_morph(&avatar);
    let base = avatar.bind;
    let mut renderer = MockRenderer::new();
    let (mut stage, target) = stage_on(&mut renderer, avatar, UVec2::new(32, 32));
    let mut input = input_at(base);
    stage.tick(&mut renderer, &input).expect("a neutral frame");
    assert!(stage.scene().morph_weights.iter().all(|w| *w == 0.0), "a neutral face moves no morph");
    input.channels.set(channel, 0.75);
    stage.tick(&mut renderer, &input).expect("a frame");
    assert!((stage.weights().expression(expression) - 0.75).abs() < 1e-6);
    let scene = stage.scene();
    let morphed: Vec<_> = scene.instances.iter().filter_map(|i| i.morph).collect();
    assert!(!morphed.is_empty(), "the morphed mesh's instance points at its weights");
    let weights: Vec<f32> = morphed
        .iter()
        .flat_map(|r| &scene.morph_weights[r.start as usize..(r.start + r.len) as usize])
        .copied()
        .collect();
    assert!(
        weights.iter().any(|w| (*w - 0.75).abs() < 1e-6),
        "the bound target is at the channel's value: {weights:?}"
    );
    scene.validate().expect("the scene the renderer gets is valid");
    stage.release(&mut renderer).expect("released");
    renderer.release(target.into()).expect("target released");
}

#[test]
fn any_stage_runs_an_avatar_through_the_one_frame_loop() {
    let avatar = avatar_of(&as_vrm(&kit()));
    let base = avatar.bind;
    let mut renderer = MockRenderer::new();
    let (stage, target) = stage_on(&mut renderer, avatar, UVec2::new(32, 32));
    let mut any = AnyStage::from(stage);
    let first = any.tick(&mut renderer, &input_at(base)).expect("a frame");
    let second = any.tick(&mut renderer, &input_at(base)).expect("a frame");
    assert_eq!(second.tick.index, first.tick.index + 1, "the fixed clock steps once a frame");
    assert!(any.trigger_expressions().0.is_empty(), "an imported face has no painted expressions for the triggers");
    any.release(&mut renderer).expect("released");
    renderer.release(target.into()).expect("target released");
}

/// On the GPU: the imported copy of the kit character covers the same pixels as the kit character itself.
#[cfg(feature = "wgpu")]
#[test]
fn on_the_gpu_the_imported_kit_has_the_kit_characters_silhouette() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_render_api::{Background, Colour, Light};
    use omuzaki_stage::UploadOptions;
    use omuzaki_stage::frame::Stage;

    let size = UVec2::new(192, 256);
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let built = kit();
    let avatar = avatar_of(&as_vrm(&built));
    // Framed on the mock character, which stands under a metre tall.
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.6, 2.2),
        Vec3::new(0.0, 0.6, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    scene.background = Background::Solid { colour: Colour::TRANSPARENT };
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.8, 1.0),
        colour: Colour::WHITE,
        intensity: 2.5,
        casts_shadow: false,
    });

    let base = built.rig.bind;
    let kit_target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let mut kit_stage = Stage::new(
        &mut renderer,
        built,
        UploadOptions::default(),
        scene.clone(),
        kit_target,
        Quality::offline(),
        StageClock::fixed(60.0),
    )
    .expect("the kit stages");
    kit_stage.tick(&mut renderer, &input_at(base)).expect("the kit renders");
    let kit_image = renderer.read_target(kit_target).expect("read");

    let avatar_base = avatar.bind;
    let avatar_target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let mut avatar_stage =
        AvatarStage::new(&mut renderer, avatar, scene, avatar_target, Quality::offline(), StageClock::fixed(60.0))
            .expect("the avatar stages");
    avatar_stage.tick(&mut renderer, &input_at(avatar_base)).expect("the avatar renders");
    let avatar_image = renderer.read_target(avatar_target).expect("read");

    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        for (name, image) in [("kit", &kit_image), ("imported", &avatar_image)] {
            let png = omuzaki_stage::ops::render::still::encode_png(image).expect("encodes");
            std::fs::write(dir.join(format!("avatar-stage-{name}.png")), png).expect("writes");
        }
    }

    let covered = |rgba: &[u8]| rgba.chunks_exact(4).map(|p| p[3] > 127).collect::<Vec<_>>();
    let (a, b) = (covered(&kit_image.rgba8), covered(&avatar_image.rgba8));
    let both = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let either = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    assert!(either > 1000, "the character is in frame: {either} pixels");
    let iou = both as f64 / either as f64;
    // Not 1: the file carries the hair as spring bones where the kit sways it with its own runtime, and its shading
    // program as the default toon look (MToon can't hold it), so the hair's edge differs by a pixel or two.
    assert!(iou > 0.95, "the imported kit covers the kit's pixels: intersection over union {iou:.4}");

    kit_stage.release(&mut renderer).unwrap();
    avatar_stage.release(&mut renderer).unwrap();
    renderer.release(kit_target.into()).unwrap();
    renderer.release(avatar_target.into()).unwrap();
}
