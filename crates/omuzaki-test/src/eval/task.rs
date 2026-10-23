//! The task file (design `docs/design/evals.md` 3.2): one agent task, as authored in `crates/<crate>/evals/<task>.toml`.

use std::collections::BTreeMap;

use omuzaki_ops_api::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// One agent task: a prompt, the project it starts from, the grants it runs with and the checks that grade it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Unique across the workspace; lowercase letters, digits, dots and dashes.
    pub id: String,
    /// A spec feature ID such as `CB-10`; the lint checks it exists in the spec.
    pub feature: String,
    /// What the task is, for the progress page.
    pub title: String,
    /// What a person would type: the symptom, never the recipe (it must not quote tool descriptions).
    pub prompt: String,
    /// The id of the starting project (a [`Fixture`](super::fixture::Fixture)).
    pub fixture: String,
    /// The grants on the attempt's credential.
    pub scopes: Vec<Scope>,
    /// `starter` puts the task in every reference run.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Bumped when the prompt, fixture or checks change meaning; starts a new series in the history.
    #[serde(default = "first_version")]
    pub version: u32,
    /// Per-attempt cap overrides; the rest come from `evals/defaults.toml`.
    #[serde(default)]
    pub caps: CapOverrides,
    /// Names for things the agent creates (design 5.2).
    #[serde(default)]
    pub bind: BTreeMap<String, Bind>,
    /// The checks; at least one must be hard (the lint enforces it).
    #[serde(default, rename = "check")]
    pub checks: Vec<Check>,
    /// `true` only for tasks whose right answer is to change nothing.
    #[serde(default)]
    pub null_passes: bool,
    /// Up to five yes/no questions for the human-rated sample.
    #[serde(default)]
    pub rubric_for_people: Vec<String>,
    /// What the reference driver answers if the server asks the person a question.
    #[serde(default)]
    pub person_reply: Option<String>,
}

fn first_version() -> u32 {
    1
}

/// Per-attempt limits, all required: the result of laying a task's overrides over the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Caps {
    /// Model turns (reference driver).
    pub turns: u64,
    /// Tool calls.
    pub tool_calls: u64,
    /// Wall-clock seconds.
    pub wall_s: u64,
    /// Input plus output tokens, all turns.
    pub tokens: u64,
}

/// The `[caps]` table of a task: any subset of [`Caps`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapOverrides {
    /// Model turns.
    pub turns: Option<u64>,
    /// Tool calls.
    pub tool_calls: Option<u64>,
    /// Wall-clock seconds.
    pub wall_s: Option<u64>,
    /// Tokens.
    pub tokens: Option<u64>,
}

impl CapOverrides {
    /// These overrides laid over `defaults`.
    #[must_use]
    pub fn over(self, defaults: Caps) -> Caps {
        Caps {
            turns: self.turns.unwrap_or(defaults.turns),
            tool_calls: self.tool_calls.unwrap_or(defaults.tool_calls),
            wall_s: self.wall_s.unwrap_or(defaults.wall_s),
            tokens: self.tokens.unwrap_or(defaults.tokens),
        }
    }
}

/// The shape of `evals/defaults.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// The caps a task gets unless it overrides them.
    pub caps: Caps,
}

/// A binding: an op call whose output names something the agent created (design 5.2).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bind {
    /// The read-only op to call after the attempt.
    pub op: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// JSON pointer (wildcards allowed) into the output.
    pub at: String,
    /// Which match to take.
    pub pick: Pick,
}

/// Which of the matches a [`Bind`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// Exactly one id that was not in the fixture; zero or several fail the check.
    New,
}

/// One check: the common fields plus a [`CheckKind`]. Evaluation belongs to the check library, not to this crate's
/// loader; here a check is only parsed and kept typed.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Check {
    /// Unique within the task.
    pub id: String,
    /// Hard checks decide the attempt; soft ones are scored only.
    #[serde(default = "hard_by_default")]
    pub hard: bool,
    /// What the check tests.
    #[serde(flatten)]
    pub kind: CheckKind,
}

fn hard_by_default() -> bool {
    true
}

// Hand-written so the kind's own `deny_unknown_fields` still applies: serde cannot combine that with `flatten`.
impl<'de> Deserialize<'de> for Check {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        // Read through a JSON value, which would turn TOML's `inf` and `nan` into `null` and so a threshold into "none"
        // (#1857): every number is checked finite on the way in, and the error names the field.
        let Value::Object(mut table) = FiniteValue::deserialize(deserializer)?.0 else {
            return Err(D::Error::custom("a check is a table"));
        };
        let Some(Value::String(id)) = table.remove("id") else {
            return Err(D::Error::custom("a check needs a string `id`"));
        };
        let hard = match table.remove("hard") {
            None => true,
            Some(Value::Bool(hard)) => hard,
            Some(_) => return Err(D::Error::custom("`hard` must be true or false")),
        };
        let kind = serde_json::from_value(Value::Object(table)).map_err(D::Error::custom)?;
        Ok(Self { id, hard, kind })
    }
}

