//! Scripts (design 6.3): the calls of an attempt as JSON lines, what the scripted driver replays.
//!
//! ```text
//! {"call": "face_set_profile", "args": {"character": "{main}", "dry_run": true}, "expect": {"ok": true, "mode": "dry_run"}, "save": "preview"}
//! {"assert": {"doc_same_as": "start"}}
//! {"call": "history_undo", "args": {}}
//! {"assert": {"doc_same_as": "start", "render_same_as": "preview"}}
//! ```
//!
//! A call line names an MCP tool, exactly as an agent would send it, with optional `expect` (a subset of the answer's
//! `structuredContent`) and `save` (a name for the call and for the document as it stands after it). An assert line
//! checks facts an end state cannot see: `doc_same_as` that the document equals the one saved under that name (or
//! `start`, the fixture as built), `render_same_as` that re-running the call saved under that name gives the same
//! answer, which for a render is the same content-addressed picture. `{main}` in any string is a name the fixture
//! exports.

use std::collections::{BTreeMap, BTreeSet};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::transcript::{Event, EventLog};

/// The name of the fixture as built, which `doc_same_as` may refer to without a `save`.
pub const START: &str = "start";

/// A call to replay.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CallStep {
    /// The MCP tool name (`face_set_profile`); an op name (`face.set_profile`) is accepted too and found in the catalog.
    pub call: String,
    /// The turn of the recorded attempt the line came from. Informational: the driver does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// Shorthand for `"expect": {"applied": ...}`: whether the call changed the project. Folded into `expect` when the
    /// script is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<bool>,
    /// Its arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// Keys of the answer's `structuredContent` that must have these values (objects are compared as subsets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// A name for this call and for the document after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<String>,
}

/// Facts about the end of a stretch of the script.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssertStep {
    /// The document equals the one saved under this name, or `start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_same_as: Option<String>,
    /// Re-running the call saved under this name gives the same answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_same_as: Option<String>,
}

/// One line of a script.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `{"call": ...}`
    Call(CallStep),
    /// `{"assert": ...}`
    Assert(AssertStep),
}

/// A step and the line of the script it came from, which is the number a failure reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// 1-based line number in the script file.
    pub number: usize,
    /// The step.
    pub step: Step,
}

/// A parsed script.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    /// Where `eval promote` took it from, as `<run>/<task>/<attempt>`: `eval gc` keeps a run folder a script names.
    pub promoted_from: Option<String>,
    /// The steps, in order.
    pub lines: Vec<Line>,
}

/// Why a script does not parse: the line, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("script line {line}: {reason}")]
pub struct ScriptError {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong.
    pub reason: String,
}

