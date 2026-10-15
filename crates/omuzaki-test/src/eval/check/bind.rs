//! Bindings (design 5.2): names for the things the agent created, resolved from the fixture and the result before any
//! check runs.

use std::collections::BTreeMap;

use super::query::allowed;
use super::report::Verdict;
use super::value::{call, pointer_all, result_failure};
use crate::eval::task::{Bind, Pick};
use omuzaki_ops_api::{OpError, Runner};

/// The ids a binding's op names, from one project. `on_error` makes the verdict of an op that failed there.
fn ids(runner: &dyn Runner, bind: &Bind, on_error: fn(&Bind, &OpError) -> Verdict) -> Result<Vec<String>, Verdict> {
    let output = call(runner, &bind.op, &bind.args).map_err(|e| on_error(bind, &e))?;
    pointer_all(&output, &bind.at)?
        .into_iter()
        .map(|found| {
            found.as_str().map(str::to_owned).ok_or_else(|| {
                Verdict::error(format!(
                    "binding `{}` at `{}` reached {found}; the pointer must name ids (strings)",
                    bind.op, bind.at
                ))
            })
        })
        .collect()
}

/// Resolves one binding: the single id the result has that the fixture did not.
fn resolve_one(fixture: &dyn Runner, result: &dyn Runner, bind: &Bind) -> Result<String, Verdict> {
    allowed(result, &bind.op)?;
    let before = ids(fixture, bind, |bind, e| Verdict::error(format!("binding `{}` on the fixture: {e}", bind.op)))?;
    let after = ids(result, bind, |bind, e| result_failure(&format!("binding `{}`", bind.op), e))?;
    let Pick::New = bind.pick;
    let mut new: Vec<String> = after.into_iter().filter(|id| !before.contains(id)).collect();
    new.sort();
    new.dedup();
    match <[String; 1]>::try_from(new) {
        Ok([id]) => Ok(id),
        Err(new) => Err(Verdict::fail(format!(
            "binding `{}` at `{}` found {} new ids ({}); exactly one was expected",
            bind.op,
            bind.at,
            new.len(),
            new.join(", ")
        ))),
    }
}

/// Every binding of a task, by name. A binding that did not resolve holds the verdict of the check that uses it.
pub(super) fn resolve(
    fixture: &dyn Runner,
    result: &dyn Runner,
    binds: &BTreeMap<String, Bind>,
) -> BTreeMap<String, Result<String, Verdict>> {
    binds.iter().map(|(name, bind)| (name.clone(), resolve_one(fixture, result, bind))).collect()
}
