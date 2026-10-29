//! Touches between cast members (#2415) on the kit's own bodies: a hug, palms together and cheek to cheek between the
//! standard and the soft-sd character, solved by the cast every frame. Each touch ends with the hands on their goals,
//! the arms outside the other body and the bodies meeting without passing through each other. With
//! `OMUZAKI_INTERACT_DIR` set, each touch is rendered from two cameras for the evidence (on lavapipe with the `wgpu`
//! feature).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::cast::CastStage;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::interact::TouchReport;
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::{Interact, InteractKind, MemberId};

fn id(name: &str) -> MemberId {
    MemberId::checked(name).expect("a valid member id")
}

/// The kit's default character on `template`, built as the app builds it.
fn kit(template: &str) -> BuiltCharacter {
    use omuzaki_char::builder::CachingBuilder;
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef};
    use omuzaki_core::time::FakeClock;
    use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{Actor, OpCtx, find};

    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(serde_json::json!({"id": "kit", "name": "Kit", "template": template, "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).expect("character.create plans");
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("the character is stored").clone();
    let def = CharacterDef::from_node(&node).expect("the stored character reads back");
    CachingBuilder::new()
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

fn camera(eye: Vec3, at: Vec3) -> Camera {
    Camera::look_at(eye, at, Vec3::Y, Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 })
}

/// Where each member stands for `kind` before the touch spaces them: `[holder, partner]`, the partner a little too far
/// so the cast has to bring it in.
fn places(kind: InteractKind) -> [Mat4; 2] {
    let at = |x: f32, z: f32, yaw_deg: f32| {
        Mat4::from_translation(Vec3::new(x, 0.0, z)) * Mat4::from_rotation_y(yaw_deg.to_radians())
    };
    match kind {
        // Face to face across the screen.
        InteractKind::Hug | InteractKind::PalmTouch => [at(-0.25, 0.0, 90.0), at(0.45, 0.0, -90.0)],
        // Side by side facing the camera, the partner on the holder's right (screen left).
        InteractKind::CheekToCheek => [at(0.2, 0.0, 0.0), at(-0.4, 0.0, 0.0)],
    }
}

/// A cast of the standard character (`holder`) and the tall one (`partner`) on `renderer`, the holder touching the
/// partner with `kind`, rendering into `target`.
fn duo(
    renderer: &mut dyn Renderer,
    kind: InteractKind,
    target: TargetId,
    quality: Quality,
) -> (CastStage<Stage>, [HumanoidPose; 2]) {
    let (standard, soft) = (kit("standard"), kit("tall"));
    let mut base = [HumanoidPose::REST; 2];
    seed_base_pose(&standard.rig, &mut base[0]);
    seed_base_pose(&soft.rig, &mut base[1]);
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let view = Scene::new(camera(Vec3::new(0.0, 1.2, 3.0), Vec3::new(0.0, 1.2, 0.0)));
    let [a, b] = places(kind);
    let mut lead = Stage::new(renderer, standard, options, view, target, quality, StageClock::live()).unwrap();
    lead.set_subject_root(a);
    let spare = renderer.create_target(&TargetDesc { size: UVec2::splat(8), alpha: AlphaEncoding::Straight }).unwrap();
    let guest = Stage::new(
        renderer,
        soft,
        options,
        Scene::new(camera(Vec3::Z, Vec3::ZERO)),
        spare,
        quality,
        StageClock::live(),
    )
    .unwrap();
    let mut cast = CastStage::new(id("holder"), lead);
    assert!(cast.add_member(id("partner"), guest, b).is_none());
    cast.set_touches([(id("holder"), Interact::new(id("partner"), kind))]);
    (cast, base)
}

/// Runs `frames` 60 Hz frames of the cast standing in its base poses.
fn run(cast: &mut CastStage<Stage>, renderer: &mut dyn Renderer, base: &[HumanoidPose; 2], frames: u32) {
    for i in 0..frames {
        let t = f64::from(i) / 60.0;
        let ours = FrameInput { pose: base[0], dt: 1.0 / 60.0, time: Some(Seconds(t)), ..FrameInput::default() };
        let theirs = FrameInput { pose: base[1], ..ours.clone() };
        cast.tick_members(renderer, &[(id("holder"), &ours), (id("partner"), &theirs)]).unwrap();
    }
}

/// The gap the partner's root stands from the holder's, metres.
fn apart(cast: &CastStage<Stage>) -> f32 {
    use omuzaki_stage::cast::StageMember;
    let at = |who: &str| cast.member(&id(who)).unwrap().subject_root().w_axis.truncate();
    at("holder").distance(at("partner"))
}

fn check(kind: InteractKind, report: &TouchReport, apart: f32) {
    println!(
        "{}: apart {:.0} mm, hands {:?} mm, arms clear {:.1} mm, torsos {:.1} mm, heads {:.1} mm",
        kind.name(),
        apart * 1e3,
        report.hand_miss.map(|m| m.map(|m| (m * 1e3).round())),
        report.arms_clear * 1e3,
        report.torsos * 1e3,
        report.heads * 1e3
    );
    if std::env::var_os("INTERACT_DEBUG").is_some() {
        return;
    }
    // The arms stay on or outside the other body (within a centimetre of the surface the guard fitted to 97% of the
    // clothed body), and the bodies don't pass through each other.
    assert!(report.arms_clear > -0.01, "{}: {report:?}", kind.name());
    assert!(report.torsos > -0.006 && report.heads > -0.006, "{}: {report:?}", kind.name());
    for miss in report.hand_miss.iter().flatten() {
        // A hug's hands aim just inside the other's back and the guard lays them on it: a few centimetres.
        assert!(*miss < 0.07, "{}: a hand misses its goal: {report:?}", kind.name());
    }
    match kind {
        // Chests and heads meet: the torsos touch, the heads pass close.
        InteractKind::Hug => assert!(report.torsos < 0.03, "{report:?}"),
        // The cheeks touch.
        InteractKind::CheekToCheek => assert!(report.heads < 0.02, "{report:?}"),
        InteractKind::PalmTouch => assert!(report.hand_miss[1].is_some() && report.hand_miss[2].is_some()),
    }
}

#[test]
fn each_touch_meets_on_the_kits_bodies_without_passing_through() {
    let mut renderer = MockRenderer::new();
    for kind in InteractKind::ALL {
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::splat(16), alpha: AlphaEncoding::Straight }).unwrap();
        let (mut cast, base) = duo(&mut renderer, kind, target, Quality::live());
        run(&mut cast, &mut renderer, &base, 90);
        let report = cast.touch_report(&id("holder")).expect("the touch is measured");
        if std::env::var_os("INTERACT_DEBUG").is_some() {
            use omuzaki_stage::cast::StageMember;
            for who in ["holder", "partner"] {
                let b = cast.member(&id(who)).unwrap().touch_body();
                println!("{who}: root {:?} hands {:?} {:?}", b.at(), b.joints[10], b.joints[11]);
            }
            let bodies =
                [cast.member(&id("holder")).unwrap().touch_body(), cast.member(&id("partner")).unwrap().touch_body()];
            for k in 0..2 {
                let (me, other) = (&bodies[k], &bodies[1 - k]);
                for side in 0..2 {
                    let (elbow, wrist) = (me.joints[8 + side].unwrap(), me.joints[10 + side].unwrap());
                    for part in other.parts.iter().flatten() {
                        let gap = [0.25_f32, 0.5, 0.75, 1.0].map(|t| part.gap_to(elbow.lerp(wrist, t), me.forearm).0);
                        if gap.iter().any(|g| *g < 0.02) {
                            println!(
                                "  member {k} side {side} vs {:?}: {:?} mm",
                                part.part,
                                gap.map(|g| (g * 1e3).round())
                            );
                        }
                    }
                }
            }
        }
        check(kind, &report, apart(&cast));

        // Clearing the touches sends the partner back where the scene placed it.
        cast.set_touches([]);
        assert!(cast.touch_report(&id("holder")).is_none());
        assert!(
            (apart(&cast) - places(kind)[0].w_axis.truncate().distance(places(kind)[1].w_axis.truncate())).abs() < 1e-5
        );
        FrameLoop::release(cast, &mut renderer).unwrap();
    }
}

