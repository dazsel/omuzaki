//! The transcript (design 7.3) as the `log.*` checks read it: what the server returned for each tool call, never what
//! the model claimed.

use std::collections::BTreeMap;

use omuzaki_mcp::McpServer;
use omuzaki_ops_api::Mode;
use rmcp::model::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool call and what the server answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The op name.
    pub op: String,
    /// The arguments as sent.
    pub args: Value,
    /// Whether the call was a dry run or applied.
    pub mode: Mode,
    /// `true` when the server answered with a result, `false` when it answered with an error.
    pub ok: bool,
    /// `true` when the server's answer carried an application record: the call really changed the project. A query
    /// runs in [`Mode::Apply`] and succeeds without one, so neither the mode nor `ok` says this.
    pub applied: bool,
}

impl ToolCall {
    /// A call of `op` with `args` in `mode`, and whether the server answered without an error. It did not change the
    /// project until [`ToolCall::with_application`] says so.
    pub fn new(op: impl Into<String>, args: Value, mode: Mode, ok: bool) -> Self {
        Self { op: op.into(), args, mode, ok, applied: false }
    }

    /// The same call, whose answer carried an application record.
    #[must_use]
    pub fn with_application(mut self) -> Self {
        self.applied = true;
        self
    }

    /// Whether this call changed the project: the server answered with an application record.
    #[must_use]
    pub fn applied(&self) -> bool {
        self.ok && self.applied
    }
}

/// The tool calls of one attempt, in the order the server saw them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    /// The calls, oldest first.
    pub calls: Vec<ToolCall>,
}

impl Transcript {
    /// A transcript of these calls.
    #[must_use]
    pub fn new(calls: Vec<ToolCall>) -> Self {
        Self { calls }
    }
}

/// How a server's tools are named, so a transcript that carries MCP tool names (`face_set_profile`) can be read in the op
/// names a task writes (`face.set_profile`).
///
/// The map comes from the server's own catalog ([`ToolNames::of_server`]: each tool's name and its `title`, the dotted
/// op name), never from replacing characters, so an op whose name already has an underscore (`face.set_profile`'s
/// `set_profile`) keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolNames {
    ops: BTreeMap<String, String>,
}

impl ToolNames {
    /// The map of the tools `server` lists: every op and every built-in (`history_undo` is `history.undo`).
    #[must_use]
    pub fn of_server(server: &McpServer) -> Self {
        Self::of_tools(&server.tools())
    }

    /// The map of a `tools/list` answer: each tool's name, and its `title`, which carries the dotted op name.
    #[must_use]
    pub fn of_tools(tools: &[Tool]) -> Self {
        Self { ops: tools.iter().filter_map(|tool| Some((tool.name.to_string(), tool.title.clone()?))).collect() }
    }

    /// A map from `(tool name, op name)` pairs.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self { ops: pairs.into_iter().collect() }
    }

    /// The op (or built-in) a tool name stands for, if the server lists such a tool.
    #[must_use]
    pub fn op_of(&self, tool: &str) -> Option<&str> {
        self.ops.get(tool).map(String::as_str)
    }

    /// The tool that stands for the op (or built-in) `op`, if the server lists one: the other way round from
    /// [`ToolNames::op_of`], for scripts that name an op.
    #[must_use]
    pub fn tool_of(&self, op: &str) -> Option<&str> {
        self.ops.iter().find(|(_, o)| o.as_str() == op).map(|(tool, _)| tool.as_str())
    }

    /// Whether `name` is an op or built-in name the map knows (as a target).
    #[must_use]
    pub fn knows_op(&self, name: &str) -> bool {
        self.ops.values().any(|op| op == name)
    }
}

/// One thing that happened in an attempt (design 7.3). The scripted driver writes the first, second and last kinds; the
/// reference driver adds the model's requests and responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A tool call sent to the server, by MCP tool name.
    ToolCall {
        /// The tool.
        tool: String,
        /// The arguments as sent.
        args: Value,
    },
    /// What the server answered: its `structuredContent` (op, mode, applied, changes, log), which `log.*` reads.
    ToolResult {
        /// The tool.
        tool: String,
        /// Whether the server marked the answer an error.
        is_error: bool,
        /// The answer's `structuredContent`, when it has one.
        structured: Option<Value>,
    },
    /// A request to the model (reference driver), without headers.
    ModelRequest {
        /// The request body.
        body: Value,
    },
    /// The model's response (reference driver).
    ModelResponse {
        /// The response body.
        body: Value,
    },
    /// A remark of the harness: the fixture's exports at the start of an attempt, a cap that stopped it.
    Note {
        /// What it is, such as `exports`.
        text: String,
        /// Anything that goes with it.
        data: Option<Value>,
    },
}

/// An [`Event`] and when it happened, in milliseconds since the attempt began.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamped {
    /// Milliseconds since the attempt began.
    pub t_ms: u64,
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
}

/// The record of an attempt: one JSON object per line, `{t_ms, kind, ...}`, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventLog {
    events: Vec<Stamped>,
}

