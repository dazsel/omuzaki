//! The tool list: one tool per registered op, plus the built-ins. Built once at startup from `Runner::ops()`, so what
//! `tools/list` shows can't drift from what the runner can run.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use omuzaki_ops_api::{Grants, OpInfo, OpKind, Scope};
use rmcp::model::{JsonObject, MetaObject, Tool};
use serde_json::json;

use crate::builtin::Builtin;
use crate::describe::{Guidance, annotations, tool_description};
use crate::error::ServerError;
use crate::names::{checked_tool_name, tool_name};
use crate::schema::tool_schema;
use crate::signature::signature;
use crate::suggest::suggest;

/// The `_meta` key that names a tool's kind, so a client can tell what a call will do before it makes one.
const KIND_KEY: &str = "omuzaki/kind";

/// A tool's `_meta`, naming its kind: `edit`, `query`, `job` or `builtin`.
pub(crate) fn kind_meta(kind: &str) -> MetaObject {
    MetaObject::from(JsonObject::from_iter([(KIND_KEY.to_owned(), json!(kind))]))
}

/// An op's kind as `_meta` names it.
const fn kind_name(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Edit => "edit",
        OpKind::Query => "query",
        OpKind::Job => "job",
    }
}

/// Which tools `tools/list` shows.
///
/// A studio has a few hundred ops, and their schemas fill a model's context before it has done anything, so a host can
/// list a small core instead: the built-ins, plus `ops_find`, `ops_describe` and `ops_run` to reach any op, and
/// `tools_enable` to list a whole area as tools. Every op stays callable by name in both modes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolMode {
    /// One tool per op, plus the built-ins (the default).
    #[default]
    All,
    /// The built-ins and the op finder; an area's ops are listed once the session turns it on.
    Core,
}

/// What a tool does when called.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Route {
    /// Run the op of the same name.
    Op {
        kind: OpKind,
        /// The op takes no arguments (a unit struct), so it is sent `null` rather than `{}`.
        unit_args: bool,
    },
    /// Handle it in the server (undo/redo/transactions).
    Builtin(Builtin),
}

/// One tool and what the server needs to serve it.
pub(crate) struct Entry {
    /// The MCP tool: `name` is what the agent calls (`face_set_profile`), `title` the dotted op name.
    pub tool: Tool,
    /// The op's registered name (`face.set_profile`), which the runner is called with.
    pub op: String,
    pub route: Route,
    /// A usage line for error messages, e.g. `face_set_profile(character: string, blend: number)`.
    pub signature: String,
    /// The scope a call needs besides `read` (`read` itself for a built-in).
    pub scope: Scope,
}

/// All tools, sorted by name.
pub(crate) struct Catalog {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    mode: ToolMode,
}

