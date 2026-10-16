//! The `input.*` ops over the real `LiveInput`, a tracker of scripted cameras and the real performer's handle: who may
//! run each op, what it starts, how each refusal maps, how a change is undone, and what is saved.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "live/support.rs"]
mod support;

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ErrorCode, ErrorDetail, OpCtx, OpError, OpKind, Plan, Scope, Services, ServicesBuilder, Undo, find,
};
use omuzaki_settings_api::service::Settings;
use omuzaki_stage::perform::host::{PerformHost, perform_channel};
use omuzaki_stage::perform::input::{InputHost, LiveInput, NoPrompt, SourceSlot};
use omuzaki_track_api::tracker::TrackerConfig;
use serde_json::{Value, json};
use support::{Call, CameraLog, Cameras, MemSettings, face_for, performer};

/// The ops' world: the input, the tracker's log and, when asked for, a settings service.
struct Rig {
    input: Arc<LiveInput>,
    log: CameraLog,
    settings: Option<Arc<MemSettings>>,
}

impl Rig {
    fn new(with_settings: bool) -> Self {
        let (tracker, log) = Cameras::new(vec![("a", face_for(5.0, 1)), ("b", face_for(5.0, 2))]);
        let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
        let perform: Arc<dyn PerformHost> = Arc::new(handle);
        let slot = Arc::new(SourceSlot::new());
        let input =
            Arc::new(LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), slot, perform));
        Self { input, log, settings: with_settings.then(|| Arc::new(MemSettings::default())) }
    }

    fn services(&self) -> Services {
        let builder = ServicesBuilder::new().insert::<dyn InputHost>(self.input.clone());
        match &self.settings {
            Some(settings) => builder.insert::<dyn Settings>(settings.clone()).build(),
            None => builder.build(),
        }
    }

    fn plan(&self, op: &str, args: Value, actor: &Actor, dry_run: bool) -> Result<Plan, OpError> {
        let op = (find(op).expect("the op registers itself").decode)(args)?;
        let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
        let services = self.services();
        op.plan(&OpCtx::new(&doc, actor, &clock).with_dry_run(dry_run).with_services(&services))
    }

    fn run(&self, op: &str, args: Value) -> Result<Plan, OpError> {
        self.plan(op, args, &person(), false)
    }

    fn output(&self, op: &str, args: Value) -> Value {
        self.run(op, args).unwrap().output.unwrap()
    }

    fn state(&self) -> Value {
        self.output("input.state", json!({}))
    }
}

fn person() -> Actor {
    Actor::Person { name: "performer".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "test".into(), model: None }
}

