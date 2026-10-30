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
use omuzaki_track_api::doctor::{Diagnosis, Finding};
use omuzaki_track_api::tracker::TrackerConfig;
use omuzaki_track_net::vmc_out::VmcOut;
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
        let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), slot, perform)
            .with_pose_out(Box::new(VmcOut::default()));
        let input = Arc::new(input);
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
        ("input.doctor", OpKind::Query, Scope::Read, Undo::None),
        ("input.recheck", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("input.calibrate", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("input.calibration", OpKind::Query, Scope::Read, Undo::None),
        ("input.health", OpKind::Query, Scope::Read, Undo::None),
        ("input.vmc_out", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("input.gestures", OpKind::Query, Scope::Read, Undo::None),
        ("input.open_video", OpKind::Edit, Scope::EditSettings, Undo::Explicit),
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
        ("input.doctor", json!({}), None, None),
        ("input.recheck", json!({}), None, None),
        ("input.calibration", json!({}), None, None),
        ("input.health", json!({}), None, None),
        ("input.calibrate", json!({ "cancel": true }), None, None),
        ("input.vmc_out", json!({ "to": "127.0.0.1:39539" }), Some("needs_person"), None),
        ("input.vmc_out", json!({ "to": null }), None, None),
        ("input.gestures", json!({}), None, None),
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
    assert_eq!(settings.value("input.camera"), Some(json!({ "id": "b" })));

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

#[test]
fn undoing_an_unbind_or_a_change_of_source_brings_the_running_preview_back() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();

    let unbind = rig.run("input.bind_camera", json!({ "camera": null })).unwrap().effect.unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }), "unbinding stopped the preview");
    assert_eq!(unbind.undo.args, json!({ "camera": "a", "resume_preview": true }));
    rig.run(&unbind.undo.op, unbind.undo.args).unwrap();
    assert_eq!(rig.state()["camera"], "a");
    assert_eq!(rig.state()["preview"], json!({ "state": "on", "camera": "a" }), "undo restarted it");

    let change = rig.run("input.select", json!({ "source": "life_layer" })).unwrap().effect.unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }));
    assert_eq!(change.undo.args, json!({ "source": "webcam", "resume_preview": true }));
    rig.run(&change.undo.op, change.undo.args).unwrap();
    assert_eq!(rig.state()["preview"], json!({ "state": "on", "camera": "a" }));

    // With no preview running, undo starts none.
    rig.run("input.preview", json!({ "on": false })).unwrap();
    let plain = rig.run("input.bind_camera", json!({ "camera": null })).unwrap().effect.unwrap();
    assert_eq!(plain.undo.args, json!({ "camera": "a" }));
}

