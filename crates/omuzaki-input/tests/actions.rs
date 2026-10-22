//! The binding dispatcher (#469, PF-05; design §5 "Consumers"): a binding's op runs once per press, as the person,
//! with `cause: binding` and the binding's name, and undoes like the person's own edit; a trigger binding hands every
//! edge to the trigger sink in order; a failing op is reported and the next hit still runs; the dispatcher's thread
//! runs a press without anyone pumping it, and stops when dropped.
// Helpers outside a #[test] unwrap too.
#![allow(clippy::unwrap_used)]

mod support;

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_input::BindingRunner;
use omuzaki_input_api::binding::{BindingAction, BindingHit, BindingId, Edge};
use omuzaki_input_api::event::DeviceClass;
use omuzaki_input_api::feed::BindingFeed;
use omuzaki_ops_api::{
    Actor, CauseKind, ErrorCode, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo,
};
use omuzaki_ops_mock::MockRunner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::{F9, F10, KEYBOARD, Rig};

fn smile() -> DocPath {
    DocPath::parse("smile").unwrap()
}

/// Sets `smile` to `value`: a stand-in for an expression op.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetSmile {
    value: i64,
}

impl Op for SetSmile {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bindingtest.set_smile",
            summary: "Set the smile.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(smile(), ctx.doc.get(&smile()).cloned(), Some(json!(self.value)))))
    }
}
omuzaki_ops_api::register_op!(SetSmile);

fn showing() -> DocPath {
    DocPath::parse("showing").unwrap()
}

/// Shows expression `name` while `down`: a stand-in for `trigger.press`, the op a held binding runs.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Show {
    name: String,
    down: bool,
}

impl Op for Show {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bindingtest.show",
            summary: "Show an expression while down.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let now = self.down.then(|| json!(self.name));
        Ok(Plan::edit(Patch::new().with(showing(), ctx.doc.get(&showing()).cloned(), now)))
    }
}
omuzaki_ops_api::register_op!(Show);

/// A feed with nothing in it, left in the rig once the dispatcher takes the real one.
struct Taken;

impl BindingFeed for Taken {
    fn poll(&mut self) -> Option<BindingHit<'_>> {
        None
    }
}

fn person() -> Actor {
    Actor::Person { name: "Performer".to_owned() }
}

/// The rig, a runner over an empty document, and a dispatcher over the rig's binding feed.
fn rigged() -> (Rig, Arc<MockRunner<MemDoc>>, BindingRunner) {
    let mut rig = Rig::started();
    let runner = Arc::new(MockRunner::new(MemDoc::default()));
    let feed = std::mem::replace(&mut rig.bindings, Box::new(Taken));
    let dispatcher = BindingRunner::new(feed, runner.clone(), rig.control.clone(), person());
    (rig, runner, dispatcher)
}

/// A key binding named `name` doing `action`, on F9 or F10.
fn bind(rig: &Rig, name: &str, usage: u16, action: BindingAction) -> BindingId {
    let id = rig.control.add_binding(name, DeviceClass::Keyboard, action).unwrap();
    rig.hub.bind_key(id, usage, 0).unwrap();
    id
}

#[test]
fn an_op_binding_runs_once_per_press_as_the_person_and_undoes_like_their_own_edit() {
    let (mut rig, runner, mut dispatcher) = rigged();
    bind(&rig, "Big smile", F9, BindingAction::op("bindingtest.set_smile", json!({ "value": 1 })));

    rig.desk.key(KEYBOARD, F9, true);
    rig.desk.repeat(KEYBOARD, F9);
    rig.desk.repeat(KEYBOARD, F9);
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(dispatcher.pump(), 4, "press, two repeats and the release");

    let records = runner.records();
    assert_eq!(records.len(), 1, "the press runs the op; repeats and the release run nothing");
    let record = &records[0];
    assert_eq!(record.op, "bindingtest.set_smile");
    assert_eq!(record.actor, person());
    let cause = record.cause.clone().unwrap();
    assert_eq!((cause.kind, cause.text.as_deref()), (CauseKind::Binding, Some("Big smile")));
    assert_eq!(runner.get(&smile(), &Grants::person()).unwrap(), Some(json!(1)));
    assert_eq!(dispatcher.report().ran, 1);

    // The person's undo takes the binding's edit back: it is theirs.
    assert!(runner.undo(&person(), &Grants::person()).unwrap().is_some());
    assert_eq!(runner.get(&smile(), &Grants::person()).unwrap(), None);

    // The next press runs it again.
    rig.desk.key(KEYBOARD, F9, true);
    dispatcher.pump();
    assert_eq!(runner.records().len(), 2);
}

