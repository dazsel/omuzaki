//! The OS input hub in the running app (ADR 0060 WIRE-2, #469): the `input.*` capture and binding ops are in the app's
//! runner, capture is off at start and refuses to start without a drawn indicator, and a held binding pressed from
//! outside the app (an OSC message, as a Stream Deck through Companion sends it) shows its expression on the running
//! performer through `trigger.press`, as the person, with `cause: binding` in the log.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use omuzaki_anim_api::HumanoidPose;
use omuzaki_doc_api::Value;
use omuzaki_input_api::binding::BindingAction;
use omuzaki_input_api::capture::{BackendState, CaptureState, InputError};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_input_api::feed::InputHub;
use omuzaki_ops_api::{Actor, CauseKind, Grants, Mode, Request, Runner};
use omuzaki_osc::{Arg, Writer};
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::wiring::{LiveApp, NoTracker, RealClock};
use serde_json::json;

fn run(runner: &Arc<dyn Runner>, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "you".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("{op}: {e:?}")).plan.output.unwrap_or(Value::Null)
}

/// Waits up to a generous limit for `check` to hold. The limit only bounds a failure; what is waited for is another
/// thread (the OSC backend, the dispatcher, the perform thread) taking its step.
fn eventually(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    check()
}

/// Whether the trigger on `key` is showing its expression now.
fn active(runner: &Arc<dyn Runner>, key: &str) -> bool {
    let listing = run(runner, "trigger.list", json!({}));
    let found = listing["triggers"].as_array().and_then(|all| all.iter().find(|t| t["binding"]["key"] == key));
    found.unwrap_or_else(|| panic!("{key} is not bound: {listing}"))["active"] == true
}

fn start() -> LiveApp {
    omuzaki_ui::link();
    LiveApp::start(Box::new(NoTracker), Box::new(RealClock::new()), Transparency::Fallback, HumanoidPose::REST)
        .expect("the live slice starts")
}

#[test]
fn the_input_ops_are_in_the_app_and_capture_is_off_until_a_person_starts_it_from_a_drawn_indicator() {
    let mut app = start();
    let runner = app.runner();
    let state = run(&runner, "input.capture.state", json!({}));
    assert!(state.get("state").is_some(), "{state}");
    assert_eq!(run(&runner, "input.binding.list", json!({}))["bindings"], json!([]));
    let hub = app.input_hub().expect("the hub started");
    assert_eq!(hub.control().report().state, CaptureState::Off);
    assert_eq!(hub.control().start(DeviceMask::ALL), Err(InputError::NoIndicator), "no indicator is drawn");
    app.shutdown().expect("stops cleanly");
    assert!(app.input_hub().is_none(), "the hub stops with the slice");
}

#[test]
fn an_osc_message_shows_an_expression_in_the_running_app_through_a_held_binding() {
    let mut app = start();
    let runner = app.runner();
    run(&runner, "trigger.bind", json!({ "key": "F1", "expression": "happy", "mode": "hold" }));
    let hub = app.input_hub().expect("the hub started");
    let control = hub.control();
    let action = BindingAction::hold("trigger.press", json!({ "key": "F1" }));
    let id = control.add_binding("Happy while held", DeviceClass::Osc, action).unwrap();

    // The shell's indicator, drawn every frame while the test runs.
    let indicator = hub.indicator_host().install();
    indicator.beat();
    let drawing = Arc::new(AtomicBool::new(true));
    let frames = {
        let (indicator, drawing) = (indicator.clone(), Arc::clone(&drawing));
        std::thread::spawn(move || {
            while drawing.load(Ordering::Acquire) {
                indicator.beat();
                std::thread::park_timeout(Duration::from_millis(16));
            }
        })
    };
    control.start(DeviceMask::NONE.with(DeviceClass::Osc)).unwrap();
    let running = || control.report().backends.iter().any(|b| b.name == "osc.udp" && b.state == BackendState::Running);
    assert!(eventually(running), "the OSC backend listens: {:?}", control.report());

    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = format!("/omuzaki/binding/{}", id.0);
    // A backend reports `Running` as its run opens, a moment before its socket listens, so each level is sent until it
    // shows: a press repeated while the binding is held is not a second press, and a release of nothing ends nothing.
    let send = |down: f32| {
        let packet = Writer::message(&address, &[Arg::Float(down)]);
        let _sent = sender.send_to(&packet, (Ipv4Addr::LOCALHOST, 39570));
    };
    let report = |app: &LiveApp| (app.binding_report(), app.input_hub().map(|h| h.control().report()));
    let shown = eventually(|| {
        send(1.0);
        active(&runner, "F1")
    });
    assert!(shown, "held: the expression shows: {:?}", report(&app));
    let faded = eventually(|| {
        send(0.0);
        !active(&runner, "F1")
    });
    assert!(faded, "let go: it fades: {:?}", report(&app));

    let log = runner.log(0, 1000, &Grants::person()).unwrap();
    let pressed: Vec<_> = log.iter().filter(|r| r.op == "trigger.press").collect();
    assert_eq!(pressed.len(), 2, "a press and a release: {pressed:?}");
    for record in pressed {
        assert_eq!(record.actor, Actor::Person { name: "you".into() });
        let cause = record.cause.clone().unwrap();
        assert_eq!((cause.kind, cause.text.as_deref()), (CauseKind::Binding, Some("Happy while held")));
    }

    drawing.store(false, Ordering::Release);
    frames.join().unwrap();
    app.shutdown().expect("stops cleanly");
}
