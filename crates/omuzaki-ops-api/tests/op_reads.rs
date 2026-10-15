//! `Op::reads` (design `comments.md` section 8): the default declares nothing, an override is computed from the
//! parsed arguments, and `DynOp` forwards it.

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{DynOp, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn path(s: &str) -> DocPath {
    DocPath::parse(s).expect("test path parses")
}

fn meta(name: &'static str) -> OpMeta {
    OpMeta { name, summary: "A test op.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

/// Does not override `reads`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Plain {}

impl Op for Plain {
    fn meta() -> OpMeta {
        meta("test.reads_plain")
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}

/// Reads the character its argument names.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Export {
    character: String,
}

impl Op for Export {
    fn meta() -> OpMeta {
        meta("test.reads_export")
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn reads(&self) -> Vec<DocPath> {
        DocPath::parse(&format!("characters/{}", self.character)).into_iter().collect()
    }
}

#[test]
fn default_reads_is_empty() {
    assert!(Op::reads(&Plain {}).is_empty());
}

#[test]
fn override_is_computed_from_arguments() {
    let op = Export { character: "ana".into() };
    assert_eq!(Op::reads(&op), vec![path("characters/ana")]);
    let other = Export { character: "bo".into() };
    assert_eq!(Op::reads(&other), vec![path("characters/bo")]);
}

#[test]
fn dyn_op_forwards_reads() {
    let plain: Box<dyn DynOp> = Box::new(Plain {});
    assert!(plain.reads().is_empty());
    let export: Box<dyn DynOp> = Box::new(Export { character: "ana".into() });
    assert_eq!(export.reads(), vec![path("characters/ana")]);
}
