//! The `query` check: a read-only op's output at a pointer meets an expectation.

use omuzaki_ops_api::{OpInfo, Runner, Scope};
use serde_json::{Map, Value};

use super::Checker;
use super::number::{Num, in_range, moved, same_value};
use super::report::Verdict;
use super::value::{call, pointer_all, result_failure};
use crate::eval::task::QueryExpect;

/// The single value at `at` in an op's output. `on_miss` makes the verdict when it is not exactly one value: a `fail`
/// for the result, which the agent shaped, and an `error` for the fixture, which it did not.
fn single(output: &Value, at: &str, on_miss: fn(String) -> Verdict) -> Result<Value, Verdict> {
    match pointer_all(output, at)?.as_slice() {
        [one] => Ok((*one).clone()),
        found => Err(on_miss(format!("`{at}` reached {} values; a query reads exactly one", found.len()))),
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
        // A `change` is measured from the fixture, so the fixture comes first: a pointer that is absent there is a
        // broken check, and must not be read as the agent losing the value.
        let mut before = None;
        if expect.change.is_some() {
            let value = self
                .before(op, args)
                .and_then(|output| single(&output, at, |detail| Verdict::error(format!("on the fixture: {detail}"))));
            match value {
                Ok(value) if value.is_number() => before = Some(value),
                Ok(value) => return Verdict::error(format!("change needs a number on the fixture, found {value}")),
                Err(verdict) => return verdict,
            }
        }
        let after = match call(&*self.result, op, args) {
            Ok(output) => match single(&output, at, Verdict::fail) {
                Ok(value) => value,
                Err(verdict) => return verdict,
            },
            Err(e) => return result_failure(&format!("`{op}`"), &e),
        };
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
        match Num::of(after) {
            Some(n) if in_range(n, min, max) => {}
            Some(_) => return Verdict::fail(format!("{after} is outside {min}..={max}")),
            None => return Verdict::fail(format!("expected a number in {min}..={max}, found {after}")),
        }
    }
    if let (Some(change), Some(before)) = (expect.change, before) {
        let Some(b) = Num::of(before) else {
            return Verdict::error(format!("change needs a number on the fixture, found {before}"));
        };
        let Some(a) = Num::of(after) else {
            return Verdict::fail(format!("change needs a number, found {before} then {after}"));
        };
        if !moved(b, a, change, expect.at_least, expect.relative) {
            return Verdict::fail(format!("{before} to {after} is not {change:?} by the amount asked"));
        }
    }
    Verdict::pass("the value meets every condition")
}
