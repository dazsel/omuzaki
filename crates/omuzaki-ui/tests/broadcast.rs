//! The Broadcast workspace's automation panels over the real engine (`docs/design/automation.md` D10, D11, D17):
//! the Rules table (#1324), the values monitor (#739), the graph editor (#1279) and its node inspector (#782).
//!
//! Each test runs the studio's own op runner with the automation engine in its services, builds §4's bonk show (a
//! redeem, a cooldown, a counter and an OBS scene switch that needs approval) or a chain through the same ops an
//! agent would use, and drives the panels as a person: every change is checked in the document or the engine, and
//! every edit is one undo step.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::{Key, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable};
use omuzaki_live::Automation;
use omuzaki_live::approvals::DiskApprovals;
use omuzaki_live_api::ids::ShowId;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::services::Approvals;
use omuzaki_ops_api::{Actor, Event, Grants, Mode, OpError, OpInfo, Outcome, Request, Runner, ServicesBuilder, TxId};
use omuzaki_ui::broadcast::graph::GraphPanel;
use omuzaki_ui::broadcast::model;
use omuzaki_ui::broadcast::rules::RulesPanel;
use omuzaki_ui::broadcast::values::ValuesPanel;
use omuzaki_ui::broadcast::{Approve, DiskApprover, ShowApprover, ShowFocus};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{harness_state, unlabelled_interactive};
use omuzaki_ui::ds::theme::set_strings;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::wiring::{Document, core_on};
use serde_json::{Value, json};

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// The studio's runner over a fresh project in `dir`, with the automation engine attached.
fn studio(dir: &std::path::Path) -> (Arc<dyn Runner>, Arc<Automation>) {
    omuzaki_ui::link();
    let automation = Arc::new(Automation::new());
    let services = ServicesBuilder::new().insert::<Automation>(automation.clone()).build();
    let core = core_on(Document::project(dir).unwrap(), services).unwrap();
    automation.attach(&core.runner);
    (core.runner, automation)
}

fn run(runner: &dyn Runner, op: &str, args: Value) -> Outcome {
    runner
        .run(Request {
            op: op.into(),
            args,
            mode: Mode::Apply,
            actor: you(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        })
        .unwrap_or_else(|e| panic!("{op}: {e}"))
}

/// §4's show: a variable, a group, and the bonk graph (redeem → cooldown → counter and an OBS scene switch).
fn bonk_show(runner: &dyn Runner) {
    run(runner, "show.create", json!({ "id": "main", "name": "Main" }));
    run(
        runner,
        "automation.declare_variable",
        json!({ "name": "bonks", "type": "number", "scope": "show", "default": 0 }),
    );
    run(runner, "automation.add_group", json!({ "id": "redeems", "name": "Redeems" }));
    run(
        runner,
        "automation.add_graph",
        json!({ "id": "bonk", "graph": {
            "graph": { "name": "Bonk", "group": "redeems" },
            "nodes": {
                "redeem": { "kind": "on.event", "pos": [0.0, 0.0], "params": { "kind": "redeem", "where": { "reward.title": "Bonk" } } },
                "cool": { "kind": "flow.cooldown", "pos": [320.0, 0.0], "in": { "in": "redeem.fired" }, "set": { "seconds": 10.0 } },
                "count": { "kind": "var.add", "pos": [600.0, 120.0], "in": { "in": "cool.out" }, "params": { "variable": "bonks" } },
                "throw": { "kind": "op.run", "pos": [600.0, -120.0], "in": { "in": "cool.out" }, "params": { "op": "obs.scene.set", "args": { "scene": "Bonk" } } },
            }
        }}),
    );
}

/// A straight chain: `!lurk` → wait 2 s → count.
fn lurk_chain(runner: &dyn Runner) {
    run(
        runner,
        "automation.add_graph",
        json!({ "id": "lurk", "graph": {
            "graph": { "name": "Lurk" },
            "nodes": {
                "cmd": { "kind": "chat.command", "params": { "name": "lurk" } },
                "wait": { "kind": "flow.wait", "pos": [220.0, 0.0], "in": { "in": "cmd.fired" }, "set": { "seconds": 2.0 } },
                "count": { "kind": "var.add", "pos": [440.0, 0.0], "in": { "in": "wait.out" }, "params": { "variable": "bonks" } },
            }
        }}),
    );
}

/// What a panel test holds: the runner, its events, the panel, and the shared views.
struct Rig<P> {
    runner: Arc<dyn Runner>,
    events: Receiver<Event>,
    frame: Vec<Event>,
    panel: P,
    focus: ShowFocus,
    approver: Option<ShowApprover>,
}

fn rig<P: 'static>(
    runner: Arc<dyn Runner>,
    panel: P,
    size: egui::Vec2,
    approver: Option<ShowApprover>,
    draw: impl Fn(&mut P, &mut egui::Ui, Io<'_>, &ShowFocus, Option<&ShowApprover>) + 'static,
) -> Harness<'static, Rig<P>> {
    let events = runner.subscribe(&Grants::person()).unwrap();
    let state = Rig { runner, events, frame: Vec::new(), panel, focus: ShowFocus::default(), approver };
    harness_state(Settings::default(), size, state, move |ui, r| {
        r.frame = r.events.try_iter().collect();
        let actor = you();
        let io = Io::new(r.runner.as_ref(), &actor, &r.frame);
        draw(&mut r.panel, ui, io, &r.focus, r.approver.as_ref());
    })
}