impl Catalog {
    /// Builds the catalog.
    ///
    /// # Errors
    /// [`ServerError::UnusableOp`] for an op that can't be a tool (a name clients would reject, or an argument schema
    /// that isn't an object) and [`ServerError::DuplicateTool`] for two ops that become the same tool name. Failing
    /// here keeps the tool list identical to `ops()`.
    pub(crate) fn build(
        ops: &[OpInfo],
        grants: &Grants,
        guidance: &Guidance,
        registry: bool,
        mode: ToolMode,
    ) -> Result<Self, ServerError> {
        let mut entries = ops.iter().map(|op| op_entry(op, grants, guidance)).collect::<Result<Vec<_>, _>>()?;
        entries.extend(Builtin::ALL.into_iter().filter(|b| b.offered(registry, mode)).map(|b| {
            let tool = b.tool();
            let signature = signature(b.name(), &tool.input_schema);
            Entry { tool, op: b.title().to_owned(), route: Route::Builtin(b), signature, scope: Scope::Read }
        }));
        entries.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
        let mut index = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            if let Some(other) = index.insert(entry.tool.name.to_string(), i) {
                return Err(ServerError::DuplicateTool {
                    name: entry.tool.name.to_string(),
                    first: entries[other].op.clone(),
                    second: entry.op.clone(),
                });
            }
        }
        Ok(Self { entries, index, mode })
    }

    /// The tool called `name`. The dotted op name (`face.set_profile`, as shown in descriptions) is accepted too: an
    /// agent that copies it from there shouldn't need a round trip.
    pub(crate) fn get(&self, name: &str) -> Option<&Entry> {
        let i = self.index.get(name).or_else(|| self.index.get(&tool_name(name)))?;
        Some(&self.entries[*i])
    }

    /// Every tool, listed or not.
    pub(crate) fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.entries.iter().map(|e| &e.tool)
    }

    pub(crate) const fn mode(&self) -> ToolMode {
        self.mode
    }

    /// The tools `tools/list` shows a session that turned on the areas in `enabled`: every tool in
    /// [`ToolMode::All`]; the built-ins and the enabled areas' ops in [`ToolMode::Core`].
    pub(crate) fn listed<'a>(&'a self, enabled: &'a BTreeSet<String>) -> impl Iterator<Item = &'a Tool> {
        self.entries
            .iter()
            .filter(move |e| match (self.mode, e.area()) {
                (ToolMode::All, _) | (ToolMode::Core, None) => true,
                (ToolMode::Core, Some(area)) => enabled.contains(area),
            })
            .map(|e| &e.tool)
    }

    /// The op entries (no built-ins), sorted by tool name.
    pub(crate) fn ops(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.area().is_some())
    }

    /// Each area (the op name up to its first dot) and how many ops it has.
    pub(crate) fn areas(&self) -> BTreeMap<&str, usize> {
        let mut areas = BTreeMap::new();
        for area in self.ops().filter_map(Entry::area) {
            *areas.entry(area).or_insert(0) += 1;
        }
        areas
    }

    /// Tool names close to a misspelt `wanted`.
    pub(crate) fn similar(&self, wanted: &str) -> Vec<&str> {
        suggest(&tool_name(wanted), self.entries.iter().map(|e| e.tool.name.as_ref()))
    }
}

impl Entry {
    /// The op's area: its name up to the first dot (`face` for `face.set_profile`). `None` for a built-in.
    pub(crate) fn area(&self) -> Option<&str> {
        match self.route {
            Route::Op { .. } => Some(self.op.split('.').next().unwrap_or(&self.op)),
            Route::Builtin(_) => None,
        }
    }

    /// The op's summary: the first line of its description, without the leading `name: `.
    pub(crate) fn summary(&self) -> &str {
        let first = self.tool.description.as_deref().and_then(|d| d.lines().next()).unwrap_or_default();
        first.split_once(": ").map_or(first, |(_, summary)| summary)
    }
}

