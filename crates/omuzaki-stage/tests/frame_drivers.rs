//! The character's bone drivers in the stage's frame (#800): a channel-driven bone moves with its channel every frame,
//! and the drivers run before the springs, so a spring chain under a driven bone follows it in the same frame; a pin
//! gives the driven bones back their channels when it ends.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{BoneDef, BoneId, Channel, ChannelBuf, Skeleton, SpringChain, SpringParams, Tick, Transform};
use omuzaki_char_api::{
    BoneDriver, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, JointRef, RegistryBuilder,
};
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};

const FPS: f32 = 60.0;

/// How far the wing folds at channel value 1, radians about its Z axis.
const FOLD: f32 = 1.2;

fn fold_channel() -> Channel {
    Channel::custom(0).expect("custom slot 0 exists")
}

/// The wing's rest: 5 cm to the side of the head.
fn wing_rest() -> Transform {
    Transform { translation: Vec3::new(0.05, 0.0, 0.0), rotation: Quat::IDENTITY }
}

/// The wing folded all the way.
fn wing_folded() -> Transform {
    Transform { rotation: Quat::from_rotation_z(FOLD), ..wing_rest() }
}

/// The mock character with a wing under the head and three feather bones hanging from it as a spring chain. With
/// `driven`, the wing rests open and the `custom00` channel folds it; without, it rests folded and nothing drives it.
/// The bones are the wing, then the feathers; returns their ids.
fn winged(driven: bool) -> (BuiltCharacter, [BoneId; 4]) {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let mut built = RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds");
    let rig = &mut built.rig;
    let head = rig.bone(&JointRef::HEAD).expect("a head");
    let mut bones = rig.skeleton.bones().to_vec();
    let first = u16::try_from(bones.len()).expect("a small skeleton");
    let ids = [0, 1, 2, 3].map(|i| BoneId(first + i));
    let bone = |name: &str, parent: BoneId, rest: Transform| BoneDef {
        name: name.to_owned(),
        parent: Some(parent),
        rest,
        humanoid: None,
    };
    let feather = Transform { translation: Vec3::new(0.04, -0.01, 0.0), rotation: Quat::IDENTITY };
    bones.push(bone("wing", head, if driven { wing_rest() } else { wing_folded() }));
    bones.push(bone("feather_0", ids[0], feather));
    bones.push(bone("feather_1", ids[1], feather));
    bones.push(bone("feather_2", ids[2], feather));
    rig.skeleton = Skeleton::new(bones).expect("the extra bones follow their parents");

    let params = SpringParams { stiffness: 0.5, gravity_power: 1.0, drag_force: 0.2, ..SpringParams::default() };
    rig.springs.chains.push(SpringChain::new("feathers", &ids[1..], params));
    if driven {
        rig.drivers.push(BoneDriver::channel(ids[0], fold_channel(), wing_rest(), wing_folded()));
    }
    (built, ids)
}

fn staged(renderer: &mut dyn Renderer, character: BuiltCharacter) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    Stage::new(renderer, character, UploadOptions::default(), scene, target, Quality::offline(), StageClock::fixed(FPS))
        .expect("the stage starts")
}

/// A frame with the fold channel at `fold`.
fn folding(fold: f32) -> FrameInput {
    let mut channels = ChannelBuf::NEUTRAL;
    channels.set(fold_channel(), fold);
    FrameInput { channels, ..FrameInput::default() }
}

/// The model-space transform of `bone` after the last frame: its skinning matrix times its bind transform.
fn posed(stage: &Stage, bone: BoneId) -> Mat4 {
    let bind = stage.built().rig.inverse_bind()[bone.index()].inverse();
    stage.skin()[bone.index()] * bind
}

