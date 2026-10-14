//! End-to-end check of the op contract: register an op in its own file, find it by name, decode JSON args,
//! plan against a document, apply, and undo with the inverse patch.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, find, registrations};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Set the blend between a character's base face and its face-shape profile.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetProfileBlend {
    /// The character.
    character: omuzaki_core::id::Slug,
    /// 0 = base face, 1 = full profile.
    blend: f32,
}

impl Op for SetProfileBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_profile_blend",
            summary: "Set the blend between the base face and the face-shape profile.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.blend) {
            return Err(OpError::invalid("blend", format!("{} is out of range", self.blend)).hint("use 0..1"));
        }
        let ch =
            DocPath::parse("characters").map_err(|e| OpError::internal(e.to_string()))?.join(self.character.as_str());
        if ctx.doc.get(&ch).is_none() {
            return Err(OpError::not_found(format!("no character {}", self.character)));
        }
        let path = ch.join("face").join("blend");
        Ok(Plan::edit(Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), Some(json!(self.blend)))))
    }
}

omuzaki_ops_api::register_op!(SetProfileBlend);

fn plan(doc: &MemDoc, args: serde_json::Value) -> Result<Plan, OpError> {
    let reg = find("test.set_profile_blend").expect("registered");
    let op = (reg.decode)(args)?;
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Agent { id: "test".into(), model: None };
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

#[test]
fn registered_op_is_discoverable_with_schema() {
    assert!(registrations().any(|r| (r.meta)().name == "test.set_profile_blend"));
    let reg = find("test.set_profile_blend").unwrap();
    let schema = serde_json::to_value((reg.schema)()).unwrap();
    assert!(schema["properties"]["blend"].is_object(), "schema: {schema}");
    assert_eq!(schema["properties"]["character"]["pattern"], "^[a-z0-9][a-z0-9_-]{0,63}$");
}

#[test]
fn dry_run_apply_and_undo() {
    let mut doc = MemDoc::from_value(json!({"characters": {"mira": {"face": {}}}}));
    let before = doc.clone();
    let p = plan(&doc, json!({"character": "mira", "blend": 0.6})).unwrap();
    assert_eq!(doc.version().0, 0, "planning must not change the document");
    doc.apply(&p.patch).unwrap();
    assert_eq!(doc.get(&DocPath::parse("characters/mira/face/blend").unwrap()), Some(&json!(0.6_f32)));
    doc.apply(&p.patch.inverse()).unwrap();
    assert_eq!(doc.root(), before.root());
}

#[test]
fn errors_name_the_field_and_hint_a_fix() {
    let doc = MemDoc::from_value(json!({"characters": {"mira": {}}}));
    let e = plan(&doc, json!({"character": "mira", "blend": 2.0})).unwrap_err();
    assert_eq!((e.code, e.field.as_deref(), e.hint.as_deref()), (ErrorCode::Invalid, Some("blend"), Some("use 0..1")));
    let e = plan(&doc, json!({"character": "nobody", "blend": 0.5})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = plan(&doc, json!({"character": "Bad Name", "blend": 0.5})).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("character")));
}
