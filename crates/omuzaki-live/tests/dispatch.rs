//! Dispatch (design D7, D8, D12; #1176) through the service, with a fake runner and fake sessions: the show's calls
//! run in its own automation session with D7's cause, a test event fired by an agent acts as that agent, the ordered
//! lane keeps request order under random latencies, four jobs run at once beside it, the network, program and
//! per-minute limits refuse with `limited`, nothing is retried, pause maps to the session's, and stop ends it.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_live::Automation;
use omuzaki_live_api::event::EventSink;
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, ShowId};
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::services::{AutomationControl, SystemWallClock};
use omuzaki_live_api::show::ShowDef;
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, CauseKind, Decision, EndReason, Ended, ErrorCode, Event, Grants, Limits, Op, OpCtx,
    OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, PendingApproval, Plan, Request, Resolution, Runner, Scope,
    SessionId, SessionInfo, SessionSpec, SessionState, SessionUndo, Sessions, Spend, TxId, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── Test node kinds ────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `test.ask`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AskParams {
    /// The event kind it listens to.
    #[serde(default)]
    kind: String,
    /// The op it calls.
    #[serde(default)]
    op: String,
}

/// On its event, calls its op with the event's payload; a failed reply is reported to `test.report` with its code.
struct Ask {
    op: String,
}

impl NodeKind for Ask {
    type Params = AskParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.ask", "Asks.", NodeClass::Trigger, Category::Triggers)
    }
    fn ports(_: &AskParams) -> Ports {
        Ports::new()
    }
    fn listens(params: &AskParams) -> Vec<EventKind> {
        vec![EventKind::new(params.kind.as_str()).unwrap()]
    }
    fn new(params: AskParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for Ask {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { event, .. } => cx.run_op(&self.op, event.payload.clone()).map(|_| ()),
            Input::Reply { outcome: Err(e), .. } => {
                cx.run_op("test.report", json!({ "op": self.op, "code": e.code.to_string() })).map(|_| ())
            }
            _ => Ok(()),
        }
    }
}
register_node!(Ask);

// ── A job op, so the registry knows `test.job` is one ──────────────────────────────────────────────────────────

/// A job (the fake runner does its work).
#[derive(Serialize, Deserialize, JsonSchema)]
struct TestJob {}

impl Op for TestJob {
    fn meta() -> OpMeta {
        OpMeta { name: "test.job", summary: "A job.", kind: OpKind::Job, scope: Scope::Perform, undo: Undo::None }
    }
    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}
register_op!(TestJob);

// ── A fake runner ──────────────────────────────────────────────────────────────────────────────────────────────

/// Records every request; holds the ops in `held` until released, tracking how many of each run at once; answers
/// `test.waiting` with `waiting` and `test.slow` after the payload's `micros`.
#[derive(Default)]
struct FakeRunner {
    state: Mutex<RunnerState>,
    changed: Condvar,
}

#[derive(Default)]
struct RunnerState {
    requests: Vec<Request>,
    held: Vec<String>,
    running: Vec<(String, usize)>,
    most: Vec<(String, usize)>,
}

fn bump(list: &mut Vec<(String, usize)>, op: &str, by: isize) -> usize {
    let slot = list.iter().position(|(o, _)| o == op).unwrap_or_else(|| {
        list.push((op.to_owned(), 0));
        list.len() - 1
    });
    list[slot].1 = list[slot].1.saturating_add_signed(by);
    list[slot].1
}

fn count(list: &[(String, usize)], op: &str) -> usize {
    list.iter().find(|(o, _)| o == op).map_or(0, |(_, n)| *n)
}

impl FakeRunner {
    fn requests(&self) -> Vec<Request> {
        self.state.lock().unwrap().requests.clone()
    }

    fn of(&self, op: &str) -> Vec<Request> {
        self.requests().into_iter().filter(|r| r.op == op).collect()
    }

    fn hold(&self, op: &str) {
        self.state.lock().unwrap().held.push(op.to_owned());
    }

    fn release(&self, op: &str) {
        self.state.lock().unwrap().held.retain(|o| o != op);
        self.changed.notify_all();
    }

    fn running(&self, op: &str) -> usize {
        count(&self.state.lock().unwrap().running, op)
    }

    fn most(&self, op: &str) -> usize {
        count(&self.state.lock().unwrap().most, op)
    }
}

