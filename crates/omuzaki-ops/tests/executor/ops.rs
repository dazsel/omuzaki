//! Ops the tests run. Each exercises one path through the executor; they register like production ops.

use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::rig;

const fn meta(name: &'static str, summary: &'static str, kind: OpKind, scope: Scope) -> OpMeta {
    let undo = if matches!(kind, OpKind::Query) { Undo::None } else { Undo::Auto };
    OpMeta { name, summary, kind, scope, undo }
}

/// The parent of `path` must exist: the document never creates parents implicitly.
fn check_parent(ctx: &OpCtx<'_>, path: &DocPath) -> Result<(), OpError> {
    let (parent, _) = path.split_last().ok_or_else(|| OpError::invalid("path", "can't replace the root"))?;
    if ctx.doc.get(&parent).is_some() {
        Ok(())
    } else {
        Err(OpError::not_found(format!("no parent {parent}")).hint("create the parent first"))
    }
}

/// Sets the value at a path (needs `edit:character`).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Set {
    pub path: DocPath,
    pub value: Value,
}

impl Op for Set {
    fn meta() -> OpMeta {
        meta("test.set", "Set the value at a path.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_parent(ctx, &self.path)?;
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }
}
register_op!(Set);

/// Sets the value at a path, as an absolute setter that opts in to coalescing (`Op::coalesces`).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Setpoint {
    pub path: DocPath,
    pub value: Value,
}

impl Op for Setpoint {
    fn meta() -> OpMeta {
        meta("test.setpoint", "Set the value at a path; repeated calls coalesce.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_parent(ctx, &self.path)?;
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }

    fn coalesces() -> bool {
        true
    }
}
register_op!(Setpoint);

/// Removes the value at a path.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Remove {
    pub path: DocPath,
}

impl Op for Remove {
    fn meta() -> OpMeta {
        meta("test.remove", "Remove the value at a path.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&self.path).cloned().ok_or_else(|| OpError::not_found("nothing to remove"))?;
        Ok(Plan::edit(Patch::new().with(self.path.clone(), Some(before), None)))
    }
}
register_op!(Remove);

/// Sets a path only if it still holds `expect` (optimistic concurrency: the caller read the value earlier).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SetIf {
    pub path: DocPath,
    pub expect: Option<Value>,
    pub value: Value,
}

impl Op for SetIf {
    fn meta() -> OpMeta {
        meta("test.set_if", "Set a path if it still holds the expected value.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(self.path.clone(), self.expect.clone(), Some(self.value.clone()))))
    }
}
register_op!(SetIf);

/// An edit that needs a scope agents don't have by default.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Publish {
    pub path: DocPath,
    pub value: Value,
}

impl Op for Publish {
    fn meta() -> OpMeta {
        meta("test.publish", "Record a publication at a path.", OpKind::Edit, Scope::Publish)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_parent(ctx, &self.path)?;
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }
}
register_op!(Publish);

/// Writes the planning clock's time into the document.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Stamp {
    pub path: DocPath,
}

impl Op for Stamp {
    fn meta() -> OpMeta {
        meta("test.stamp", "Write the current time at a path.", OpKind::Edit, Scope::EditScene)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_parent(ctx, &self.path)?;
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(json!(ctx.clock.now().0)))))
    }
}
register_op!(Stamp);

/// Reads a value (needs `read`).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Get {
    pub path: DocPath,
}

impl Op for Get {
    fn meta() -> OpMeta {
        meta("test.get", "Read the value at a path.", OpKind::Query, Scope::Read)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.doc.get(&self.path).cloned().map(Plan::answer).ok_or_else(|| OpError::not_found("nothing there"))
    }
}
register_op!(Get);

/// A query behind a scope other than `read` (like a preview render).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ScopedQuery {
    pub path: DocPath,
}

impl Op for ScopedQuery {
    fn meta() -> OpMeta {
        meta("test.scoped_query", "Read a value behind the render scope.", OpKind::Query, Scope::Render)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.doc.get(&self.path).cloned().map(Plan::answer).ok_or_else(|| OpError::not_found("nothing there"))
    }
}
register_op!(ScopedQuery);

/// A buggy query that plans a change.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SneakyQuery {
    pub path: DocPath,
}

impl Op for SneakyQuery {
    fn meta() -> OpMeta {
        meta("test.sneaky_query", "A query that tries to change the document.", OpKind::Query, Scope::Read)
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(self.path.clone(), None, Some(json!("sneaked")))))
    }
}
register_op!(SneakyQuery);

/// A buggy edit whose patch goes through a parent that doesn't exist.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Broken {}

impl Op for Broken {
    fn meta() -> OpMeta {
        meta("test.broken", "An edit with an unappliable patch.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let orphan = DocPath::parse("no/such/parent").map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::edit(Patch::new().with(orphan, None, Some(json!(1)))))
    }
}
register_op!(Broken);

/// An edit that always refuses.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Reject {}

impl Op for Reject {
    fn meta() -> OpMeta {
        meta("test.reject", "An edit that fails validation.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Err(OpError::invalid("blend", "out of range").hint("use 0..1"))
    }
}
register_op!(Reject);

/// An op whose planning panics, to check the executor survives a poisoned lock.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Explode {}

impl Op for Explode {
    fn meta() -> OpMeta {
        meta("test.explode", "An op that panics while planning.", OpKind::Edit, Scope::EditCharacter)
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        panic!("test.explode panics on purpose");
    }
}
register_op!(Explode);

#[test]
fn ops_report_whether_they_coalesce() {
    let ops = rig(json!({})).exec.ops();
    let coalesces = |name: &str| ops.iter().find(|o| o.name == name).unwrap().coalesces;
    assert!(coalesces("test.setpoint"));
    assert!(!coalesces("test.set"));
}