/// A JSON value read from any format, refusing a number that is not finite (`inf`, `-inf`, `nan`) with the path of
/// keys that leads to it.
struct FiniteValue(Value);

impl<'de> Deserialize<'de> for FiniteValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(FiniteVisitor).map(FiniteValue)
    }
}

struct FiniteVisitor;

impl<'de> serde::de::Visitor<'de> for FiniteVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom(format!("{v} is not a finite number; a check needs a finite one")))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        FiniteValue::deserialize(deserializer).map(|v| v.0)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        use serde::de::Error;
        let mut items = Vec::new();
        loop {
            match seq.next_element::<FiniteValue>() {
                Ok(Some(item)) => items.push(item.0),
                Ok(None) => return Ok(Value::Array(items)),
                Err(error) => return Err(A::Error::custom(format!("[{}]: {error}", items.len()))),
            }
        }
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        use serde::de::Error;
        let mut table = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            match map.next_value::<FiniteValue>() {
                Ok(value) => {
                    table.insert(key, value.0);
                }
                Err(error) => return Err(A::Error::custom(format!("`{key}`: {error}"))),
            }
        }
        Ok(Value::Object(table))
    }
}

/// The check vocabulary of design 5.3.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum CheckKind {
    /// The document value at `path` equals `value`.
    #[serde(rename = "doc.equals")]
    DocEquals {
        /// Document path (globs allowed).
        path: String,
        /// The expected value.
        value: Value,
    },
    /// The document number at `path` lies in `min..=max`.
    #[serde(rename = "doc.range")]
    DocRange {
        /// Document path (globs allowed).
        path: String,
        /// Lower bound.
        min: Option<f64>,
        /// Upper bound.
        max: Option<f64>,
    },
    /// Something exists at `path`.
    #[serde(rename = "doc.exists")]
    DocExists {
        /// Document path (globs allowed).
        path: String,
    },
    /// Nothing exists at `path`.
    #[serde(rename = "doc.absent")]
    DocAbsent {
        /// Document path (globs allowed).
        path: String,
    },
    /// The value at `path` moved as `change` says since the fixture.
    #[serde(rename = "doc.change")]
    DocChange {
        /// Document path.
        path: String,
        /// The direction.
        change: Change,
        /// Smallest move that counts.
        at_least: Option<f64>,
        /// Whether `at_least` is relative to the old value.
        #[serde(default)]
        relative: bool,
    },
    /// Every difference between fixture and result is under one of `paths`.
    #[serde(rename = "doc.changed_only")]
    DocChangedOnly {
        /// Allowed path globs.
        paths: Vec<String>,
    },
    /// Nothing under `paths` (the whole document when empty) differs.
    #[serde(rename = "doc.unchanged")]
    DocUnchanged {
        /// Path globs.
        #[serde(default)]
        paths: Vec<String>,
    },
    /// A read-only op's output at a pointer meets an expectation.
    #[serde(rename = "query")]
    Query {
        /// The op to call, before and after.
        op: String,
        /// Its arguments.
        #[serde(default)]
        args: Map<String, Value>,
        /// JSON pointer into the output.
        at: String,
        /// What the value must satisfy.
        expect: QueryExpect,
    },
    /// Before and after renders differ where they should.
    #[serde(rename = "render.diff")]
    RenderDiff {
        /// The read-only render op call.
        render: OpCall,
        /// A rect, or an op call and pointer that gives one.
        region: Region,
        /// The thresholds.
        expect: RenderDiffExpect,
    },
    /// Before and after renders are identical within the golden tolerance.
    #[serde(rename = "render.same")]
    RenderSame {
        /// The read-only render op call.
        render: OpCall,
    },
    /// Gate ops pass on the result.
    #[serde(rename = "gate")]
    Gate {
        /// The named gate ops.
        #[serde(default)]
        ops: Vec<String>,
        /// Read the failed gates from `project.describe` instead.
        #[serde(default)]
        declared: bool,
    },
    /// The transcript shows the op applied.
    #[serde(rename = "log.applied")]
    LogApplied {
        /// Op name or glob.
        op: String,
    },
    /// The transcript never shows the op applied.
    #[serde(rename = "log.not_applied")]
    LogNotApplied {
        /// Op name or glob.
        op: String,
    },
    /// Every applied op is one of `ops`.
    #[serde(rename = "log.within")]
    LogWithin {
        /// Allowed op names or globs.
        ops: Vec<String>,
    },
    /// Every applied edit was dry-run first with the same arguments.
    #[serde(rename = "log.previewed")]
    LogPreviewed {},
    /// At most `n` tool calls returned an error.
    #[serde(rename = "log.max_errors")]
    LogMaxErrors {
        /// The most errors allowed.
        n: u64,
    },
    /// Yes/no questions put to a judge; soft until calibrated (design 10.4).
    #[serde(rename = "judge")]
    Judge {
        /// The questions.
        rubric: Vec<String>,
        /// A render the judge looks at.
        render: Option<OpCall>,
    },
}

