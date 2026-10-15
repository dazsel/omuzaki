//! The `log.*` checks: facts from the transcript, which records what the server returned.

use omuzaki_ops_api::{Mode, OpKind};
use serde_json::Value;

use super::Checker;
use super::report::Verdict;
use super::value::glob_match;
use crate::eval::transcript::ToolCall;

/// The wire flag that makes an MCP call a preview: transport, not an argument of the op.
const DRY_RUN_FLAG: &str = "dry_run";

/// Whether a preview's arguments and an apply's are the same call, once the `dry_run` flag is set aside.
fn same_args(preview: &Value, applied: &Value) -> bool {
    fn bare(args: &Value) -> Value {
        match args {
            Value::Object(map) => Value::Object(
                map.iter().filter(|(k, _)| *k != DRY_RUN_FLAG).map(|(k, v)| (k.clone(), v.clone())).collect(),
            ),
            other => other.clone(),
        }
    }
    bare(preview) == bare(applied)
}

impl Checker {
    fn applied(&self) -> impl Iterator<Item = &ToolCall> {
        self.transcript.calls.iter().filter(|c| c.applied())
    }

    pub(super) fn log_applied(&self, op: &str, expected: bool) -> Verdict {
        let found = self.applied().any(|c| glob_match(op, &c.op));
        Verdict::when(found == expected, format!("`{op}` {} applied", if found { "was" } else { "was never" }))
    }

    pub(super) fn log_within(&self, ops: &[String]) -> Verdict {
        match self.applied().find(|c| !ops.iter().any(|allowed| glob_match(allowed, &c.op))) {
            Some(call) => Verdict::fail(format!("`{}` was applied but is not in the allowed list", call.op)),
            None => Verdict::pass("every applied op is allowed"),
        }
    }

    pub(super) fn log_max_errors(&self, n: u64) -> Verdict {
        let errors = self.transcript.calls.iter().filter(|c| !c.ok).count();
        Verdict::when(
            u64::try_from(errors).is_ok_and(|e| e <= n),
            format!("{errors} tool calls returned an error; at most {n} allowed"),
        )
    }

    /// Every applied edit was dry-run first, successfully, with the same arguments (not counting the `dry_run` flag
    /// the preview carries over the wire).
    pub(super) fn log_previewed(&self) -> Verdict {
        let edits: Vec<&str> = self.result.ops().iter().filter(|i| i.kind == OpKind::Edit).map(|i| i.name).collect();
        let calls = &self.transcript.calls;
        for (index, call) in calls.iter().enumerate().filter(|(_, c)| c.applied() && edits.contains(&c.op.as_str())) {
            let previewed = calls[..index]
                .iter()
                .any(|c| c.mode == Mode::DryRun && c.ok && c.op == call.op && same_args(&c.args, &call.args));
            if !previewed {
                return Verdict::fail(format!("`{}` was applied without a dry run with the same arguments", call.op));
            }
        }
        Verdict::pass("every applied edit was previewed")
    }
}
