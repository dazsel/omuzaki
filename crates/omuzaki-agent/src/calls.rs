//! A turn's tool calls: in the order the model emitted them, one at a time, answered in one message (D2.4).

use std::sync::Arc;
use std::time::Duration;

use omuzaki_agent_api::entry::{Entry, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_ops_api::EndReason;
use rmcp::model::JsonObject;
use serde_json::Value;

use crate::content::{plain_text, tool_result_blocks};
use crate::history::{Delivery, delivery_of, heads_up_of};
use crate::link::{LateFn, LinkError, Pending, ReplyBlock, ToolInfo, ToolReply};
use crate::refusal::{self, Refusal};
use crate::run::{AnswerRun, Flow};
use crate::tx::{self, TxUse};
use crate::worker::Worker;

/// What one call gives the model: its result blocks, whether it failed, and what the result tells the model once it is
/// stored (`history.rs`).
pub(crate) type CallOutcome = (Vec<ContentBlock>, bool, Option<Delivery>);

/// The longest a rate-limit pause is honoured (D2.6).
const MAX_RETRY_WAIT: f64 = 10.0;
/// How long one `approval_wait` is asked to hold, in seconds; `omuzaki-mcp` holds it at most this long.
const APPROVAL_HOLD_S: u64 = 20;
/// A call's result when an earlier answer's changes could not be closed (`tx.rs`). No call runs then; the text exists so
/// every `Flow` has one.
const UNCLOSED_TEXT: &str = "not run: the changes of an earlier answer are still open";

/// One tool call of a turn.
#[derive(Clone, Debug)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The tool calls in `blocks`, in the order the model emitted them.
pub(crate) fn tool_calls(blocks: &[ContentBlock]) -> Vec<ToolCall> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input, .. } => {
                Some(ToolCall { id: id.clone(), name: name.clone(), input: input.clone() })
            }
            _ => None,
        })
        .collect()
}

/// The tool's title (or its name, made readable) plus its main arguments: "Face profile: egg-oval, 50 %".
///
/// Strings come first, then numbers and booleans, each group in key order, at most three in all, so the line does not
/// depend on how the model ordered its arguments. A number between 0 and 1 is shown as a percentage.
pub(crate) fn summary(name: &str, title: Option<&str>, input: &Value) -> String {
    let heading = title.filter(|t| !t.is_empty()).map_or_else(|| humanize(name), humanize_title);
    let Some(args) = input.as_object() else { return heading };
    let shown = |v: &Value| match v {
        Value::String(s) => Some((0, s.chars().take(40).collect::<String>())),
        Value::Number(n) => n.as_f64().map(|f| (1, percent_or_plain(f))),
        Value::Bool(b) => Some((1, b.to_string())),
        _ => None,
    };
    let mut parts: Vec<(u8, String)> =
        args.iter().filter(|(key, _)| key.as_str() != "base").filter_map(|(_, v)| shown(v)).collect();
    parts.sort_by_key(|(group, _)| *group);
    parts.truncate(3);
    if parts.is_empty() {
        return heading;
    }
    let parts: Vec<String> = parts.into_iter().map(|(_, text)| text).collect();
    format!("{heading}: {}", parts.join(", "))
}

fn percent_or_plain(value: f64) -> String {
    if (0.0..=1.0).contains(&value) && value.fract() != 0.0 {
        format!("{} %", (value * 100.0).round())
    } else {
        format!("{value}")
    }
}

fn humanize_title(title: &str) -> String {
    title.to_owned()
}

