//! IN-8's acceptance (design §4, §5, §13): the `input.capture.*` and `input.binding.*` ops over a real hub, with a
//! person, an agent, an automation and a read-only actor, and a typed string that never reaches an op's answer.

// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::fmt::Write as _;
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_input::{Hub, HubConfig};
use omuzaki_input_api::binding::{BindingAction, Edge};
use omuzaki_input_api::capture::{CaptureState, PauseWhy};
use omuzaki_input_api::event::DeviceClass;
use omuzaki_input_api::feed::{InputControl, InputHub};
use omuzaki_ops_api::{
    Actor, ErrorCode, ErrorDetail, Grants, OpCtx, OpError, OpRegistration, Scope, Services, ServicesBuilder, find,
};
use serde_json::{Value, json};
use support::{DESK, F9, KEYBOARD, PADS, Rig, W, edges};

fn person() -> Actor {
    Actor::Person { name: "Pat".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "helper".into(), model: None }
}

fn automation() -> Actor {
    Actor::System
}

fn services(control: Arc<dyn InputControl>) -> Services {
    ServicesBuilder::new().insert::<dyn InputControl>(control).build()
}

/// Plans the registered op `name` with `args` as `actor` holding `scopes`; for these ops planning is running.
fn run(name: &str, args: Value, actor: &Actor, scopes: &[Scope], services: &Services) -> Result<Value, OpError> {
    run_with(name, args, actor, scopes, services, None)
}

fn run_with(
    name: &str,
    args: Value,
    actor: &Actor,
    scopes: &[Scope],
    services: &Services,
    confirmed: Option<&Actor>,
) -> Result<Value, OpError> {
    let registration: &OpRegistration = find(name).unwrap();
    let op = (registration.decode)(args)?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let grants = Grants::of(scopes.iter().copied());
    let mut ctx = OpCtx::new(&doc, actor, &clock).with_services(services).with_grants(&grants);
    if let Some(person) = confirmed {
        ctx = ctx.with_confirmation(person);
    }
    op.plan(&ctx).map(|plan| plan.output.unwrap_or(Value::Null))
}

fn scope_of(name: &str) -> Scope {
    (find(name).unwrap().meta)().scope
}

fn person_only(error: &OpError) -> bool {
    error.code == ErrorCode::Denied && matches!(error.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. }))
}

fn needs_person(error: &OpError) -> bool {
    error.code == ErrorCode::Denied && matches!(error.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. }))
}

const ALL: &[Scope] = &[Scope::Read, Scope::Input, Scope::Perform, Scope::Process];

#[test]
fn the_ops_declare_the_design_s_scopes() {
    let expected = [
        ("input.capture.start", Scope::Input),
        ("input.capture.pause", Scope::Read),
        ("input.capture.stop", Scope::Read),
        ("input.capture.state", Scope::Read),
        ("input.capture.protect", Scope::Process),
        ("input.binding.list", Scope::Read),
        ("input.binding.set_action", Scope::Perform),
        ("input.binding.remove", Scope::Perform),
        ("input.binding.apply_preset", Scope::Perform),
    ];
    for (name, scope) in expected {
        assert_eq!(scope_of(name), scope, "{name}");
    }
}

#[test]
fn only_a_person_starts_capture_and_only_with_an_indicator_drawn() {
    let rig = Rig::new();
    let services = services(rig.control.clone());
    for actor in [agent(), automation()] {
        let refused = run("input.capture.start", json!({}), &actor, ALL, &services).unwrap_err();
        assert!(person_only(&refused), "{actor:?}: Denied, never queued: {refused:?}");
    }
    let confirmed = run_with("input.capture.start", json!({}), &agent(), ALL, &services, Some(&person())).unwrap_err();
    assert!(person_only(&confirmed), "nobody can confirm a start for the person");
    assert_eq!(rig.state(), CaptureState::Off);

    let started = run("input.capture.start", json!({ "classes": ["keyboard"] }), &person(), ALL, &services).unwrap();
    assert_eq!(started["state"]["state"], "on");
    assert!(rig.state().is_on());

    // A person's start with no indicator installed.
    let clock = Arc::new(FakeClock::new(Seconds(1.0)));
    let bare = Hub::new(HubConfig::new(clock, None).with_test_backends(&[DESK, PADS]).manual()).unwrap();
    let refused = run("input.capture.start", json!({}), &person(), ALL, &services_of(&bare)).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.message.contains("indicator"), "{refused:?}");
}

fn services_of(hub: &Hub) -> Services {
    services(hub.control())
}