#[test]
fn a_camera_that_will_not_open_changes_nothing() {
    let rig = Rig::new(true);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    rig.log.refuse("b");
    let error = rig.run("input.bind_camera", json!({ "camera": "b" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error:?}");
    let state = rig.state();
    assert_eq!(state["camera"], "a", "the old binding stays");
    assert_eq!(state["preview"], json!({ "state": "on", "camera": "a" }), "and the old camera runs again");
    assert_eq!(
        rig.settings.as_ref().unwrap().value("input.camera"),
        Some(json!({ "id": "a" })),
        "nothing was saved for the refused camera"
    );
}

#[test]
fn a_refused_save_puts_the_host_back() {
    let rig = Rig::new(true);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    rig.settings.as_ref().unwrap().refuse_writes(true);

    let error = rig.run("input.bind_camera", json!({ "camera": "b" })).unwrap_err();
    assert!(error.message.contains("could not be saved"), "{}", error.message);
    let state = rig.state();
    assert_eq!(state["camera"], "a", "what runs is what is saved");
    assert_eq!(state["preview"], json!({ "state": "on", "camera": "a" }), "and the preview runs on it");

    let error = rig.run("input.select", json!({ "source": "life_layer" })).unwrap_err();
    assert!(error.message.contains("could not be saved"), "{}", error.message);
    let state = rig.state();
    assert_eq!(state["source"], "webcam");
    assert_eq!(state["preview"], json!({ "state": "on", "camera": "a" }));
}

#[test]
fn a_change_that_cannot_be_taken_back_is_kept_recorded_and_reported() {
    let rig = Rig::new(true);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.settings.as_ref().unwrap().refuse_writes(true);
    rig.log.unplug("a"); // the old camera is gone, so binding it back fails too

    let plan = rig.run("input.bind_camera", json!({ "camera": "b" })).expect("the change stays, so the op reports it");
    let answer = plan.output.unwrap();
    assert_eq!(answer["saved"], false);
    let retained = answer["retained"].as_array().expect("what could not be undone is listed");
    assert!(retained[0].as_str().unwrap().contains("could not be taken back"), "{retained:?}");
    assert_eq!(rig.state()["camera"], "b", "the new binding is what runs");
    let effect = plan.effect.expect("a retained change has its undo record");
    assert_eq!(effect.undo.args, json!({ "camera": "a" }));
}

#[test]
fn a_dry_run_of_a_resume_is_refused_as_the_real_run_would() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    for dry_run in [true, false] {
        let error = rig
            .plan("input.select", json!({ "source": "life_layer", "resume_preview": true }), &person(), dry_run)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict, "dry_run {dry_run}: {error:?}");
    }
    assert_eq!(rig.state()["source"], "webcam", "nothing changed");
    let error = rig.plan("input.bind_camera", json!({ "camera": null, "resume_preview": true }), &person(), true);
    assert_eq!(error.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(rig.state()["camera"], "a");
}

#[test]
fn a_partial_rollback_describes_the_state_it_left_not_the_one_asked_for() {
    let rig = Rig::new(true);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    rig.settings.as_ref().unwrap().refuse_writes(true);
    rig.log.refuse("a"); // the camera will not open again once it is stopped

    // Choosing the life layer stops the preview; the save is refused; the source goes back, but the preview cannot.
    let plan =
        rig.run("input.select", json!({ "source": "life_layer" })).expect("what is left is reported, not hidden");
    let answer = plan.output.unwrap();
    assert_eq!(answer["source"], "webcam", "the source the host holds");
    assert_eq!(answer["requested"], "life_layer");
    assert_eq!(answer["saved"], false);
    assert!(answer["retained"][0].as_str().unwrap().contains("could not be taken back"), "{answer}");
    let state = rig.state();
    assert_eq!((&state["source"], &state["preview"]), (&json!("webcam"), &json!({ "state": "off" })));
    let effect = plan.effect.expect("the stopped preview is recorded");
    assert_eq!(effect.undo.op, "input.preview");
    assert_eq!(effect.undo.args, json!({ "on": true }));
    assert_eq!(effect.redo.args, json!({ "on": false }));
}

#[test]
fn a_camera_that_will_not_open_and_an_old_one_that_will_not_restart_stop_the_preview_visibly() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    rig.log.refuse("a");
    rig.log.refuse("b");

    let plan = rig.run("input.bind_camera", json!({ "camera": "b" })).expect("the stopped preview is reported");
    let answer = plan.output.unwrap();
    assert_eq!(answer["camera"], "a", "the binding did not move");
    assert_eq!(answer["requested"], "b");
    assert!(answer["retained"][0].as_str().unwrap().contains("preview is stopped"), "{answer}");
    assert_eq!(rig.state()["preview"], json!({ "state": "off" }), "capture stopped");
    let effect = plan.effect.expect("the preview change is recorded");
    assert_eq!((effect.undo.op.as_str(), &effect.undo.args), ("input.preview", &json!({ "on": true })));
}

