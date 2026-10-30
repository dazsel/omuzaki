//! Frames for scoring the body tracker (PF-06, PF-07) against a known answer: the kit's own character, posed in each
//! pose of the pose library and in a sweep of arm poses, rendered front-on at a webcam's framing (head to just
//! below the hips, 1280x720, the webcam's default mode), each written as a binary PPM beside a JSON of the pose it shows. `omuzaki-test-track`'s `track_body`
//! reads them, tracks them and scores the solved arms and fingers against the JSON.
//!
//! Gated on `OMUZAKI_BODY_FRAMES_DIR`; on lavapipe with the `wgpu` feature, through the render mock without it (whose
//! pictures are flat, so only the plumbing is exercised).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_anim::poses::PoseLibrary;
use omuzaki_anim_api::{HumanoidBone, HumanoidPose, Pose};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef};
use omuzaki_core::glam::{Quat, UVec2, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, find};
use omuzaki_render_api::{Camera, Projection, Quality, Renderer};
use omuzaki_stage::ops::render::pose::PoseArg;
use omuzaki_stage::ops::render::still::{Shot, StillLight, StillSpec, render_stills};
use omuzaki_stage::recipe::LightRig;
use serde_json::{Value, json};

/// The bones the score reads: the upper chest, both arms and every finger.
const BONES: [HumanoidBone; 37] = {
    use HumanoidBone as B;
    [
        B::UpperChest,
        B::LeftUpperArm,
        B::LeftLowerArm,
        B::LeftHand,
        B::RightUpperArm,
        B::RightLowerArm,
        B::RightHand,
        B::LeftThumbMetacarpal,
        B::LeftThumbProximal,
        B::LeftThumbDistal,
        B::LeftIndexProximal,
        B::LeftIndexIntermediate,
        B::LeftIndexDistal,
        B::LeftMiddleProximal,
        B::LeftMiddleIntermediate,
        B::LeftMiddleDistal,
        B::LeftRingProximal,
        B::LeftRingIntermediate,
        B::LeftRingDistal,
        B::LeftLittleProximal,
        B::LeftLittleIntermediate,
        B::LeftLittleDistal,
        B::RightThumbMetacarpal,
        B::RightThumbProximal,
        B::RightThumbDistal,
        B::RightIndexProximal,
        B::RightIndexIntermediate,
        B::RightIndexDistal,
        B::RightMiddleProximal,
        B::RightMiddleIntermediate,
        B::RightMiddleDistal,
        B::RightRingProximal,
        B::RightRingIntermediate,
        B::RightRingDistal,
        B::RightLittleProximal,
        B::RightLittleIntermediate,
        B::RightLittleDistal,
    ]
};

