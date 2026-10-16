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
    /// The transcript's calls in registered op names, built once. The transcript records what the server was called
    /// with, which is an MCP tool name (`face_set_profile`); a task writes op names (`face.set_profile`). A name is
    /// read through the server's catalog ([`ToolNames`](crate::eval::transcript::ToolNames)), or accepted as it is when
    /// it is already a registered op. A name that is neither is a transcript the harness cannot read, and fails every
    /// `log.*` check as an `error`: guessing would let `log.within` reject ordinary edits, or `log.previewed` pass
    /// because it never recognised the edits it was to look at.
    fn calls(&self) -> Result<&[ToolCall], Verdict> {
        self.calls
            .get_or_init(|| {
                let registered: Vec<&str> = self.result.ops().iter().map(|info| info.name).collect();
                self.transcript
                    .calls
                    .iter()
                    .map(|call| {
                        let op = match self.tool_names.as_ref().and_then(|names| names.op_of(&call.op)) {
                            Some(op) => op,
                            None if registered.contains(&call.op.as_str())
                                || self.tool_names.as_ref().is_some_and(|names| names.knows_op(&call.op)) =>
                            {
                                call.op.as_str()
                            }
                            None => {
                                return Err(Verdict::error(format!(
                                    "the transcript calls `{}`, which is neither a tool of the server nor a registered op",
                                    call.op
                                )));
                            }
                        };
                        Ok(ToolCall { op: op.to_owned(), ..call.clone() })
                    })
                    .collect()
            })
            .as_deref()
            .map_err(Clone::clone)
    }

    fn applied(&self) -> Result<impl Iterator<Item = &ToolCall>, Verdict> {
        Ok(self.calls()?.iter().filter(|c| c.applied()))
    }

    pub(super) fn log_applied(&self, op: &str, expected: bool) -> Verdict {
        let found = match self.applied() {
            Ok(mut applied) => applied.any(|c| glob_match(op, &c.op)),
            Err(verdict) => return verdict,
        };
        Verdict::when(found == expected, format!("`{op}` {} applied", if found { "was" } else { "was never" }))
    }

    pub(super) fn log_within(&self, ops: &[String]) -> Verdict {
        let mut applied = match self.applied() {
            Ok(applied) => applied,
            Err(verdict) => return verdict,
        };
        match applied.find(|c| !ops.iter().any(|allowed| glob_match(allowed, &c.op))) {
            Some(call) => Verdict::fail(format!("`{}` was applied but is not in the allowed list", call.op)),
            None => Verdict::pass("every applied op is allowed"),
        }
    }

    pub(super) fn log_max_errors(&self, n: u64) -> Verdict {
        let calls = match self.calls() {
            Ok(calls) => calls,
            Err(verdict) => return verdict,
        };
        let errors = calls.iter().filter(|c| !c.ok).count();
        Verdict::when(
            u64::try_from(errors).is_ok_and(|e| e <= n),
            format!("{errors} tool calls returned an error; at most {n} allowed"),
        )
    }

    /// Every applied edit was dry-run first, successfully, with the same arguments (not counting the `dry_run` flag
    /// the preview carries over the wire).
    pub(super) fn log_previewed(&self) -> Verdict {
        let edits: Vec<&str> = self.result.ops().iter().filter(|i| i.kind == OpKind::Edit).map(|i| i.name).collect();
        let calls = match self.calls() {
            Ok(calls) => calls,
            Err(verdict) => return verdict,
        };
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