#[test]
fn the_doctor_answers_the_running_trackers_findings_and_a_recheck_clears_them() {
    let rig = Rig::new(false);
    rig.log.find(vec![Diagnosis::new(Finding::LowLight, 0.12, 0.25)]);
    let healthy = json!({ "healthy": true, "findings": [] });
    assert_eq!(rig.output("input.doctor", json!({})), healthy, "no capture runs, so nothing is measured");
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    let doctor = rig.output("input.doctor", json!({}));
    assert_eq!(doctor["healthy"], false);
    let found = &doctor["findings"][0];
    assert_eq!(found["finding"], "low_light");
    assert!((found["measured"].as_f64().unwrap() - 0.12).abs() < 1e-6, "{found}");
    assert!(found["fix"].as_str().unwrap().contains("lamp"), "{found}");
    // A dry run says what it would clear and clears nothing; an agent may ask for a recheck.
    let dry = rig.plan("input.recheck", json!({}), &agent(), true).unwrap().output.unwrap();
    assert_eq!((dry["cleared"][0]["finding"].clone(), rig.log.rechecks()), (json!("low_light"), 0));
    let done = rig.plan("input.recheck", json!({}), &agent(), false).unwrap().output.unwrap();
    assert_eq!((done["cleared"].as_array().unwrap().len(), rig.log.rechecks()), (1, 1));
    assert_eq!(rig.output("input.doctor", json!({})), healthy);
}

#[test]
fn calibrating_needs_the_person_and_a_running_preview_and_anyone_may_cancel() {
    let rig = Rig::new(false);
    assert_eq!(refusal(rig.plan("input.calibrate", json!({}), &agent(), false)), Some("needs_person"));
    let error = rig.run("input.calibrate", json!({})).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{error:?}");
    assert_eq!(rig.output("input.calibration", json!({})), json!({ "state": "idle" }));
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    let started = rig.output("input.calibrate", json!({}));
    assert_eq!((started["started"].clone(), started["status"]["state"].clone()), (json!(true), json!("waiting")));
    let stopped = rig.plan("input.calibrate", json!({ "cancel": true }), &agent(), false).unwrap().output.unwrap();
    assert_eq!(stopped["cancelled"], true);
    assert_eq!(rig.output("input.calibration", json!({})), json!({ "state": "cancelled" }));
}

#[test]
fn health_reads_the_running_capture_against_its_budget() {
    let rig = Rig::new(false);
    let idle = rig.output("input.health", json!({}));
    assert_eq!((idle["running"].clone(), idle["budget"].clone()), (json!(false), Value::Null));
    rig.log.find(vec![Diagnosis::new(Finding::Jitter, 30.0, 15.0)]);
    rig.run("input.select", json!({ "source": "webcam" })).unwrap();
    rig.run("input.bind_camera", json!({ "camera": "a" })).unwrap();
    rig.run("input.preview", json!({ "on": true })).unwrap();
    let health = rig.output("input.health", json!({}));
    assert_eq!(health["running"], true);
    assert_eq!(health["budget"]["key"], "track.face");
    assert_eq!(health["budget"]["budget"], 5.0);
    assert_eq!(health["findings"], json!(["jitter"]));
}

#[test]
fn vmc_out_sends_each_performed_step_to_a_receiver_and_its_undo_stops_it() {
    use omuzaki_anim_api::{DeviceProfile, TrackFrame};
    use omuzaki_track_net::protocol::Decoder;
    use omuzaki_track_net::protocol::vmc::VmcDecoder;

    let (tracker, log) = Cameras::new(vec![("a", face_for(5.0, 1))]);
    let mut performer = performer(&HumanoidPose::REST);
    let (handle, mut control) = perform_channel(&performer);
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let slot = Arc::new(SourceSlot::new());
    let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), slot, perform)
        .with_pose_out(Box::new(VmcOut::default()));
    let rig = Rig { input: Arc::new(input), log, settings: None };

    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let to = receiver.local_addr().unwrap().to_string();
    let plan = rig.run("input.vmc_out", json!({ "to": to })).unwrap();
    assert_eq!(plan.output.unwrap(), json!({ "to": to, "before": null, "dry_run": false }));
    let effect = plan.effect.unwrap();
    assert_eq!((effect.undo.args.clone(), effect.redo.args), (json!({ "to": null }), json!({ "to": to })));

    // The perform thread takes the tap with its next requests and publishes the step it runs.
    for i in 0..3 {
        control.advance(&mut performer, support::mid_step(i));
    }
    let mut decoder = VmcDecoder::new();
    let mut frame = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit);
    let mut buf = vec![0u8; 65_536];
    let got = loop {
        let n = receiver.recv(&mut buf).expect("the step reaches the receiver");
        if decoder.decode(&buf[..n], &mut frame).unwrap().is_some() {
            break frame;
        }
    };
    let head = got.head.expect("the head bone arrives");
    // Near rest: with no camera the life layer only sways the head a little.
    assert!(head.yaw.abs() < 0.05 && head.pitch.abs() < 0.05, "near the rest pose: {head:?}");
    assert_eq!(rig.input.pose_out().target, Some(receiver.local_addr().unwrap()));

    rig.run("input.vmc_out", effect.undo.args).unwrap();
    assert_eq!(rig.input.pose_out().target, None);
}

