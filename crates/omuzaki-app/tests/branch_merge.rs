//! `branch.merge`, `branch.list` and `branch.status` end to end (`docs/design/branches.md` 7 and 12.3): units merge
//! on their own, conflicts block until chosen, picks leave the record alone, undoing a merge offers it again, the
//! result is validated, library records follow what they made, and who may apply a merge is what 7.7 says.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::branch::{BranchId, BranchInfo, BranchName, Branches, Lane, NewBranch};
use omuzaki_ops_api::{
    Actor, Budget, ErrorCode, Grants, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope,
    SessionInfo, SessionSpec, Transport, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Writes `value` at `path` (removes it for `null`): every edit these tests need.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Put {
    path: String,
    value: Value,
}

impl Op for Put {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.put",
            summary: "Write a value.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse(&self.path).map_err(|e| OpError::invalid("path", e.reason))?;
        let after = (!self.value.is_null()).then(|| self.value.clone());
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), after)))
    }
}
register_op!(Put);

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn request(actor: &Actor, op: &str, args: Value, mode: Mode) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

fn put(runner: &dyn Runner, path: &str, value: Value) {
    runner.run(request(&person(), "test.put", json!({"path": path, "value": value}), Mode::Apply)).unwrap();
}

fn get(runner: &dyn Runner, path: &str) -> Option<Value> {
    runner.get(&DocPath::parse(path).unwrap(), &Grants::person()).unwrap()
}

fn merge(runner: &dyn Runner, args: Value, mode: Mode) -> Result<Value, OpError> {
    runner.run(request(&person(), "branch.merge", args, mode)).map(|o| o.plan.output.unwrap_or_default())
}

fn unit<'a>(output: &'a Value, key: &str) -> &'a Value {
    output["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["key"] == key)
        .unwrap_or_else(|| panic!("no unit {key}: {output:#}"))
}

/// A project whose character `kit` has a body, hair and a robe, and a branch `aria/oval` made from it, with that
/// branch's lane.
fn project() -> (tempfile::TempDir, AppCore, BranchInfo, Lane) {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", AppOptions::studio(person())).unwrap();
    put(
        core.runner().as_ref(),
        "characters",
        json!({"kit": {
            "name": "Kit",
            "template": "adult",
            "parts": {
                "body": {"kind": "body", "layer": 0},
                "hair": {"kind": "hair", "layer": 1, "params": {"length": 1}},
                "robe": {"kind": "robe", "layer": 40},
            },
            "sources": {"hair": {"origin": "kit"}},
        }}),
    );
    let oval = core
        .branches()
        .create(NewBranch::new(BranchName::new("aria/oval").unwrap(), BranchId::main()), &person())
        .unwrap();
    let lane = core.branches().lane(&oval.id).unwrap();
    (tmp, core, oval, lane)
}

fn checkpoint(core: &AppCore, branch: &BranchInfo) {
    core.branches().checkpoint(&branch.id, "work", None, &person()).unwrap();
}

