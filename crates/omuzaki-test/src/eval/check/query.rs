//! The `query` check: a read-only op's output at a pointer meets an expectation.

use omuzaki_ops_api::{OpInfo, Runner, Scope};
use serde_json::{Map, Value};

use super::Checker;
use super::report::Verdict;
use super::value::{call, moved, pointer_all, result_failure, same_value};
use crate::eval::task::QueryExpect;

/// The single value at `at` in an op's output.
fn single(output: &Value, at: &str) -> Result<Value, Verdict> {
    match pointer_all(output, at).as_slice() {
        [one] => Ok((*one).clone()),
        found => Err(Verdict::fail(format!("`{at}` reached {} values; a query reads exactly one", found.len()))),
    }
}

/// The op's info, if it exists and is safe for a check to call: its scope must be `read` or `render`.
pub(super) fn allowed(runner: &dyn Runner, op: &str) -> Result<OpInfo, Verdict> {
    let info = runner
        .ops()
        .into_iter()
        .find(|info| info.name == op)
        .ok_or_else(|| Verdict::error(format!("`{op}` is not a registered op")))?;
    if matches!(info.scope, Scope::Read | Scope::Render) {
        Ok(info)
    } else {
        Err(Verdict::error(format!(
            "`{op}` is not read-only (scope {:?}); a query may call only read or render ops",
            info.scope
        )))
    }
}

impl Checker {
    pub(super) fn query(&self, op: &str, args: &Map<String, Value>, at: &str, expect: &QueryExpect) -> Verdict {
        if expect.equals.is_none() && expect.range.is_none() && expect.change.is_none() {
            return Verdict::error("`expect` names no condition: a query that expects nothing passes whatever happens");
        }
        if let Err(refused) = allowed(&*self.result, op) {
            return refused;
        }
        let after = match call(&*self.result, op, args) {
            Ok(output) => match single(&output, at) {
                Ok(value) => value,
                Err(verdict) => return verdict,
            },
            Err(e) => return result_failure(&format!("`{op}`"), &e),
        };
        let mut before = None;
        if expect.change.is_some() {
            match self.before(op, args).and_then(|output| single(&output, at)) {
                Ok(value) => before = Some(value),
                Err(verdict) => return verdict,
            }
        }
        let verdict = judge(expect, before.as_ref(), &after);
        verdict.seen(before, Some(after))
    }
}

/// Every condition `expect` gives must hold.
fn judge(expect: &QueryExpect, before: Option<&Value>, after: &Value) -> Verdict {
    if let Some(want) = &expect.equals
        && !same_value(want, after)
    {
        return Verdict::fail(format!("expected {want}, found {after}"));
    }
    if let Some([min, max]) = expect.range {
        match after.as_f64() {
            Some(n) if (min..=max).contains(&n) => {}
            Some(n) => return Verdict::fail(format!("{n} is outside {min}..={max}")),
            None => return Verdict::fail(format!("expected a number in {min}..={max}, found {after}")),
        }
    }
    if let (Some(change), Some(before)) = (expect.change, before) {
        let (Some(b), Some(a)) = (before.as_f64(), after.as_f64()) else {
            return Verdict::fail(format!("change needs numbers, found {before} then {after}"));
        };
        if !moved(b, a, change, expect.at_least, expect.relative) {
            return Verdict::fail(format!("{b} to {a} is not {change:?} by the amount asked"));
        }
    }
    Verdict::pass("the value meets every condition")
}
