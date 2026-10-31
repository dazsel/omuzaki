//! `fit.check` (#331, #2604) on the kit's default characters, through the registry as an agent calls it: every stress
//! pose keeps the body covered by its outfit.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpError, find};
use omuzaki_stage::ops::fit::check::CheckFitResult;
use serde_json::{Value, json};

/// Runs op `name` with `args` on `doc`, applying what it changes; its output.
fn run(doc: &mut MemDoc, name: &str, args: Value) -> Result<Value, OpError> {
    let (actor, clock) = (Actor::Agent { id: "test".into(), model: None }, FakeClock::default());
    let op = (find(name).expect("the op registers itself").decode)(args)?;
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock))?;
    doc.apply(&plan.patch).unwrap();
    Ok(plan.output.unwrap_or(Value::Null))
}

fn kit(template: &str) -> MemDoc {
    let mut doc = MemDoc::new();
    run(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template, "seed": 7})).unwrap();
    doc
}

fn check(doc: &mut MemDoc, args: Value) -> CheckFitResult {
    serde_json::from_value(run(doc, "fit.check", args).unwrap()).unwrap()
}

#[test]
fn the_standard_outfit_covers_its_body_in_every_stress_pose() {
    omuzaki_stage::link();
    let mut doc = kit("standard");
    let out = check(&mut doc, json!({"character": "kit"}));
    println!("covered {:.0} cm2, layers {:?}", out.covered_cm2, out.layers);
    for p in &out.poses {
        println!(
            "{:>16}: body {:6.2} cm2, layers {:6.2} cm2, deepest {:.1} mm, look at {:.3?}",
            p.pose, p.body_cm2, p.layers_cm2, p.deepest_mm, p.look_at
        );
        for at in &p.at {
            println!("{:>20} {} over {}: {:.2} cm2", "", at.layer, at.bone, at.cm2);
        }
    }
    assert!(out.covered_cm2 > 500.0, "{out:?}");
    // Ceilings on what shows, square centimetres, a little over what the kit gives now. Before the garments took the
    // skin's weights the stomach showed through the shirt in every bend (190 to 345 cm²) and the tights through the
    // skirt. What is left is a sliver at the shoulder seam and, with the arms crossed, the armpit's crease, which the
    // linear blend of the shoulder and the upper arm folds out past the sleeve.
    let ceilings = [
        ("rest", 0.5),
        ("bend-forward-30", 1.0),
        ("bend-back-30", 3.0),
        ("bend-left-30", 1.5),
        ("bend-right-30", 1.5),
        ("twist-left-30", 5.0),
        ("twist-right-30", 4.0),
        ("fall-back", 1.5),
        ("arms-up", 3.0),
        ("arms-crossed", 24.0),
    ];
    assert_eq!(out.poses.len(), ceilings.len(), "every stress pose has a ceiling");
    for (pose, (name, ceiling)) in out.poses.iter().zip(ceilings) {
        assert_eq!(pose.pose, name);
        assert!(
            pose.body_cm2 <= ceiling,
            "{name}: {:.2} cm² of body shows (at most {ceiling}): {:?}",
            pose.body_cm2,
            pose.at
        );
        assert!(
            pose.layers_cm2 <= omuzaki_stage::ops::fit::check::FITS_CM2,
            "{name}: a garment shows through another: {:?}",
            pose.at
        );
    }
}

/// The kit's default character on `template`, built as the app builds it.
fn built(template: &str) -> omuzaki_char_api::BuiltCharacter {
    use omuzaki_char::builder::CachingBuilder;
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef};
    use omuzaki_doc_api::{DocPath, DocRead};

    omuzaki_stage::link();
    let doc = kit(template);
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("the character is stored").clone();
    let def = CharacterDef::from_node(&node).expect("the stored character reads back");
    CachingBuilder::new()
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

/// Renders every stress pose from the front, the side and the back into `OMUZAKI_FIT_DIR` for the evidence (on
/// lavapipe with the `wgpu` feature).
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_stress_poses_when_asked() {
    use omuzaki_core::glam::{UVec2, Vec3};
    use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc};
    use omuzaki_stage::UploadOptions;
    use omuzaki_stage::fit::battery;
    use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
    use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
    use omuzaki_stage::ops::render::still::encode_png;
    use omuzaki_stage::recipe::LightRig;
    use omuzaki_stage::stages::FrameLoop;

    let Some(directory) = std::env::var_os("OMUZAKI_FIT_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_FIT_DIR to write the pictures");
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

    let character = built("standard");
    let poses = battery(&character.rig.bind);
    let camera = |eye: Vec3, at: Vec3| {
        Camera::look_at(eye, at, Vec3::Y, Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 })
    };
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(480, 720), alpha: AlphaEncoding::Straight }).unwrap();
    let at = Vec3::new(0.0, 1.0, 0.0);
    let view = Scene::new(camera(Vec3::new(0.0, 1.0, 2.4), at));
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let mut stage =
        Stage::new(&mut renderer, character, options, view, target, Quality::high(), StageClock::live()).unwrap();
    stage.set_face(&mut renderer, FaceRig::standard().with_executor(Box::new(InlineExecutor))).unwrap();
    let rig = LightRig::load("viewer-studio").unwrap();
    {
        let scene = stage.scene_mut();
        scene.lights = rig.to_lights().unwrap();
        scene.background = rig.background();
        scene.post = rig.post(false);
    }
    let views =
        [("front", Vec3::new(0.0, 1.0, 2.4)), ("side", Vec3::new(2.4, 1.0, 0.3)), ("back", Vec3::new(-0.3, 1.0, -2.4))];
    for stress in &poses {
        let input = FrameInput { pose: stress.pose, dt: 1.0 / 60.0, ..FrameInput::default() };
        for _ in 0..3 {
            stage.tick(&mut renderer, &input).unwrap();
        }
        for (name, eye) in views {
            stage.scene_mut().camera = camera(eye, at);
            stage.redraw(&mut renderer, &FrameInput { dt: 0.0, ..input.clone() }).unwrap();
            let image = renderer.read_target(target).unwrap();
            std::fs::write(directory.join(format!("{}-{name}.png", stress.name)), encode_png(&image).unwrap()).unwrap();
        }
    }
    FrameLoop::release(stage, &mut renderer).unwrap();
}