#[test]
fn every_op_declares_its_kind_scope_and_undo() {
    let cases = [
        ("input.select", OpKind::Edit, Scope::EditSettings, Undo::Explicit),
        ("input.bind_camera", OpKind::Edit, Scope::EditSettings, Undo::Explicit),
        ("input.preview", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("input.request_camera", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("input.state", OpKind::Query, Scope::Read, Undo::None),
    ];
    for (name, kind, scope, undo) in cases {
        let meta = (find(name).expect("registered").meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");
    }
}

/// Whether `result` is a refusal no one can confirm (`person_only`), or one a person can (`needs_person`).
fn refusal(result: Result<Plan, OpError>) -> Option<&'static str> {
    let error = result.err()?;
    assert_eq!(error.code, ErrorCode::Denied, "{error:?}");
    match error.detail.as_deref() {
        Some(ErrorDetail::PersonOnly { .. }) => Some("person_only"),
        Some(ErrorDetail::NeedsPerson { .. }) => Some("needs_person"),
        _ => panic!("an unexpected refusal: {error:?}"),
    }
}

#[test]
fn an_agent_is_refused_the_device_ops_and_queued_for_the_capture_ops() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    // (op, args, what an agent gets, what a person gets)
    let table = [
        ("input.select", json!({ "source": "webcam" }), Some("needs_person"), None),
        ("input.bind_camera", json!({ "camera": "b" }), Some("person_only"), None),
        ("input.request_camera", json!({}), Some("person_only"), None),
        ("input.preview", json!({ "on": true }), Some("needs_person"), None),
        ("input.preview", json!({ "on": false }), None, None),
        ("input.state", json!({}), None, None),
    ];
    for (op, args, as_agent, as_person) in table {
        for dry_run in [true, false] {
            assert_eq!(refusal(rig.plan(op, args.clone(), &agent(), dry_run)), as_agent, "agent {op} {args}");
        }
        assert_eq!(refusal(rig.plan(op, args.clone(), &person(), true)), as_person, "person {op} {args}");
    }
    assert!(!rig.log.started(), "nothing an agent asked started a capture");
    assert_eq!(rig.state()["camera"], "a", "the agent's bind changed nothing");
}

#[test]
fn a_confirmed_agent_request_runs_but_a_device_binding_never_does() {
    let rig = Rig::new(false);
    let (doc, clock, services) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), rig.services());
    let (agent, person) = (agent(), person());
    let ctx = OpCtx::new(&doc, &agent, &clock).with_services(&services).with_confirmation(&person);
    let select = (find("input.select").unwrap().decode)(json!({ "source": "webcam" })).unwrap();
    assert!(select.plan(&ctx).is_ok(), "a person confirmed it");
    let bind = (find("input.bind_camera").unwrap().decode)(json!({ "camera": "a" })).unwrap();
    assert_eq!(refusal(bind.plan(&ctx)), Some("person_only"), "nobody can confirm a device binding for the person");
}

#[test]
fn select_starts_no_capture_and_stops_a_running_one_of_another_kind() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    assert!(!rig.log.started(), "choosing the source and the camera starts nothing");
    rig.run("input.preview", json!({ "on": true })).unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "on", "camera": "a" }));
    rig.run("input.select", json!({ "source": "life_layer" })).unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }));
    assert_eq!(rig.log.calls().last(), Some(&Call::Stop));
}

#[test]
fn the_stop_path_is_idempotent() {
    let rig = Rig::new(false);
    for _ in 0..2 {
        let plan = rig.run("input.preview", json!({ "on": false })).unwrap();
        assert_eq!(plan.output.unwrap()["before"], false);
        assert!(plan.effect.is_none(), "stopping a stopped preview changes nothing to undo");
    }
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    assert_eq!(rig.log.calls(), vec![Call::Start("a".into())], "starting a running preview starts nothing again");
    rig.run("input.preview", json!({ "on": false })).unwrap();
    rig.run("input.preview", json!({ "on": false })).unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }));
}

#[test]
fn a_missing_camera_is_refused_at_every_step_and_nothing_starts() {
    let rig = Rig::new(true);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    let error = rig.run("input.preview", json!({ "on": true })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "no camera bound: {error:?}");
    let error = rig.run("input.bind_camera", json!({ "camera": "nope" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    assert!(error.hint.unwrap().contains("input.state"));
    assert_eq!(rig.settings.as_ref().unwrap().value("input.camera"), None, "a refused camera is not saved");
    let error = rig.run("input.bind_camera", json!({ "camera": " " })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);

    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.log.unplug("a");
    assert_eq!(rig.state()["camera_present"], false, "the state says the bound camera is gone");
    let error = rig.run("input.preview", json!({ "on": true })).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "the bound camera is unplugged: {error:?}");
    assert!(!rig.log.started());
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }));
}

#[test]
fn the_life_layer_and_the_iphone_have_no_preview_to_start() {
    let rig = Rig::new(false);
    assert_eq!(rig.run("input.preview", json!({ "on": true })).unwrap_err().code, ErrorCode::Conflict);
    rig.run("input.select", json!({ "source": "iphone" })).unwrap();
    assert_eq!(rig.run("input.preview", json!({ "on": true })).unwrap_err().code, ErrorCode::Unsupported);
    let error = rig.run("input.select", json!({ "source": "phone" })).unwrap_err();
    assert!(error.is_decode_error(), "{error:?}");
}