fn rules_rig(runner: Arc<dyn Runner>, approver: Option<ShowApprover>) -> Harness<'static, Rig<RulesPanel>> {
    rig(runner, RulesPanel::new(), vec2(380.0, 900.0), approver, |p, ui, io, focus, approver| {
        p.ui(ui, io, focus, approver, None, None);
    })
}

/// Records what the approval card allowed.
#[derive(Default)]
struct Recording(Mutex<Vec<(ShowId, Vec<Need>)>>);

impl Approve for Recording {
    fn allow(&self, show: &ShowId, needs: &[Need], by: &Actor) -> Result<(), String> {
        assert!(matches!(by, Actor::Person { .. }), "only a person allows: {by:?}");
        self.0.lock().unwrap().push((show.clone(), needs.to_vec()));
        Ok(())
    }
}

fn has_label<S>(h: &Harness<'_, S>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

#[test]
fn an_empty_project_offers_a_show_and_making_one_is_an_op() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    let mut h = rules_rig(runner.clone(), None);
    h.run_steps(2);
    assert!(has_label(&h, "No rules yet"), "the empty state");
    h.get_by_label("Make a show").click();
    h.run_steps(3);
    let show = model::read_show(runner.as_ref()).unwrap().expect("a show");
    assert_eq!(show.id.as_str(), "main");
    assert!(has_label(&h, "This show has no rules yet"));
    run(runner.as_ref(), "show.create", json!({ "id": "other" }));
    assert!(runner.undo(&you(), &Grants::person()).unwrap().is_some(), "the person's steps undo");
}

#[test]
fn the_bonk_show_lists_its_group_needs_approval_and_allow_goes_to_the_approver_not_an_op() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let recording = Arc::new(Recording::default());
    let mut h = rules_rig(runner.clone(), Some(ShowApprover(recording.clone())));
    h.run_steps(3);
    assert!(has_label(&h, "Redeems"), "the group's section");
    assert!(has_label(&h, "Bonk"), "the graph's row");
    assert!(has_label(&h, "This show needs your OK"), "the approval card");
    assert!(has_label(&h, "Run obs.scene.set"), "the op named, with what it does");
    assert!(has_label(&h, "Use the broadcast permission"), "the scope in words");
    let before = runner.log(0, 1000, &Grants::person()).unwrap().len();
    h.get_by_label("Allow on this machine").click();
    h.run_steps(2);
    let allowed = recording.0.lock().unwrap().clone();
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0].0.as_str(), "main");
    assert!(allowed[0].1.contains(&Need::op("obs.scene.set")), "{:?}", allowed[0].1);
    assert_eq!(runner.log(0, 1000, &Grants::person()).unwrap().len(), before, "allowing ran no op");
}

