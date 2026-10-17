//! Adapter knobs (D2, D3): a manifest declares each `params` entry a request may set, and the host checks a request's
//! `params` against the declaration before anything runs, so a typo is `Invalid` naming `params/<name>`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::request::RequestProblem;

/// An adapter knob a request may set in `params`. Declared per capability, shown by `connect.describe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    /// The key in `params`: a slug.
    pub name: String,
    /// Its type and range.
    pub kind: ParamKind,
    /// The value the adapter uses when the request does not set it; inside the kind's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// One line for people and agents.
    pub summary: String,
}

/// The type and range of a [`ParamDecl`]. In TOML: `kind = { int = { min = 1, max = 150 } }`, `kind = "bool"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ParamKind {
    /// A whole number in `min..=max`.
    #[non_exhaustive]
    Int {
        /// The smallest allowed value.
        min: i64,
        /// The largest allowed value.
        max: i64,
    },
    /// A finite number in `min..=max`.
    #[non_exhaustive]
    Float {
        /// The smallest allowed value.
        min: f64,
        /// The largest allowed value.
        max: f64,
    },
    /// `true` or `false`.
    Bool,
    /// A string of at most `max_bytes` bytes.
    #[non_exhaustive]
    Text {
        /// The longest allowed value, in bytes.
        max_bytes: u32,
    },
    /// One of `options`.
    #[non_exhaustive]
    Choice {
        /// The allowed values.
        options: Vec<String>,
    },
}

impl ParamKind {
    /// [`ParamKind::Int`].
    pub fn int(min: i64, max: i64) -> Self {
        Self::Int { min, max }
    }

    /// [`ParamKind::Float`].
    pub fn float(min: f64, max: f64) -> Self {
        Self::Float { min, max }
    }

    /// [`ParamKind::Text`].
    pub fn text(max_bytes: u32) -> Self {
        Self::Text { max_bytes }
    }

    /// [`ParamKind::Choice`].
    pub fn choice(options: Vec<String>) -> Self {
        Self::Choice { options }
    }

    /// What is wrong with the kind itself (`min > max`, a non-finite bound, no or repeated options), if anything.
    pub fn problem(&self) -> Option<String> {
        match self {
            ParamKind::Int { min, max } if min > max => Some(format!("min {min} is above max {max}")),
            ParamKind::Float { min, max } if !(min.is_finite() && max.is_finite()) => {
                Some("min and max must be finite numbers".to_owned())
            }
            ParamKind::Float { min, max } if min > max => Some(format!("min {min} is above max {max}")),
            ParamKind::Choice { options } if options.is_empty() => {
                Some("a choice needs at least one option".to_owned())
            }
            ParamKind::Choice { options } => {
                let mut seen = std::collections::BTreeSet::new();
                options.iter().find(|o| !seen.insert(o.as_str())).map(|o| format!("option {o:?} is listed twice"))
            }
            _ => None,
        }
    }

    /// Why `value` is not a value of this kind, or `None` when it is.
    pub fn reject(&self, value: &Value) -> Option<String> {
        match self {
            ParamKind::Int { min, max } => match value.as_i64() {
                Some(v) if (*min..=*max).contains(&v) => None,
                Some(v) => Some(format!("{v} is outside {min} to {max}")),
                None => Some(format!("expected a whole number from {min} to {max}")),
            },
            ParamKind::Float { min, max } => match value.as_f64() {
                Some(v) if v.is_finite() && *min <= v && v <= *max => None,
                Some(v) => Some(format!("{v} is outside {min} to {max}")),
                None => Some(format!("expected a number from {min} to {max}")),
            },
            ParamKind::Bool => (!value.is_boolean()).then(|| "expected true or false".to_owned()),
            ParamKind::Text { max_bytes } => match value.as_str() {
                Some(s) if s.len() <= *max_bytes as usize => None,
                Some(s) => Some(format!("{} bytes is longer than {max_bytes}", s.len())),
                None => Some("expected a string".to_owned()),
            },
            ParamKind::Choice { options } => match value.as_str() {
                Some(s) if options.iter().any(|o| o == s) => None,
                _ => Some(format!("expected one of {}", options.join(", "))),
            },
        }
    }
}

/// Every problem of `params` against `decls`, each naming `params/<name>`: a key no declaration names, or a value
/// outside its declared kind. What the host runs in `preview` and a connector may run again.
pub fn check_params(decls: &[ParamDecl], params: &BTreeMap<String, Value>) -> Vec<RequestProblem> {
    params
        .iter()
        .filter_map(|(name, value)| {
            let problem = match decls.iter().find(|d| d.name == *name) {
                None if decls.is_empty() => "this connector takes no params".to_owned(),
                None => {
                    let known: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
                    format!("unknown param; this connector takes {}", known.join(", "))
                }
                Some(decl) => decl.kind.reject(value)?,
            };
            Some(RequestProblem::new(format!("params/{name}"), problem))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decl(name: &str, kind: ParamKind) -> ParamDecl {
        ParamDecl { name: name.into(), kind, default: None, summary: String::new() }
    }

    #[test]
    fn values_are_checked_against_their_kind() {
        assert_eq!(ParamKind::int(1, 150).reject(&json!(30)), None);
        assert!(ParamKind::int(1, 150).reject(&json!(151)).is_some());
        assert!(ParamKind::int(1, 150).reject(&json!(1.5)).is_some());
        assert_eq!(ParamKind::float(0.0, 30.0).reject(&json!(7)), None);
        assert!(ParamKind::Bool.reject(&json!("yes")).is_some());
        assert!(ParamKind::text(3).reject(&json!("four")).is_some());
        assert_eq!(ParamKind::choice(vec!["a".into(), "b".into()]).reject(&json!("b")), None);
        assert!(ParamKind::choice(vec!["a".into()]).reject(&json!("c")).is_some());
    }

    #[test]
    fn kinds_report_their_own_problems() {
        assert!(ParamKind::int(2, 1).problem().is_some());
        assert!(ParamKind::float(0.0, f64::INFINITY).problem().is_some());
        assert!(ParamKind::choice(vec![]).problem().is_some());
        assert!(ParamKind::choice(vec!["a".into(), "a".into()]).problem().is_some());
        assert_eq!(ParamKind::int(1, 1).problem(), None);
    }

    #[test]
    fn params_name_every_problem_by_path() {
        let decls = [decl("steps", ParamKind::int(1, 150))];
        let params = BTreeMap::from([("steps".to_owned(), json!(0)), ("stpes".to_owned(), json!(3))]);
        let fields: Vec<String> = check_params(&decls, &params).into_iter().map(|p| p.field).collect();
        assert_eq!(fields, ["params/steps", "params/stpes"]);
        assert!(check_params(&[], &BTreeMap::from([("x".to_owned(), json!(1))]))[0].problem.contains("no params"));
    }
}
