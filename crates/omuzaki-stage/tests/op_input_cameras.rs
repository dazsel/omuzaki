//! `input.cameras` (#856): the cameras the OS reports, with none, one and several, and one in use; listing opens none.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "live/support.rs"]
mod support;

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, ServicesBuilder, Undo, find};
use omuzaki_stage::perform::host::{PerformHost, perform_channel};
use omuzaki_stage::perform::input::{InputHost, LiveInput, NoPrompt, SourceSlot};
use omuzaki_track_api::tracker::TrackerConfig;
use serde_json::{Value, json};
use support::{CameraLog, Cameras, face_for, performer};

fn input(ids: &[&str]) -> (Arc<LiveInput>, CameraLog) {
    let (tracker, log) = Cameras::new(ids.iter().map(|id| (*id, face_for(5.0, 1))).collect());
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let input = LiveInput::new(
        Box::new(tracker),
        Box::new(NoPrompt),
        TrackerConfig::default(),
        Arc::new(SourceSlot::new()),
        perform,
    );
    (Arc::new(input), log)
}

fn run(input: &Arc<LiveInput>, op: &str, args: Value, actor: &Actor) -> Value {
    let services = ServicesBuilder::new().insert::<dyn InputHost>(input.clone()).build();
    let op = (find(op).expect("registered").decode)(args).unwrap();
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    op.plan(&OpCtx::new(&doc, actor, &clock).with_services(&services)).unwrap().output.unwrap()
}

fn agent() -> Actor {
    Actor::Agent { id: "test".into(), model: None }
}

fn person() -> Actor {
    Actor::Person { name: "performer".into() }
}

fn row(id: &str, bound: bool, in_use: bool) -> Value {
    json!({ "id": id, "name": format!("Camera {id}"), "bound": bound, "in_use": in_use })
}

#[test]
fn it_is_a_read_query() {
    let meta = (find("input.cameras").expect("registered").meta)();
    assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Query, Scope::Read, Undo::None));
}

#[test]
fn none_one_and_several_cameras_and_one_in_use() {
    let cases: [(&[&str], Value); 3] = [
        (&[], json!({ "cameras": [] })),
        (&["a"], json!({ "cameras": [row("a", false, false)] })),
        (
            &["a", "b", "c"],
            json!({ "cameras": [row("a", false, false), row("b", false, false), row("c", false, false)] }),
        ),
    ];
    for (ids, expected) in cases {
        let (input, log) = input(ids);
        assert_eq!(run(&input, "input.cameras", json!({}), &agent()), expected, "{ids:?}");
        assert!(!log.started(), "listing opens no camera");
    }

    // One bound and in use: the live mirror reads it.
    let (input, log) = input(&["a", "b"]);
    run(&input, "input.select", json!({ "source": "webcam" }), &person());
    run(&input, "input.bind_camera", json!({ "camera": "b" }), &person());
    assert_eq!(
        run(&input, "input.cameras", json!({}), &agent()),
        json!({ "cameras": [row("a", false, false), row("b", true, false)] })
    );
    run(&input, "input.preview", json!({ "on": true }), &person());
    let calls = log.calls().len();
    assert_eq!(
        run(&input, "input.cameras", json!({}), &agent()),
        json!({ "cameras": [row("a", false, false), row("b", true, true)] })
    );
    assert_eq!(log.calls().len(), calls, "listing starts and stops nothing");
    run(&input, "input.preview", json!({ "on": false }), &person());
}