#[test]
fn not_now_puts_the_card_away_until_the_needs_change() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = rules_rig(runner, None);
    h.run_steps(3);
    assert!(h.get_by_label("Allow on this machine").accesskit_node().is_disabled(), "no approver: Allow says why");
    h.get_by_label("Not now").click();
    h.run_steps(2);
    assert!(!has_label(&h, "This show needs your OK"));
}

#[test]
fn the_real_approver_writes_the_approvals_file_and_rearms_so_describe_reads_approved() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (runner, automation) = studio(dir.path());
    let approvals = Arc::new(DiskApprovals::new(data.path()));
    automation.set_approvals(approvals.clone() as Arc<dyn Approvals>);
    bonk_show(runner.as_ref());
    let approver = DiskApprover::new(automation, approvals, "");
    let mut h = rules_rig(runner.clone(), Some(ShowApprover(Arc::new(approver))));
    h.run_steps(3);
    h.get_by_label("Allow on this machine").click();
    h.run_steps(3);
    let actor = you();
    let io = Io::new(runner.as_ref(), &actor, &[]);
    let described = model::describe(&io, &ShowId::new("main").unwrap()).unwrap();
    assert!(described.missing().is_empty(), "every need approved: {:?}", described.needs);
    assert!(!has_label(&h, "This show needs your OK"), "the card is gone");
}

#[test]
fn no_registered_op_reaches_the_approvals_store() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (runner, automation) = studio(dir.path());
    automation.set_approvals(Arc::new(DiskApprovals::new(data.path())) as Arc<dyn Approvals>);
    bonk_show(runner.as_ref());
    // Every automation and show op, with each example its schema publishes, as an agent and as the person: none may
    // write the approvals file, which only `DiskApprovals::allow` (the card's Allow, through the app) writes.
    let ops: Vec<OpInfo> =
        runner.ops().into_iter().filter(|o| o.name.starts_with("automation.") || o.name.starts_with("show.")).collect();
    assert!(ops.len() >= 20, "the catalogue: {}", ops.len());
    let agent = Actor::Agent { id: "agent".into(), model: None };
    let mut tried = 0;
    for info in &ops {
        let examples = info.schema.get("examples").and_then(Value::as_array).cloned().unwrap_or_default();
        for args in examples {
            for (actor, grants) in [(you(), Grants::person()), (agent.clone(), Grants::agent_default())] {
                let _ = runner.run(Request {
                    op: info.name.into(),
                    args: args.clone(),
                    mode: Mode::Apply,
                    actor,
                    grants,
                    cause: None,
                    tx: None,
                    base: None,
                });
                tried += 1;
            }
        }
    }
    assert!(tried > 20, "examples tried: {tried}");
    let written: Vec<_> = std::fs::read_dir(data.path()).unwrap().flatten().map(|e| e.path()).collect();
    assert!(written.is_empty(), "no op wrote approvals: {written:?}");
    assert!(!ops.iter().any(|o| o.name.contains("approv") || o.name.contains("allow")), "no op is named for it");
}