fn humanize(name: &str) -> String {
    let words = name.replace(['_', '.'], " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

impl Worker {
    /// The server's tool list, fetched once.
    pub(crate) fn tool_list(&mut self) -> Result<&[ToolInfo], HostError> {
        if self.tools.is_none() {
            let pending = self.link.list_tools();
            let tools = match self.await_mcp(&pending) {
                Ok(answer) => answer.map_err(HostError::from)?,
                Err(Flow::Stopped) => return Err(HostError::Mcp("stopped".into())),
                Err(Flow::Revoked(_) | Flow::Unclosed) => return Err(HostError::SessionEnded),
                Err(Flow::Fail(error)) => return Err(error),
            };
            self.tools = Some(tools);
        }
        Ok(self.tools.as_deref().unwrap_or_default())
    }

    /// The one wait for an MCP request on the host thread: every tool call (the model's and the host's own: `tx_*`,
    /// `history_log`, `session_info`, `project_describe`, `approval_wait`), every resource read and the tool list go
    /// through it, and nothing else waits on the server. Stop abandons it at once (D4): a tool call's cancel flag is
    /// Stop's, and the wait gives up the moment Stop is pressed. A closed link abandons it too. A request abandoned for
    /// any reason may have been answered by the server, which then moved its position in the op log past a heads-up the
    /// host never got, so it marks the log unread (`history.rs`). `Ok` is the server's answer, protocol errors included.
    pub(crate) fn await_mcp<T>(
        &mut self,
        pending: &Pending<Result<T, LinkError>>,
    ) -> Result<Result<T, LinkError>, Flow> {
        let flow = loop {
            match pending.poll(Duration::from_millis(1)) {
                Some(Ok(Err(LinkError::Cancelled))) => break Flow::Stopped,
                Some(Ok(Err(LinkError::Closed)) | Err(_)) => break self.link_down(),
                Some(Ok(answer)) => return Ok(answer),
                None if self.shared.stopping() => break Flow::Stopped,
                None => {}
            }
        };
        self.mark_unread();
        Err(flow)
    }

    fn tool_info(&self, name: &str) -> Option<ToolInfo> {
        self.tools.as_ref().and_then(|tools| tools.iter().find(|t| t.name == name).cloned())
    }

    /// Calls a tool and waits for the result, without any of D2.6's behaviour. Stop abandons the call at once; a link
    /// that closed ends the answer; a protocol error becomes an error result the model can read. A heads-up the result
    /// carries is held for the model until the caller says the model received it (`history.rs`).
    pub(crate) fn call_plain(&mut self, name: &str, args: Option<JsonObject>) -> Result<ToolReply, Flow> {
        self.call_plain_with(name, args, None)
    }

    fn call_plain_with(
        &mut self,
        name: &str,
        args: Option<JsonObject>,
        late: Option<LateFn>,
    ) -> Result<ToolReply, Flow> {
        let cancel = self.shared.fresh_cancel();
        let pending = self.link.call_tool(name, args, cancel, late);
        match self.await_mcp(&pending)? {
            Ok(reply) => {
                self.took_reply(&reply);
                Ok(reply)
            }
            Err(LinkError::Protocol(text)) => Ok(protocol_reply(&text)),
            // `await_mcp` gives up on the others itself.
            Err(LinkError::Cancelled | LinkError::Closed) => Err(self.link_down()),
        }
    }

    /// The flow for a link that closed: the session's own end if it has one.
    fn link_down(&self) -> Flow {
        self.ended()
            .map_or_else(|| Flow::Fail(HostError::Mcp("the connection to the studio closed".into())), Flow::Revoked)
    }

    /// Runs the calls of one turn and appends their results as one user message. If Stop or a revoke ends the answer
    /// midway, every call still gets a result, so the history stays valid.
    pub(crate) fn run_calls(&mut self, run: &mut AnswerRun, blocks: &[ContentBlock]) -> Result<(), Flow> {
        let calls = tool_calls(blocks);
        let mut results = Vec::with_capacity(calls.len());
        let mut ended: Option<Flow> = None;
        for call in &calls {
            if let Some(flow) = &ended {
                results.push(unrun(&call.id, &not_run_text(flow)));
                continue;
            }
            if run.counters.reached_during_calls(&self.cfg.caps, self.now()).is_some() {
                results.push(unrun(&call.id, "not run: the limit for this message was reached"));
                continue;
            }
            match self.execute(run, call) {
                Ok(result) => results.push(result),
                Err(flow) => {
                    results.push(unrun(&call.id, &in_flight_text(&flow)));
                    ended = Some(flow);
                }
            }
        }
        if !results.is_empty() {
            self.push_user(results);
        }
        ended.map_or(Ok(()), Err)
    }

    /// One call: its events and entries, and its result for the model.
    fn execute(&mut self, run: &mut AnswerRun, call: &ToolCall) -> Result<ContentBlock, Flow> {
        self.gate(run)?;
        let info = self.tool_info(&call.name);
        let summary = summary(&call.name, info.as_ref().and_then(|t| t.title.as_deref()), &call.input);
        self.shared.emit(AssistantEvent::tool_call(&call.id, &call.name, &summary));
        self.shared.record(Entry::tool_call(&call.id, &call.name, &summary));
        run.counters.call();
        let outcome = if crate::tools::is_host_tool(&call.name) {
            self.host_tool(run, call)
        } else {
            self.mcp_call(run, call, info.as_ref())
        };
        let (blocks, is_error, delivery) = match outcome {
            Ok(done) => done,
            Err(flow) => {
                let text = in_flight_text(&flow);
                self.shared.record(Entry::tool_result(&call.id, false, vec![StoredBlock::Text(text)]));
                return Err(flow);
            }
        };
        self.shared.emit(AssistantEvent::tool_result(&call.id, !is_error));
        let stored = self.store_blocks(&blocks);
        self.shared.record(Entry::tool_result(&call.id, !is_error, stored));
        // Stored, and in this turn's results: what it carries is told once the request that sends it is (`history.rs`).
        // Any other heads-up of the call (a refusal the host acted on) is owed.
        if let Some(delivery) = delivery {
            self.queue_delivery(delivery);
        }
        self.settle_pending();
        Ok(if is_error {
            ContentBlock::tool_error(call.id.clone(), blocks)
        } else {
            ContentBlock::tool_result(call.id.clone(), blocks)
        })
    }

    /// The blocks of a result as the log keeps them: text as text, images as blobs.
    fn store_blocks(&self, blocks: &[ContentBlock]) -> Vec<StoredBlock> {
        blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(StoredBlock::Text(text.clone())),
                ContentBlock::Image { mime, bytes, .. } => Some(
                    self.cfg
                        .sink
                        .put_blob(bytes, *mime)
                        .unwrap_or_else(|e| StoredBlock::Text(format!("[image not stored: {e}]"))),
                ),
                _ => None,
            })
            .collect()
    }

    /// A call to the server: classified, joined to the transaction, and given D2.6's behaviour.
    fn mcp_call(&mut self, run: &mut AnswerRun, call: &ToolCall, info: Option<&ToolInfo>) -> Result<CallOutcome, Flow> {
        let kind = tx::kind_of(info);
        let undo_like = matches!(call.name.as_str(), "history_undo" | "history_redo");
        if undo_like && self.tx_open() {
            let (blocks, is_error) = note_result(tx::UNDO_WHILE_OPEN);
            return Ok((blocks, is_error, None));
        }
        if tx::tx_use(kind) == TxUse::Join
            && !self.tx_open()
            && let Some(refused) = self.begin_tx(run)?
        {
            return Ok(self.given(&call.name, None, &refused));
        }
        let reply = self.send_with_refusals(run, call)?;
        Ok(self.given(&call.name, Some(&call.input), &reply))
    }

    /// `reply` as the result the model is given for its call of `tool`: its blocks, and what it tells the model, kept
    /// out of the owed heads-ups until the result is stored.
    pub(crate) fn given(&mut self, tool: &str, args: Option<&Value>, reply: &ToolReply) -> CallOutcome {
        let delivery = delivery_of(tool, args, reply);
        self.hold_for_delivery(&delivery);
        let (blocks, is_error) = tool_result_blocks(tool, reply);
        (blocks, is_error, Some(delivery))
    }

    fn arguments(call: &ToolCall) -> Option<JsonObject> {
        call.input.as_object().cloned()
    }

    /// Sends the call and acts on the refusals of D2.6.
    fn send_with_refusals(&mut self, run: &mut AnswerRun, call: &ToolCall) -> Result<ToolReply, Flow> {
        let (mut resent_paused, mut resent_limited) = (false, false);
        loop {
            let late = self.late_callback(call);
            let reply = self.call_plain_with(&call.name, Self::arguments(call), Some(late))?;
            match refusal::classify(&reply) {
                Some(Refusal::Queued(approval, need)) => {
                    // The resolution is not known here; the call's result carries it for the model and the log.
                    self.shared.record(Entry::approval(approval, need, None));
                    self.shared.emit(AssistantEvent::waiting(approval));
                    return self.wait_approval(run, approval);
                }
                Some(Refusal::Paused) if !resent_paused => {
                    resent_paused = true;
                    self.shared.emit(AssistantEvent::Paused);
                    self.wait_until_active(run)?;
                    self.shared.emit(AssistantEvent::Resumed);
                }
                Some(Refusal::RetryAfter(seconds)) if !resent_limited => {
                    resent_limited = true;
                    let wait = seconds.clamp(0.0, MAX_RETRY_WAIT);
                    crate::wait::for_seconds(self.cfg.clock.as_ref(), wait, || self.shared.stopping());
                    run.counters.exclude_seconds(wait);
                    self.check_stop()?;
                }
                Some(Refusal::Ended(reason)) => return Err(Flow::Revoked(reason)),
                _ => return Ok(reply),
            }
        }
    }

    /// Waits for a queued approval with `approval_wait` until it is no longer pending. No timeout: approvals never
    /// expire. The time spent is not wall time. Stop abandons the wait and leaves the approval queued.
    pub(crate) fn wait_approval(
        &mut self,
        run: &mut AnswerRun,
        approval: omuzaki_ops_api::ApprovalId,
    ) -> Result<ToolReply, Flow> {
        let started = self.now();
        let result = loop {
            let args = serde_json::json!({"id": approval.0, "timeout_seconds": APPROVAL_HOLD_S});
            let reply = match self.call_plain("approval_wait", args.as_object().cloned()) {
                Ok(reply) => reply,
                Err(flow) => break Err(flow),
            };
            if let Some(reason) = self.ended() {
                break Err(Flow::Revoked(reason));
            }
            let pending = self.sessions.approvals().iter().any(|a| a.id == approval);
            if !pending {
                break Ok(reply);
            }
            if self.shared.stopping() {
                break Err(Flow::Stopped);
            }
        };
        run.counters.exclude_seconds(self.now().0 - started.0);
        result
    }

    /// What records a result that arrives after Stop (D4): its entry, and a `Notice` for the heads-up it carries,
    /// together. Both are owed to the next message's context, which tells the model the call finished and what others
    /// changed (`history.rs`).
    fn late_callback(&self, call: &ToolCall) -> LateFn {
        let shared = Arc::clone(&self.shared);
        let id = call.id.clone();
        Box::new(move |result| {
            // A link that closed before the result came is not a result.
            let Ok(reply) = result else { return };
            let (ok, text) = (!reply.is_error, plain_text(&reply.blocks));
            // The id is suffixed so a resumed history does not read this as the answer to the original call.
            let mut entries = vec![Entry::tool_result(format!("{id}:late"), ok, vec![StoredBlock::Text(text)])];
            if let Some(heads_up) = heads_up_of(&reply) {
                entries.push(Entry::notice(heads_up.after, heads_up.upto, heads_up.json));
            }
            shared.record_all(entries);
        })
    }
}

