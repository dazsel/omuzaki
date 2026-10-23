//! Simulated power loss across the journal writer, the keeper and recovery (#366, journal.md 12 item 2).
//!
//! Each seed runs a session on the app core with the journal on a [`SimFs`]: two people, one-op changes, transactions
//! of one to five ops that commit or roll back, undo and redo, some of them acknowledged with `sync`. The power is cut
//! before a seeded operation of the disk (any write, between a write and its flush, while a segment is prepared,
//! while rotating into one): a seeded few operations after a seeded step, or while the project opens. Counting from a
//! step, not from the start, keeps the cut inside the session however the writer happens to batch. The core is
//! abandoned, the disk keeps what a cut keeps, and the project is opened again,
//! sometimes after a second cut during that recovery. The document must then be the one after some completed step at
//! or after the last acknowledged one, with every person's undo and redo labels as they were then.
//!
//! `OMUZAKI_POWER_SEEDS` sets how many seeds run (200 by default; the nightly workflow runs 5,000), `OMUZAKI_POWER_SEED`
//! runs one.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::journal::journal_dir;
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::JournalSink;
use omuzaki_ops_api::{Actor, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_test::sim_fs::{SimFs, SimRng};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const SYNC: Duration = Duration::from_secs(5);
const FIELDS: [&str; 4] = ["a", "b", "c", "d"];

/// Sets one of the project's test fields.
#[derive(Serialize, Deserialize, JsonSchema)]
struct PowerSet {
    field: String,
    value: u64,
}

impl Op for PowerSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "power.set",
            summary: "Set one of the power-loss test's fields.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse(&format!("project/power_{}", self.field))
            .map_err(|e| OpError::invalid("field", e.to_string()))?;
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.value)))))
    }
}
register_op!(PowerSet);

fn person(n: u64) -> Actor {
    Actor::Person { name: if n == 0 { "pat".into() } else { "sam".into() } }
}

/// The document and every person's undo and redo labels: what a recovered project must match.
#[derive(Clone, Debug, PartialEq)]
struct State {
    root: Value,
    labels: BTreeMap<String, (Vec<String>, Vec<String>)>,
}

fn state(core: &AppCore) -> State {
    let root = core.get(&DocPath::root()).unwrap_or_default();
    let labels = core
        .executor()
        .history_export()
        .actors
        .into_iter()
        .filter(|a| !a.undo.is_empty() || !a.redo.is_empty())
        .map(|a| {
            let undo = a.undo.iter().map(|s| s.label.clone()).collect();
            let redo = a.redo.iter().map(|s| s.label.clone()).collect();
            (format!("{:?}", a.actor), (undo, redo))
        })
        .collect();
    State { root, labels }
}

fn set(core: &AppCore, actor: &Actor, rng: &mut SimRng, tx: Option<omuzaki_ops_api::TxId>) {
    let field = FIELDS[usize::try_from(rng.below(4)).unwrap()];
    let mut request = core.request("power.set", json!({"field": field, "value": rng.below(1000)}), Mode::Apply);
    request.actor = actor.clone();
    request.tx = tx;
    drop(core.runner().run(request));
}

/// Where a session's cut goes: before the disk operation `ops` after the start of step `step`.
#[derive(Clone, Copy)]
struct Cut {
    step: u64,
    ops: u64,
}

/// Runs one seeded session to its cut; returns the states after each completed step and the index of the last one a
/// `sync` acknowledged.
fn session(core: &AppCore, fs: &SimFs, rng: &mut SimRng, cut: Option<Cut>) -> (Vec<State>, usize) {
    let runner = core.runner();
    let mut states = vec![state(core)];
    let mut acked = 0;
    let mut after_cut = 0;
    for step in 0..60u64 {
        if let Some(cut) = cut.filter(|cut| cut.step == step) {
            fs.cut_before(fs.ops() + cut.ops);
        }
        let actor = person(rng.below(2));
        match rng.below(8) {
            0..=2 => set(core, &actor, rng, None),
            3 | 4 => {
                let tx = runner.begin(&format!("tx {step}"), &actor);
                for _ in 0..=rng.below(5) {
                    set(core, &actor, rng, Some(tx));
                }
                if rng.below(4) == 0 {
                    drop(runner.rollback(tx, &actor));
                } else {
                    drop(runner.commit(tx, &actor));
                }
            }
            5 | 6 => drop(runner.undo(&actor, &Grants::person())),
            _ => drop(runner.redo(&actor, &Grants::person())),
        }
        states.push(state(core));
        if rng.below(3) == 0 && core.journal().sync(SYNC).is_ok() && !fs.was_cut() {
            acked = states.len() - 1;
        }
        if fs.was_cut() {
            after_cut += 1;
            if after_cut > 3 {
                break;
            }
        }
    }
    (states, acked)
}