#[test]
fn a_channel_driven_bone_moves_with_its_channel_every_frame() {
    let mut renderer = MockRenderer::new();
    let (character, [wing, ..]) = winged(true);
    let mut stage = staged(&mut renderer, character);
    let side = |stage: &Stage| posed(stage, wing).transform_vector3(Vec3::X).normalize();

    stage.tick(&mut renderer, &folding(0.0)).expect("renders");
    let open = side(&stage);
    stage.tick(&mut renderer, &folding(1.0)).expect("renders");
    let folded = side(&stage);
    stage.tick(&mut renderer, &folding(0.5)).expect("renders");
    let half = side(&stage);

    let angle = |a: Vec3, b: Vec3| a.dot(b).clamp(-1.0, 1.0).acos();
    assert!((angle(open, folded) - FOLD).abs() < 1e-3, "folded by {} rad, not {FOLD}", angle(open, folded));
    assert!((angle(open, half) - FOLD / 2.0).abs() < 1e-3, "half folded by {} rad", angle(open, half));
    stage.release(&mut renderer).expect("the stage releases");
}

/// The feathers of a wing folded by its channel move as those of a wing that rests folded: the springs saw the driven
/// wing in every frame. Were the drivers run after the springs, the feathers would have swung under the open wing and
/// be turned with it afterwards, centimetres away.
#[test]
fn a_spring_chain_under_a_driven_bone_follows_it_in_the_same_frame() {
    let (mut driven_renderer, mut rest_renderer) = (MockRenderer::new(), MockRenderer::new());
    let (character, ids) = winged(true);
    let mut driven = staged(&mut driven_renderer, character);
    let mut resting = staged(&mut rest_renderer, winged(false).0);
    let mut worst = 0.0_f32;
    for _ in 0..30 {
        driven.tick(&mut driven_renderer, &folding(1.0)).expect("renders");
        resting.tick(&mut rest_renderer, &FrameInput::default()).expect("renders");
        for bone in ids {
            let (a, b) = (posed(&driven, bone), posed(&resting, bone));
            worst = worst.max(a.w_axis.truncate().distance(b.w_axis.truncate()));
            worst = worst.max((a.transform_vector3(Vec3::X) - b.transform_vector3(Vec3::X)).length());
        }
    }
    assert!(worst < 1e-4, "the driven wing's feathers are {worst} from the folded wing's");

    // The comparison means something: an open wing's feathers are elsewhere.
    let mut open_renderer = MockRenderer::new();
    let mut open = staged(&mut open_renderer, winged(true).0);
    open.tick(&mut open_renderer, &folding(0.0)).expect("renders");
    let tip = ids[3];
    let apart = posed(&open, tip).w_axis.distance(posed(&driven, tip).w_axis);
    assert!(apart > 0.02, "folding moved the feather tip by only {apart} m");

    driven.release(&mut driven_renderer).expect("the stage releases");
    resting.release(&mut rest_renderer).expect("the stage releases");
    open.release(&mut open_renderer).expect("the stage releases");
}

/// A pinned frame with other channels doesn't leave its driven bones behind: unpinning poses them with the channels
/// of the last frame before the pin, as it does the rest of the rig.
#[test]
fn unpinning_time_gives_the_driven_bones_back_their_channels() {
    let mut renderer = MockRenderer::new();
    let (character, [wing, ..]) = winged(true);
    let mut stage = staged(&mut renderer, character);
    stage.tick(&mut renderer, &folding(0.0)).expect("renders");
    let open = posed(&stage, wing);

    let pin = stage.pin_time(Tick { index: 7, time: Seconds(1.0), dt: 0.0 });
    stage.tick(&mut renderer, &folding(1.0)).expect("renders under the pin");
    assert!(!posed(&stage, wing).abs_diff_eq(open, 1e-3), "the pinned frame folded the wing");
    stage.unpin_time(&pin);

    assert!(posed(&stage, wing).abs_diff_eq(open, 1e-5), "the wing is open again, as before the pin");
    stage.release(&mut renderer).expect("the stage releases");
}