#[test]
fn a_held_op_runs_with_down_on_the_press_and_again_without_on_the_release() {
    let (mut rig, runner, mut dispatcher) = rigged();
    bind(&rig, "Smile while held", F9, BindingAction::hold("bindingtest.show", json!({ "name": "smile" })));

    rig.desk.key(KEYBOARD, F9, true);
    rig.desk.repeat(KEYBOARD, F9);
    dispatcher.pump();
    assert_eq!(runner.get(&showing(), &Grants::person()).unwrap(), Some(json!("smile")), "shown while held");
    rig.desk.key(KEYBOARD, F9, false);
    dispatcher.pump();
    assert_eq!(runner.get(&showing(), &Grants::person()).unwrap(), None, "gone on the release");

    let calls: Vec<_> = runner.records().into_iter().map(|r| r.args).collect();
    assert_eq!(
        calls,
        [json!({ "name": "smile", "down": true }), json!({ "name": "smile", "down": false })],
        "no repeat"
    );
    assert_eq!(dispatcher.report().ran, 2);
}

#[test]
fn a_trigger_binding_hands_every_edge_to_the_sink_in_order() {
    let (mut rig, runner, dispatcher) = rigged();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let mut dispatcher = dispatcher.with_triggers(Box::new(move |slot, edge, _at: Seconds| {
        sink.lock().unwrap().push((slot, edge));
    }));
    bind(&rig, "Wave", F10, BindingAction::Trigger(4));

    rig.desk.key(KEYBOARD, F10, true);
    rig.desk.repeat(KEYBOARD, F10);
    rig.desk.key(KEYBOARD, F10, false);
    dispatcher.pump();

    assert_eq!(*seen.lock().unwrap(), [(4, Edge::Press), (4, Edge::Repeat), (4, Edge::Release)]);
    assert_eq!(dispatcher.report().triggers, 3);
    assert!(runner.records().is_empty(), "a trigger runs no op");
}

#[test]
fn a_failing_op_is_reported_and_the_next_hit_still_runs() {
    let (mut rig, runner, mut dispatcher) = rigged();
    let broken = bind(&rig, "Broken", F9, BindingAction::op("bindingtest.no_such_op", json!({})));
    bind(&rig, "Big smile", F10, BindingAction::op("bindingtest.set_smile", json!({ "value": 2 })));

    rig.desk.key(KEYBOARD, F9, true);
    rig.desk.key(KEYBOARD, F10, true);
    dispatcher.pump();

    let report = dispatcher.report();
    assert_eq!((report.ran, report.failed), (1, 1));
    let failure = report.last_failure.unwrap();
    assert_eq!((failure.id, failure.op.as_str()), (broken, "bindingtest.no_such_op"));
    assert_eq!(failure.error.code, ErrorCode::Invalid);
    assert_eq!(runner.get(&smile(), &Grants::person()).unwrap(), Some(json!(2)));
}

#[test]
fn the_dispatcher_thread_runs_a_press_on_its_own_and_stops_when_dropped() {
    let (mut rig, _runner, dispatcher) = rigged();
    let (sent, received) = channel();
    let dispatcher = dispatcher
        .with_triggers(Box::new(move |slot, edge, _at: Seconds| {
            let _ = sent.send((slot, edge));
        }))
        .spawn()
        .unwrap();
    bind(&rig, "Wave", F9, BindingAction::Trigger(1));

    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(received.recv_timeout(Duration::from_secs(10)), Ok((1, Edge::Press)));
    assert_eq!(dispatcher.report().triggers, 1);

    drop(dispatcher);
    // The thread is gone, and with it the sink: the channel is closed.
    assert!(received.recv_timeout(Duration::from_secs(10)).is_err());
}