/// Renders each touch from two cameras into `OMUZAKI_INTERACT_DIR` for the evidence.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_touches_when_asked() {
    use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
    use omuzaki_stage::ops::render::still::encode_png;
    use omuzaki_stage::recipe::LightRig;

    let Some(directory) = std::env::var_os("OMUZAKI_INTERACT_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_INTERACT_DIR to write the pictures");
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
    let mut renderer = MockRenderer::new();

    let rig = LightRig::load("viewer-studio").unwrap();
    for kind in InteractKind::ALL {
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::new(960, 960), alpha: AlphaEncoding::Straight }).unwrap();
        let (mut cast, base) = duo(&mut renderer, kind, target, Quality::high());
        let face = || FaceRig::standard().with_executor(Box::new(InlineExecutor));
        cast.lead_mut().set_face(&mut renderer, face()).unwrap();
        cast.member_mut(&id("partner")).unwrap().set_face(&mut renderer, face()).unwrap();
        {
            let scene = cast.scene_mut();
            scene.lights = rig.to_lights().unwrap();
            scene.background = rig.background();
            scene.post = rig.post(false);
        }
        run(&mut cast, &mut renderer, &base, 90);
        let report = cast.touch_report(&id("holder")).unwrap();
        check(kind, &report, apart(&cast));
        let centre = {
            use omuzaki_stage::cast::StageMember;
            let at = |who: &str| cast.member(&id(who)).unwrap().subject_root().w_axis.truncate();
            (at("holder") + at("partner")) * 0.5 + Vec3::Y * 1.3
        };
        let views = match kind {
            InteractKind::Hug | InteractKind::PalmTouch => {
                [("front", centre + Vec3::new(0.5, 0.1, 2.2)), ("over", centre + Vec3::new(-1.2, 0.6, -1.8))]
            }
            InteractKind::CheekToCheek => {
                [("front", centre + Vec3::new(-0.5, 0.05, 2.2)), ("back", centre + Vec3::new(0.6, 0.5, -2.1))]
            }
        };
        let input = FrameInput { pose: base[0], dt: 0.0, ..FrameInput::default() };
        for (name, eye) in views {
            cast.scene_mut().camera = camera(eye, centre);
            cast.redraw(&mut renderer, &input).unwrap();
            let image = renderer.read_target(target).unwrap();
            let file = directory.join(format!("{}-{name}.png", kind.name()));
            std::fs::write(file, encode_png(&image).unwrap()).unwrap();
        }
        FrameLoop::release(cast, &mut renderer).unwrap();
    }
}