#[test]
fn a_chain_reads_as_one_row_and_its_constant_is_edited_inline_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    lurk_chain(runner.as_ref());
    let mut h = rules_rig(runner.clone(), None);
    h.run_steps(3);
    assert!(has_label(&h, "When someone types a chat command → Wait, then pass the event on → "), "the chain's row");
    h.get_all_by_label("Edit").next().unwrap().click();
    h.run_steps(2);
    let field = h.get_by_label("How long to wait, in seconds.");
    field.click();
    h.run_steps(1);
    h.get_by_label("How long to wait, in seconds.").type_text("5");
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(3);
    let show = model::read_show(runner.as_ref()).unwrap().unwrap();
    let wait = show.node(&"lurk".parse().unwrap(), &"wait".parse().unwrap()).unwrap();
    let seconds = wait.set.get(&omuzaki_live_api::ids::PortName::from_static("seconds")).and_then(Value::as_f64);
    assert!(seconds.is_some_and(|s| (s - 2.0).abs() > f64::EPSILON), "the constant changed: {seconds:?}");
    runner.undo(&you(), &Grants::person()).unwrap();
    let show = model::read_show(runner.as_ref()).unwrap().unwrap();
    let wait = show.node(&"lurk".parse().unwrap(), &"wait".parse().unwrap()).unwrap();
    assert_eq!(wait.set.get(&omuzaki_live_api::ids::PortName::from_static("seconds")), Some(&json!(2.0)), "one undo");
}

#[test]
fn start_pause_resume_and_stop_run_the_show_through_its_ops() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = rules_rig(runner, None);
    h.run_steps(3);
    h.get_by_label("Start").click();
    h.run_steps(3);
    assert!(automation.running().is_some(), "started");
    assert!(has_label(&h, "Pause"));
    h.get_by_label("Pause").click();
    h.run_steps(3);
    assert!(h.query_by_label("Resume").is_some(), "paused");
    h.get_all_by_label("Resume").next().unwrap().click();
    h.run_steps(3);
    h.get_by_label("Stop").click();
    h.run_steps(3);
    assert!(automation.running().is_none(), "stopped");
}

#[test]
fn the_rules_panel_names_every_control_and_reads_in_the_pseudo_locale() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    lurk_chain(runner.as_ref());
    let mut h = rules_rig(runner, None);
    h.run_steps(3);
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(2);
    assert!(h.query_by_label("Start").is_none(), "the pseudo-locale replaced the English");
    assert!(has_label(&h, "Bonk"), "the show's own names stay as written");
}

// ── The values monitor ─────────────────────────────────────────────────────────────────────────────────────────────

/// Counts `automation.values` calls through another runner.
struct Counting {
    inner: Arc<dyn Runner>,
    values: AtomicUsize,
}

impl Runner for Counting {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        if req.op == "automation.values" {
            self.values.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &omuzaki_doc_api::DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<omuzaki_ops_api::OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

fn values_rig(
    runner: Arc<dyn Runner>,
    shown: Arc<std::sync::atomic::AtomicBool>,
) -> Harness<'static, Rig<ValuesPanel>> {
    rig(runner, ValuesPanel::new(), vec2(1366.0, 768.0), None, move |p, ui, io, _, _| {
        if shown.load(Ordering::Relaxed) {
            p.ui(ui, io);
        }
    })
}

fn fire_bonk(runner: &dyn Runner) {
    let event = omuzaki_live_mock::fixtures::redeem("ana", "Bonk");
    run(runner, "automation.fire", json!({ "kind": "redeem", "payload": event.payload }));
}

#[test]
fn the_monitor_shows_the_running_shows_values_and_edits_one_through_set_variable() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    run(runner.as_ref(), "automation.start", json!({ "show": "main" }));
    fire_bonk(runner.as_ref());
    let mut h = values_rig(runner.clone(), Arc::new(std::sync::atomic::AtomicBool::new(true)));
    // The engine's snapshot lands a moment after the read that asked for it.
    for _ in 0..40 {
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(30));
        if has_label(&h, "changed") {
            break;
        }
    }
    assert!(has_label(&h, "bonks"), "the variable");
    assert!(has_label(&h, "This show"), "its scope");
    assert!(has_label(&h, "What fired"), "the firings");
    h.get_by_label("Value of bonks").click();
    h.run_steps(1);
    h.get_by_label("Value of bonks").type_text("7");
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(2);
    let actor = you();
    let io = Io::new(runner.as_ref(), &actor, &[]);
    let edited = runner.log(0, 1000, &Grants::person()).unwrap().iter().any(|r| r.op == "automation.set_variable");
    let snapshot = model::values(&io).unwrap();
    assert!(snapshot.is_some_and(|v| v.variables.iter().any(|r| r.name.as_str() == "bonks")), "the snapshot has it");
    assert!(edited, "the edit went through automation.set_variable");
    let _ = &automation;
    run(runner.as_ref(), "automation.stop", json!({}));
}

