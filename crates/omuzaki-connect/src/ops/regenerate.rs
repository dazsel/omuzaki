//! `connect.regenerate`: re-issue a tray candidate's request, with a new seed or the same one, optionally changed.

use omuzaki_connect_api::{CandidateId, Request, tray};
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::call::{Limits, finish_call, plan_call, work_call};

/// Which seed a regenerated call uses.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SeedChoice {
    /// A new one, drawn from the new run.
    #[default]
    New,
    /// The candidate's own: the same request again.
    Same,
}

/// Makes a tray candidate again with the same connector: its stored request, with a new seed (the default) or the
/// same one, and `changes` merged in (a JSON merge patch, such as `{"prompt": "..."}`). Works on any candidate,
/// pending, accepted or rejected; the new ones land pending, naming the candidate they came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectRegenerate {
    /// The candidate, as `connect.tray_list` names it.
    pub candidate: CandidateId,
    /// A new seed or the same.
    #[serde(default)]
    pub seed: SeedChoice,
    /// Changes to the stored request, as a JSON merge patch.
    #[serde(default)]
    pub changes: Option<Value>,
    /// The price the person was shown, micro-dollars.
    #[serde(default)]
    pub expect_usd_micros: Option<u64>,
    /// The most this call may bill, micro-dollars. Omitted means the estimate.
    #[serde(default)]
    pub max_usd_micros: Option<u64>,
}

impl ConnectRegenerate {
    /// The request to send: the candidate's, changed, its seed cleared unless the same one is wanted.
    fn request(&self, doc: &dyn DocRead) -> Result<(Request, Limits), OpError> {
        let c = tray::get(doc, &self.candidate)
            .map_err(super::tray_error)?
            .ok_or_else(|| OpError::not_found(format!("no tray candidate {}", self.candidate)).hint("connect.tray_list shows them"))?;
        let mut json =
            serde_json::to_value(&c.request).map_err(|e| OpError::internal(format!("reading the request: {e}")))?;
        if let Some(changes) = &self.changes {
            merge_patch(&mut json, changes);
        }
        if self.seed == SeedChoice::New
            && let Value::Object(fields) = &mut json
            && fields.contains_key("seed")
        {
            fields.insert("seed".into(), Value::Null);
        }
        let request: Request = serde_json::from_value(json).map_err(|e| OpError::invalid("changes", e.to_string()))?;
        let problems = request.check();
        if let Some(first) = problems.first() {
            let all = problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
            return Err(OpError::invalid(format!("changes/{}", first.field), all));
        }
        let limits = Limits {
            connector: Some(c.connector),
            expect_usd_micros: self.expect_usd_micros,
            max_usd_micros: self.max_usd_micros,
            reuse: None,
            regenerated_from: Some(self.candidate.clone()),
        };
        Ok((request, limits))
    }

    fn limits(&self) -> Limits {
        Limits {
            expect_usd_micros: self.expect_usd_micros,
            max_usd_micros: self.max_usd_micros,
            regenerated_from: Some(self.candidate.clone()),
            ..Limits::default()
        }
    }
}

/// RFC 7386: `patch` merged into `target`; a `null` removes a field.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(changes) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(fields) = target {
        for (key, value) in changes {
            if value.is_null() {
                fields.remove(key);
            } else {
                merge_patch(fields.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

impl Op for ConnectRegenerate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.regenerate",
            summary: "Make a tray candidate again with its connector: the same request with a new seed (or the same), optionally changed.",
            kind: OpKind::Job,
            scope: Scope::Net,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (request, limits) = self.request(ctx.doc)?;
        plan_call(ctx, &request, &limits)
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        work_call(job, &self.limits())
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        finish_call(ctx, result, &self.limits())
    }
}

register_op!(ConnectRegenerate);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_patch_follows_rfc_7386() {
        let mut doc = json!({"a": "b", "c": {"d": "e", "f": "g"}});
        merge_patch(&mut doc, &json!({"a": "z", "c": {"f": null}}));
        assert_eq!(doc, json!({"a": "z", "c": {"d": "e"}}));
        let mut doc = json!({"a": [1]});
        merge_patch(&mut doc, &json!({"a": [2, 3], "n": 1}));
        assert_eq!(doc, json!({"a": [2, 3], "n": 1}));
    }
}
