//! `omuzaki` with demo ops linked in, so the end-to-end tests (`tests/e2e.rs`, `tests/crash_recovery.rs`,
//! `tests/headless.rs`) have something to run.
//!
//! The real `omuzaki` binary links the product's crates (`main.rs`); this one adds the demo ops. It is the same
//! program ([`omuzaki_cli::app::run`]) with the ops below registered the way every op registers itself, which is all
//! an end-to-end test of the CLI and `omuzaki mcp` needs. It is a test fixture, not a product.

use std::process::ExitCode;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Where the demo ops keep their note: a key in `project.toml`.
fn note_path() -> DocPath {
    DocPath::root().join("project").join("note")
}

/// Set the project's note.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetNote {
    /// The note's text.
    #[schemars(length(min = 1, max = 80))]
    text: String,
}

impl Op for SetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.set_note",
            summary: "Set the project's note.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = note_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.text)))))
    }
}

register_op!(SetNote);

/// Read the project's note.
#[derive(Serialize, Deserialize, JsonSchema)]
struct GetNote {}

impl Op for GetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.get_note",
            summary: "Read the project's note.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "note": ctx.doc.get(&note_path()) })))
    }
}

register_op!(GetNote);

/// One of the demo fields a crash test sets.
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Field {
    A,
    B,
    C,
    D,
}

impl Field {
    fn path(self) -> DocPath {
        let key = match self {
            Self::A => "field_a",
            Self::B => "field_b",
            Self::C => "field_c",
            Self::D => "field_d",
        };
        DocPath::root().join("project").join(key)
    }
}

/// Set one of the project's demo fields.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetField {
    /// Which field.
    field: Field,
    /// Its text.
    #[schemars(length(min = 1, max = 80))]
    text: String,
}

impl Op for SetField {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.set_field",
            summary: "Set one of the project's demo fields.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = self.field.path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.text)))))
    }
}

register_op!(SetField);

/// Read the project's table: its name, note and demo fields.
#[derive(Serialize, Deserialize, JsonSchema)]
struct GetProject {}

impl Op for GetProject {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.get_project",
            summary: "Read the project's table.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "project": ctx.doc.get(&DocPath::root().join("project")) })))
    }
}

register_op!(GetProject);

/// Publish the project's note (pretend): a live action that needs `publish`, for the headless grant tests
/// (`tests/headless.rs`).
#[derive(Serialize, Deserialize, JsonSchema)]
struct PublishNote {}

impl Op for PublishNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.publish_note",
            summary: "Publish the project's note (a demo: nothing leaves the machine).",
            kind: OpKind::Edit,
            scope: Scope::Publish,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "published": ctx.doc.get(&note_path()) })))
    }
}

register_op!(PublishNote);

/// Set the project's note through a paid service (pretend): an edit that costs 40 cents, for the headless budget tests.
#[derive(Serialize, Deserialize, JsonSchema)]
struct PaidNote {
    /// The note's text.
    #[schemars(length(min = 1, max = 80))]
    text: String,
}

impl Op for PaidNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "demo.paid_note",
            summary: "Set the project's note through a paid service (a demo: it charges the session 40 cents).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = note_path();
        let mut plan = Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.text))));
        plan.cost = Some(CostHint { usd: Some(0.4), ..CostHint::default() });
        Ok(plan)
    }
}

register_op!(PaidNote);

fn main() -> ExitCode {
    omuzaki_cli::app::run_with(abort_at_switch_step)
}

/// `OMUZAKI_FIXTURE_ABORT_AT=switch:marked` (or another step of a branch switch) kills the process there, with no
/// unwinding and nothing flushed, as a crash would (`tests/branches_crash.rs`).
fn abort_at_switch_step(options: omuzaki_app::AppOptions) -> omuzaki_app::AppOptions {
    let Ok(step) = std::env::var("OMUZAKI_FIXTURE_ABORT_AT") else { return options };
    options.with_switch_failpoint(move |at| if at == step { std::process::abort() } else { Ok(()) })
}