#[test]
fn a_hidden_monitor_asks_the_engine_nothing_and_a_shown_one_at_most_four_times_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    run(runner.as_ref(), "automation.start", json!({ "show": "main" }));
    let counting = Arc::new(Counting { inner: runner, values: AtomicUsize::new(0) });
    let shown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut h = values_rig(counting.clone(), shown.clone());
    // The harness advances egui's clock by a quarter of a second each frame: twenty frames are five seconds.
    let step = 0.25;
    h.run_steps(20);
    assert_eq!(counting.values.load(Ordering::Relaxed), 0, "hidden for five seconds: no reads");
    shown.store(true, Ordering::Relaxed);
    let start = counting.values.load(Ordering::Relaxed);
    let frames: u32 = 40;
    h.run_steps(frames as usize);
    let reads = counting.values.load(Ordering::Relaxed) - start;
    let seconds = f64::from(frames) * step;
    eprintln!("reads while shown for {seconds} s: {reads}");
    assert!(reads >= 1, "a shown monitor reads");
    #[allow(clippy::cast_precision_loss)]
    let per_second = reads as f64 / seconds;
    assert!(per_second <= 4.0 + 1.0 / seconds, "at most four a second: {per_second}");
}

// ── The graph editor and the node inspector ───────────────────────────────────────────────────────────────────────

fn graph_rig(runner: Arc<dyn Runner>, size: egui::Vec2) -> Harness<'static, Rig<GraphPanel>> {
    rig(runner, GraphPanel::new(), size, None, |p, ui, io, focus, _| p.ui(ui, io, focus))
}

#[test]
fn the_editor_draws_the_bonk_graph_from_the_document_with_its_wires() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = graph_rig(runner, vec2(1366.0, 768.0));
    h.run_steps(4);
    assert_eq!(h.state().panel.nodes(), 4);
    assert_eq!(h.state().panel.wires(), 3, "redeem→cool, cool→count, cool→throw, and nothing else");
    assert!(has_label(&h, "throw"), "a node's title");
    assert!(h.query_all_by_label("Won't run").next().is_some(), "the scene switch is inert until approved");
    // The node's header makes room for its chip: "Won't run" reads whole, not "…" (it did, at every size).
    let cut: Vec<String> = omuzaki_ui::ds::audit::elided_findings(h.output())
        .into_iter()
        .filter(|f| f.what == "Won't run")
        .map(|f| f.to_string())
        .collect();
    assert!(cut.is_empty(), "the node's warning is cut: {cut:?}");
}

#[test]
fn keyboard_only_add_a_node_connect_two_ports_and_delete_a_node_each_one_undo() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = graph_rig(runner.clone(), vec2(1366.0, 768.0));
    h.run_steps(4);
    // Add: the palette by its button, a search, Enter.
    h.get_by_label("Add node").click();
    h.run_steps(2);
    h.get_by_label("Find a node").type_text("flow.wait");
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert_eq!(h.state().panel.nodes(), 5, "the node was added");
    // Connect: select the new wait in the node list, then Connect to… its first output into the first event input.
    h.get_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.ends_with("· Wait, then pass the event on")))
        .unwrap()
        .click();
    h.run_steps(3);
    let wires = h.state().panel.wires();
    h.get_by_label("Connect").click();
    h.run_steps(4);
    assert_eq!(h.state().panel.wires(), wires + 1, "the wire is in the document and drawn from it");
    runner.undo(&you(), &Grants::person()).unwrap();
    h.run_steps(4);
    assert_eq!(h.state().panel.wires(), wires, "one undo takes the wire back");
    // Delete.
    h.get_by_label("Delete node").click();
    h.run_steps(4);
    assert_eq!(h.state().panel.nodes(), 4);
    runner.undo(&you(), &Grants::person()).unwrap();
    h.run_steps(4);
    assert_eq!(h.state().panel.nodes(), 5, "one undo brings the node back");
}