impl Runner for FakeRunner {
    fn ops(&self) -> Vec<OpInfo> {
        Vec::new()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let mut state = self.state.lock().unwrap();
        state.requests.push(req.clone());
        let now = bump(&mut state.running, &req.op, 1);
        if now > count(&state.most, &req.op) {
            bump(&mut state.most, &req.op, 1);
        }
        self.changed.notify_all();
        let mut state = self.changed.wait_while(state, |s| s.held.contains(&req.op)).unwrap();
        drop(state);
        if let Some(micros) = req.args.get("micros").and_then(serde_json::Value::as_u64) {
            std::thread::sleep(Duration::from_micros(micros));
        }
        state = self.state.lock().unwrap();
        bump(&mut state.running, &req.op, -1);
        drop(state);
        self.changed.notify_all();
        if req.op == "test.waiting" {
            return Err(OpError { code: ErrorCode::Waiting, ..OpError::conflict("waiting for a person") });
        }
        Ok(Outcome::new(Plan::answer(json!({}))))
    }
    fn begin(&self, _: &str, _: &Actor) -> TxId {
        TxId(0)
    }
    fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
        Err(OpError::unsupported("no events"))
    }
    fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

// ── Fake sessions ──────────────────────────────────────────────────────────────────────────────────────────────

/// Opens sessions and records what was asked of them; an agent `tester` has a session of its own with `read` and
/// `perform`.
#[derive(Default)]
struct FakeSessions {
    log: Mutex<Vec<String>>,
    opened: Mutex<Vec<SessionSpec>>,
}

fn person() -> Actor {
    Actor::Person { name: "Streamer".to_owned() }
}

fn tester() -> Actor {
    Actor::Agent { id: "tester".to_owned(), model: None }
}

fn info(id: u64, actor: Actor, spec: &SessionSpec) -> SessionInfo {
    SessionInfo {
        id: SessionId(id),
        actor,
        on_behalf_of: spec.on_behalf_of.clone(),
        transport: spec.transport.clone(),
        client: spec.client.clone(),
        model: None,
        parent: None,
        role: None,
        grants: spec.grants.clone(),
        fence: Vec::new(),
        budget: spec.budget,
        spent: Spend::default(),
        limits: spec.limits,
        state: SessionState::Active,
        open_tx: None,
        pending: Vec::new(),
        steps: 0,
        started_at: Seconds(0.0),
        last_active_at: Seconds(0.0),
    }
}

impl FakeSessions {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn opened(&self) -> Vec<SessionSpec> {
        self.opened.lock().unwrap().clone()
    }
}

fn unused<T>() -> Result<T, OpError> {
    Err(OpError::unsupported("not in this fake"))
}