#[test]
fn a_read_only_actor_pauses_and_stops_capture_and_both_are_idempotent() {
    let rig = Rig::started();
    let services = services(rig.control.clone());
    let read_only = [Scope::Read];
    run("input.capture.pause", json!({}), &agent(), &read_only, &services).unwrap();
    assert_eq!(rig.state(), CaptureState::paused(None));
    run("input.capture.pause", json!({}), &automation(), &read_only, &services).unwrap();
    assert_eq!(rig.state(), CaptureState::paused(None));
    run("input.capture.stop", json!({}), &agent(), &read_only, &services).unwrap();
    assert_eq!(rig.state(), CaptureState::Off);
    let again = run("input.capture.stop", json!({}), &agent(), &read_only, &services).unwrap();
    assert_eq!(again["state"]["state"], "off");
    run("input.capture.pause", json!({}), &agent(), &read_only, &services).unwrap();
    assert_eq!(rig.state(), CaptureState::Off, "pausing what is off leaves it off");
}

#[test]
fn a_dry_run_changes_nothing() {
    let rig = Rig::started();
    let services = services(rig.control.clone());
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let grants = Grants::of(ALL.iter().copied());
    let who = person();
    let ctx = OpCtx::new(&doc, &who, &clock).with_services(&services).with_grants(&grants).with_dry_run(true);
    for (name, args) in [
        ("input.capture.stop", json!({})),
        ("input.capture.pause", json!({})),
        ("input.capture.protect", json!({ "programs": ["vault"] })),
        ("input.binding.apply_preset", json!({ "preset": "locomotion" })),
    ] {
        let op = (find(name).unwrap().decode)(args).unwrap();
        let plan = op.plan(&ctx).unwrap();
        assert_eq!(plan.output.unwrap()["dry_run"], true, "{name}");
    }
    assert!(rig.state().is_on());
    assert!(rig.control.bindings().is_empty());
    assert!(rig.hub.protect_list().get().is_empty());
}

#[test]
fn the_pause_reason_is_shown_only_with_process_so_the_reasons_cannot_be_told_apart() {
    for why in [PauseWhy::ProtectedApp, PauseWhy::IndicatorLost] {
        let rig = Rig::started();
        let services = services(rig.control.clone());
        if why == PauseWhy::ProtectedApp {
            run("input.capture.protect", json!({ "programs": ["vault"] }), &person(), ALL, &services).unwrap();
            rig.processes.set(&["vault"]);
        } else {
            rig.advance(1.6);
        }
        rig.hub.tick();
        assert_eq!(rig.state(), CaptureState::paused(Some(why)));
        let without = run("input.capture.state", json!({}), &agent(), &[Scope::Read, Scope::Input], &services).unwrap();
        assert_eq!(without["state"], json!({ "state": "paused", "why": null }), "{why:?} hidden without process");
        let with = run("input.capture.state", json!({}), &agent(), &[Scope::Read, Scope::Process], &services).unwrap();
        let expected = serde_json::to_value(CaptureState::paused(Some(why))).unwrap();
        assert_eq!(with["state"], expected, "{why:?} shown with process");
        assert!(!with.to_string().contains("vault"), "no answer names a program");
    }
}

#[test]
fn the_protected_list_needs_process_and_a_person_and_its_answer_names_no_program() {
    let rig = Rig::started();
    let services = services(rig.control.clone());
    assert_eq!(scope_of("input.capture.protect"), Scope::Process, "the runner refuses it without process");
    let asked = run("input.capture.protect", json!({ "programs": ["vault"] }), &agent(), ALL, &services).unwrap_err();
    assert!(needs_person(&asked), "an agent's change waits for the person: {asked:?}");
    let set = run("input.capture.protect", json!({ "programs": ["vault", " "] }), &person(), ALL, &services).unwrap();
    assert_eq!(set["programs"], 1);
    assert!(!set.to_string().contains("vault"));
    assert_eq!(rig.hub.protect_list().get(), ["vault"]);
}

#[test]
fn a_binding_can_run_only_an_op_its_setter_could_run() {
    let rig = Rig::started();
    let services = services(rig.control.clone());
    let id = rig.trigger_on(1, F9, 0);
    let protect =
        json!({ "id": id, "action": { "op": { "op": "input.capture.protect", "args": { "programs": [] } } } });
    let perform_only = [Scope::Read, Scope::Input, Scope::Perform];
    let refused = run("input.binding.set_action", protect.clone(), &person(), &perform_only, &services).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Denied, "{refused:?}");
    assert!(refused.message.contains("process"));
    assert_eq!(rig.control.bindings()[0].action, BindingAction::Trigger(1), "unchanged");

    run("input.binding.set_action", protect, &person(), ALL, &services).unwrap();
    assert!(matches!(&rig.control.bindings()[0].action, BindingAction::Op { op, .. } if op == "input.capture.protect"));

    let unknown = json!({ "id": id, "action": { "op": { "op": "nope.nothing", "args": {} } } });
    let refused = run("input.binding.set_action", unknown, &person(), ALL, &services).unwrap_err();
    assert_eq!((refused.code, refused.field.as_deref()), (ErrorCode::Invalid, Some("action")));

    let by_agent = json!({ "id": id, "action": { "trigger": 2 } });
    assert!(needs_person(&run("input.binding.set_action", by_agent.clone(), &agent(), ALL, &services).unwrap_err()));
    run_with("input.binding.set_action", by_agent, &agent(), ALL, &services, Some(&person())).unwrap();
    assert_eq!(rig.control.bindings()[0].action, BindingAction::Trigger(2), "a confirmed agent may");

    let missing = run("input.binding.remove", json!({ "id": 999 }), &person(), ALL, &services).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    run("input.binding.remove", json!({ "id": id }), &person(), ALL, &services).unwrap();
    assert!(rig.control.bindings().is_empty());
    let listed = run("input.binding.list", json!({}), &agent(), &[Scope::Read], &services).unwrap();
    assert_eq!(listed, json!({ "bindings": [], "presets": ["locomotion"] }));
}