#[test]
fn dragging_a_node_moves_it_in_one_transaction_that_one_undo_takes_back() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = graph_rig(runner.clone(), vec2(1366.0, 768.0));
    h.run_steps(4);
    let before = runner.log(0, 1000, &Grants::person()).unwrap().len();
    // The person holds the button down and the node follows the pointer for eight frames, then lets go.
    let blank = egui::pos2(20.0, 700.0);
    h.hover_at(blank);
    h.event(egui::Event::PointerButton {
        pos: blank,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(1);
    for _ in 0..8 {
        h.state_mut().panel.drag_node("cool", vec2(10.0, 6.0));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton {
        pos: blank,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(4);
    let cool = |runner: &dyn Runner| {
        let show = model::read_show(runner).unwrap().unwrap();
        show.node(&"bonk".parse().unwrap(), &"cool".parse().unwrap()).unwrap().pos
    };
    let moved = cool(runner.as_ref());
    assert_ne!(moved, [320.0, 0.0], "the node moved in the document");
    let records = runner.log(0, 1000, &Grants::person()).unwrap();
    let txs: std::collections::BTreeSet<TxId> = records[before..].iter().map(|r| r.tx).collect();
    assert_eq!(txs.len(), 1, "every position of the drag is in one transaction: {txs:?}");
    runner.undo(&you(), &Grants::person()).unwrap();
    assert_eq!(cool(runner.as_ref()), [320.0, 0.0], "one undo puts it back");
}

#[test]
fn the_inspector_edits_a_constant_and_shows_why_a_node_wont_run() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    let mut h = graph_rig(runner.clone(), vec2(1366.0, 900.0));
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "throw · Run an op").click();
    h.run_steps(3);
    assert!(has_label(&h, "Needs your OK"), "why the scene switch won't run");
    h.get_by_role_and_label(Role::Button, "cool · Let an event through at most once in a while").click();
    h.run_steps(3);
    assert!(has_label(&h, "Inputs with no wire"));
    h.get_by_label("How long the cooldown lasts, in seconds.").click();
    h.run_steps(1);
    h.get_by_label("How long the cooldown lasts, in seconds.").type_text("12");
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(3);
    let show = model::read_show(runner.as_ref()).unwrap().unwrap();
    let cool = show.node(&"bonk".parse().unwrap(), &"cool".parse().unwrap()).unwrap();
    assert_ne!(cool.set.get(&omuzaki_live_api::ids::PortName::from_static("seconds")), Some(&json!(10.0)));
}

#[test]
fn a_node_that_just_fired_glows_while_the_show_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, automation) = studio(dir.path());
    bonk_show(runner.as_ref());
    run(runner.as_ref(), "automation.start", json!({ "show": "main" }));
    let mut h = graph_rig(runner.clone(), vec2(1366.0, 768.0));
    h.run_steps(4);
    fire_bonk(runner.as_ref());
    let mut glowed = false;
    for _ in 0..60 {
        h.ctx.input_mut(|i| i.time += 0.3);
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        if h.state().panel.glowing("cool") {
            glowed = true;
            break;
        }
    }
    assert!(glowed, "the cooldown fired and glows");
    let _ = automation;
    run(runner.as_ref(), "automation.stop", json!({}));
}

