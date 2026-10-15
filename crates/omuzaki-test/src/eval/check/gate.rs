//! The `gate` check: gate ops pass on the result, or the failed gates the project declares are none.

use serde_json::{Map, Value};

use super::Checker;
use super::query::allowed;
use super::report::Verdict;
use super::value::{call, pointer_all, result_failure};

/// The op that lists a project's declared gates (VF-09).
const DESCRIBE_OP: &str = "project.describe";
/// Where `project.describe` lists the gates that fail, as an array of names (confirmed shape for VF-09).
const FAILED_GATES: &str = "/gates/failed";
/// Where a gate op's output says whether it passed, as a boolean. Anything else (absent, not a boolean) is an `error`:
/// a gate op that answers in another shape cannot pass vacuously, and is not the agent's fault either.
const PASSED: &str = "/passed";

impl Checker {
    pub(super) fn gate(&self, ops: &[String], declared: bool) -> Verdict {
        let mut failed = Vec::new();
        for op in ops {
            if let Err(refused) = allowed(&*self.result, op) {
                return refused;
            }
            match call(&*self.result, op, &Map::new()) {
                Ok(output) => match pointer_all(&output, PASSED) {
                    Ok(found) => match found.as_slice() {
                        [Value::Bool(true)] => {}
                        [Value::Bool(false)] => failed.push(op.clone()),
                        _ => return Verdict::error(format!("gate `{op}` has no boolean `{PASSED}` in its answer")),
                    },
                    Err(verdict) => return verdict,
                },
                Err(e) => return result_failure(&format!("gate `{op}`"), &e),
            }
        }
        if declared {
            match self.declared_failures() {
                Ok(names) => failed.extend(names),
                Err(verdict) => return verdict,
            }
        }
        Verdict::when(
            failed.is_empty(),
            if failed.is_empty() {
                "every gate passes".to_owned()
            } else {
                format!("failing gates: {}", failed.join(", "))
            },
        )
    }

    /// The gates the result project declares and fails, read from `project.describe`.
    fn declared_failures(&self) -> Result<Vec<String>, Verdict> {
        if !self.result.ops().iter().any(|info| info.name == DESCRIBE_OP) {
            return Err(Verdict::error(format!(
                "declared gates are not available: no `{DESCRIBE_OP}` op is registered"
            )));
        }
        allowed(&*self.result, DESCRIBE_OP)?;
        let output = call(&*self.result, DESCRIBE_OP, &Map::new()).map_err(|e| result_failure(DESCRIBE_OP, &e))?;
        match pointer_all(&output, FAILED_GATES)?.as_slice() {
            [Value::Array(names)] => names
                .iter()
                .map(|n| {
                    n.as_str().map(str::to_owned).ok_or_else(|| {
                        Verdict::error(format!("`{DESCRIBE_OP}` lists a failed gate that is not a name: {n}"))
                    })
                })
                .collect(),
            _ => Err(Verdict::error(format!("`{DESCRIBE_OP}` has no `{FAILED_GATES}` list"))),
        }
    }
}