#[test]
fn a_preset_that_moves_a_held_binding_s_chord_ends_its_old_action_on_the_old_key() {
    let mut rig = Rig::started();
    let services = services(rig.control.clone());
    let id = rig.control.add_binding("walk forward", DeviceClass::Keyboard, BindingAction::Trigger(4)).unwrap();
    rig.hub.bind_key(id, F9, 0).unwrap();
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, BindingAction::Trigger(4), Edge::Press)]);
    let applied =
        run("input.binding.apply_preset", json!({ "preset": "locomotion" }), &person(), ALL, &services).unwrap();
    assert_eq!(applied["bindings"][0], json!(id), "the binding of the same name is reused");
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, BindingAction::Trigger(4), Edge::Release)], "the held action ends on the old key");
    rig.desk.key(KEYBOARD, W, true);
    assert_eq!(edges(&rig.intents(), id), [Edge::Press], "W now walks");
    let unknown =
        run("input.binding.apply_preset", json!({ "preset": "dance" }), &person(), ALL, &services).unwrap_err();
    assert_eq!((unknown.code, unknown.field.as_deref()), (ErrorCode::Invalid, Some("preset")));
}

/// What would show a typed string in text: the string, its keys' labels in a row, or its key codes.
fn leaks(text: &str, typed: &str, usages: &[u16]) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();
    if lower.contains(&typed.to_lowercase()) {
        found.push(format!("the string {typed:?}"));
    }
    let decimal = usages.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    if lower.replace(' ', "").contains(&decimal) {
        found.push("the key codes in decimal".to_owned());
    }
    for usage in usages {
        if lower.contains(&format!("0x{usage:02x}")) {
            found.push(format!("the key code {usage:#04x}"));
        }
    }
    found
}

#[test]
fn a_typed_string_never_reaches_an_op_s_answer() {
    let mut rig = Rig::started();
    let services = services(rig.control.clone());
    rig.locomotion();
    let typed = "hunter2";
    // h u n t e r 2 by HID usage.
    let usages = [0x0B_u16, 0x18, 0x11, 0x17, 0x08, 0x15, 0x1F];
    let wave = rig.trigger_on(3, usages[0], 0);
    for usage in usages {
        rig.desk.key(KEYBOARD, usage, true);
        rig.desk.key(KEYBOARD, usage, false);
    }
    let _ = (rig.motion(), rig.intents(), rig.hits());

    let mut text = String::new();
    let mut record = |result: Result<Value, OpError>| match result {
        Ok(value) => text.push_str(&value.to_string()),
        Err(error) => text.push_str(&serde_json::to_string(&error).unwrap()),
    };
    for (name, args) in [
        ("input.capture.state", json!({})),
        ("input.binding.list", json!({})),
        ("input.binding.set_action", json!({ "id": wave, "action": { "trigger": 5 } })),
        ("input.binding.apply_preset", json!({ "preset": "locomotion" })),
        ("input.capture.protect", json!({ "programs": [] })),
        ("input.capture.pause", json!({})),
        ("input.capture.start", json!({})),
        ("input.capture.stop", json!({})),
        ("input.binding.remove", json!({ "id": wave })),
        ("input.binding.remove", json!({ "id": wave })),
        ("input.capture.state", json!({})),
    ] {
        record(run(name, args.clone(), &person(), ALL, &services));
        record(run(name, args, &agent(), &[Scope::Read], &services));
    }
    write!(text, "{:?}", rig.indicator.view()).unwrap();
    assert!(text.len() > 1000, "the scan read the answers ({} bytes)", text.len());
    assert_eq!(leaks(&text, typed, &usages), Vec::<String>::new());
    // The scan finds a planted leak, so it is not vacuous.
    assert!(!leaks(&format!("{text}{typed}"), typed, &usages).is_empty());
    assert!(!leaks(&format!("{text}[11, 24, 17, 23, 8, 21, 31]"), typed, &usages).is_empty());
    assert!(!leaks(&format!("{text}0x0b"), typed, &usages).is_empty());
}