#[test]
fn one_sided_changes_merge_clean_record_the_merge_and_undo_offers_it_again() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/params/length", json!(3));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/parts/body/layer", json!(2));

    let preview = merge(core.runner().as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    assert_eq!(preview["full"], json!(true));
    assert_eq!(unit(&preview, "characters/kit/parts/hair")["status"], "clean", "{preview:#}");
    assert_eq!(unit(&preview, "characters/kit/parts/body")["status"], "unchanged", "ours only");
    assert!(preview["base"].is_string(), "the fork point is the base");
    assert_eq!(get(core.runner().as_ref(), "characters/kit/parts/hair/params/length"), Some(json!(1)), "a dry-run");

    let done = merge(core.runner().as_ref(), json!({"from": "aria/oval"}), Mode::Apply).unwrap();
    assert!(done["label"].as_str().unwrap().starts_with("Merge aria/oval into main"), "{done}");
    assert_eq!(get(core.runner().as_ref(), "characters/kit/parts/hair/params/length"), Some(json!(3)));
    assert_eq!(get(core.runner().as_ref(), "characters/kit/parts/body/layer"), Some(json!(2)), "mine stays");
    let record = get(core.runner().as_ref(), &format!("merges/{}", oval.id)).expect("the merge is recorded");
    assert_eq!(record["name"], "aria/oval");

    // Merged once, nothing more is offered; undone, the record goes with it and the changes are offered again.
    let again = merge(core.runner().as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    assert!(again["units"].as_array().unwrap().iter().all(|u| u["status"] != "clean"), "{again:#}");
    core.undo().unwrap();
    assert_eq!(get(core.runner().as_ref(), "merges"), None);
    let offered = merge(core.runner().as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    assert_eq!(unit(&offered, "characters/kit/parts/hair")["status"], "clean");
    drop(side);
    core.close().unwrap();
}

#[test]
fn a_conflict_blocks_apply_until_chosen_and_each_choice_gives_its_tree() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/params/length", json!(3));
    put(side.runner.as_ref(), "characters/kit/parts/hair/layer", json!(5));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/parts/hair/params/length", json!(2));
    let runner = core.runner();
    let hair = "characters/kit/parts/hair";

    let preview = merge(runner.as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    assert_eq!(unit(&preview, hair)["status"], "conflict");
    assert!(preview["summary"].as_str().unwrap().contains("need a choice"), "{preview}");
    let refused = merge(runner.as_ref(), json!({"from": "aria/oval"}), Mode::Apply).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.message.contains(hair), "{refused:?}");

    // Mine keeps the whole unit; theirs takes it whole; per parameter takes the conflict one way and the rest auto.
    merge(runner.as_ref(), json!({"from": "aria/oval", "resolve": {hair: "mine"}}), Mode::Apply).unwrap();
    assert_eq!(get(runner.as_ref(), hair).unwrap()["params"]["length"], json!(2));
    assert_eq!(get(runner.as_ref(), hair).unwrap()["layer"], json!(1));
    core.undo().unwrap();
    merge(runner.as_ref(), json!({"from": "aria/oval", "resolve": {hair: "theirs"}}), Mode::Apply).unwrap();
    assert_eq!(get(runner.as_ref(), hair).unwrap()["params"]["length"], json!(3));
    core.undo().unwrap();
    let per_path = json!({"paths": {"characters/kit/parts/hair/params/length": "mine"}});
    merge(runner.as_ref(), json!({"from": "aria/oval", "resolve": {hair: per_path}}), Mode::Apply).unwrap();
    let merged = get(runner.as_ref(), hair).unwrap();
    assert_eq!((merged["params"]["length"].clone(), merged["layer"].clone()), (json!(2), json!(5)));
    drop(side);
    core.close().unwrap();
}

#[test]
fn a_pick_leaves_the_record_alone_and_the_next_merge_offers_the_rest() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/layer", json!(5));
    put(side.runner.as_ref(), "characters/kit/parts/robe/layer", json!(45));
    checkpoint(&core, &oval);
    let runner = core.runner();

    let pick =
        merge(runner.as_ref(), json!({"from": "aria/oval", "exclude": ["characters/kit/parts/robe"]}), Mode::Apply)
            .unwrap();
    assert_eq!(pick["full"], json!(false));
    assert_eq!(unit(&pick, "characters/kit/parts/robe")["status"], "excluded");
    assert_eq!(get(runner.as_ref(), "characters/kit/parts/hair/layer"), Some(json!(5)));
    assert_eq!(get(runner.as_ref(), "characters/kit/parts/robe/layer"), Some(json!(40)));
    assert_eq!(get(runner.as_ref(), "merges"), None, "a pick records nothing");
    let rest = merge(runner.as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    assert_eq!(unit(&rest, "characters/kit/parts/robe")["status"], "clean");
    assert_eq!(unit(&rest, "characters/kit/parts/hair")["status"], "unchanged", "already taken");
    drop(side);
    core.close().unwrap();
}

#[test]
fn a_missing_parent_is_invalid_until_a_side_is_chosen() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/clasp", json!({"kind": "clasp", "parent": "robe", "layer": 41}));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/parts/robe", Value::Null);
    let runner = core.runner();

    let preview = merge(runner.as_ref(), json!({"from": "aria/oval"}), Mode::DryRun).unwrap();
    let clasp = unit(&preview, "characters/kit/parts/clasp");
    assert_eq!(clasp["status"], "invalid", "{preview:#}");
    assert!(clasp["note"].as_str().unwrap().contains("robe"), "{clasp}");
    assert_eq!(unit(&preview, "characters/kit/parts/robe")["status"], "invalid", "the parent's unit too");
    assert_eq!(
        merge(runner.as_ref(), json!({"from": "aria/oval"}), Mode::Apply).unwrap_err().code,
        ErrorCode::Conflict
    );
    let resolved = json!({"from": "aria/oval", "resolve": {"characters/kit/parts/clasp": "mine"}});
    merge(runner.as_ref(), resolved, Mode::Apply).unwrap();
    assert_eq!(get(runner.as_ref(), "characters/kit/parts/clasp"), None);
    drop(side);
    core.close().unwrap();
}