fn op_entry(op: &OpInfo, grants: &Grants, guidance: &Guidance) -> Result<Entry, ServerError> {
    let name = checked_tool_name(op.name)?;
    let schema = tool_schema(op.name, op.kind, &op.schema)?;
    let signature = signature(&name, &schema.input);
    let tool = Tool::new(name, tool_description(op, grants, guidance), schema.input)
        .with_title(op.name)
        .with_annotations(annotations(op))
        .with_meta(kind_meta(kind_name(op.kind)));
    let route = Route::Op { kind: op.kind, unit_args: schema.unit };
    Ok(Entry { tool, op: op.name.to_owned(), route, signature, scope: op.scope })
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{OpMeta, Undo};

    use super::*;

    fn op(name: &'static str, kind: OpKind) -> OpInfo {
        let meta = OpMeta { name, summary: "Does a thing.", kind, scope: Scope::Read, undo: Undo::None };
        let schema =
            serde_json::from_value(serde_json::json!({"type": "object", "properties": {"x": {"type": "number"}}}));
        OpInfo::new(meta, schema.unwrap())
    }

    fn build(ops: &[OpInfo]) -> Result<Catalog, ServerError> {
        Catalog::build(ops, &Grants::agent_default(), &Guidance::default(), false, ToolMode::All)
    }

    #[test]
    fn lists_ops_and_builtins_under_safe_names_sorted() {
        let catalog = build(&[op("render.preview", OpKind::Query), op("face.set_profile", OpKind::Edit)]).unwrap();
        let names: Vec<_> = catalog.tools().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "doc_get",
                "face_set_profile",
                "history_log",
                "history_redo",
                "history_undo",
                "ops_describe",
                "ops_find",
                "render_preview",
                "tx_begin",
                "tx_commit",
                "tx_rollback"
            ]
        );
        let entry = catalog.get("face_set_profile").unwrap();
        assert_eq!((entry.op.as_str(), entry.tool.title.as_deref()), ("face.set_profile", Some("face.set_profile")));
        assert!(entry.tool.description.as_deref().unwrap().starts_with("face.set_profile: Does a thing.\n"));
        assert!(entry.signature.starts_with("face_set_profile("), "errors speak the tool name the agent sees");
        assert!(entry.tool.input_schema["properties"]["dry_run"].is_object());
    }

    #[test]
    fn every_tool_names_its_kind_in_meta() {
        let catalog = build(&[
            op("render.preview", OpKind::Query),
            op("face.set_profile", OpKind::Edit),
            op("export.vrm", OpKind::Job),
        ])
        .unwrap();
        let kind_of =
            |name: &str| catalog.get(name).unwrap().tool.meta.as_ref().and_then(|m| m.0.get(KIND_KEY).cloned());
        assert_eq!(kind_of("render_preview"), Some(json!("query")));
        assert_eq!(kind_of("face_set_profile"), Some(json!("edit")));
        assert_eq!(kind_of("export_vrm"), Some(json!("job")));
        assert_eq!(kind_of("history_undo"), Some(json!("builtin")));
        assert_eq!(kind_of("tx_begin"), Some(json!("builtin")));
        assert!(catalog.tools().all(|tool| tool.meta.as_ref().is_some_and(|m| m.0.contains_key(KIND_KEY))));
    }

    #[test]
    fn the_dotted_op_name_also_finds_the_tool() {
        let catalog = build(&[op("face.set_profile", OpKind::Edit)]).unwrap();
        assert_eq!(catalog.get("face.set_profile").unwrap().tool.name, "face_set_profile");
        assert_eq!(catalog.get("history.undo").unwrap().tool.name, "history_undo");
    }

    #[test]
    fn builtin_names_are_reserved() {
        for clash in ["history.undo", "history_undo"] {
            let result = build(&[op(clash, OpKind::Edit)]);
            assert!(
                matches!(&result, Err(ServerError::DuplicateTool { name, .. }) if name == "history_undo"),
                "{clash}: {:?}",
                result.as_ref().err()
            );
        }
    }

    #[test]
    fn ops_that_become_the_same_tool_stop_startup_and_are_named() {
        let result = build(&[op("face.set_profile", OpKind::Edit), op("face_set.profile", OpKind::Edit)]);
        let Err(ServerError::DuplicateTool { name, first, second }) = result else { panic!("expected a clash") };
        assert_eq!(name, "face_set_profile");
        assert_eq!((first.as_str(), second.as_str()), ("face.set_profile", "face_set.profile"));
    }

    #[test]
    fn names_clients_reject_stop_startup() {
        let long = Box::leak(format!("area.{}", "x".repeat(60)).into_boxed_str());
        for bad in ["face set profile", long] {
            assert!(matches!(build(&[op(bad, OpKind::Edit)]), Err(ServerError::UnusableOp { .. })), "{bad}");
        }
    }

    #[test]
    fn unknown_names_get_suggestions_in_the_safe_form() {
        let catalog = build(&[op("face.set_profile", OpKind::Edit)]).unwrap();
        assert_eq!(catalog.similar("set_profile"), ["face_set_profile"]);
        assert_eq!(catalog.similar("face.set_profil"), ["face_set_profile"]);
        assert!(catalog.get("nope").is_none());
    }
}
