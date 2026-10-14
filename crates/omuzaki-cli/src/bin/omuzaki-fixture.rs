//! `omuzaki` with two demo ops linked in, so the end-to-end tests (`tests/e2e.rs`) have something to run.
//!
//! The real `omuzaki` binary links the product's crates (`main.rs`); this one links only the demo ops. It
//!  is the same program ([`omuzaki_cli::app::run`]) with the two ops below registered the way every op registers
//! itself, which is all an end-to-end test of the CLI and `omuzaki mcp` needs. It is a test fixture, not a product.

use std::process::ExitCode;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
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

fn main() -> ExitCode {
    omuzaki_cli::app::run()
}