#[test]
fn keep_both_adds_theirs_as_a_disabled_part_with_its_source_row_and_one_undo_removes_it() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/params/length", json!(3));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/parts/hair/params/length", json!(2));
    let runner = core.runner();
    let both = json!({"from": "aria/oval", "resolve": {"characters/kit/parts/hair": "both"}});

    let done = merge(runner.as_ref(), both, Mode::Apply).unwrap();
    assert!(unit(&done, "characters/kit/parts/hair")["note"].as_str().unwrap().contains("hair-oval"), "{done:#}");
    assert_eq!(get(runner.as_ref(), "characters/kit/parts/hair/params/length"), Some(json!(2)), "mine stays");
    let kept = get(runner.as_ref(), "characters/kit/parts/hair-oval").expect("theirs is kept beside");
    assert_eq!((kept["enabled"].clone(), kept["params"]["length"].clone()), (json!(false), json!(3)));
    assert_eq!(get(runner.as_ref(), "characters/kit/sources/hair-oval"), Some(json!({"origin": "kit"})));
    core.undo().unwrap();
    assert_eq!(get(runner.as_ref(), "characters/kit/parts/hair-oval"), None);
    assert_eq!(get(runner.as_ref(), "characters/kit/sources/hair-oval"), None);

    put(side.runner.as_ref(), "characters/kit/name", json!("Kit B"));
    checkpoint(&core, &oval);
    let not_a_part = json!({"from": "aria/oval", "resolve": {"characters/kit/basics": "both"}});
    assert_eq!(merge(runner.as_ref(), not_a_part, Mode::DryRun).unwrap_err().code, ErrorCode::Invalid);
    drop(side);
    core.close().unwrap();
}

#[test]
fn a_library_item_merges_as_one_unit_and_its_record_follows_what_it_made() {
    let (_tmp, core, _, first) = project();
    drop(first);
    let record = |version: &str| json!({"item": "cat-ears", "version": version, "slot": "ears", "created": ["characters/kit/parts/ears"]});
    // Both sides updated the item to different versions: one conflicting unit, taken whole from one side.
    put(core.runner().as_ref(), "characters/kit/applied", json!({"cat-ears": record("1")}));
    put(core.runner().as_ref(), "characters/kit/parts/ears", json!({"kind": "ears", "layer": 50}));
    checkpoint(&core, &core.branches().by_name(&BranchName::main()).unwrap());
    let ears = NewBranch::new(BranchName::new("aria/ears").unwrap(), BranchId::main());
    let oval = core.branches().create(ears, &person()).unwrap();
    let side = core.branches().lane(&oval.id).unwrap();
    put(side.runner.as_ref(), "characters/kit/applied/cat-ears/version", json!("3"));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/applied/cat-ears/version", json!("2"));
    let runner = core.runner();
    let preview = merge(runner.as_ref(), json!({"from": "aria/ears"}), Mode::DryRun).unwrap();
    let item = unit(&preview, "characters/kit/applied/cat-ears");
    assert_eq!(
        (item["status"].clone(), item["kind"].clone()),
        (json!("conflict"), json!("application")),
        "{preview:#}"
    );
    let theirs = json!({"from": "aria/ears", "resolve": {"characters/kit/applied/cat-ears": "theirs"}});
    merge(runner.as_ref(), theirs, Mode::Apply).unwrap();
    assert_eq!(get(runner.as_ref(), "characters/kit/applied/cat-ears/version"), Some(json!("3")));

    // The other branch removed the record but kept the part it made: the record stays, with a rights note.
    put(side.runner.as_ref(), "characters/kit/applied/cat-ears", Value::Null);
    checkpoint(&core, &oval);
    let kept = merge(runner.as_ref(), json!({"from": "aria/ears"}), Mode::Apply).unwrap();
    assert!(get(runner.as_ref(), "characters/kit/applied/cat-ears").is_some(), "{kept:#}");
    assert!(kept["rights"][0].as_str().unwrap().contains("cat-ears"), "{kept:#}");
    drop(side);
    core.close().unwrap();
}