#[test]
fn gestures_list_their_keys_and_none_shows_before_a_capture() {
    let rig = Rig::new(false);
    let answer = rig.output("input.gestures", json!({}));
    let rows = answer["gestures"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows[0],
        json!({ "gesture": "tongue_out", "key": "gesture:tongue_out", "expression": null, "active": false })
    );
    let hands = answer["hands"].as_array().unwrap();
    let keys: Vec<_> = hands.iter().map(|row| row["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["gesture:open_palm", "gesture:fist", "gesture:point", "gesture:peace", "gesture:thumbs_up"]);
    assert!(hands.iter().all(|row| row["active"] == false));
}

#[test]
fn a_video_file_becomes_the_webcams_camera_plays_in_the_preview_and_undoes_back() {
    let rig = Rig::new(false);
    rig.run("input.select", json!({ "source": "iphone" })).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    std::fs::write(&clip, b"a recording").unwrap();
    let path = clip.display().to_string();

    // Reading a recording of the person is theirs to confirm; a dry run adds nothing.
    assert_eq!(refusal(rig.plan("input.open_video", json!({ "path": path }), &agent(), false)), Some("needs_person"));
    let dry = rig.plan("input.open_video", json!({ "path": path }), &person(), true).unwrap();
    assert_eq!(dry.output.unwrap()["camera"], Value::Null);
    assert!(!rig.state()["cameras"].to_string().contains("video:"), "{}", rig.state());

    let plan = rig.run("input.open_video", json!({ "path": path })).unwrap();
    let answer = plan.output.clone().unwrap();
    let id = format!("video:{path}");
    assert_eq!(answer["camera"]["id"], json!(id));
    assert_eq!((answer["camera"]["width"].clone(), answer["camera"]["fps"].clone()), (json!(640), json!(30)));
    assert_eq!(answer["before"], json!({ "source": "iphone", "camera": null }));
    let state = rig.state();
    assert_eq!((state["source"].clone(), state["camera"].clone()), (json!("webcam"), json!(id)), "{state}");

    // The preview plays it, as a webcam.
    rig.run("input.preview", json!({ "on": true })).unwrap();
    assert_eq!(rig.log.calls().last(), Some(&support::Call::Start(id.clone())));

    // Undo puts the source and camera back; redo opens it again.
    let effect = plan.effect.unwrap();
    rig.run(&effect.undo.op, effect.undo.args.clone()).unwrap();
    let state = rig.state();
    assert_eq!((state["source"].clone(), state["camera"].clone()), (json!("iphone"), Value::Null), "{state}");
    rig.run(&effect.redo.op, effect.redo.args.clone()).unwrap();
    assert_eq!(rig.state()["camera"], json!(id));
}

#[test]
fn a_video_path_must_be_absolute_and_name_a_file() {
    let rig = Rig::new(false);
    let refused = |args: Value| rig.run("input.open_video", args).unwrap_err();
    let relative = refused(json!({ "path": "clip.mp4" }));
    assert_eq!((relative.code, relative.field.as_deref()), (ErrorCode::Invalid, Some("path")));
    let missing = refused(json!({ "path": "/no/such/clip.mp4" }));
    assert_eq!(missing.code, ErrorCode::Invalid);
    assert!(missing.message.contains("no video file"), "{missing:?}");
    assert_eq!(refused(json!({})).code, ErrorCode::Invalid, "a path or a restore");
}