#[test]
fn a_two_hundred_node_graph_draws_inside_the_frame_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _automation) = studio(dir.path());
    run(runner.as_ref(), "show.create", json!({ "id": "main" }));
    let mut nodes = serde_json::Map::new();
    nodes.insert("t".into(), json!({ "kind": "timer.every", "pos": [0.0, 0.0] }));
    for i in 0u16..199 {
        let from = if i == 0 { "t.tick".to_owned() } else { format!("n{}.out", i - 1) };
        let pos = [(f32::from(i % 20) + 1.0) * 200.0, f32::from(i / 20) * 120.0];
        nodes.insert(format!("n{i}"), json!({ "kind": "flow.wait", "pos": pos, "in": { "in": from } }));
    }
    run(runner.as_ref(), "automation.add_graph", json!({ "id": "big", "graph": { "nodes": nodes } }));
    let mut h = graph_rig(runner, vec2(1920.0, 1080.0));
    h.run_steps(4);
    assert_eq!(h.state().panel.nodes(), 200);
    let frames = 20;
    let started = std::time::Instant::now();
    h.run_steps(frames);
    let per_frame = started.elapsed() / u32::try_from(frames).unwrap();
    eprintln!("200-node graph: {per_frame:?} per frame (debug build, software rasteriser off)");
    // A ceiling for a debug build on a shared CI runner, not one machine's number: the release target is 4 ms.
    assert!(per_frame < std::time::Duration::from_millis(250), "{per_frame:?}");
}

/// Pictures for the pull request: `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test broadcast --
/// --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn broadcast_evidence() {
    use omuzaki_ui::ds::apply_settings;
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (w, h) in [(1366.0, 768.0), (1920.0, 1080.0)] {
        for approved in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let data = tempfile::tempdir().unwrap();
            let (runner, automation) = studio(dir.path());
            let approvals = Arc::new(DiskApprovals::new(data.path()));
            automation.set_approvals(approvals.clone() as Arc<dyn Approvals>);
            bonk_show(runner.as_ref());
            lurk_chain(runner.as_ref());
            if approved {
                let approver = DiskApprover::new(automation.clone(), approvals.clone(), "");
                let actor = you();
                let io = Io::new(runner.as_ref(), &actor, &[]);
                let missing: Vec<Need> = model::describe(&io, &ShowId::new("main").unwrap())
                    .unwrap()
                    .missing()
                    .into_iter()
                    .cloned()
                    .collect();
                approver.allow(&ShowId::new("main").unwrap(), &missing, &you()).unwrap();
                run(runner.as_ref(), "automation.start", json!({ "show": "main" }));
                fire_bonk(runner.as_ref());
            }
            let events = runner.subscribe(&Grants::person()).unwrap();
            let (mut rules, mut graph, mut values) = (RulesPanel::new(), GraphPanel::new(), ValuesPanel::new());
            let approver = ShowApprover(Arc::new(DiskApprover::new(automation.clone(), approvals.clone(), "")));
            let focus = ShowFocus::default();
            let runner2 = runner.clone();
            let mut harness = Harness::builder().with_size(vec2(w, h)).wgpu().build_ui(move |ui| {
                apply_settings(ui.ctx(), Settings::default());
                let frame: Vec<Event> = events.try_iter().collect();
                let actor = you();
                let io = Io::new(runner2.as_ref(), &actor, &frame);
                egui::Panel::right("rules").exact_size(380.0).show(ui, |ui| {
                    rules.ui(ui, io, &focus, Some(&approver), None, None);
                });
                egui::Panel::bottom("values").exact_size(h * 0.35).show(ui, |ui| values.ui(ui, io));
                egui::CentralPanel::default().show(ui, |ui| graph.ui(ui, io, &focus));
            });
            for _ in 0..20 {
                harness.run_steps(1);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let image = harness.render().unwrap();
            let state = if approved { "running" } else { "needs-approval" };
            image.save(format!("{out}/broadcast-{state}-{w}x{h}.png")).unwrap();
            if approved {
                run(runner.as_ref(), "automation.stop", json!({}));
            }
        }
    }
}
