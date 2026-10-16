//! `script.run` through the real executor (#1220; `docs/design/scripting.md` §3, §4 and §6, ADR 0036 decision 10).
//!
//! An agent's run is a driver job: its inner calls are the agent's own, with the `Script` cause, in one transaction
//! that is one undo step, rolled back whole on an error or a cancel. Joined to the agent's open transaction it reverts
//! only its own edits, and never another actor's. It never queues an approval, never borrows a person's standing, and
//! never starts a job inside the job.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)] // a test file

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, CauseKind, Decision, ErrorCode, ErrorDetail, Event, Grants, JobState, Jobs, Limits, Mode, Op, OpCtx,
    OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, SessionSpec, Sessions, Transport, TxId, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A run's memory is limited through the allocator, which every binary that runs scripts installs.
#[global_allocator]
static ALLOCATOR: omuzaki_script::MemoryBudget = omuzaki_script::MemoryBudget::system();

// -- the ops the scripts call ------------------------------------------------------------------------------------------

/// Sets a character's look: the edit the scripts loop over.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetLook {
    character: String,
    look: String,
}

impl Op for SetLook {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.set",
            summary: "Set a character's look.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = look_path(&self.character);
        let before = ctx.doc.get(&at).cloned();
        Ok(Plan::edit(Patch::new().with(at, before, Some(json!(self.look)))))
    }
}
register_op!(SetLook);

/// Publishes: needs the `publish` scope, which the agent's session does not hold.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Publish {}

impl Op for Publish {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.publish",
            summary: "Publish the looks.",
            kind: OpKind::Edit,
            scope: Scope::Publish,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}
register_op!(Publish);

/// Needs a person to confirm it when an agent asks: an approval.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Confirm {}

impl Op for Confirm {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.confirm",
            summary: "Lock the looks.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("locking the looks is the person's call")?;
        Ok(Plan::default())
    }
}
register_op!(Confirm);

/// Something only a person may do, never on an agent's behalf.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Rename {}

impl Op for Rename {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.rename_project",
            summary: "Rename the project.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        match ctx.actor {
            Actor::Person { .. } => Ok(Plan::default()),
            _ => Err(OpError::person_only("the project's name is the person's to choose")),
        }
    }
}
register_op!(Rename);

/// A job: a script may not start one.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Render {}

impl Op for Render {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.render",
            summary: "Render the looks.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}
register_op!(Render);

// -- the rig ------------------------------------------------------------------------------------------------------------

const CHARACTERS: [&str; 6] = ["aki", "ren", "mira", "sora", "kai", "yui"];

fn look_path(character: &str) -> DocPath {
    DocPath::parse(&format!("characters/{character}/look")).unwrap()
}

fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

/// What the agent's session may do: the agent defaults and `script`, not `publish`.
fn scripting() -> Grants {
    Grants::agent_default().with(Scope::Script)
}

/// The real executor over an in-memory document of six characters, with the `script.*` ops linked in and one agent
/// session working for alice that holds `ceiling`.
struct Rig {
    exec: Executor,
    agent: Actor,
}

fn rig(ceiling: Grants) -> Rig {
    omuzaki_script::link();
    let characters: serde_json::Map<String, Value> =
        CHARACTERS.iter().map(|name| ((*name).to_owned(), json!({"look": "plain"}))).collect();
    let doc = MemDoc::from_value(json!({ "characters": characters }));
    let exec = Executor::new(Box::new(doc), Arc::new(FakeClock::new(Seconds(0.0)))).unwrap();
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "claude".into(),
        model: None,
        on_behalf_of: person(),
        grants: ceiling,
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    let agent = exec.open(spec).unwrap().actor;
    Rig { exec, agent }
}

impl Rig {
    /// `script.run` of `source` as the agent, with `params`, in the agent's transaction `tx` if given.
    fn script(&self, source: &str, params: Value, tx: Option<TxId>) -> Request {
        Request {
            op: "script.run".into(),
            args: json!({ "source": source, "params": params }),
            mode: Mode::Apply,
            actor: self.agent.clone(),
            grants: scripting(),
            cause: None,
            tx,
            base: None,
        }
    }