impl Sessions for FakeSessions {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        self.opened.lock().unwrap().push(spec.clone());
        self.log.lock().unwrap().push(format!("open {}", spec.client));
        Ok(info(7, Actor::Agent { id: format!("{}#7", spec.client), model: None }, &spec))
    }
    fn sessions(&self) -> Vec<SessionInfo> {
        Vec::new()
    }
    fn session(&self, _: SessionId) -> Option<SessionInfo> {
        None
    }
    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        (actor == &tester()).then(|| {
            let spec = SessionSpec {
                transport: omuzaki_ops_api::Transport::Stdio,
                client: "tester".to_owned(),
                model: None,
                on_behalf_of: person(),
                grants: Grants::of([Scope::Read, Scope::Perform]),
                budget: Budget::default(),
                limits: Limits::default(),
                parent: None,
                role: None,
                fence: Vec::new(),
            };
            info(3, tester(), &spec)
        })
    }
    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        self.log.lock().unwrap().push(format!("pause {} by {by:?}", id.0));
        unused()
    }
    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        self.log.lock().unwrap().push(format!("resume {} by {by:?}", id.0));
        unused()
    }
    fn set_grants(&self, _: SessionId, _: Grants, _: &Actor) -> Result<SessionInfo, OpError> {
        unused()
    }
    fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
        unused()
    }
    fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
        unused()
    }
    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        self.log.lock().unwrap().push(format!("end {} {reason:?}", id.0));
        unused()
    }
    fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
        unused()
    }
    fn approvals(&self) -> Vec<PendingApproval> {
        Vec::new()
    }
    fn resolve(&self, _: ApprovalId, _: Decision, _: &Actor) -> Result<Resolution, OpError> {
        unused()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

struct Rig {
    automation: Automation,
    runner: Arc<FakeRunner>,
    sessions: Arc<FakeSessions>,
    clock: Arc<FakeClock>,
    _keep: (Arc<dyn Runner>, Arc<dyn Sessions>),
}

/// A show whose graph `<op>` calls `<op>` on each `custom.<op with dots as underscores>`, started by a person.
fn rig(ops: &[&str]) -> Rig {
    let runner = Arc::new(FakeRunner::default());
    let sessions = Arc::new(FakeSessions::default());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let automation = Automation::with_clocks(clock.clone(), Arc::new(SystemWallClock));
    let as_runner: Arc<dyn Runner> = runner.clone();
    let as_sessions: Arc<dyn Sessions> = sessions.clone();
    automation.attach(&as_runner);
    automation.attach_sessions(&as_sessions);
    let def = ops.iter().fold(ShowDef::new("Test").with_seed(7), |def, op| {
        let graph = GraphDef::from_node(&json!({ "nodes": {
            "ask": { "kind": "test.ask", "params": { "kind": kind(op), "op": op } },
        }}))
        .unwrap();
        def.with_graph(GraphId::new(op.replace('.', "-")).unwrap(), graph)
    });
    automation.start_def_by(&ShowId::new("main").unwrap(), &def, &person()).unwrap();
    Rig { automation, runner, sessions, clock, _keep: (as_runner, as_sessions) }
}

fn kind(op: &str) -> String {
    format!("custom.{}", op.replace('.', "_"))
}

impl Rig {
    fn send(&self, op: &str, payload: serde_json::Value) {
        let event = omuzaki_live_api::event::LiveEvent::new(
            EventKind::new(kind(op)).unwrap(),
            omuzaki_live_api::event::Source::Test,
            Seconds(0.0),
            payload,
        );
        self.automation.publish(event).unwrap();
    }

    fn reports(&self) -> Vec<serde_json::Value> {
        self.runner.of("test.report").into_iter().map(|r| r.args).collect()
    }
}

/// Waits for `cond`, checking every millisecond, for at most ten seconds.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn calls_run_in_the_shows_own_session_with_the_rule_as_their_cause() {
    let rig = rig(&["test.op"]);
    rig.send("test.op", json!({ "n": 1 }));
    until("the call", || rig.runner.of("test.op").len() == 1);
    let opened = rig.sessions.opened();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].client, "automation:main");
    assert_eq!(opened[0].transport, omuzaki_ops_api::Transport::Automation);
    assert_eq!(opened[0].on_behalf_of, person());
    assert_eq!(opened[0].limits.edits_per_minute, 1_200);
    let request = &rig.runner.of("test.op")[0];
    assert_eq!(request.actor, Actor::Agent { id: "automation:main#7".to_owned(), model: None });
    let cause = request.cause.as_ref().unwrap();
    assert_eq!(cause.kind, CauseKind::Rule);
    let text = cause.text.as_deref().unwrap();
    assert!(text.starts_with("main/test-op/ask on custom.test_op #"), "{text}");
    assert_eq!((request.tx, request.base), (None, None));
    until("the session id in the status", || {
        rig.automation.status().running.is_some_and(|r| r.session == SessionId(7))
    });
}

#[test]
fn a_test_event_an_agent_fires_acts_as_that_agent_with_its_own_grants() {
    let rig = rig(&["test.op"]);
    rig.automation.fire(EventKind::new(kind("test.op")).unwrap(), json!({}), &tester()).unwrap();
    until("the call", || rig.runner.of("test.op").len() == 1);
    let request = &rig.runner.of("test.op")[0];
    assert_eq!(request.actor, tester());
    assert_eq!(request.grants, Grants::of([Scope::Read, Scope::Perform]));
}

#[test]
fn the_ordered_lane_keeps_request_order_under_random_latencies() {
    let rig = rig(&["test.slow"]);
    let mut seed = 0x2545_f491_u64;
    for chunk in 0..20 {
        for i in 0..50 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            rig.send("test.slow", json!({ "n": chunk * 50 + i, "micros": seed % 300 }));
        }
        // At most 50 calls are ever waiting, under the engine's 64 in flight.
        until("the chunk", || rig.runner.of("test.slow").len() == (chunk + 1) * 50);
    }
    let order: Vec<u64> = rig.runner.of("test.slow").iter().map(|r| r.args["n"].as_u64().unwrap()).collect();
    assert_eq!(order, (0..1_000).collect::<Vec<_>>());
    assert_eq!(rig.runner.most("test.slow"), 1, "the ordered lane runs one call at a time");
}

#[test]
fn four_jobs_run_at_once_beside_the_ordered_lane() {
    let rig = rig(&["test.job", "test.op"]);
    rig.runner.hold("test.job");
    for n in 0..6 {
        rig.send("test.job", json!({ "n": n }));
    }
    until("four jobs running", || rig.runner.running("test.job") == 4);
    // The ordered lane isn't held by the jobs.
    rig.send("test.op", json!({}));
    until("the ordered call", || rig.runner.of("test.op").len() == 1);
    assert_eq!(rig.runner.running("test.job"), 4);
    rig.runner.release("test.job");
    until("every job", || rig.runner.of("test.job").len() == 6);
    assert_eq!(rig.runner.most("test.job"), 4);
}

