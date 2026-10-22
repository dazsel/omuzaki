//! The `perform.*` ops over a recording stand-in for the performer: what each op asks of the host, what it answers,
//! how each refusal maps, and what an agent may do on its own.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, Scope, Services, ServicesBuilder, Undo, find};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use serde_json::{Value, json};

/// Records every call and answers from a script.
struct FakeHost {
    calls: Mutex<Vec<String>>,
    /// What every request answers with, when it is an error.
    refuse: Option<PerformError>,
    /// The settings as requested, the settings the "loop" has applied, and the last request number of each.
    state: Mutex<Settings>,
}

#[derive(Default)]
struct Settings {
    requested: RetargetConfig,
    applied: RetargetConfig,
    requested_seq: u64,
    applied_seq: u64,
}

impl FakeHost {
    /// What the perform loop does on a later step: take what was requested.
    fn loop_takes_requests(&self) {
        let mut state = self.state.lock().unwrap();
        (state.applied, state.applied_seq) = (state.requested.clone(), state.requested_seq);
    }

    fn new(refuse: Option<PerformError>) -> Arc<Self> {
        Arc::new(Self { calls: Mutex::default(), refuse, state: Mutex::default() })
    }

    fn record(&self, call: String) -> Result<(), PerformError> {
        self.calls.lock().unwrap().push(call);
        self.refuse.map_or(Ok(()), Err)
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl PerformHost for FakeHost {
    fn calibrate(&self, frames: usize) -> Result<(), PerformError> {
        self.record(format!("calibrate {frames}"))
    }

    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        self.record(format!("set_retarget {change:?}"))?;
        let mut state = self.state.lock().unwrap();
        let config = &mut state.requested;
        config.smoothing = change.smoothing.unwrap_or(config.smoothing);
        config.exaggeration = change.exaggeration.unwrap_or(config.exaggeration);
        config.head_gain = change.head_gain.unwrap_or(config.head_gain);
        config.mirror = change.mirror.unwrap_or(config.mirror);
        let requested = config.clone();
        state.requested_seq += 1;
        Ok(RetargetReceipt { seq: state.requested_seq, requested })
    }

    fn set_calibration(&self, _calibration: Calibration) -> Result<(), PerformError> {
        self.record("set_calibration".to_owned())
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Webcam)
    }

    fn requested_retarget(&self) -> RetargetConfig {
        self.state.lock().unwrap().requested.clone()
    }

    fn status(&self) -> PerformStatus {
        self.calls.lock().unwrap().push("status".to_owned());
        let state = self.state.lock().unwrap();
        PerformStatus {
            tracking: Tracking::Tracking,
            calibration: CalibrationState::Capturing { frames: 12, of: 150 },
            retarget: state.applied.clone(),
            retarget_seq: state.applied_seq,
            source_fps: 29.5,
        }
    }
}

fn services(host: &Arc<FakeHost>) -> Services {
    ServicesBuilder::new().insert::<dyn PerformHost>(host.clone()).build()
}

fn person() -> Actor {
    Actor::Person { name: "performer".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "test".into(), model: None }
}

fn plan(op: &str, args: Value, actor: &Actor, services: Option<&Services>, dry_run: bool) -> Result<Plan, OpError> {
    let op = (find(op).expect("the op registers itself").decode)(args)?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_dry_run(dry_run);
    match services {
        Some(services) => op.plan(&ctx.with_services(services)),
        None => op.plan(&ctx),
    }
}

fn run(host: &Arc<FakeHost>, op: &str, args: Value) -> Result<Plan, OpError> {
    plan(op, args, &person(), Some(&services(host)), false)
}