    /// `look.set` as `actor` (the agent's requests carry its session's grants).
    fn set_look(&self, actor: &Actor, character: &str, look: &str, tx: Option<TxId>) {
        let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { scripting() };
        let request = Request {
            op: "look.set".into(),
            args: json!({ "character": character, "look": look }),
            mode: Mode::Apply,
            actor: actor.clone(),
            grants,
            cause: None,
            tx,
            base: None,
        };
        self.exec.run(request).unwrap();
    }

    fn look(&self, character: &str) -> Value {
        self.exec.get(&look_path(character), &Grants::person()).unwrap().unwrap_or(Value::Null)
    }

    fn looks(&self, characters: &[&str]) -> Vec<Value> {
        characters.iter().map(|c| self.look(c)).collect()
    }

    /// Waits (up to 10 s) until `character`'s look is `look`: an inner edit applies at once, before the run ends.
    fn wait_for(&self, character: &str, look: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.look(character) != json!(look) {
            assert!(Instant::now() < deadline, "{character} never became {look}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn agent_undo(&self) -> Option<String> {
        self.exec.undo(&self.agent, &scripting()).unwrap()
    }

    fn agent_redo(&self) -> Option<String> {
        self.exec.redo(&self.agent, &scripting()).unwrap()
    }
}

/// The `OpApplied` events of `look.set` waiting on `rx`.
fn looks_applied(rx: &Receiver<Event>) -> usize {
    rx.try_iter().filter(|e| matches!(e, Event::OpApplied { record, .. } if record.op == "look.set")).count()
}

/// Sets every character in `params.characters` to `params.look`, failing on the one named `params.fail_at`.
const LOOP: &str = r#"
let done = 0;
for name in params.characters {
    if "fail_at" in params && name == params.fail_at {
        throw `no look fits ${name}`;
    }
    ops::look::set(#{ character: name, look: params.look });
    done += 1;
}
done
"#;

/// Sets aki's and ren's looks, then waits until someone changes kai's, then fails.
const SET_TWO_THEN_FAIL: &str = r#"
ops::look::set(#{ character: "aki", look: "script" });
ops::look::set(#{ character: "ren", look: "script" });
while get("characters/kai/look") == "plain" {}
throw "stopped on purpose";
"#;

// -- the cases ----------------------------------------------------------------------------------------------------------

#[test]
fn without_the_script_scope_an_agents_run_is_refused_and_nothing_changes() {
    let rig = rig(Grants::agent_default());
    let request = Request {
        grants: Grants::agent_default(),
        ..rig.script(LOOP, json!({"characters": ["aki"], "look": "x"}), None)
    };
    let error = rig.exec.run(request).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error}");
    assert!(
        matches!(error.detail.as_deref(), Some(ErrorDetail::MissingScope { scope: Scope::Script })),
        "{:?}",
        error.detail
    );
    assert_eq!(rig.look("aki"), json!("plain"));
    assert!(rig.exec.approvals().is_empty(), "no one can grant what the transport withholds, so nothing is queued");
}

#[test]
fn one_run_is_one_undo_step_of_the_agents_whose_inner_records_are_the_agents_script_calls() {
    let rig = rig(scripting());
    let rx = rig.exec.subscribe(&Grants::person()).unwrap();
    let names = ["aki", "ren", "mira", "sora", "kai"];
    let outcome = rig.exec.run(rig.script(LOOP, json!({"characters": names, "look": "oval"}), None)).unwrap();
    let value = outcome.plan.output.as_ref().map(|o| o["value"].clone());
    assert_eq!(value, Some(json!(5)), "the script's last value: {:?}", outcome.plan.output);
    assert_eq!(rig.looks(&names), vec![json!("oval"); 5]);
    assert_eq!(looks_applied(&rx), 5);

    let inner: Vec<_> =
        rig.exec.log(0, usize::MAX, &Grants::person()).unwrap().into_iter().filter(|r| r.op == "look.set").collect();
    assert_eq!(inner.len(), 5);
    for record in &inner {
        assert_eq!(record.actor, rig.agent, "the agent's own call, checked against its session");
        assert_eq!(record.on_behalf_of, Some(person()));
        assert_eq!(record.cause.as_ref().map(|c| c.kind), Some(CauseKind::Script), "{:?}", record.cause);
        assert_eq!(record.tx, inner[0].tx, "every call in the job's one transaction");
    }

    assert!(rig.agent_undo().is_some(), "one step");
    assert_eq!(rig.looks(&names), vec![json!("plain"); 5], "undo reverts all five edits");
    assert!(rig.exec.undo(&person(), &Grants::person()).unwrap().is_none(), "and it was the only step");
    assert!(rig.agent_redo().is_some());
    assert_eq!(rig.looks(&names), vec![json!("oval"); 5], "redo restores them");
}

#[test]
fn a_run_that_fails_on_the_fourth_character_rolls_everything_back_and_emits_no_edit() {
    let rig = rig(scripting());
    let rx = rig.exec.subscribe(&Grants::person()).unwrap();
    let names = ["aki", "ren", "mira", "sora", "kai"];
    let params = json!({"characters": names, "look": "oval", "fail_at": "sora"});
    let error = rig.exec.run(rig.script(LOOP, params, None)).unwrap_err();
    assert!(error.message.contains("no look fits sora"), "{error}");
    assert_eq!(rig.looks(&names), vec![json!("plain"); 5], "the three edits before it are rolled back");
    assert_eq!(looks_applied(&rx), 0, "and no one heard of them");
    assert!(rig.agent_undo().is_none(), "nothing in history");
}

#[test]
fn a_cancel_midway_rolls_the_run_back_and_the_job_ends_cancelled() {
    let rig = rig(scripting());
    let rx = rig.exec.subscribe(&Grants::person()).unwrap();
    let job = rig.exec.start(rig.script(SET_TWO_THEN_FAIL, json!({}), None)).unwrap();
    rig.wait_for("ren", "script");
    rig.exec.cancel(job.id, &rig.agent).unwrap();
    let ended = rig.exec.wait(job.id, Some(Duration::from_secs(30))).unwrap();
    assert!(matches!(ended.state, JobState::Cancelled { .. }), "{:?}", ended.state);
    assert_eq!(rig.looks(&["aki", "ren"]), vec![json!("plain"); 2]);
    assert_eq!(looks_applied(&rx), 0);
    assert!(rig.agent_undo().is_none());
}

#[test]
fn a_run_joined_to_the_agents_transaction_neither_begins_nor_commits_it() {
    let rig = rig(scripting());
    let rx = rig.exec.subscribe(&Grants::person()).unwrap();
    let tx = rig.exec.begin("restyle", &rig.agent);
    rig.set_look(&rig.agent, "yui", "agent", Some(tx));
    assert_eq!(looks_applied(&rx), 1, "the agent's own edit is heard at once, as any edit in a transaction");
    let names = ["aki", "ren"];
    rig.exec.run(rig.script(LOOP, json!({"characters": names, "look": "oval"}), Some(tx))).unwrap();
    assert_eq!(rig.looks(&["aki", "ren", "yui"]), vec![json!("oval"), json!("oval"), json!("agent")]);
    assert_eq!(looks_applied(&rx), 0, "the run's events wait for the agent's commit");

    rig.exec.commit(tx, &rig.agent).unwrap();
    assert_eq!(looks_applied(&rx), 2);
    assert_eq!(rig.agent_undo().as_deref(), Some("restyle"), "the run's edits joined the agent's step");
    assert_eq!(rig.looks(&["aki", "ren", "yui"]), vec![json!("plain"); 3]);
    assert!(rig.agent_undo().is_none(), "one step");
}

#[test]
fn a_failing_joined_run_reverts_only_its_own_edits_and_leaves_the_transaction_open() {
    let rig = rig(scripting());
    let tx = rig.exec.begin("restyle", &rig.agent);
    rig.set_look(&rig.agent, "yui", "agent", Some(tx));
    let params = json!({"characters": ["aki", "ren", "mira"], "look": "oval", "fail_at": "mira"});
    let error = rig.exec.run(rig.script(LOOP, params, Some(tx))).unwrap_err();
    assert!(error.message.contains("no look fits mira"), "{error}");
    assert_eq!(rig.looks(&["aki", "ren", "yui"]), vec![json!("plain"), json!("plain"), json!("agent")]);

    rig.exec.commit(tx, &rig.agent).unwrap();
    assert_eq!(rig.agent_undo().as_deref(), Some("restyle"), "the agent's own edit is still its step");
    assert_eq!(rig.look("yui"), json!("plain"));
}

#[test]
fn a_failing_joined_run_whose_edit_a_person_changed_reverts_nothing_and_says_so() {
    let rig = rig(scripting());
    let tx = rig.exec.begin("restyle", &rig.agent);
    let job = rig.exec.start(rig.script(SET_TWO_THEN_FAIL, json!({}), Some(tx))).unwrap();
    rig.wait_for("ren", "script");
    // The person changes one of the run's edits, then lets the run fail.
    rig.set_look(&person(), "aki", "person", None);
    rig.set_look(&person(), "kai", "go", None);
    let ended = rig.exec.wait(job.id, Some(Duration::from_secs(30))).unwrap();
    let JobState::Failed { error } = &ended.state else { panic!("{:?}", ended.state) };
    assert_eq!(error.code, ErrorCode::Conflict, "{error}");
    assert!(matches!(error.detail.as_deref(), Some(ErrorDetail::Unreverted { .. })), "{:?}", error.detail);
    assert_eq!(rig.looks(&["aki", "ren"]), vec![json!("person"), json!("script")], "nothing was reverted");
}

#[test]
fn an_inner_call_that_needs_an_approval_is_refused_and_nothing_is_queued() {
    let rig = rig(scripting());
    // Called by the agent itself, the op waits for alice to confirm it.
    let direct = Request { op: "look.confirm".into(), args: json!({}), ..rig.script("", json!({}), None) };
    let waiting = rig.exec.run(direct).unwrap_err();
    assert_eq!(waiting.code, ErrorCode::Waiting, "{waiting}");
    let queued = rig.exec.approvals();
    assert_eq!(queued.len(), 1);
    rig.exec.resolve(queued[0].id, Decision::Reject { comment: None }, &person()).unwrap();

    // Inside a run it is refused: queued, it would run on its own after the run had ended.
    let source = "ops::look::set(#{ character: \"aki\", look: \"oval\" }); ops::look::confirm(#{})";
    let error = rig.exec.run(rig.script(source, json!({}), None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error}");
    assert!(matches!(error.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{:?}", error.detail);
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("outside the script")), "{:?}", error.hint);
    assert!(rig.exec.approvals().is_empty(), "nothing is left pending");
    assert_eq!(rig.look("aki"), json!("plain"), "and the run before it is rolled back");

    // A scope the session lacks is refused the same way: a run's grants are its session's, never more.
    let request =
        Request { grants: scripting().with(Scope::Publish), ..rig.script("ops::look::publish(#{})", json!({}), None) };
    let error = rig.exec.run(request).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error}");
    assert!(
        matches!(error.detail.as_deref(), Some(ErrorDetail::MissingScope { scope: Scope::Publish })),
        "{:?}",
        error.detail
    );
    assert!(rig.exec.approvals().is_empty());
}

#[test]
fn a_script_cannot_borrow_a_persons_standing_nor_start_a_job() {
    let rig = rig(scripting());
    let person_only = rig.exec.run(rig.script("ops::look::rename_project(#{})", json!({}), None)).unwrap_err();
    assert_eq!(person_only.code, ErrorCode::Denied, "{person_only}");
    assert!(matches!(person_only.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. })), "{:?}", person_only.detail);

    let nested = rig.exec.run(rig.script("ops::look::render(#{})", json!({}), None)).unwrap_err();
    assert_eq!(nested.code, ErrorCode::Invalid, "a job inside the run is refused before it starts: {nested}");
    let again = rig.exec.run(rig.script("ops::script::run(#{ source: \"1\" })", json!({}), None)).unwrap_err();
    assert_eq!(again.code, ErrorCode::Invalid, "{again}");
    let jobs = rig.exec.jobs(&Grants::person()).unwrap();
    assert!(jobs.iter().all(|job| job.op == "script.run"), "no job started inside a run: {jobs:?}");
}
