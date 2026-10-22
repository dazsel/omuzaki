//! The project's own ops over its files (journal.md 6): `project.save` makes the files current now, and
//! `project.reload` takes back in what was edited outside Omuzaki, as one undoable step.

use std::time::Duration;

use omuzaki_doc_api::Patch;
use omuzaki_history_api::{SnapshotError, Snapshots};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// How long `project.save` waits for the files.
const SAVE_WAIT: Duration = Duration::from_secs(30);

fn snapshot_error(error: SnapshotError) -> OpError {
    match error {
        SnapshotError::Invalid(reason) => OpError::invalid("paths", reason),
        SnapshotError::Timeout => OpError::conflict("the project files took too long to save")
            .hint("the disk may be busy or full; try again, and check the status bar for the reason"),
        other => OpError::conflict(other.to_string()).hint("check the status bar for why saving fails"),
    }
}

/// Writes the project files now and returns once they match the document. Files trail edits by up to about two
/// seconds; call this before reading or editing them directly, or committing them.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ProjectSave {}

impl Op for ProjectSave {
    fn meta() -> OpMeta {
        OpMeta {
            name: "project.save",
            summary: "Write the project files now, and return when they match the document.",
            kind: OpKind::Edit,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let snapshots = ctx.service::<dyn Snapshots>()?;
        if !ctx.dry_run {
            snapshots.save_files(SAVE_WAIT).map_err(snapshot_error)?;
        }
        Ok(Plan::answer(json!({ "saved": !ctx.dry_run })))
    }
}
register_op!(ProjectSave);

/// Reads project files that were edited outside Omuzaki back into the document, as one undoable step. Undo puts the
/// document back, and the next save then writes it over the file again.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "paths": ["project.toml"] }]))]
pub struct ProjectReload {
    /// The files to read back, relative to the project (`characters/mia/face.toml`). Empty: every project file.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Op for ProjectReload {
    fn meta() -> OpMeta {
        OpMeta {
            name: "project.reload",
            summary: "Read project files edited outside Omuzaki back into the document, as one undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let read = ctx.service::<dyn Snapshots>()?.reload_patch(&self.paths).map_err(snapshot_error)?;
        // The service read the files; the document's current values are the `before`s, and a file that already
        // matches changes nothing.
        let mut patch = Patch::new();
        let mut changed = Vec::new();
        for change in read.changes {
            let before = ctx.doc.get(&change.path).cloned();
            if before != change.after {
                changed.push(change.path.to_string());
                patch = patch.with(change.path, before, change.after);
            }
        }
        let summary = json!({ "reloaded": changed });
        Ok(Plan { output: Some(summary), ..Plan::edit(patch) })
    }
}
register_op!(ProjectReload);
