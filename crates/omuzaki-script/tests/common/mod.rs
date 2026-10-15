//! A fake `Runner` with two fake ops, and the helpers the script tests share. Later script tasks reuse them.

#![allow(dead_code)] // each test binary uses a different part
#![allow(clippy::unwrap_used, clippy::large_enum_variant, clippy::type_complexity)] // a test fake: a poisoned lock should panic

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch, Value, lookup};
use omuzaki_ops_api::{
    Actor, Event, Grants, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Plan, Request, Runner, Scope, TxId, Undo,
};
use omuzaki_script::{Cancel, Limits, RunRequest, RunnerCalls, ScriptError, ScriptHost, ScriptOutcome};
use schemars::Schema;

/// What the fake runner was asked, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Begin(TxId),
    Request(Request),
    Commit(TxId),
    Rollback(TxId),
}

/// A runner that records every call. `face.set_profile` is an edit, `measure.likeness` a query, `face.fail` an edit
/// that always fails, `measure.report` a query answering a map with a 10-byte key. `render.video` (a job) and `file.write` (an
/// edit with outside effects) are listed but must never be sent.
#[derive(Default)]
pub struct FakeRunner {
    pub log: Mutex<Vec<Entry>>,
    next_tx: AtomicU64,
    /// Called with the number of requests received so far, after each one is recorded.
    pub on_request: Mutex<Option<Box<dyn Fn(usize) + Send + Sync>>>,
    /// The next commit fails (once) and leaves the transaction open.
    pub fail_commit: AtomicBool,
    /// The next rollback fails (once) and leaves the transaction open, as the real runner does on a conflict.
    pub fail_rollback: AtomicBool,
    /// What `get` reads; `Null` (nothing) until a test sets it.
    pub document: Mutex<Value>,
    /// The subscribers' ends; `emit` sends to all of them.
    senders: Mutex<Vec<Sender<Event>>>,
}

impl FakeRunner {
    pub fn entries(&self) -> Vec<Entry> {
        self.log.lock().unwrap().clone()
    }

    /// Sends `event` to every subscriber, as the runner does when something happens.
    pub fn emit(&self, event: &Event) {
        self.senders.lock().unwrap().iter().for_each(|sender| drop(sender.send(event.clone())));
    }

    pub fn requests(&self) -> Vec<Request> {
        self.entries().into_iter().filter_map(|e| if let Entry::Request(r) = e { Some(r) } else { None }).collect()
    }

    fn count(&self, f: fn(&Entry) -> bool) -> usize {
        self.entries().iter().filter(|e| f(e)).count()
    }

    pub fn begins(&self) -> usize {
        self.count(|e| matches!(e, Entry::Begin(_)))
    }

    pub fn commits(&self) -> usize {
        self.count(|e| matches!(e, Entry::Commit(_)))
    }

    pub fn rollbacks(&self) -> usize {
        self.count(|e| matches!(e, Entry::Rollback(_)))
    }

    /// The run changed nothing durable: no commit, and every transaction it began was rolled back.
    pub fn assert_rolled_back(&self) {
        assert_eq!(self.commits(), 0, "nothing may be committed");
        assert_eq!(self.begins(), self.rollbacks(), "every transaction begun is rolled back");
    }
}

fn info(name: &'static str, kind: OpKind) -> OpInfo {
    let undo = if kind == OpKind::Edit { Undo::Auto } else { Undo::None };
    info_with(name, kind, undo)
}

fn info_with(name: &'static str, kind: OpKind, undo: Undo) -> OpInfo {
    let meta = OpMeta { name, summary: "a fake op", kind, scope: Scope::Read, undo };
    OpInfo::new(meta, Schema::from(true))
}

/// An op with a real argument schema and a scope, so the check has fields and scopes to look at.
fn info_schema(name: &'static str, kind: OpKind, scope: Scope, schema: Value) -> OpInfo {
    let undo = if kind == OpKind::Edit { Undo::Auto } else { Undo::None };
    let meta = OpMeta { name, summary: "a fake op", kind, scope, undo };
    OpInfo::new(meta, Schema::try_from(schema).unwrap())
}