impl EventLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `event`, which happened `t_ms` into the attempt.
    pub fn push(&mut self, t_ms: u64, event: Event) {
        self.events.push(Stamped { t_ms, event });
    }

    /// The events, oldest first.
    #[must_use]
    pub fn events(&self) -> &[Stamped] {
        &self.events
    }

    /// The log as JSON lines. Without `with_times` every `t_ms` is 0, which is the form two runs of one script agree
    /// on byte for byte: the timestamps are the only thing in a scripted transcript that varies.
    ///
    /// # Errors
    /// A serialisation error, which the event types cannot produce; returned rather than hidden.
    pub fn to_jsonl(&self, with_times: bool) -> Result<String, serde_json::Error> {
        let mut out = String::new();
        for stamped in &self.events {
            let line = Stamped { t_ms: if with_times { stamped.t_ms } else { 0 }, event: stamped.event.clone() };
            out.push_str(&serde_json::to_string(&line)?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Reads a log written by [`EventLog::to_jsonl`]. Blank lines are skipped.
    ///
    /// # Errors
    /// The line number and the parser's message for the first line that is not an event.
    pub fn from_jsonl(text: &str) -> Result<Self, String> {
        let mut events = Vec::new();
        for (index, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            events.push(serde_json::from_str(line).map_err(|e| format!("line {}: {e}", index + 1))?);
        }
        Ok(Self { events })
    }

    /// The tool calls of the log as the `log.*` checks read them: each call joined with its answer. A call the server
    /// never answered is a failed one. The tool names are as sent; the checker reads them through [`ToolNames`].
    #[must_use]
    pub fn transcript(&self) -> Transcript {
        let mut calls = Vec::new();
        let mut pending: Option<(&str, &Value)> = None;
        let mut finish = |pending: &mut Option<(&str, &Value)>, answer: Option<(bool, Option<&Value>)>| {
            let Some((tool, args)) = pending.take() else { return };
            let (ok, structured) = answer.map_or((false, None), |(is_error, structured)| (!is_error, structured));
            let dry = structured.and_then(|s| s.get("mode")).and_then(Value::as_str) == Some("dry_run");
            let mut call = ToolCall::new(tool, args.clone(), if dry { Mode::DryRun } else { Mode::Apply }, ok);
            if ok && structured.and_then(|s| s.get("applied")).and_then(Value::as_bool) == Some(true) {
                call = call.with_application();
            }
            calls.push(call);
        };
        for stamped in &self.events {
            match &stamped.event {
                Event::ToolCall { tool, args } => {
                    finish(&mut pending, None);
                    pending = Some((tool, args));
                }
                Event::ToolResult { is_error, structured, .. } => {
                    finish(&mut pending, Some((*is_error, structured.as_ref())));
                }
                _ => {}
            }
        }
        finish(&mut pending, None);
        Transcript::new(calls)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn log() -> EventLog {
        let mut log = EventLog::new();
        log.push(1, Event::Note { text: "exports".to_owned(), data: Some(json!({"main": "c1"})) });
        log.push(2, Event::ToolCall { tool: "a_b".to_owned(), args: json!({"dry_run": true}) });
        log.push(
            3,
            Event::ToolResult {
                tool: "a_b".to_owned(),
                is_error: false,
                structured: Some(json!({"ok": true, "mode": "dry_run", "applied": false})),
            },
        );
        log.push(4, Event::ToolCall { tool: "a_b".to_owned(), args: json!({}) });
        log.push(
            5,
            Event::ToolResult {
                tool: "a_b".to_owned(),
                is_error: false,
                structured: Some(json!({"ok": true, "mode": "apply", "applied": true})),
            },
        );
        log.push(6, Event::ToolCall { tool: "c_d".to_owned(), args: json!({}) });
        log.push(7, Event::ToolResult { tool: "c_d".to_owned(), is_error: true, structured: None });
        log
    }

    #[test]
    fn a_log_round_trips_through_json_lines_and_times_can_be_left_out() {
        let log = log();
        let text = log.to_jsonl(true).unwrap();
        assert_eq!(EventLog::from_jsonl(&text).unwrap(), log);
        assert!(text.lines().next().unwrap().starts_with("{\"t_ms\":1,\"kind\":\"note\""));
        let bare = log.to_jsonl(false).unwrap();
        assert!(bare.lines().all(|l| l.starts_with("{\"t_ms\":0,")));
        assert!(EventLog::from_jsonl("{\"t_ms\":1,\"kind\":\"bogus\"}").unwrap_err().starts_with("line 1"));
    }

    #[test]
    fn calls_are_read_with_the_answer_the_server_gave() {
        let calls = log().transcript().calls;
        let seen: Vec<(Mode, bool, bool)> = calls.iter().map(|c| (c.mode, c.ok, c.applied())).collect();
        assert_eq!(seen, [(Mode::DryRun, true, false), (Mode::Apply, true, true), (Mode::Apply, false, false)]);
    }

    #[test]
    fn a_call_the_server_never_answered_is_a_failed_one() {
        let mut log = EventLog::new();
        log.push(0, Event::ToolCall { tool: "a_b".to_owned(), args: json!({}) });
        let calls = log.transcript().calls;
        assert_eq!((calls.len(), calls[0].ok), (1, false));
    }
}
