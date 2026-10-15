//! Bindings (design 5.2): names for the things the agent created, resolved from the fixture and the result before any
//! check runs.

use std::collections::BTreeMap;

use serde_json::Value;

use super::report::Verdict;
use super::value::{call, pointer_all, result_failure};
use crate::eval::task::{Bind, Pick};
use omuzaki_ops_api::Runner;

/// The ids a binding's op names, from one project.
fn ids(runner: &dyn Runner, bind: &Bind) -> Result<Vec<String>, omuzaki_ops_api::OpError> {
    let output = call(runner, &bind.op, &bind.args)?;
    Ok(pointer_all(&output, &bind.at).into_iter().filter_map(Value::as_str).map(str::to_owned).collect())
}

/// Resolves one binding: the single id the result has that the fixture did not.
fn resolve_one(fixture: &dyn Runner, result: &dyn Runner, bind: &Bind) -> Result<String, Verdict> {
    let before =
        ids(fixture, bind).map_err(|e| Verdict::error(format!("binding `{}` on the fixture: {e}", bind.op)))?;
    let after = ids(result, bind).map_err(|e| result_failure(&format!("binding `{}`", bind.op), &e))?;
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

/// Replaces `{name}` in every string of `value` with the bound id.
///
/// # Errors
/// The verdict of the first binding used here that did not resolve.
pub(super) fn substitute(value: &mut Value, bound: &BTreeMap<String, Result<String, Verdict>>) -> Result<(), Verdict> {
    match value {
        Value::String(text) => {
            for (name, id) in bound {
                let placeholder = format!("{{{name}}}");
                if text.contains(&placeholder) {
                    *text = text.replace(&placeholder, id.as_ref().map_err(Clone::clone)?);
                }
            }
            Ok(())
        }
        Value::Array(items) => items.iter_mut().try_for_each(|v| substitute(v, bound)),
        Value::Object(map) => map.values_mut().try_for_each(|v| substitute(v, bound)),
        _ => Ok(()),
    }
}