impl Runner for FakeRunner {
    fn ops(&self) -> Vec<OpInfo> {
        vec![
            info("face.fail", OpKind::Edit),
            info_schema(
                "face.set_profile",
                OpKind::Edit,
                Scope::EditCharacter,
                serde_json::json!({
                    "type": "object",
                    "properties": { "character": { "type": "string" }, "profile": { "type": "string" }, "blend": { "type": "number" } },
                    "required": ["character"],
                    "additionalProperties": false,
                }),
            ),
            info_schema(
                "face.set_blend",
                OpKind::Edit,
                Scope::EditCharacter,
                serde_json::json!({ "type": "object", "properties": { "character": { "type": "string" }, "blend": { "type": "number" } } }),
            )
            .with_coalesces(true),
            info_schema(
                "export.publish_clip",
                OpKind::Edit,
                Scope::Publish,
                serde_json::json!({ "type": "object", "properties": { "clip": { "type": "string" } } }),
            ),
            info("measure.likeness", OpKind::Query),
            info("measure.report", OpKind::Query),
            info("render.video", OpKind::Job),
            info("script.run", OpKind::Job),
            info_with("file.write", OpKind::Edit, Undo::Explicit),
        ]
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let op = req.op.clone();
        let args = req.args.clone();
        let n = {
            let mut log = self.log.lock().unwrap();
            log.push(Entry::Request(req));
            log.iter().filter(|e| matches!(e, Entry::Request(_))).count()
        };
        if let Some(hook) = self.on_request.lock().unwrap().as_ref() {
            hook(n);
        }
        match op.as_str() {
            "face.set_profile" | "face.set_blend" | "export.publish_clip" => {
                let mut plan = Plan::edit(Patch::default());
                plan.output = Some(args);
                Ok(Outcome::new(plan))
            }
            "measure.likeness" => Ok(Outcome::new(Plan::answer(Value::from(0.5)))),
            "measure.report" => Ok(Outcome::new(Plan::answer(serde_json::json!({ "0123456789": 1 })))),
            "face.fail" => Err(OpError::invalid("blend", "blend must be between 0 and 1").hint("use 0.5")),
            _ => Err(OpError::not_found(format!("no op {op}"))),
        }
    }

    fn begin(&self, _label: &str, _actor: &Actor) -> TxId {
        let tx = TxId(self.next_tx.fetch_add(1, Ordering::SeqCst) + 1);
        self.log.lock().unwrap().push(Entry::Begin(tx));
        tx
    }

    fn commit(&self, tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        if self.fail_commit.swap(false, Ordering::SeqCst) {
            return Err(OpError::conflict("the fake commit failed"));
        }
        self.log.lock().unwrap().push(Entry::Commit(tx));
        Ok(())
    }

    fn rollback(&self, tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        if self.fail_rollback.swap(false, Ordering::SeqCst) {
            return Err(OpError::conflict("the fake rollback failed"));
        }
        self.log.lock().unwrap().push(Entry::Rollback(tx));
        Ok(())
    }

    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Err(OpError::unsupported("fake"))
    }

    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Err(OpError::unsupported("fake"))
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        if !grants.allows(Scope::Read) {
            return Err(OpError::denied("subscribing needs read"));
        }
        let (sender, receiver) = channel();
        self.senders.lock().unwrap().push(sender);
        Ok(receiver)
    }

    fn get(&self, path: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(lookup(&self.document.lock().unwrap(), path).cloned())
    }

    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

/// A `Cancel` a test sets by hand.
#[derive(Default)]
pub struct Flag(pub AtomicBool);

impl Flag {
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Cancel for Flag {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Never cancelled, but each poll moves the fake clock on a second: the wall clock needs no sleep.
pub struct Ticking(pub Arc<FakeClock>);

impl Cancel for Ticking {
    fn is_cancelled(&self) -> bool {
        self.0.advance(Seconds(1.0));
        false
    }
}

pub fn request(source: &str) -> RunRequest {
    RunRequest {
        name: "test script".into(),
        source: source.into(),
        actor: Actor::Person { name: "tester".into() },
        grants: Grants::person(),
        tx: None,
        params: serde_json::Map::new(),
        dry_run: false,
    }
}

pub fn host(runner: &FakeRunner, limits: Limits, clock: &Arc<FakeClock>) -> ScriptHost {
    ScriptHost::new(&runner.ops(), limits).with_clock(clock.clone())
}

pub fn calls(runner: &Arc<FakeRunner>) -> RunnerCalls {
    RunnerCalls::new(runner.clone(), "test script", Actor::Person { name: "tester".into() }, Grants::person())
}

/// Runs `source` under the console limits against a fresh fake runner.
pub fn run(source: &str) -> (Arc<FakeRunner>, Result<ScriptOutcome, ScriptError>) {
    run_with(request(source), &Flag::default(), Limits::console(), FakeRunner::default())
}

pub fn run_with(
    request: RunRequest,
    cancel: &dyn Cancel,
    limits: Limits,
    runner: FakeRunner,
) -> (Arc<FakeRunner>, Result<ScriptOutcome, ScriptError>) {
    let runner = Arc::new(runner);
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let result = host(&runner, limits, &clock).run(request, &calls(&runner), cancel);
    (runner, result)
}
