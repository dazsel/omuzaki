//! Keep both on a real kit character (`docs/design/branches.md` 7.5): a part both branches changed merges as ours,
//! with theirs kept beside it as a disabled part; the character builds as ours, and enabling theirs in place of ours
//! builds theirs' look. The source row comes along, so rights carry over, and one undo takes it all back.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::branch::{BranchId, BranchName, Branches, NewBranch};
use omuzaki_ops_api::{
    Actor, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Writes `value` at `path`.
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
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(self.value.clone()))))
    }
}
register_op!(Put);

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn run(runner: &dyn Runner, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("{op}: {e}")).plan.output.unwrap_or_default()
}

fn get(runner: &dyn Runner, path: &str) -> Option<Value> {
    runner.get(&DocPath::parse(path).unwrap(), &Grants::person()).unwrap()
}

fn build(runner: &dyn Runner) -> BuiltCharacter {
    let node = get(runner, "characters/mia").expect("the character");
    let def = CharacterDef::from_node(&node).unwrap();
    RegistryBuilder
        .build(&CharId::new("mia").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// What a part that isn't built leaves.
const NOTHING: Vec<[f32; 3]> = Vec::new();

/// The vertex positions `part`'s meshes built, in order.
fn geometry(built: &BuiltCharacter, part: &str) -> Vec<[f32; 3]> {
    built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == part)
        .flat_map(|m| m.mesh.geometry.positions.iter().map(omuzaki_core::glam::Vec3::to_array))
        .collect()
}

#[test]
fn keep_both_builds_ours_and_enabling_theirs_builds_their_look() {
    omuzaki_char::link();
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", AppOptions::studio(person())).unwrap();
    let main = core.runner();
    run(main.as_ref(), "character.create", json!({"id": "mia", "name": "Mia", "template": "soft-sd"}));
    // The kit's shirt: both sides change its length.
    let part = "shirt";
    let at = format!("characters/mia/parts/{part}/params/length");
    let length = get(main.as_ref(), &at).and_then(|v| v.as_f64()).expect("the shirt has a length");

    let oval = core
        .branches()
        .create(NewBranch::new(BranchName::new("aria/oval").unwrap(), BranchId::main()), &person())
        .unwrap();
    let side = core.branches().lane(&oval.id).unwrap();
    run(side.runner.as_ref(), "test.put", json!({"path": at, "value": length * 0.8}));
    let theirs = geometry(&build(side.runner.as_ref()), part);
    core.branches().checkpoint(&oval.id, "shorter", None, &person()).unwrap();
    run(main.as_ref(), "test.put", json!({"path": at, "value": length * 0.9}));
    let ours = geometry(&build(main.as_ref()), part);
    assert_ne!(ours, NOTHING);
    assert_ne!(ours, theirs, "the two sides' shirts differ");

    let unit = format!("characters/mia/parts/{part}");
    let merged = run(main.as_ref(), "branch.merge", json!({"from": "aria/oval", "resolve": {unit: "both"}}));
    let kept = format!("{part}-oval");
    assert!(merged.to_string().contains(&kept), "{merged:#}");

    // The character builds with ours visible and theirs listed, disabled.
    let built = build(main.as_ref());
    assert_eq!(geometry(&built, part), ours);
    assert_eq!(geometry(&built, &kept), NOTHING, "a disabled part builds nothing");
    let listed = get(main.as_ref(), &format!("characters/mia/parts/{kept}")).unwrap();
    assert_eq!(listed["enabled"], json!(false));
    assert_eq!(
        get(main.as_ref(), &format!("characters/mia/sources/{kept}")),
        get(main.as_ref(), &format!("characters/mia/sources/{part}")),
        "the source row comes along, so rights carry over"
    );

    // One undo takes the whole merge back; redo brings it again.
    assert!(core.undo().unwrap().is_some());
    assert_eq!(get(main.as_ref(), &format!("characters/mia/parts/{kept}")), None);
    assert_eq!(get(main.as_ref(), &format!("characters/mia/sources/{kept}")), None);
    assert!(core.redo().unwrap().is_some());

    // Theirs enabled in place of ours builds theirs' look.
    run(main.as_ref(), "part.set_enabled", json!({"character": "mia", "part": kept, "enabled": true}));
    run(main.as_ref(), "part.set_enabled", json!({"character": "mia", "part": part, "enabled": false}));
    let built = build(main.as_ref());
    assert_eq!(geometry(&built, part), NOTHING);
    assert_eq!(geometry(&built, &kept), theirs, "theirs builds as it did on its branch");
    drop(side);
    core.close().unwrap();
}