/// What one seed covered.
#[derive(Clone, Copy, Debug, Default)]
struct Covered {
    /// The cut came during the session, not at its end.
    mid_session: bool,
    /// Steps a `sync` acknowledged before the cut.
    acked: usize,
    /// The steps run.
    steps: usize,
}

fn run_seed(seed: u64) -> Covered {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = AppCore::create(dir, "power", AppOptions::headless(person(0))).unwrap();
    let initial = state(&core);
    core.close().unwrap();
    let mut rng = SimRng::new(seed);
    let journal = journal_dir(dir);

    let fs = SimFs::load(&journal, seed).unwrap();
    // One cut in eight while the project opens; the rest a few disk operations into a seeded step.
    let cut = if rng.below(8) == 0 {
        fs.cut_before(1 + rng.below(12));
        None
    } else {
        Some(Cut { step: rng.below(60), ops: 1 + rng.below(8) })
    };
    let options = AppOptions::headless(person(0)).with_journal_fs(Arc::new(fs.clone()));
    let (states, acked) = match AppCore::open(dir, options.clone()) {
        Ok(core) => {
            let ran = session(&core, &fs, &mut rng, cut);
            core.abandon();
            ran
        }
        // Cut while the session started: nothing of it was acknowledged.
        Err(_) => (vec![initial], 0),
    };
    let mid_session = fs.was_cut();
    fs.materialize(&journal).unwrap();

    // Sometimes the power goes again while recovering.
    if rng.below(2) == 0 {
        let again = SimFs::load(&journal, seed ^ 0x5eed).unwrap();
        again.cut_before(1 + rng.below(12));
        let options = AppOptions::headless(person(0)).with_journal_fs(Arc::new(again.clone()));
        if let Ok(core) = AppCore::open(dir, options) {
            core.abandon();
        }
        again.materialize(&journal).unwrap();
    }

    let core = AppCore::open(dir, AppOptions::headless(person(0)))
        .unwrap_or_else(|e| panic!("seed {seed}: the project doesn't open after the cut: {e}"));
    let recovered = state(&core);
    let matched = states.iter().enumerate().skip(acked).find(|(_, s)| **s == recovered).map(|(i, _)| i);
    assert!(
        matched.is_some(),
        "seed {seed}: recovered a state that is no completed step at or after the acknowledged one ({acked} of {}).\n\
         recovered: {recovered:#?}\nacknowledged: {:#?}\nlast: {:#?}\nrecovery: {:?}",
        states.len() - 1,
        states[acked],
        states.last(),
        core.recovery(),
    );
    core.close().unwrap();
    Covered { mid_session, acked, steps: states.len() - 1 }
}

fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("OMUZAKI_POWER_SEED").ok().and_then(|s| s.parse().ok()) {
        return vec![seed];
    }
    let count = std::env::var("OMUZAKI_POWER_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(200u64);
    (1..=count).collect()
}

#[test]
fn after_a_power_cut_the_project_is_at_a_completed_step_no_older_than_the_last_acknowledged() {
    let seeds = seeds();
    let mut covered = Vec::new();
    for batch in seeds.chunks(8) {
        std::thread::scope(|scope| {
            let runs: Vec<_> = batch.iter().map(|&seed| scope.spawn(move || run_seed(seed))).collect();
            covered.extend(runs.into_iter().map(|run| run.join().unwrap()));
        });
    }
    let mid = covered.iter().filter(|c| c.mid_session).count();
    let acked = covered.iter().filter(|c| c.acked > 0).count();
    let steps: usize = covered.iter().map(|c| c.steps).sum();
    println!("{} seeds: {mid} cut mid-session, {acked} with acknowledged steps, {steps} steps", covered.len());
    // Not vacuous: most cuts land inside the session, after something was acknowledged.
    if covered.len() >= 50 {
        assert!(mid * 2 >= covered.len() && acked * 3 >= covered.len(), "{mid} mid-session, {acked} acknowledged");
    }
}
