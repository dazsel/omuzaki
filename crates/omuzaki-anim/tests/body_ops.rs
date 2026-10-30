//! The living body's query ops through the registry, as an agent calls them: every declared example decodes and
//! answers, and `measure.reaction` reports a bonk and a fall the way `act.react` will play them.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim::body::measure::measure_poses;
use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, find};
use serde_json::{Value, json};

fn ask(op: &str, args: Value) -> Value {
    let reg = find(op).expect("the op registers itself");
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let plan = (reg.decode)(args).unwrap().plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
    assert!(plan.effect.is_none() && plan.patch.is_empty(), "{op} is a query");
    plan.output.unwrap()
}

/// Links the crate's registrations into this test binary.
fn link() {
    std::hint::black_box(omuzaki_anim::body::ops::standing_base());
}

#[test]
fn every_body_query_declares_examples_that_answer() {
    link();
    for op in ["gesture.list", "body.styles", "measure.body_motion", "measure.reaction", "measure.clip_motion"] {
        let reg = find(op).expect("registered");
        let meta = (reg.meta)();
        assert_eq!((meta.kind, meta.scope), (OpKind::Query, Scope::Read), "{op}");
        let examples = reg.examples();
        assert!(!examples.is_empty(), "{op} declares examples");
        for example in examples {
            let answer = ask(op, example.clone());
            assert!(answer.is_object(), "{op} {example}: {answer}");
        }
    }
}

#[test]
fn measure_reaction_reports_a_bonk_and_a_fall() {
    link();
    let bonk = ask("measure.reaction", json!({}));
    let head = bonk["reaction"]["head_deg"].as_f64().unwrap();
    assert!((8.0..=30.0).contains(&head), "{bonk}");
    assert_eq!(bonk["reaction"]["exact_after_recovery"], json!(true), "{bonk}");
    let fall = ask("measure.reaction", json!({ "impact": { "kind": "fall", "at": "chest" } }));
    assert!(fall["reaction"]["chest_deg"].as_f64().unwrap() > 25.0, "{fall}");
    assert!(fall["reaction"]["settled_s"].as_f64().unwrap() <= 3.0 + 1e-3, "{fall}");
}

#[test]
fn measure_clip_motion_reads_where_the_arms_rest_and_how_much_a_clip_moves() {
    link();
    let t_pose = measure_poses(&vec![HumanoidPose::REST; 30], 60.0);
    assert!((t_pose.arms_from_down_deg - 90.0).abs() < 1e-3, "{t_pose:?}");
    assert_eq!((t_pose.arms_deg_s, t_pose.frozen_share, t_pose.hand_up_share), (0.0, 1.0, 0.0));
    let standing = measure_poses(&vec![omuzaki_anim::body::ops::standing_base(); 30], 60.0);
    assert!((15.0..30.0).contains(&standing.arms_from_down_deg), "{standing:?}");
    // A wave holds a hand up and moves the arm far more than a calm talking body does.
    let wave = ask("measure.clip_motion", json!({ "clip": "wave" }));
    let (clip, living) = (&wave["clip_motion"], &wave["living"]);
    assert!(clip["hand_up_share"].as_f64().unwrap() >= 0.45, "{wave}");
    assert!(clip["arms_deg_s"].as_f64().unwrap() > living["arms_deg_s"].as_f64().unwrap(), "{wave}");
    let missing = (find("measure.clip_motion").unwrap().decode)(json!({ "clip": "nope" })).unwrap();
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let e = missing.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap_err();
    assert_eq!(e.field.as_deref(), Some("clip"), "{e:?}");
}