#[test]
fn the_three_ops_declare_their_kind_scope_and_undo() {
    let meta = |name: &str| (find(name).expect("registered").meta)();
    for (name, kind, scope, undo) in [
        ("perform.calibrate", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("perform.set_retarget", OpKind::Edit, Scope::Perform, Undo::Explicit),
        ("perform.status", OpKind::Query, Scope::Read, Undo::None),
    ] {
        let m = meta(name);
        assert_eq!((m.kind, m.scope, m.undo), (kind, scope, undo), "{name}");
    }
}

/// Runs a stored undo or redo call as the history would.
fn replay(host: &Arc<FakeHost>, call: &omuzaki_ops_api::OpCall) -> Plan {
    run(host, &call.op, call.args.clone()).expect("the stored call runs")
}

#[test]
fn set_retarget_records_the_calls_that_put_the_dials_back_and_again() {
    let host = FakeHost::new(None);
    let first = run(&host, "perform.set_retarget", json!({ "smoothing": 0.25, "exaggeration": 2.0 })).unwrap();
    let before = host.requested_retarget();
    let second = run(&host, "perform.set_retarget", json!({ "smoothing": 0.9, "mirror": true })).unwrap();
    let after = host.requested_retarget();
    assert!(first.effect.is_some() && first.patch.is_empty(), "the dials are outside the document");
    let effect = second.effect.expect("the step can be undone");
    // The undo sets every dial as it was requested before, not only the ones this change named.
    assert_eq!(
        effect.undo.args,
        json!({ "smoothing": 0.25, "exaggeration": 2.0, "head_gain": before.head_gain, "mirror": before.mirror })
    );
    replay(&host, &effect.undo);
    assert_eq!(host.requested_retarget(), before);
    replay(&host, &effect.redo);
    assert_eq!(host.requested_retarget(), after);
    // A dry run changes nothing, so it has nothing to undo.
    let dry = plan("perform.set_retarget", json!({ "smoothing": 0.5 }), &person(), Some(&services(&host)), true);
    assert!(dry.unwrap().effect.is_none());
}

#[test]
fn calibrate_is_undone_by_putting_the_previous_calibration_back_and_redone_by_capturing_again() {
    let host = FakeHost::new(None);
    let effect = run(&host, "perform.calibrate", json!({ "frames": 90 })).unwrap().effect.expect("an effect");
    assert_eq!(effect.undo.op, "perform.calibrate");
    assert!(effect.undo.args["restore"].is_object(), "{}", effect.undo.args);
    assert_eq!(effect.redo.args["frames"], 90);
    // The undo sets the calibration in use before the capture (here the identity, so no range is listed).
    let undone = replay(&host, &effect.undo);
    assert_eq!(undone.output.unwrap()["restored"], true);
    assert_eq!(effect.undo.args["restore"]["ranges"], json!({}));
    replay(&host, &effect.redo);
    assert_eq!(host.calls(), ["calibrate 90", "set_calibration", "calibrate 90"]);
    // A restore is itself undoable, and a channel that isn't ARKit is refused by name.
    let restore = json!({ "restore": { "profile": "webcam", "ranges": { "jawOpen": { "lo": 0.125, "hi": 0.75 } }, "head_rest": { "yaw": 0.0, "pitch": 0.0, "roll": 0.0 } } });
    let restored = run(&host, "perform.calibrate", restore).unwrap();
    assert_eq!(restored.effect.unwrap().redo.args["restore"]["ranges"]["jawOpen"], json!({ "lo": 0.125, "hi": 0.75 }));
    let bad = json!({ "restore": { "profile": "webcam", "ranges": { "tongue": { "lo": 0.0, "hi": 1.0 } }, "head_rest": { "yaw": 0.0, "pitch": 0.0, "roll": 0.0 } } });
    let e = run(&host, "perform.calibrate", bad).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("restore/ranges/tongue")));
}

#[test]
fn calibrate_starts_a_capture_and_defaults_to_150_frames() {
    let host = FakeHost::new(None);
    let out = run(&host, "perform.calibrate", json!({ "frames": 200 })).unwrap();
    assert!(out.patch.is_empty(), "a live action changes no document");
    assert_eq!(out.output, Some(json!({ "started": true, "frames": 200 })));
    let out = run(&host, "perform.calibrate", json!({})).unwrap();
    assert_eq!(out.output, Some(json!({ "started": true, "frames": 150 })));
    assert_eq!(host.calls(), ["calibrate 200", "calibrate 150"]);
}

#[test]
fn calibrate_rejects_frame_counts_outside_30_to_600_naming_the_field() {
    let host = FakeHost::new(None);
    for frames in [0, 29, 601] {
        let e = run(&host, "perform.calibrate", json!({ "frames": frames })).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("frames")), "{frames}: {e:?}");
    }
    for frames in [30, 600] {
        run(&host, "perform.calibrate", json!({ "frames": frames })).unwrap();
    }
    assert_eq!(host.calls(), ["calibrate 30", "calibrate 600"], "out-of-range values reached the host");
}

#[test]
fn set_retarget_replies_at_once_with_the_requested_settings_and_status_shows_them_once_applied() {
    let host = FakeHost::new(None);
    let out = run(&host, "perform.set_retarget", json!({ "smoothing": 0.7, "mirror": true })).unwrap().output.unwrap();
    assert_eq!(out["seq"], json!(1));
    assert_eq!(out["requested"]["smoothing"], json!(0.7f32));
    assert_eq!(out["requested"]["mirror"], json!(true));
    assert_eq!(out["requested"]["head_gain"], json!(1.0), "fields left out keep their value");
    assert!(out["requested"].get("lost").is_some(), "the whole config comes back");
    assert_eq!(
        host.calls(),
        [
            "set_retarget RetargetChange { smoothing: Some(0.7), exaggeration: None, head_gain: None, mirror: Some(true) }"
        ]
    );

    // The loop has not taken it yet, so the status is still the old settings, and says which request it includes.
    let status = |host: &Arc<FakeHost>| run(host, "perform.status", json!({})).unwrap().output.unwrap();
    let before = status(&host);
    assert_eq!(before["retarget"]["smoothing"], json!(RetargetConfig::default().smoothing));
    assert_eq!(before["retarget_seq"], json!(0));
    host.loop_takes_requests();
    let after = status(&host);
    assert_eq!(after["retarget"]["smoothing"], json!(0.7f32));
    assert_eq!(after["retarget"]["mirror"], json!(true));
    assert_eq!(after["retarget_seq"], out["seq"], "the status reaches the receipt's seq once it is applied");
}

