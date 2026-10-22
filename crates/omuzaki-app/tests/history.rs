//! History through the app core: the transactions of every session, any past state rebuilt from the journal, and the
//! ops that list, compare, restore and name them.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use proptest::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets one of a few values under the character `kit`: enough to give history something to rebuild.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetValue {
    key: String,
    value: i64,
}

impl Op for SetValue {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_value",
            summary: "Set a value of the test character.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let characters = DocPath::parse("characters").unwrap();
        let root = characters.join("kit");
        let mut fields = serde_json::Map::new();
        fields.insert(self.key.clone(), json!(self.value));
        let kit = json!({"values": fields});
        match (ctx.doc.get(&characters), ctx.doc.get(&root)) {
            (None, _) => return Ok(Plan::edit(Patch::new().with(characters, None, Some(json!({"kit": kit}))))),
            (Some(_), None) => return Ok(Plan::edit(Patch::new().with(root, None, Some(kit)))),
            _ => {}
        }
        let values = root.join("values");
        let at = values.join(self.key.as_str());
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.value)))))
    }
}
register_op!(SetValue);

fn options() -> AppOptions {
    AppOptions::studio(Actor::Person { name: "pat".into() })
}

fn set(core: &AppCore, key: &str, value: i64) -> u64 {
    let outcome = core.run("test.set_value", json!({"key": key, "value": value})).unwrap();
    outcome.applied.expect("applied").tx.0
}

fn kit(core: &AppCore) -> Option<Value> {
    core.get(&DocPath::parse("characters/kit").unwrap())
}

fn output(core: &AppCore, op: &str, args: Value) -> Value {
    core.run(op, args).unwrap_or_else(|e| panic!("{op}: {e}")).plan.output.unwrap_or(Value::Null)
}

#[test]
fn every_session_is_listed_newest_first_and_pages_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let first = set(&core, "a", 1);
    set(&core, "a", 2);
    core.close().unwrap();
    let core = AppCore::open(tmp.path(), options()).unwrap();
    let third = set(&core, "b", 3);
    core.undo().unwrap();

    let page = output(&core, "history.list", json!({"limit": 2}));
    let txs: Vec<u64> = page["transactions"].as_array().unwrap().iter().map(|t| t["tx"].as_u64().unwrap()).collect();
    assert_eq!(txs.len(), 2, "{page}");
    assert_eq!(txs[0], third, "newest first: {page}");
    assert_eq!(page["transactions"][0]["outcome"], "undone", "{page}");
    assert_eq!(page["transactions"][0]["actor"], "person pat");
    assert_eq!(page["transactions"][0]["label"], "test.set_value");
    assert!(page["transactions"][0]["time"].as_u64().is_some_and(|t| t > 1_600_000_000_000), "{page}");
    let rest = output(&core, "history.list", json!({"limit": 2, "before": page["next"]}));
    let older: Vec<u64> = rest["transactions"].as_array().unwrap().iter().map(|t| t["tx"].as_u64().unwrap()).collect();
    assert_eq!(older.last(), Some(&first), "{rest}");
    assert_ne!(
        page["transactions"][0]["session"], rest["transactions"][0]["session"],
        "the two sessions are told apart"
    );
    core.close().unwrap();
}

#[test]
fn diff_restore_and_undo_across_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let early = set(&core, "a", 1);
    set(&core, "b", 2);
    core.close().unwrap();
    let core = AppCore::open(tmp.path(), options()).unwrap();
    set(&core, "a", 5);

    let diff = output(&core, "history.diff", json!({"from": {"tx": early}}));
    assert_eq!(diff["changes"], 2, "{diff}");
    let unit = &diff["units"][0];
    assert_eq!(unit["unit"], "characters/kit/values", "{diff}");
    let paths: Vec<&str> = unit["changes"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["characters/kit/values/a", "characters/kit/values/b"]);
    assert_eq!(unit["changes"][0]["before"], 1);
    assert_eq!(unit["changes"][0]["after"], 5);
    let same = output(&core, "history.diff", json!({"from": "now"}));
    assert_eq!(same["changes"], 0, "{same}");

    // The dry run says what would change and changes nothing.
    let before = kit(&core);
    let preview = core.dry_run("history.restore", json!({"to": {"tx": early}})).unwrap();
    assert_eq!(preview.plan.output.as_ref().unwrap()["changes"], 2);
    assert_eq!(kit(&core), before);

    let restored = output(&core, "history.restore", json!({"to": {"tx": early}}));
    assert!(restored["restored_to"].as_str().unwrap().contains(&early.to_string()), "{restored}");
    assert_eq!(kit(&core), Some(json!({"values": {"a": 1}})));
    // One step: undo returns to now.
    core.undo().unwrap();
    assert_eq!(kit(&core), Some(json!({"values": {"a": 5, "b": 2}})));
    core.close().unwrap();
}