/// Which way a value moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Went up.
    Up,
    /// Went down.
    Down,
    /// Moved either way.
    Any,
    /// Did not move.
    None,
}

/// An op name with arguments.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpCall {
    /// The op.
    pub op: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
}

/// Where a render check looks.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Region {
    /// An op call whose output (at `at`) is a rect.
    FromOp(RegionOp),
    /// A fixed `[x, y, w, h]` rect.
    Rect(Vec<f64>),
}

/// An op call and the pointer to the rect in its output.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionOp {
    /// The op.
    pub op: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// JSON pointer to the rect.
    pub at: String,
}

/// What a [`CheckKind::Query`] value must satisfy: any of these, all of which given must hold.
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryExpect {
    /// Equals this value.
    pub equals: Option<Value>,
    /// Lies in `[min, max]`.
    pub range: Option<[f64; 2]>,
    /// Moved this way between before and after.
    pub change: Option<Change>,
    /// Smallest move that counts for `change`.
    pub at_least: Option<f64>,
    /// Whether `at_least` is relative.
    #[serde(default)]
    pub relative: bool,
}

/// Thresholds of a [`CheckKind::RenderDiff`].
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderDiffExpect {
    /// Fraction of the changed pixels that must lie inside the region.
    pub changed_inside_at_least: Option<f64>,
    /// Fraction of all pixels that must change.
    pub changed_fraction_at_least: Option<f64>,
    /// Fraction of all pixels that may change.
    pub changed_fraction_at_most: Option<f64>,
}

/// Every check `kind` the vocabulary has, for messages and the lint's friendly "unknown kind" finding.
pub const CHECK_KINDS: [&str; 17] = [
    "doc.equals",
    "doc.range",
    "doc.exists",
    "doc.absent",
    "doc.change",
    "doc.changed_only",
    "doc.unchanged",
    "query",
    "render.diff",
    "render.same",
    "gate",
    "log.applied",
    "log.not_applied",
    "log.within",
    "log.previewed",
    "log.max_errors",
    "judge",
];

#[cfg(test)]
mod tests {
    use super::*;

    const RENDER_DIFF: &str = "id = 'c'\nkind = 'render.diff'\nrender = { op = 'render.preview' }\nexpect = {}\n";

    fn check(region: &str) -> Result<Check, toml::de::Error> {
        toml::from_str(&format!("{RENDER_DIFF}region = {region}"))
    }

    #[test]
    fn a_region_from_an_op_is_read_and_a_misspelt_key_is_refused() {
        assert!(check("{ op = 'x.y', at = '/rect' }").is_ok());
        assert!(check("{ op = 'x.y', at = '/rect', agrs = {} }").is_err());
        assert!(check("[0, 0, 4, 4]").is_ok());
    }

    #[test]
    fn unknown_keys_inside_a_check_are_refused() {
        assert!(toml::from_str::<Check>("id = 'c'\nkind = 'doc.exists'\npath = 'a'\npth = 'b'").is_err());
    }

    #[test]
    fn a_number_that_is_not_finite_is_refused_with_its_field() {
        let cases = [
            ("id = 'c'\nkind = 'doc.change'\npath = 'a'\nchange = 'up'\nat_least = inf", "`at_least`"),
            ("id = 'c'\nkind = 'doc.range'\npath = 'a'\nmin = 0.0\nmax = -inf", "`max`"),
            ("id = 'c'\nkind = 'doc.equals'\npath = 'a'\nvalue = nan", "`value`"),
            ("id = 'c'\nkind = 'doc.equals'\npath = 'a'\nvalue = { b = [1.0, nan] }", "`value`: `b`: [1]"),
        ];
        for (text, field) in cases {
            let message = toml::from_str::<Check>(text).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(message.contains(field) && message.contains("not a finite number"), "{text:?}: {message}");
        }
        let finite = "id = 'c'\nkind = 'doc.range'\npath = 'a'\nmin = -1.5\nmax = 2.0";
        let Check { kind, .. } = toml::from_str::<Check>(finite).unwrap();
        assert_eq!(kind, CheckKind::DocRange { path: "a".into(), min: Some(-1.5), max: Some(2.0) });
    }

    #[test]
    fn every_listed_kind_is_in_the_vocabulary() {
        for kind in CHECK_KINDS {
            let message = toml::from_str::<Check>(&format!("id = 'c'\nkind = '{kind}'")).err().map(|e| e.to_string());
            assert!(!message.is_some_and(|m| m.contains("unknown variant")), "{kind}");
        }
    }
}