#[test]
fn set_retarget_rejects_each_out_of_range_dial_naming_it_and_never_clamps() {
    let host = FakeHost::new(None);
    let cases = [
        ("smoothing", json!(-0.1)),
        ("smoothing", json!(1.1)),
        ("exaggeration", json!(0.9)),
        ("exaggeration", json!(3.1)),
        ("head_gain", json!(-1.0)),
        ("head_gain", json!(3.5)),
    ];
    for (field, value) in cases {
        let e = run(&host, "perform.set_retarget", json!({ field: value })).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{field}={value}: {e:?}");
    }
    assert!(host.calls().is_empty(), "nothing out of range reached the performer");
    for (field, value) in [("smoothing", 1.0), ("exaggeration", 3.0), ("head_gain", 0.0)] {
        run(&host, "perform.set_retarget", json!({ field: value })).unwrap();
    }
}

#[test]
fn status_answers_with_the_performer_status() {
    let host = FakeHost::new(None);
    let out = plan("perform.status", json!({}), &agent(), Some(&services(&host)), false).unwrap();
    assert!(out.patch.is_empty());
    let out = out.output.unwrap();
    assert_eq!(out["tracking"], "tracking");
    assert_eq!(out["calibration"], json!({ "state": "capturing", "frames": 12, "of": 150 }));
    assert_eq!(out["source_fps"], json!(29.5));
    assert_eq!(out["retarget"]["smoothing"], json!(RetargetConfig::default().smoothing));
}

#[test]
fn a_dry_run_calls_nothing() {
    let host = FakeHost::new(None);
    let services = services(&host);
    for (op, args) in
        [("perform.calibrate", json!({ "frames": 60 })), ("perform.set_retarget", json!({ "mirror": true }))]
    {
        let out = plan(op, args, &person(), Some(&services), true).unwrap();
        assert_eq!(out.output.unwrap()["dry_run"], json!(true), "{op}");
    }
    assert!(host.calls().is_empty(), "{:?}", host.calls());
    // A dry run still checks the arguments.
    let e = plan("perform.calibrate", json!({ "frames": 5 }), &person(), Some(&services), true).unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[test]
fn each_refusal_maps_to_its_error_code() {
    for (refusal, code) in [
        (PerformError::CalibrationRunning, ErrorCode::Conflict),
        (PerformError::QueueFull, ErrorCode::Conflict),
        (PerformError::Disconnected, ErrorCode::Unavailable),
        (PerformError::Stalled, ErrorCode::Unavailable),
        (PerformError::Unsupported, ErrorCode::Unsupported),
    ] {
        let host = FakeHost::new(Some(refusal));
        for (op, args) in [("perform.calibrate", json!({})), ("perform.set_retarget", json!({ "mirror": true }))] {
            let e = run(&host, op, args).unwrap_err();
            assert_eq!(e.code, code, "{op} with {refusal:?}: {e:?}");
        }
    }
    let e = run(&FakeHost::new(Some(PerformError::CalibrationRunning)), "perform.calibrate", json!({})).unwrap_err();
    assert!(e.message.contains("calibration"), "{e:?}");
}

#[test]
fn without_the_service_every_op_is_unavailable() {
    for (op, args) in
        [("perform.calibrate", json!({})), ("perform.set_retarget", json!({})), ("perform.status", json!({}))]
    {
        let e = plan(op, args, &person(), None, false).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unavailable, "{op}: {e:?}");
    }
}

#[test]
fn an_agent_needs_a_person_to_calibrate_but_not_to_tune_or_read() {
    let host = FakeHost::new(None);
    let services = services(&host);
    let e = plan("perform.calibrate", json!({}), &agent(), Some(&services), false).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.message.contains("calibration reads the performer's face"), "{e:?}");
    assert!(host.calls().is_empty(), "the performer was not asked");
    // The answer is the same in a dry run, so the approval card can show it.
    let e = plan("perform.calibrate", json!({}), &agent(), Some(&services), true).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    plan("perform.set_retarget", json!({ "smoothing": 0.5 }), &agent(), Some(&services), false).unwrap();
    plan("perform.status", json!({}), &agent(), Some(&services), false).unwrap();
}

#[test]
fn the_argument_schemas_name_the_fields_and_their_types() {
    let schema = |name: &str| serde_json::to_value((find(name).unwrap().schema)()).unwrap();
    let calibrate = schema("perform.calibrate");
    assert_eq!(calibrate["properties"]["frames"]["type"], "integer");
    assert!(calibrate["properties"]["frames"]["description"].as_str().unwrap().contains("30"));
    assert!(calibrate.get("required").is_none_or(|r| r.as_array().unwrap().is_empty()), "frames has a default");
    let set = schema("perform.set_retarget");
    let props = set["properties"].as_object().unwrap();
    assert_eq!(
        props.keys().map(String::as_str).collect::<Vec<_>>(),
        ["exaggeration", "head_gain", "mirror", "smoothing"]
    );
    assert!(
        set["description"].as_str().unwrap().contains("queued"),
        "the schema says the change is queued, not waited for"
    );
    assert_eq!(schema("perform.status")["type"], "object");
}