/// The default character of the standard template, created through `character.create`.
fn kit_def() -> CharacterDef {
    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(json!({"id": "kit", "name": "Kit", "template": "standard", "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).expect("the character is created");
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("stored").clone();
    CharacterDef::from_node(&node).expect("reads back")
}

/// The character built as the app builds it, by `builder` (which caches, so a rebuild per still is cheap).
fn build(builder: &CachingBuilder, def: &CharacterDef) -> BuiltCharacter {
    builder
        .build(&CharId::new("kit").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

/// A pose that turns the arms by `upper` and `lower` (T space, the left arm; the right arm mirrored).
fn arms(upper: Quat, lower: Quat) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::LeftUpperArm, upper);
    pose.set(HumanoidBone::LeftLowerArm, lower);
    let mirrored = pose.mirrored();
    pose.set(HumanoidBone::RightUpperArm, mirrored.get(HumanoidBone::RightUpperArm));
    pose.set(HumanoidBone::RightLowerArm, mirrored.get(HumanoidBone::RightLowerArm));
    pose
}

/// The poses to score: every library pose, then a sweep of the arms.
fn poses(built: &BuiltCharacter) -> Vec<(String, HumanoidPose)> {
    let library = PoseLibrary::builtin().expect("the pose library loads");
    let mut out: Vec<(String, HumanoidPose)> = library
        .iter()
        .filter_map(|def| {
            let pose = PoseArg::new(def.id.clone()).resolve_on("pose", &built.rig.skeleton).ok()?;
            Some((def.id.clone(), pose))
        })
        .collect();
    let deg = f32::to_radians;
    // Arms down at 60 degrees, then elbows bent forward by steps, then arms raised.
    for (name, down, bend) in [
        ("arms-a-pose", -60.0, 0.0),
        ("arms-down-bend-45", -70.0, 45.0),
        ("arms-down-bend-90", -70.0, 90.0),
        ("arms-down-bend-120", -70.0, 120.0),
        ("arms-level-bend-90", 0.0, 90.0),
        ("arms-raised-30", 30.0, 20.0),
    ] {
        // Down is a turn about +Z for the left arm (its +X toward -Y); the elbow bends the forearm toward +Z.
        let upper = Quat::from_rotation_z(deg(down));
        let lower = Quat::from_rotation_y(-deg(bend));
        out.push((name.to_owned(), arms(upper, lower)));
    }
    out
}

/// A webcam's view of the character: front-on, from just above the head to a little below the hips (where hands
/// hanging at the sides reach), 16:9.
fn shot(built: &BuiltCharacter) -> Shot {
    let skeleton = &built.rig.skeleton;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    let joint = |bone| world.local[skeleton.humanoid_bone(bone).expect("a humanoid bone").index()].translation;
    let hips = joint(HumanoidBone::Hips);
    // The top of the hair, from the meshes; the bottom 20 cm below the hips, where hands hanging at the sides reach.
    let hair = built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.y)).fold(f32::MIN, f32::max);
    let (top, bottom) = (hair + 0.03, hips.y - 0.2);
    let centre = Vec3::new(hips.x, 0.5 * (top + bottom), hips.z);
    let fov_y_deg = 45.0_f32;
    let distance = 0.5 * (top - bottom) / (0.5 * fov_y_deg.to_radians()).tan();
    Shot::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, distance),
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
    ))
}

/// The picture as a binary PPM (RGB, alpha dropped over the background the still already has).
fn ppm(image: &omuzaki_render_api::Image) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
    for pixel in image.rgba8.chunks_exact(4) {
        out.extend_from_slice(&pixel[..3]);
    }
    out
}

fn truth(pose: &HumanoidPose) -> Value {
    let bones: serde_json::Map<String, Value> =
        BONES.iter().map(|&bone| (format!("{bone:?}"), json!(pose.get(bone).to_array()))).collect();
    json!({ "bones": bones })
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_the_body_tracking_frames_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_BODY_FRAMES_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_BODY_FRAMES_DIR to write the frames");
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    #[cfg(feature = "wgpu")]
    let mut renderer = {
        use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
            .expect("a software adapter (install mesa-vulkan-drivers)")
    };
    #[cfg(not(feature = "wgpu"))]
    let mut renderer = omuzaki_render_mock::MockRenderer::new();
    let (builder, def) = (CachingBuilder::new(), kit_def());
    let built = build(&builder, &def);
    let rig = LightRig::load("viewer-studio").unwrap();
    let light = StillLight::new(rig.to_lights().unwrap(), rig.background(), rig.post(false));
    let shots = [shot(&built)];
    for (name, pose) in poses(&built) {
        let spec = StillSpec::new(light.clone(), UVec2::new(1280, 720), Quality::offline()).with_pose(Some(pose));
        let stills = render_stills(&mut renderer as &mut dyn Renderer, build(&builder, &def), &spec, &shots).unwrap();
        std::fs::write(directory.join(format!("{name}.ppm")), ppm(&stills[0].image)).unwrap();
        std::fs::write(directory.join(format!("{name}.json")), truth(&pose).to_string()).unwrap();
    }
}
