//! `measure.arm_room` (#2683) on the kit character `character.create` makes, through the registry as an agent calls
//! it: the body's parts as the arm guard measured them (its flared skirt a hull of its own), how close its arms hang,
//! and how far its body moves the arms of a take imported with `motion.import`: a take that hangs the arms straight is
//! moved onto the skirt, and less without the skirt.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, find};
use omuzaki_stage::ops::motion::arm_room::ArmRoomResult;
use serde_json::{Value, json};

/// Runs op `name` with `args` on `doc`, applying what it changes; its output.
fn run(doc: &mut MemDoc, name: &str, args: Value) -> Result<Value, OpError> {
    let (actor, clock) = (Actor::Agent { id: "test".into(), model: None }, FakeClock::default());
    let op = (find(name).expect("the op registers itself").decode)(args)?;
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock))?;
    doc.apply(&plan.patch).unwrap();
    Ok(plan.output.unwrap_or(Value::Null))
}

fn room(doc: &mut MemDoc, args: Value) -> ArmRoomResult {
    serde_json::from_value(run(doc, "measure.arm_room", args).unwrap()).unwrap()
}

/// Two seconds of a body standing with both arms hanging straight down, elbows straight: what a performer's arms
/// do at rest, closer to the body than a skirt leaves room for.
fn hanging_take() -> String {
    let joint = |name: &str, offset: &str, inner: &str| {
        format!("JOINT {name} {{ OFFSET {offset} CHANNELS 3 Zrotation Xrotation Yrotation {inner} }}")
    };
    let end = "End Site { OFFSET 0 0 0 }";
    let arm = |side: &str, x: f32| {
        joint(
            &format!("{side}Shoulder"),
            &format!("{} 0 0", x * 2.0),
            &joint(
                &format!("{side}Arm"),
                &format!("{} 0 0", x * 10.0),
                &joint(
                    &format!("{side}ForeArm"),
                    &format!("{} 0 0", x * 25.0),
                    &joint(&format!("{side}Hand"), &format!("{} 0 0", x * 22.0), end),
                ),
            ),
        )
    };
    let head = joint("Neck", "0 20 0", &joint("Head", "0 8 0", end));
    let chest = joint("Chest", "0 15 0", &format!("{head} {} {}", arm("Left", 1.0), arm("Right", -1.0)));
    let hierarchy = format!(
        "HIERARCHY\nROOT Hips {{ OFFSET 0 0 0 CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation {} }}\n",
        joint("Spine", "0 10 0", &chest)
    );
    // Hips (6), spine, chest, neck, head, then each side's shoulder, arm, forearm and hand (3 each).
    let frame = |down: f32| {
        let mut values = vec!["0 90 0 0 0 0".to_owned()];
        values.extend(std::iter::repeat_n("0 0 0".to_owned(), 4));
        for z in [-down, down] {
            values.extend(["0 0 0".to_owned(), format!("{z} 0 0"), "0 0 0".to_owned(), "0 0 0".to_owned()]);
        }
        values.join(" ")
    };
    format!("{hierarchy}MOTION\nFrames: 3\nFrame Time: 1\n{}\n{}\n{}\n", frame(88.0), frame(88.0), frame(88.0))
}

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    run(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "standard", "seed": 7})).unwrap();
    run(&mut doc, "motion.import", json!({"character": "kit", "id": "hang", "motion": {"text": hanging_take()}}))
        .unwrap();
    doc
}

#[test]
fn every_example_measures_the_kit() {
    omuzaki_stage::link();
    let reg = find("measure.arm_room").unwrap();
    let mut doc = kit();
    for example in reg.examples() {
        let out = room(&mut doc, example.clone());
        assert!(out.guarded && !out.parts.is_empty(), "{example}: {out:?}");
        if example.get("clip").is_some() {
            let clip = out.clip.unwrap();
            assert_eq!((clip.clip.as_str(), clip.frames > 0), ("wave", true));
            assert!((0.0..=1.0).contains(&clip.moved_share));
        }
    }
}

#[test]
fn a_skirt_is_its_own_part_and_holds_hanging_arms_on_it() {
    omuzaki_stage::link();
    let mut doc = kit();
    let dressed = room(&mut doc, json!({"character": "kit", "clip": "hang"}));
    let names: Vec<&str> = dressed.parts.iter().map(|p| p.part.as_str()).collect();
    println!("parts: {names:?}, hang {:.1} degrees", dressed.hang_deg);
    for part in ["waist", "torso", "pelvis", "left_thigh", "right_thigh", "head", "skirt"] {
        assert!(names.contains(&part), "{part} in {names:?}");
    }
    let widest = |out: &ArmRoomResult, part: &str| {
        out.parts.iter().filter(|p| p.part == part).map(|p| p.width_mm.max(p.end_width_mm)).fold(0.0, f32::max)
    };
    // The thigh is the leg's own; the skirt's hem stands out past it.
    assert!(widest(&dressed, "left_thigh") < 110.0 && widest(&dressed, "skirt") > 180.0, "{dressed:?}");
    assert!((5.0..20.0).contains(&dressed.hang_deg), "{}", dressed.hang_deg);

    // The take hangs the arms 2 degrees out: the skirt holds them out on every frame.
    let clip = dressed.clip.clone().unwrap();
    println!("dressed: {clip:?}");
    assert!(clip.moved_share > 0.9, "{clip:?}");
    assert!(clip.wrist_off_cm.mean > 3.0 && clip.arm_off_deg.mean > 4.0, "{clip:?}");

    // Without the skirt there is more room: the arms come closer to what the take asks.
    run(&mut doc, "part.remove", json!({"character": "kit", "part": "skirt"})).unwrap();
    let bare = room(&mut doc, json!({"character": "kit", "clip": "hang"}));
    let bare_clip = bare.clip.clone().unwrap();
    println!("bare: hang {:.1} degrees, {bare_clip:?}", bare.hang_deg);
    assert!(!bare.parts.iter().any(|p| p.part == "skirt"));
    assert!(bare.hang_deg < dressed.hang_deg, "{} vs {}", bare.hang_deg, dressed.hang_deg);
    assert!(bare_clip.wrist_off_cm.mean < clip.wrist_off_cm.mean, "{bare_clip:?} vs {clip:?}");
}

#[test]
fn unknown_characters_and_clips_are_refused_by_name() {
    omuzaki_stage::link();
    let mut doc = kit();
    let missing = run(&mut doc, "measure.arm_room", json!({"character": "nobody"})).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let clip = run(&mut doc, "measure.arm_room", json!({"character": "kit", "clip": "no-such-clip"})).unwrap_err();
    assert_eq!(clip.field.as_deref(), Some("clip"), "{clip:?}");
}