impl Script {
    /// Parses JSON lines; blank lines are skipped.
    ///
    /// # Errors
    /// The first line that is not a call or an assert, names something twice, or asserts about a name nothing saved.
    pub fn parse(text: &str) -> Result<Self, ScriptError> {
        let mut lines = Vec::new();
        let mut promoted_from = None;
        let mut saved: BTreeSet<String> = BTreeSet::new();
        for (index, raw) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let number = index + 1;
            let fail = |reason: String| ScriptError { line: number, reason };
            let value: Value = serde_json::from_str(raw).map_err(|e| fail(e.to_string()))?;
            let Value::Object(mut map) = value else { return Err(fail("a script line is a JSON object".to_owned())) };
            if let Some(source) = map.remove("promoted_from") {
                let Value::String(source) = source else {
                    return Err(fail("`promoted_from` is the text `<run>/<task>/<attempt>`".to_owned()));
                };
                if !map.is_empty() || promoted_from.replace(source).is_some() {
                    return Err(fail("`promoted_from` stands alone on one line, once".to_owned()));
                }
                continue;
            }
            let step = if map.contains_key("call") {
                let mut call: CallStep = serde_json::from_value(Value::Object(map)).map_err(|e| fail(e.to_string()))?;
                if let Some(applied) = call.applied.take() {
                    let mut expect = match call.expect.take() {
                        None => Map::new(),
                        Some(Value::Object(map)) => map,
                        Some(_) => return Err(fail("`expect` is an object of keys the answer must have".to_owned())),
                    };
                    if expect
                        .insert("applied".to_owned(), Value::Bool(applied))
                        .is_some_and(|old| old != Value::Bool(applied))
                    {
                        return Err(fail("`applied` and `expect.applied` disagree".to_owned()));
                    }
                    call.expect = Some(Value::Object(expect));
                }
                if call.call.trim().is_empty() {
                    return Err(fail("`call` names no tool".to_owned()));
                }
                if call.expect.as_ref().is_some_and(|e| !e.is_object()) {
                    return Err(fail("`expect` is an object of keys the answer must have".to_owned()));
                }
                if let Some(name) = &call.save {
                    if name == START || name.trim().is_empty() {
                        return Err(fail(format!("`save` cannot be \"{START}\" or empty")));
                    }
                    if !saved.insert(name.clone()) {
                        return Err(fail(format!("`{name}` is saved twice")));
                    }
                }
                Step::Call(call)
            } else if let Some(body) = map.remove("assert") {
                if !map.is_empty() {
                    return Err(fail("an assert line has nothing but `assert`".to_owned()));
                }
                let body: AssertStep = serde_json::from_value(body).map_err(|e| fail(e.to_string()))?;
                if body.doc_same_as.is_none() && body.render_same_as.is_none() {
                    return Err(fail("an assert gives `doc_same_as` or `render_same_as`".to_owned()));
                }
                if let Some(name) = &body.doc_same_as
                    && name != START
                    && !saved.contains(name)
                {
                    return Err(fail(format!("`doc_same_as` names `{name}`, which no earlier call saved")));
                }
                if let Some(name) = &body.render_same_as
                    && !saved.contains(name)
                {
                    return Err(fail(format!("`render_same_as` names `{name}`, which no earlier call saved")));
                }
                Step::Assert(body)
            } else {
                return Err(fail("a script line has `call` or `assert`".to_owned()));
            };
            lines.push(Line { number, step });
        }
        Ok(Self { promoted_from, lines })
    }

    /// The script as JSON lines.
    ///
    /// # Errors
    /// A serialisation error, which the step types cannot produce; returned rather than hidden.
    pub fn to_jsonl(&self) -> Result<String, serde_json::Error> {
        let mut out = String::new();
        if let Some(source) = &self.promoted_from {
            out.push_str(&serde_json::to_string(&serde_json::json!({ "promoted_from": source }))?);
            out.push('\n');
        }
        for line in &self.lines {
            let value = match &line.step {
                Step::Call(call) => serde_json::to_value(call)?,
                Step::Assert(assert) => serde_json::json!({ "assert": assert }),
            };
            out.push_str(&serde_json::to_string(&value)?);
            out.push('\n');
        }
        Ok(out)
    }

    /// A script of the tool calls of `log`, in order: what `eval promote` writes. An argument that is exactly an id the
    /// attempt's `exports` note names is written back as `{name}`, so the script runs on a fixture built again.
    #[must_use]
    pub fn from_log(log: &EventLog) -> Self {
        let exports = exports_of(log);
        let mut lines = Vec::new();
        for stamped in log.events() {
            if let Event::ToolCall { tool, args } = &stamped.event {
                let Value::Object(args) = unfill(args, &exports) else { continue };
                lines.push(Line {
                    number: lines.len() + 1,
                    step: Step::Call(CallStep {
                        call: tool.clone(),
                        turn: None,
                        applied: None,
                        args,
                        expect: None,
                        save: None,
                    }),
                });
            }
        }
        Self { promoted_from: None, lines }
    }

    /// The same script, marked as taken from the attempt folder `source` (`<run>/<task>/<attempt>`).
    #[must_use]
    pub fn promoted_from(mut self, source: impl Into<String>) -> Self {
        self.promoted_from = Some(source.into());
        self
    }

    /// The run a promoted script was taken from.
    #[must_use]
    pub fn promoted_run(&self) -> Option<&str> {
        self.promoted_from.as_deref().and_then(|s| s.split('/').next())
    }
}