#[test]
fn each_edit_answers_the_inverse_call_that_undoes_it() {
    let rig = Rig::new(false);
    let plan = rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    let effect = plan.effect.unwrap();
    assert_eq!((effect.undo.op.as_str(), &effect.undo.args), ("input.select", &json!({ "source": "life_layer" })));
    assert_eq!(effect.redo.args, json!({ "source": "webcam" }));
    let effect = rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap().effect.unwrap();
    assert_eq!(effect.undo.args, json!({ "camera": null }));
    let effect = rig.run("input.preview", json!({ "on": true })).unwrap().effect.unwrap();
    assert_eq!(effect.undo.args, json!({ "on": false }));

    // Undo runs the inverse calls in reverse order: everything is as before.
    rig.run("input.preview", json!({ "on": false })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": null })).unwrap();
    rig.run("input.select", json!({ "source": "life_layer" })).unwrap();
    let state = rig.state();
    assert_eq!(
        (&state["source"], &state["camera"], &state["preview"]),
        (&json!("life_layer"), &Value::Null, &json!({ "state": "off" }))
    );
    assert!(
        rig.run("input.select", json!({ "source": "life_layer" })).unwrap().effect.is_none(),
        "no change, no effect"
    );
}

#[test]
fn a_dry_run_checks_everything_and_changes_nothing() {
    let rig = Rig::new(true);
    let plan = rig.plan("input.select", json!({ "source": "webcam" }), &person(), true).unwrap();
    assert_eq!(plan.output.unwrap()["dry_run"], true);
    assert_eq!(rig.state()["source"], "life_layer");
    assert_eq!(rig.settings.as_ref().unwrap().value("input.source"), None);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.plan("input.bind_camera", json!({ "camera": "a" }), &person(), true).unwrap();
    assert_eq!(rig.state()["camera"], Value::Null);
    assert_eq!(
        rig.plan("input.preview", json!({ "on": true }), &person(), true).unwrap_err().code,
        ErrorCode::Conflict
    );
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.plan("input.preview", json!({ "on": true }), &person(), true).unwrap();
    let permission = rig.plan("input.request_camera", json!({}), &person(), true).unwrap().output.unwrap();
    assert_eq!(permission["dry_run"], true);
    assert!(!rig.log.started());
}

#[test]
fn the_choices_are_saved_as_machine_settings_when_the_app_has_a_store() {
    let rig = Rig::new(true);
    let out = rig.output("input.select", json!({ "source": "webcam" }));
    assert_eq!(out["saved"], true);
    rig.run("input.bind_camera", json!({ "camera": "b" })).unwrap();
    let settings = rig.settings.as_ref().unwrap();
    assert_eq!(settings.value("input.source"), Some(json!("webcam")));
    assert_eq!(settings.value("input.camera"), Some(json!("b")));

    let without = Rig::new(false);
    assert_eq!(without.output("input.select", json!({ "source": "webcam" }))["saved"], false);
}

#[test]
fn the_state_reports_the_devices_the_permission_and_the_tracking() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "b" })).unwrap();
    let state = rig.state();
    assert_eq!(
        state["cameras"],
        json!([{ "id": "a", "name": "Camera a", "bound": false }, { "id": "b", "name": "Camera b", "bound": true }])
    );
    assert_eq!(
        (&state["camera_present"], &state["permission"], &state["tracking"]),
        (&json!(true), &json!("granted"), &json!("no_frames"))
    );
    assert_eq!(rig.output("input.request_camera", json!({})), json!({ "permission": "granted" }));
}

#[test]
fn without_an_input_host_the_ops_say_it_is_unavailable() {
    let (doc, clock, services, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Services::default(), person());
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(&services);
    for (op, args) in [("input.state", json!({})), ("input.preview", json!({ "on": false }))] {
        let op = (find(op).unwrap().decode)(args).unwrap();
        assert_eq!(op.plan(&ctx).unwrap_err().code, ErrorCode::Unavailable);
    }
}