#[test]
fn the_ninth_network_call_in_flight_fails_limited_at_once() {
    let rig = rig(&["net.http"]);
    rig.runner.hold("net.http");
    for n in 0..9 {
        rig.send("net.http", json!({ "n": n }));
    }
    until("the refusal", || rig.reports().len() == 1);
    assert_eq!(rig.reports()[0], json!({ "op": "net.http", "code": "limited" }));
    assert_eq!(rig.runner.running("net.http"), 8);
    rig.runner.release("net.http");
    until("the eight", || rig.runner.running("net.http") == 0);
    assert_eq!(rig.runner.of("net.http").len(), 8, "nothing is retried");
}

#[test]
fn programs_are_limited_to_four_running_and_thirty_started_a_minute() {
    let rig = rig(&["exec.run"]);
    rig.runner.hold("exec.run");
    for n in 0..5 {
        rig.send("exec.run", json!({ "n": n }));
    }
    until("the fifth refused", || rig.reports().len() == 1);
    assert_eq!(rig.runner.running("exec.run"), 4);
    rig.runner.release("exec.run");
    until("the four done", || rig.runner.running("exec.run") == 0);
    // 26 more make 30 started in this minute; the 31st is refused until a minute of runner time has passed.
    for n in 0..27 {
        rig.send("exec.run", json!({ "n": 100 + n }));
        until("the program", || {
            rig.runner.of("exec.run").len() + rig.reports().len() == 6 + usize::try_from(n).unwrap()
        });
    }
    assert_eq!(rig.runner.of("exec.run").len(), 30);
    assert_eq!(rig.reports().len(), 2);
    rig.clock.advance(Seconds(61.0));
    rig.send("exec.run", json!({ "n": 200 }));
    until("allowed again", || rig.runner.of("exec.run").len() == 31);
}

#[test]
fn a_waiting_reply_fails_the_node_and_nothing_is_retried() {
    let rig = rig(&["test.waiting"]);
    rig.send("test.waiting", json!({}));
    until("the failure", || rig.reports().len() == 1);
    assert_eq!(rig.reports()[0], json!({ "op": "test.waiting", "code": "waiting" }));
    assert_eq!(rig.runner.of("test.waiting").len(), 1);
}

#[test]
fn pause_and_resume_map_to_the_session_and_stop_ends_it_within_two_seconds() {
    let rig = rig(&["test.op"]);
    rig.send("test.op", json!({}));
    until("the call", || rig.runner.of("test.op").len() == 1);
    rig.automation.pause(&person()).unwrap();
    rig.automation.resume(&person()).unwrap();
    until("the session paused and resumed", || rig.sessions.log().len() == 3);
    assert_eq!(rig.sessions.log()[1], format!("pause 7 by {:?}", person()));
    assert_eq!(rig.sessions.log()[2], format!("resume 7 by {:?}", person()));
    // A call still running holds the end back, for at most two seconds.
    rig.runner.hold("test.op");
    rig.send("test.op", json!({}));
    until("the held call", || rig.runner.running("test.op") == 1);
    let stopped = Instant::now();
    rig.automation.stop(&person()).unwrap();
    assert!(stopped.elapsed() < Duration::from_millis(500), "stop never waits on the executor");
    until("the end", || rig.sessions.log().len() == 4);
    let waited = stopped.elapsed();
    assert!(waited >= Duration::from_millis(1_900) && waited < Duration::from_secs(4), "{waited:?}");
    assert_eq!(rig.sessions.log()[3], "end 7 Closed");
    rig.runner.release("test.op");
}

#[test]
fn a_start_with_nobody_behind_it_runs_in_omuzakis_own_name() {
    let runner = Arc::new(FakeRunner::default());
    let as_runner: Arc<dyn Runner> = runner.clone();
    let automation = Automation::new();
    automation.attach(&as_runner);
    let graph = GraphDef::from_node(&json!({ "nodes": {
        "ask": { "kind": "test.ask", "params": { "kind": kind("test.op"), "op": "test.op" } },
    }}))
    .unwrap();
    let def = ShowDef::new("Test").with_graph(GraphId::new("g").unwrap(), graph);
    automation.start_def(&ShowId::new("main").unwrap(), &def).unwrap();
    let event = omuzaki_live_api::event::LiveEvent::new(
        EventKind::new(kind("test.op")).unwrap(),
        omuzaki_live_api::event::Source::Test,
        Seconds(0.0),
        json!({}),
    );
    automation.publish(event).unwrap();
    until("the call", || runner.of("test.op").len() == 1);
    assert_eq!(runner.of("test.op")[0].actor, Actor::System);
    automation.stop(&Actor::System).unwrap();
}
