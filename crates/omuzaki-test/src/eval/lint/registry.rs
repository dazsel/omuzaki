//! What the lint reads from the linked op registry: the tool descriptions and server instructions an agent reads, and the
//! ops a check may call (design 3.3).
//!
//! The registry is the one `omuzaki mcp` serves, built by the same constructor, so the lint reads what an agent reads and
//! cannot drift from it. Nothing is dumped to a file: the texts come from [`omuzaki_mcp::McpServer`] over a runner on the ops
//! linked into this binary.

use std::collections::BTreeMap;

use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{Grants, OpInfo, OpKind, Scope};
use rmcp::ServerHandler;

use super::{Finding, ToolText, finding};
use crate::eval::task::{Check, CheckKind, Region};
use crate::eval::{Discovery, TaskFile};

/// The ops and texts of the linked registry.
#[derive(Debug, Clone)]
pub struct Linked {
    /// Every registered op.
    pub ops: Vec<OpInfo>,
    /// The server instructions and every tool's description.
    pub texts: Vec<ToolText>,
}

/// Builds the registry the way `omuzaki mcp` does, with every scope granted so that every description is the longest it
/// can be.
///
/// # Errors
/// A message when the runner cannot be built or an op cannot become a tool: the lint cannot run, which is harness trouble.
pub fn linked() -> Result<Linked, String> {
    // The ops are linked into the binary that calls this; `link()` keeps the linker from dropping them (ADR 0015).
    omuzaki_cli::link();
    let cli = omuzaki_cli::cli::try_parse_from(["omuzaki", "ops"]).map_err(|e| e.to_string())?;
    let runner = omuzaki_cli::session::open_runner(&cli).map_err(|e| e.to_string())?;
    let ops = runner.ops();
    let server = McpServer::new(runner, McpOptions::with_grants(Grants::person())).map_err(|e| e.to_string())?;
    let mut texts = Vec::new();
    if let Some(instructions) = server.session().get_info().instructions {
        texts.push(ToolText { source: "server instructions".to_owned(), text: instructions });
    }
    for tool in server.tools() {
        let description = tool.description.as_deref().unwrap_or_default().to_owned();
        texts.push(ToolText { source: format!("tool {}", tool.name), text: description });
    }
    Ok(Linked { ops, texts })
}

/// Every op a check calls, with what it calls it for.
fn ops_of(check: &Check) -> Vec<&str> {
    match &check.kind {
        CheckKind::Query { op, .. } => vec![op],
        CheckKind::Gate { ops, .. } => ops.iter().map(String::as_str).collect(),
        CheckKind::RenderSame { render } | CheckKind::Judge { render: Some(render), .. } => vec![&render.op],
        CheckKind::RenderDiff { render, region, .. } => {
            let mut ops = vec![render.op.as_str()];
            if let Region::FromOp(from) = region {
                ops.push(&from.op);
            }
            ops
        }
        _ => Vec::new(),
    }
}

/// Why `op` may not be called by a check, if it may not: it is not registered, it is not a query, or its scope is more
/// than `read` or `render`. Checks never change anything.
fn why_not(ops: &BTreeMap<&str, &OpInfo>, op: &str) -> Option<String> {
    let Some(info) = ops.get(op) else {
        return Some(format!("`{op}` is not a registered op"));
    };
    if info.kind != OpKind::Query {
        return Some(format!("`{op}` is {:?}, not a query: a check never changes anything", info.kind));
    }
    if !matches!(info.scope, Scope::Read | Scope::Render) {
        return Some(format!("`{op}` has scope {:?}: a check may call only `read` or `render` ops", info.scope));
    }
    None
}

/// The op-scope rule (design 3.3): the ops a task's checks and bindings call are registered read-only queries.
#[must_use]
pub fn op_findings(discovery: &Discovery, ops: &[OpInfo]) -> Vec<Finding> {
    let by_name: BTreeMap<&str, &OpInfo> = ops.iter().map(|info| (info.name, info)).collect();
    let mut out = Vec::new();
    for file in &discovery.tasks {
        out.extend(task_op_findings(file, &by_name));
    }
    out
}

fn task_op_findings(file: &TaskFile, ops: &BTreeMap<&str, &OpInfo>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (name, bind) in &file.task.bind {
        if let Some(why) = why_not(ops, &bind.op) {
            out.push(finding(&file.path, format!("binding `{name}` calls {why}")));
        }
    }
    for check in &file.task.checks {
        for op in ops_of(check) {
            // An op named by a `{placeholder}` is filled in at grading time: only the grader can judge it.
            if !op.contains('{')
                && let Some(why) = why_not(ops, op)
            {
                out.push(finding(&file.path, format!("check `{}` calls {why}", check.id)));
            }
        }
    }
    out
}