fn protocol_reply(text: &str) -> ToolReply {
    ToolReply { blocks: vec![ReplyBlock::Text(text.to_owned())], is_error: true, structured: None }
}

fn note_result(text: &str) -> (Vec<ContentBlock>, bool) {
    (vec![ContentBlock::text(text)], true)
}

fn unrun(id: &str, text: &str) -> ContentBlock {
    ContentBlock::tool_error(id, vec![ContentBlock::text(text)])
}

fn in_flight_text(flow: &Flow) -> String {
    match flow {
        Flow::Stopped => "stopped by the person before its result arrived".to_owned(),
        Flow::Revoked(_) => "the session ended before its result arrived".to_owned(),
        Flow::Fail(e) => format!("not finished: {e}"),
        Flow::Unclosed => UNCLOSED_TEXT.to_owned(),
    }
}

fn not_run_text(flow: &Flow) -> String {
    match flow {
        Flow::Stopped => "not run: stopped by the person".to_owned(),
        Flow::Revoked(EndReason::Closed) => "not run: the session was closed".to_owned(),
        Flow::Revoked(_) => "not run: the session ended".to_owned(),
        Flow::Fail(e) => format!("not run: {e}"),
        Flow::Unclosed => UNCLOSED_TEXT.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_summary_is_the_title_plus_the_main_arguments() {
        let input = json!({"profile": "egg-oval", "blend": 0.5});
        assert_eq!(summary("face_set_profile", Some("Face profile"), &input), "Face profile: egg-oval, 50 %");
    }

    #[test]
    fn a_tool_without_a_title_reads_its_name() {
        assert_eq!(
            summary("render_preview", None, &json!({"view": "front", "size": 512})),
            "Render preview: front, 512"
        );
        assert_eq!(summary("history_log", None, &json!({})), "History log");
    }

    #[test]
    fn at_most_three_arguments_are_shown_and_base_is_not() {
        let input = json!({"a": "1", "b": "2", "c": "3", "d": "4", "base": 9});
        assert_eq!(summary("t", Some("T"), &input), "T: 1, 2, 3");
    }

    #[test]
    fn tool_calls_keep_the_order_the_model_emitted() {
        let blocks = vec![
            ContentBlock::text("x"),
            ContentBlock::tool_use("c2", "b", json!({})),
            ContentBlock::tool_use("c1", "a", json!({})),
        ];
        let ids: Vec<String> = tool_calls(&blocks).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["c2", "c1"]);
    }
}