/// Where `eval promote` writes a script for the task in `task_file`: beside it, as `<task-stem>.<name>.{pass,fail}.jsonl`
/// (the name discovery looks for).
///
/// # Errors
/// A message when `name` is not lowercase words joined by dashes, or the task file has no stem.
pub fn promoted_path(task_file: &Path, name: &str, expect_pass: bool) -> Result<PathBuf, String> {
    let word = |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !name.split('-').all(word) {
        return Err(format!("`{name}` is not a script name: lowercase letters and digits, joined by dashes"));
    }
    let stem = task_file
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: no file name", task_file.display()))?;
    let verdict = if expect_pass { "pass" } else { "fail" };
    Ok(task_file.with_file_name(format!("{stem}.{name}.{verdict}.jsonl")))
}

/// The fixture exports a log recorded in its `exports` note, by id.
fn exports_of(log: &EventLog) -> BTreeMap<String, String> {
    log.events()
        .iter()
        .find_map(|stamped| match &stamped.event {
            Event::Note { text, data: Some(Value::Object(map)) } if text == "exports" => Some(
                map.iter()
                    .filter_map(|(name, id)| Some((id.as_str()?.to_owned(), name.clone())))
                    .collect::<BTreeMap<_, _>>(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn unfill(value: &Value, by_id: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(text) => {
            by_id.get(text).map_or_else(|| value.clone(), |name| Value::String(format!("{{{name}}}")))
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| unfill(v, by_id)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), unfill(v, by_id))).collect()),
        other => other.clone(),
    }
}

/// Whether every key of `expect` is in `actual` with that value; objects are compared as subsets, anything else exactly.
#[must_use]
pub fn is_subset(expect: &Value, actual: &Value) -> bool {
    match (expect, actual) {
        (Value::Object(want), Value::Object(have)) => {
            want.iter().all(|(key, value)| have.get(key).is_some_and(|got| is_subset(value, got)))
        }
        (want, have) => want == have,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SCRIPT: &str = r#"
{"call": "set_counter", "args": {"value": 4, "dry_run": true}, "expect": {"ok": true}, "save": "preview"}

{"assert": {"doc_same_as": "start"}}
{"call": "set_counter", "args": {"value": 4}}
{"assert": {"render_same_as": "preview"}}
"#;

    #[test]
    fn a_script_parses_with_the_line_numbers_of_its_file() {
        let script = Script::parse(SCRIPT).unwrap();
        let numbers: Vec<usize> = script.lines.iter().map(|l| l.number).collect();
        assert_eq!(numbers, [2, 4, 5, 6]);
        assert!(matches!(&script.lines[0].step, Step::Call(c) if c.save.as_deref() == Some("preview")));
    }

    #[test]
    fn the_older_line_shape_is_read_with_applied_folded_into_expect() {
        let script = Script::parse(
            "{\"turn\":1,\"call\":\"character.rename\",\"args\":{\"name\":\"M\"},\"applied\":true}\n\
             {\"call\":\"a\",\"applied\":true,\"expect\":{\"ok\":true}}\n",
        )
        .unwrap();
        let Step::Call(first) = &script.lines[0].step else { panic!("a call") };
        assert_eq!((first.turn, first.applied, first.expect.clone()), (Some(1), None, Some(json!({"applied": true}))));
        let Step::Call(second) = &script.lines[1].step else { panic!("a call") };
        assert_eq!(second.expect, Some(json!({"applied": true, "ok": true})));
        let clash = Script::parse("{\"call\":\"a\",\"applied\":true,\"expect\":{\"applied\":false}}").unwrap_err();
        assert!(clash.reason.contains("disagree"), "{clash}");
    }

    #[test]
    fn a_script_round_trips_through_json_lines() {
        let script = Script::parse(SCRIPT).unwrap();
        let again = Script::parse(&script.to_jsonl().unwrap()).unwrap();
        let steps = |s: &Script| s.lines.iter().map(|l| l.step.clone()).collect::<Vec<_>>();
        assert_eq!(steps(&script), steps(&again));
    }

    #[test]
    fn bad_lines_are_named_by_number() {
        for (text, line, word) in [
            ("{\"call\": \"a\"}\nnot json", 2, "expected"),
            ("[1]", 1, "object"),
            ("{\"hello\": 1}", 1, "`call` or `assert`"),
            ("{\"call\": \"a\", \"agrs\": {}}", 1, "agrs"),
            ("{\"call\": \"\"}", 1, "no tool"),
            ("{\"call\": \"a\", \"expect\": 1}", 1, "expect"),
            ("{\"call\": \"a\", \"save\": \"start\"}", 1, "start"),
            ("{\"call\": \"a\", \"save\": \"x\"}\n{\"call\": \"a\", \"save\": \"x\"}", 2, "twice"),
            ("{\"assert\": {}}", 1, "gives"),
            ("{\"assert\": {\"doc_same_as\": \"nope\"}}", 1, "nope"),
            ("{\"assert\": {\"render_same_as\": \"start\"}}", 1, "start"),
            ("{\"assert\": {\"doc_same_as\": \"start\"}, \"call\": \"a\"}", 1, "assert"),
        ] {
            let error = Script::parse(text).unwrap_err();
            assert_eq!(error.line, line, "{text}: {error}");
            assert!(error.reason.contains(word), "{text}: {error}");
        }
    }

    #[test]
    fn a_promoted_script_remembers_the_run_it_came_from() {
        let script = Script::from_log(&EventLog::new()).promoted_from("2026-10-13T1000Z-nightly/t.x/2");
        assert_eq!(script.promoted_run(), Some("2026-10-13T1000Z-nightly"));
        let text = script.to_jsonl().unwrap();
        assert_eq!(text, "{\"promoted_from\":\"2026-10-13T1000Z-nightly/t.x/2\"}\n");
        assert_eq!(Script::parse(&text).unwrap().promoted_from, script.promoted_from);
        for bad in ["{\"promoted_from\": 3}", "{\"promoted_from\": \"a\", \"call\": \"x\"}"] {
            assert!(Script::parse(bad).is_err(), "{bad}");
        }
        assert!(Script::parse("{\"promoted_from\": \"a\"}\n{\"promoted_from\": \"b\"}").is_err());
    }

    #[test]
    fn expect_is_a_subset_match() {
        let actual = json!({"ok": true, "mode": "dry_run", "output": {"a": 1, "b": 2}});
        assert!(is_subset(&json!({"ok": true}), &actual));
        assert!(is_subset(&json!({"output": {"a": 1}}), &actual));
        assert!(!is_subset(&json!({"output": {"a": 2}}), &actual));
        assert!(!is_subset(&json!({"missing": null}), &actual));
        assert!(!is_subset(&json!({"mode": "apply"}), &actual));
    }

    #[test]
    fn a_promoted_script_lands_beside_its_task_under_a_name_discovery_finds() {
        let task = Path::new("crates/c/evals/face-more-oval.toml");
        assert_eq!(
            promoted_path(task, "run-2", true).unwrap(),
            Path::new("crates/c/evals/face-more-oval.run-2.pass.jsonl")
        );
        assert!(promoted_path(task, "wrong", false).unwrap().ends_with("face-more-oval.wrong.fail.jsonl"));
        for bad in ["", "A", "a b", "a--b", "-a", "../x", "a.b"] {
            assert!(promoted_path(task, bad, true).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_promoted_log_writes_exports_back_as_names() {
        let mut log = EventLog::new();
        log.push(0, Event::Note { text: "exports".to_owned(), data: Some(json!({"main": "c-77"})) });
        log.push(1, Event::ToolCall { tool: "face_set".to_owned(), args: json!({"character": "c-77", "blend": 0.5}) });
        let script = Script::from_log(&log);
        assert_eq!(
            script.to_jsonl().unwrap(),
            "{\"args\":{\"blend\":0.5,\"character\":\"{main}\"},\"call\":\"face_set\"}\n"
        );
    }
}