fn agent_session(lane: &Lane, grants: Grants) -> SessionInfo {
    lane.sessions
        .open(SessionSpec {
            transport: Transport::Stdio,
            client: "designer".into(),
            model: None,
            on_behalf_of: person(),
            grants,
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        })
        .unwrap()
}

#[test]
fn an_agent_s_merge_into_the_folder_asks_the_person_and_a_scene_needs_edit_scene() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/layer", json!(5));
    checkpoint(&core, &oval);
    let folder = core.branches().lane(&BranchId::main()).unwrap();
    let agent = agent_session(&folder, Grants::person()).actor;
    let mut ask = request(&agent, "branch.merge", json!({"from": "aria/oval"}), Mode::Apply);
    let preview = folder.runner.run(Request { mode: Mode::DryRun, ..ask.clone() }).unwrap();
    assert_eq!(preview.plan.output.unwrap()["needs"], json!(["person"]), "the dry-run is the preview");
    let waiting = folder.runner.run(ask.clone()).unwrap_err();
    assert_eq!(waiting.code, ErrorCode::Waiting, "{waiting:?}");

    // On its own branch an agent merges main in without asking, but a scene needs `edit:scene`.
    put(core.runner().as_ref(), "scenes", json!({"stage": {"light": 1}}));
    checkpoint(&core, &core.branches().by_name(&BranchName::main()).unwrap());
    let limited = Grants::of([Scope::Read, Scope::EditCharacter]);
    let own = agent_session(&side, limited.clone()).actor;
    ask = Request { actor: own, grants: limited, args: json!({"from": "main"}), ..ask };
    let denied = side.runner.run(ask).unwrap_err();
    assert_eq!(denied.code, ErrorCode::Denied, "{denied:?}");
    assert!(denied.message.contains("scene"), "{denied:?}");
    drop((side, folder));
    core.close().unwrap();
}

#[test]
fn list_and_status_say_what_changed_on_each_side_since_the_base() {
    let (_tmp, core, oval, side) = project();
    put(side.runner.as_ref(), "characters/kit/parts/hair/layer", json!(5));
    checkpoint(&core, &oval);
    put(core.runner().as_ref(), "characters/kit/parts/body/layer", json!(2));
    put(core.runner().as_ref(), "characters/kit/parts/robe/layer", json!(42));
    checkpoint(&core, &core.branches().by_name(&BranchName::main()).unwrap());

    let listed = core.runner().run(request(&person(), "branch.list", json!({}), Mode::Apply)).unwrap();
    let rows = listed.plan.output.unwrap()["branches"].as_array().unwrap().clone();
    assert_eq!(rows.iter().map(|r| r["name"].clone()).collect::<Vec<_>>(), [json!("main"), json!("aria/oval")]);
    assert_eq!(rows[1]["changed"], json!(["kit: part hair"]));

    let status = side.runner.run(request(&person(), "branch.status", json!({}), Mode::Apply)).unwrap();
    let status = status.plan.output.unwrap();
    assert_eq!(status["changed_here"], json!(["kit: part hair"]));
    assert_eq!(status["changed_on_folder"], json!(["kit: part body", "kit: part robe"]));
    assert_eq!(status["summary"], "1 changed here, 2 on main");
    drop(side);
    core.close().unwrap();
}
