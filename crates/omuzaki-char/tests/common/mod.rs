//! The op-test harness shared by the `omuzaki-char` integration tests.
//!
//! Each op is decoded from JSON and planned exactly as the executor does. [`round_trip`] spells out the editing contract
//! every edit keeps: a dry-run changes nothing, applying the patch changes the document, its inverse restores it
//! exactly, applying it again gives the same document as the first apply, and no written value holds a `null`. The
//! project files have no null, so an edit that wrote one would not survive a save.
//!
//! `omuzaki-ops` is an implementation crate, so the tests do not depend on it: `omuzaki-ops-api` gives the registry and the
//! planning context, and the executor's rules are repeated here.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Each test binary uses the part of the harness it needs.
#![allow(dead_code)]

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_char_mock::CHARACTER_JSON;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// Decodes `args` for `op` and plans it against `doc` as a dry-run.
pub fn plan(doc: &MemDoc, op: &str, args: Value) -> Result<Plan, OpError> {
    // Links this crate's ops into the test binary, as every App does at startup (ADR 0015): nothing else here names it.
    omuzaki_char::link();
    let registration = find(op).unwrap_or_else(|| panic!("{op} isn't registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock).with_dry_run(true))
}

/// Whether `value` holds a `null` anywhere in it.
fn holds_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(holds_null),
        Value::Object(fields) => fields.values().any(holds_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// Plans `op` and checks the editing contract on `doc`, leaving `doc` with the edit applied.
pub fn round_trip(doc: &mut MemDoc, op: &str, args: Value) -> Plan {
    let original = doc.root().clone();
    let planned = plan(doc, op, args).unwrap_or_else(|e| panic!("{op} was refused: {e:?}"));
    assert_eq!(doc.root(), &original, "{op}: planning (a dry-run) must not change the document");
    assert!(!planned.patch.is_empty(), "{op}: expected a change");
    for change in &planned.patch.changes {
        assert!(!change.after.as_ref().is_some_and(holds_null), "{op}: writes a null at {}", change.path);
    }

    doc.apply(&planned.patch).unwrap();
    let applied = doc.root().clone();
    assert_ne!(applied, original, "{op}: applying changes the document");

    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &original, "{op}: undo must restore the document exactly");

    doc.apply(&planned.patch).unwrap();
    assert_eq!(doc.root(), &applied, "{op}: redo must give the same document as the first apply");
    planned
}

/// Plans `op` and applies it (no round trip), for setting a scene up.
pub fn apply(doc: &mut MemDoc, op: &str, args: Value) -> Plan {
    let planned = plan(doc, op, args).unwrap_or_else(|e| panic!("{op} was refused: {e:?}"));
    doc.apply(&planned.patch).unwrap();
    planned
}

/// Plans `op` and expects it to be refused.
pub fn refused(doc: &MemDoc, op: &str, args: Value) -> OpError {
    plan(doc, op, args).expect_err("the op should have been refused")
}

/// Checks an error's class, the argument it names, and that its message and hint say what the caller needs.
#[track_caller]
pub fn assert_error(e: &OpError, code: ErrorCode, field: &str, message: &[&str], hint: &[&str]) {
    assert_eq!((e.code, e.field.as_deref()), (code, Some(field)), "{e:?}");
    for part in message {
        assert!(e.message.contains(part), "the message should say {part:?}: {}", e.message);
    }
    let hint_text = e.hint.as_deref().unwrap_or_else(|| panic!("no hint on {e:?}"));
    for part in hint {
        assert!(hint_text.contains(part), "the hint should say {part:?}: {hint_text}");
    }
}

/// The mock character's stored node, as the fixture writes it.
pub fn mock_node() -> Value {
    serde_json::from_str(CHARACTER_JSON).unwrap()
}

/// A project holding the mock character as `mock` (body, head, hair, shirt, vest). The fixture leaves out defaults
/// (`enabled`, `layer`, `parent`), as a hand-written project file does.
pub fn project() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mock": mock_node() } }))
}

/// The value stored at `path`, if any.
pub fn at(doc: &MemDoc, path: &str) -> Option<Value> {
    doc.get(&DocPath::parse(path).unwrap()).cloned()
}

/// The character as the typed format sees it: only its definition keys, so the variants, colourways, rig, look and
/// licence beside it (each owned by another file or op) are left out, exactly as the ops read it.
pub fn definition(doc: &MemDoc, id: &str) -> CharacterDef {
    CharacterDef::from_node(&at(doc, &format!("characters/{id}")).unwrap()).unwrap()
}

/// Builds the character `id` with the registry builder and default options.
pub fn build(doc: &MemDoc, id: &str) -> BuiltCharacter {
    let (char_id, def) = (CharId::new(id).unwrap(), definition(doc, id));
    RegistryBuilder
        .build(&char_id, &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("{id} doesn't build: {e}"))
}