#[test]
fn a_rolled_back_or_unknown_transaction_has_no_state() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(&core, "a", 1);
    let error = core.run("history.diff", json!({"from": {"tx": 999}})).unwrap_err();
    assert!(error.to_string().contains("999"), "{error}");
    core.close().unwrap();
}

#[test]
fn a_version_names_now_or_a_past_state_and_names_are_unique() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let early = set(&core, "a", 1);
    set(&core, "a", 2);
    let named = output(&core, "history.name", json!({"name": "first look", "at": {"tx": early}}));
    assert_eq!(named["named"], true, "{named}");
    output(&core, "history.name", json!({"name": "latest"}));
    let taken = core.run("history.name", json!({"name": "first look"})).unwrap_err();
    assert!(taken.to_string().contains("already named"), "{taken}");

    let page = output(&core, "history.list", json!({}));
    let names_of = |tx: u64| -> Vec<Value> {
        let item = page["transactions"].as_array().unwrap().iter().find(|t| t["tx"] == tx).unwrap();
        item["names"].as_array().cloned().unwrap_or_default()
    };
    assert_eq!(names_of(early), [json!("first look")], "{page}");
    let latest = page["transactions"][0]["tx"].as_u64().unwrap();
    assert_eq!(names_of(latest), [json!("latest")], "{page}");

    // The named past state is that state: a diff from its snapshot to the transaction is empty.
    let snapshot = named["snapshot"].as_str().unwrap();
    let diff = output(&core, "history.diff", json!({"from": {"snapshot": snapshot}, "to": {"tx": early}}));
    assert_eq!(diff["changes"], 0, "{diff}");
    core.close().unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 6, ..ProptestConfig::default() })]

    /// Random edits over two sessions, with undos: the state rebuilt at every committed transaction equals the
    /// document as it was right after it, and restoring a random one then undoing returns exactly to before.
    #[test]
    fn every_transaction_rebuilds_as_it_was(edits in prop::collection::vec((0..3usize, 0..100i64, any::<bool>()), 4..14), pick in any::<prop::sample::Index>()) {
        let tmp = tempfile::tempdir().unwrap();
        let mut core = AppCore::create(tmp.path(), "demo", options()).unwrap();
        let mut seen: Vec<(u64, Option<Value>)> = Vec::new();
        let half = edits.len() / 2;
        for (i, (key, value, undo)) in edits.iter().enumerate() {
            if i == half {
                core.close().unwrap();
                core = AppCore::open(tmp.path(), options()).unwrap();
            }
            let name = ["a", "b", "c"][*key];
            let outcome = core.run("test.set_value", json!({"key": name, "value": value})).unwrap();
            let applied = outcome.applied.expect("applied");
            // A value set to what it was changes nothing, and history lists only changes.
            if !applied.patch.is_empty() {
                seen.push((applied.tx.0, kit(&core)));
            }
            if *undo {
                core.undo().unwrap();
            }
        }
        // Every committed transaction rebuilds as it was: restoring it gives that document, and undo comes back.
        let now = kit(&core);
        for (tx, state) in &seen {
            let restored = output(&core, "history.restore", json!({"to": {"tx": tx}}));
            prop_assert_eq!(&kit(&core), state, "transaction {}", tx);
            // A restore that changes nothing is no step of its own.
            if restored["changes"] != 0 {
                core.undo().unwrap();
            }
            prop_assert_eq!(&kit(&core), &now);
        }
        prop_assume!(!seen.is_empty());
        let (tx, _) = &seen[pick.index(seen.len())];
        let back = output(&core, "history.diff", json!({"from": {"tx": tx}, "to": {"tx": tx}}));
        prop_assert_eq!(&back["changes"], &json!(0));
        core.close().unwrap();
    }
}

/// A thousand transactions after the session's base snapshot rebuild well inside a ceiling that holds on a loaded CI
/// machine in a debug build (the design's budget is 50 ms in release; the measured time is printed).
#[test]
fn a_thousand_transactions_after_a_snapshot_rebuild_quickly() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let mut last = 0;
    for i in 0..1000 {
        last = set(&core, ["a", "b", "c", "d"][i % 4], i64::try_from(i).unwrap());
    }
    let started = std::time::Instant::now();
    let diff = output(&core, "history.diff", json!({"from": {"tx": last}}));
    let took = started.elapsed();
    eprintln!("rebuilt the state after transaction {last} in {took:?}");
    assert_eq!(diff["changes"], 0, "{diff}");
    assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    core.close().unwrap();
}
