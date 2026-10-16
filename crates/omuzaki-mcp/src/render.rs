//! Turns what the runner returned into what the agent reads: a short text block it can act on, and the full outcome as
//! structured JSON. Clients differ in which of the two they show a model, so the text carries every fact needed to
//! continue (changes, answers, next steps) and the JSON carries everything else.

use omuzaki_doc_api::Change;
use omuzaki_ops_api::{
    CostHint, ErrorCode, ErrorDetail, JobInfo, JobState, Mode, OpError, OpKind, Outcome, PauseReason, TxId,
};
use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;
use serde_json::{Value, json};

use crate::approvals::need_text;
use crate::builtin::Builtin;
use crate::changes::{Report, actor_label, plain};
use crate::floats;

/// Change lines listed in the first text block. A longer patch is listed in full in a second block.
const MAX_CHANGE_LINES: usize = 100;
/// Longest before/after value shown on a change line, in characters; the second block carries the full value.
const MAX_VALUE_CHARS: usize = 200;
/// An output larger than this is announced in the first line (it is still included in full).
const LARGE_OUTPUT_BYTES: usize = 100_000;

/// An open transaction, so results can remind the agent to finish it.
#[derive(Clone, Debug)]
pub(crate) struct OpenTx {
    pub id: TxId,
    pub label: String,
    /// What the person asked for, stamped on each edit that joins the transaction (`tx_begin`'s `cause`).
    pub cause: Option<String>,
}

/// What a successful op call needs beyond the outcome itself.
pub(crate) struct Call<'a> {
    pub op: &'a str,
    pub kind: OpKind,
    pub mode: Mode,
    /// The session's open transaction, when the op joined it.
    pub tx: Option<&'a OpenTx>,
    /// "Check afterwards" advice, for edits.
    pub verify: Option<&'a str>,
}

/// A successful op call (dry-run or applied).
///
/// Answers are never cut: clients that only show `content` to the model must get everything, so the text carries the
/// full output as pretty JSON (announced in the first line when it is large) and the structured copy repeats it.
pub(crate) fn success(call: &Call<'_>, outcome: &Outcome) -> CallToolResult {
    let plan = &outcome.plan;
    // Shown without the digits an `f32` widened to `f64` brings (see `floats`); the document keeps what it holds.
    let tidy_changes: Vec<Change> = plan
        .patch
        .changes
        .iter()
        .map(|c| Change {
            path: c.path.clone(),
            before: c.before.clone().map(floats::tidied),
            after: c.after.clone().map(floats::tidied),
        })
        .collect();
    let changes = &tidy_changes;
    let shown_output = plan.output.clone().map(floats::tidied);
    let applied = outcome.applied.is_some();
    let mut data = json!({"ok": true, "op": call.op});
    if call.kind != OpKind::Query {
        // A query has nothing to preview or apply, so it reports neither.
        data["mode"] = value_of(&call.mode);
        data["applied"] = json!(applied);
    }
    let fields = [
        ("output", shown_output.clone()),
        ("changes", (!changes.is_empty()).then(|| value_of(changes))),
        ("cost", plan.cost.as_ref().map(value_of)),
        ("log", outcome.applied.as_ref().map(|r| json!({"seq": r.seq, "tx": r.tx.0, "version": r.version.0}))),
    ];
    for (key, value) in fields {
        if let Some(value) = value {
            data[key] = value;
        }
    }

    let output = shown_output.as_ref().map(pretty);
    let mut text = vec![headline(call, outcome)];
    if let Some(output) = output.as_ref().filter(|o| o.len() > LARGE_OUTPUT_BYTES) {
        let note = format!(" The output is large ({} KB); all of it follows.", output.len() / 1000);
        text[0].push_str(&note);
    }
    let listing = change_lines(changes);
    text.extend(listing.lines);
    text.extend(plan.cost.as_ref().and_then(cost_line));
    text.extend(next_steps(call, applied));
    if let Some(output) = output {
        text.push(format!("Output:\n{output}"));
    }
    let mut blocks = vec![text.join("\n")];
    if !listing.complete {
        blocks.push(format!("Full changes (the list above is shortened):\n{}", pretty(&value_of(changes))));
    }
    result(blocks, data, false)
}

fn headline(call: &Call<'_>, outcome: &Outcome) -> String {
    let n = outcome.plan.patch.changes.len();
    let values = |n: usize| format!("{n} value{}", if n == 1 { "" } else { "s" });
    let op = call.op;
    match (call.kind, call.mode, &outcome.applied) {
        // Queries never change anything, so `dry_run` makes no difference to them.
        (OpKind::Query, ..) => format!("Ran {op} (read-only)."),
        (OpKind::Job, Mode::DryRun, _) if n == 0 => format!("Dry run of {op}: nothing was started."),
        (_, Mode::DryRun, _) if n == 0 => format!("Dry run of {op}: it would change nothing. Nothing was applied."),
        (_, Mode::DryRun, _) => format!("Dry run of {op}: it would change {}. Nothing was applied.", values(n)),
        (_, Mode::Apply, Some(record)) => format!(
            "Applied {op} (log #{}, transaction {}, document version {}): {}.",
            record.seq,
            record.tx.0,
            record.version.0,
            if n == 0 { "no values changed".to_owned() } else { format!("changed {}", values(n)) }
        ),
        (_, Mode::Apply, None) => format!("Ran {op}."),
    }
}

/// The change lines for the first text block, and whether they show every change in full.
struct Listing {
    lines: Vec<String>,
    complete: bool,
}

fn change_lines(changes: &[Change]) -> Listing {
    let mut complete = changes.len() <= MAX_CHANGE_LINES;
    let mut show = |v: &Option<Value>| match v {
        None => "absent".to_owned(),
        Some(v) => {
            let (text, cut) = shorten(&v.to_string(), MAX_VALUE_CHARS);
            complete &= !cut;
            text
        }
    };
    let mut lines: Vec<String> = changes
        .iter()
        .take(MAX_CHANGE_LINES)
        .map(|c| format!("  {}: {} -> {}", c.path, show(&c.before), show(&c.after)))
        .collect();
    if changes.len() > MAX_CHANGE_LINES {
        lines.push(format!("  ... and {} more changes (listed in full below)", changes.len() - MAX_CHANGE_LINES));
    }
    Listing { lines, complete }
}

fn cost_line(cost: &CostHint) -> Option<String> {
    let parts: Vec<String> = [
        cost.seconds.map(|s| format!("about {s} s")),
        cost.gpu_mib.map(|m| format!("{m} MiB of GPU memory")),
        cost.usd.map(|u| format!("${u:.2}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("Estimated cost: {}.", parts.join(", ")))
}

/// What the agent should do next, depending on what just happened.
fn next_steps(call: &Call<'_>, applied: bool) -> Vec<String> {
    let mut steps = Vec::new();
    match (call.mode, call.kind) {
        (_, OpKind::Query) => {}
        (Mode::DryRun, _) => steps.push(format!("To apply, call {} again without dry_run.", call.op)),
        (Mode::Apply, OpKind::Edit | OpKind::Job) if applied => {
            if let Some(tx) = call.tx {
                steps.push(format!(
                    "Part of open transaction {} ({:?}): call tx_commit to finish it as one undo step, or tx_rollback \
                     to discard it.",
                    tx.id.0, tx.label
                ));
            } else {
                steps.push("To revert, call history_undo.".to_owned());
            }
            if let (OpKind::Edit, Some(verify)) = (call.kind, call.verify) {
                steps.push(format!("Next: {verify}."));
            }
        }
        _ => {}
    }
    steps
}

/// A job that ended and applied its change: the task's result. When the job kept the op's `Outcome` (it does, unless
/// it was recovered from a journal) this is exactly the result the blocking call of the same op returns; otherwise
/// it reports what the snapshot knows: the log entry and the undo step.
pub(crate) fn job_done(op: &str, info: &JobInfo, verify: Option<&str>) -> CallToolResult {
    let JobState::Done { tx, seq, .. } = &info.state else {
        return failure(op, &OpError::internal("the job has not finished"), None);
    };
    if let Some(outcome) = info.state.outcome() {
        return success(&Call { op, kind: OpKind::Job, mode: Mode::Apply, tx: None, verify }, outcome);
    }
    let (log, headline) = match seq {
        Some(seq) => (json!(seq), format!("Ran {op} as job {} (log #{seq}, transaction {}).", info.id.0, tx.0)),
        None => (Value::Null, format!("Ran {op} as job {}; it changed nothing.", info.id.0)),
    };
    let data = json!({"ok": true, "op": op, "job": info.id.0, "label": info.label, "applied": seq.is_some(), "log": log, "tx": tx.0});
    result(vec![headline], data, false)
}

/// A failed call. Always a tool error (not a protocol error) so the agent sees code, field and hint.
///
/// `signature` is added for `invalid` errors so the agent sees the expected call shape next to the complaint.
pub(crate) fn failure(op: &str, error: &OpError, signature: Option<&str>) -> CallToolResult {
    let signature = signature.filter(|_| error.code == ErrorCode::Invalid);
    let explained = explain(error);
    let message = explained.as_ref().map_or(error.message.as_str(), |e| e.message.as_str());
    let hint = match signature {
        // An argument that doesn't decode carries ops-api's hint, which is aimed at the CLI (`omuzaki help`); over MCP
        // the expected call is right here.
        Some(_) if error.is_decode_error() => {
            "match the `Expected:` call below; the message says what didn't fit".to_owned()
        }
        _ => explained
            .as_ref()
            .map(|e| e.hint.clone())
            .or_else(|| error.hint.clone())
            .unwrap_or_else(|| default_hint(error.code).to_owned()),
    };
    let mut data = json!({
        "ok": false,
        "op": op,
        "code": value_of(&error.code),
        "message": message,
        "hint": hint,
    });
    if let Some(field) = &error.field {
        data["field"] = json!(field);
    }
    if let Some(detail) = error.detail.as_deref().and_then(detail_json) {
        data["detail"] = detail;
    }
    if let Some(signature) = signature {
        data["signature"] = json!(signature);
    }

    // `args` means the arguments as a whole didn't fit the schema; the message says why.
    let at = error.field.as_ref().filter(|f| *f != "args").map(|f| format!(" (argument `{f}`)")).unwrap_or_default();
    let mut text = vec![format!("{op} failed [{}]{at}: {message}", error.code), format!("Hint: {hint}")];
    if let Some(signature) = signature {
        text.push(format!("Expected: {signature}"));
    }
    result(vec![text.join("\n")], data, true)
}

/// The words for a refusal whose detail says more than the executor's own message: the session is paused, the request
/// was queued for a person, or a limit was hit. They name what to do next, not what the executor did.
struct Explained {
    message: String,
    hint: String,
}

fn explain(error: &OpError) -> Option<Explained> {
    match (error.code, error.detail.as_deref()?) {
        (ErrorCode::Waiting, ErrorDetail::Paused { reason, .. }) => {
            let why = match reason {
                PauseReason::Person => "the person paused this session",
                PauseReason::Budget => "this session paused itself because its budget ran out",
                PauseReason::Undo => "the person is undoing this session's work, so it is paused",
                PauseReason::Step => "this session pauses after each step",
                PauseReason::Pairing => "this session is waiting to be paired in Omuzaki's window",
            };
            Some(Explained {
                message: format!("{why}; nothing ran"),
                hint: "wait for a person to resume it; do not retry. session_info says why it waits".to_owned(),
            })
        }
        (ErrorCode::Waiting, ErrorDetail::Queued { approval, need }) => Some(Explained {
            message: format!("approval #{} asks the person to {}; nothing ran", approval.0, need_text(need)),
            hint: format!(
                "don't retry: call approval_wait with id {} to wait for the answer, or carry on with other work",
                approval.0
            ),
        }),
        (ErrorCode::Limited, ErrorDetail::RetryAfter { seconds, limit }) => Some(Explained {
            message: format!("this session hit its {} limit; nothing ran", plain(limit, 60)),
            hint: format!("wait {seconds:.1} seconds, then retry"),
        }),
        _ => None,
    }
}

/// The structured form of a refusal's detail, for the details an agent can act on. Names that other parties chose go
/// through the same sanitiser as the heads-up.
fn detail_json(detail: &ErrorDetail) -> Option<Value> {
    Some(match detail {
        ErrorDetail::Fenced { path, holder } => {
            let mut out = json!({"kind": "fenced", "path": plain(&path.to_string(), 100)});
            if let Some(holder) = holder {
                out["holder"] = json!(actor_label(holder));
            }
            out
        }
        ErrorDetail::Paused { by, reason } => {
            json!({"kind": "paused", "by": actor_label(by), "reason": value_of(reason)})
        }
        ErrorDetail::Queued { approval, need } => {
            json!({"kind": "queued", "approval": approval.0, "need": value_of(need), "asks": need_text(need)})
        }
        ErrorDetail::RetryAfter { seconds, limit } => {
            json!({"kind": "retry_after", "seconds": seconds, "limit": plain(limit, 60)})
        }
        ErrorDetail::Changed { path, by, version } => {
            json!({"kind": "changed", "path": plain(&path.to_string(), 100), "by": actor_label(by), "version": version.0})
        }
        _ => return None,
    })
}

/// What to do about an error whose op gave no hint of its own.
fn default_hint(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Invalid => "check the argument against the tool's inputSchema (tools/list) and retry",
        ErrorCode::NotFound => "check the id or name with a read-only tool, then retry",
        ErrorCode::Denied => "this session lacks a required scope; tell the user, who can grant it",
        ErrorCode::Conflict => {
            "the project changed since the call was planned; read its current state with a read-only tool, then retry"
        }
        ErrorCode::Unsupported => "not available here; try another approach or tell the user",
        ErrorCode::Unavailable => "the app has not wired in a service this needs; tell the user, or retry once it is",
        ErrorCode::Internal => "this is a bug in Omuzaki; tell the user and don't retry unchanged",
        ErrorCode::Waiting => "nothing ran: a person must decide in Omuzaki first; don't retry, wait for the result",
        ErrorCode::Limited => "this session hit a rate limit; wait the time in the detail, then retry",
        ErrorCode::Cancelled => "the job was cancelled; start it again if you still want it",
        // A code added later reads as a failure the caller can't act on until this table learns it.
        _ => "check the error's message and hint, then retry or tell the user",
    }
}

/// Undo or redo finished: `label` is what changed, or `None` when there was nothing to do.
pub(crate) fn history(direction: Builtin, label: Option<&str>) -> CallToolResult {
    let (past, noun, reverse) = match direction {
        Builtin::Redo => ("Redid", "redo", "history_undo"),
        _ => ("Undid", "undo", "history_redo"),
    };
    let (text, data) = match label {
        Some(label) => (
            format!("{past} {label:?}. To reverse this, call {reverse}."),
            json!({"ok": true, "done": true, "label": label}),
        ),
        None => (format!("Nothing to {noun}."), json!({"ok": true, "done": false})),
    };
    result(vec![text], data, false)
}

/// Puts what others changed since the agent's last call at the top of `result`'s text and under
/// `structuredContent.since_your_last_call`.
///
/// It goes first because it can change what the agent does with everything below it, and in the same text block so
/// that a client showing only the first block still shows it. It applies to every result, errors included: a
/// `conflict` is often exactly the other party's edit.
pub(crate) fn with_heads_up(mut result: CallToolResult, report: &Report) -> CallToolResult {
    let heads_up = report.text();
    match result.content.first().and_then(ContentBlock::as_text) {
        Some(first) => result.content[0] = ContentBlock::text(format!("{heads_up}\n\n{}", first.text)),
        None => result.content.insert(0, ContentBlock::text(heads_up)),
    }
    if let Some(Value::Object(data)) = result.structured_content.as_mut() {
        data.insert("since_your_last_call".to_owned(), report.data());
    }
    result
}

/// Puts a paragraph of news about the agent's own approvals at the top of `result`'s text, and the same facts under
/// `structuredContent.approvals`.
pub(crate) fn with_approvals(mut result: CallToolResult, text: &str, data: Value) -> CallToolResult {
    match result.content.first().and_then(ContentBlock::as_text) {
        Some(first) => result.content[0] = ContentBlock::text(format!("{text}\n\n{}", first.text)),
        None => result.content.insert(0, ContentBlock::text(text)),
    }
    if let Some(Value::Object(map)) = result.structured_content.as_mut() {
        map.insert("approvals".to_owned(), data);
    }
    result
}

/// Puts `warning` at the top of `result`'s text, as a `Warning:` line, and under `structuredContent.warning`. It goes
/// in front of whatever is already there, the heads-up included, so it is the first line of the reply.
pub(crate) fn with_warning(mut result: CallToolResult, warning: &str) -> CallToolResult {
    let line = format!("Warning: {warning}");
    match result.content.first().and_then(ContentBlock::as_text) {
        Some(first) => result.content[0] = ContentBlock::text(format!("{line}\n\n{}", first.text)),
        None => result.content.insert(0, ContentBlock::text(line)),
    }
    if let Some(Value::Object(data)) = result.structured_content.as_mut() {
        data.insert("warning".to_owned(), json!(warning));
    }
    result
}

/// A plain confirmation with structured data (`tx.*`).
pub(crate) fn confirmation(text: String, data: Value) -> CallToolResult {
    result(vec![text], data, false)
}

/// The tool name wasn't found.
pub(crate) fn unknown_tool(name: &str, similar: &[&str]) -> CallToolResult {
    let hint = if similar.is_empty() {
        "list the available tools with tools/list".to_owned()
    } else {
        format!("did you mean `{}`? (all tools: tools/list)", similar.join("`, `"))
    };
    failure(name, &OpError::not_found(format!("there is no tool named `{name}`")).hint(hint), None)
}

/// `blocks` become the result's text content blocks, one each.
fn result(blocks: Vec<String>, data: Value, is_error: bool) -> CallToolResult {
    let content: Vec<ContentBlock> = blocks.into_iter().map(ContentBlock::text).collect();
    let mut result = if is_error { CallToolResult::error(content) } else { CallToolResult::success(content) };
    result.structured_content = Some(data);
    result
}

/// Plain data as JSON. These types can't fail to serialise; if one ever does, say so in the output rather than
/// panicking or dropping the field silently.
fn value_of<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).map_or_else(|e| json!({"serialisation_error": e.to_string()}), floats::tidied)
}

/// Indented JSON: the form agents read best.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `text` cut to `max` characters, and whether it was cut.
fn shorten(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_owned(), false);
    }
    let cut: String = text.chars().take(max).collect();
    (format!("{cut}... ({} chars)", text.chars().count()), true)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use omuzaki_core::time::Seconds;
    use omuzaki_doc_api::{DocPath, DocVersion, Patch};
    use omuzaki_ops_api::{Actor, Event, OpRecord, Plan};

    use super::*;
    use crate::changes::summarise;
    use crate::events::Entry;

    fn patch(n: usize) -> Patch {
        (0..n).fold(Patch::new(), |p, i| {
            p.with(DocPath::parse(&format!("a/{i}")).unwrap(), Some(json!(i)), Some(json!(i + 1)))
        })
    }

    fn record() -> OpRecord {
        OpRecord::new(
            7,
            "face.set_profile",
            json!({}),
            Actor::System,
            TxId(4),
            Seconds(0.0),
            Patch::new(),
            DocVersion(12),
        )
    }

    fn call(mode: Mode) -> Call<'static> {
        Call {
            op: "face.set_profile",
            kind: OpKind::Edit,
            mode,
            tx: None,
            verify: Some("check it with `render_preview`"),
        }
    }

    fn text(result: &CallToolResult) -> String {
        result.content[0].as_text().unwrap().text.clone()
    }

    #[test]
    fn dry_run_lists_changes_and_says_nothing_was_applied() {
        let outcome = Outcome::new(Plan::edit(patch(2)));
        let result = success(&call(Mode::DryRun), &outcome);
        let text = text(&result);
        assert!(
            text.starts_with("Dry run of face.set_profile: it would change 2 values. Nothing was applied."),
            "{text}"
        );
        assert!(text.contains("  a/0: 0 -> 1") && text.contains("again without dry_run"));
        let data = result.structured_content.unwrap();
        assert_eq!((data["ok"].clone(), data["applied"].clone()), (json!(true), json!(false)));
        assert_eq!(data["changes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn applied_edit_reports_the_log_entry_and_how_to_undo_and_verify() {
        let outcome = Outcome::new(Plan::edit(patch(1))).with_applied(record());
        let result = success(&call(Mode::Apply), &outcome);
        let text = text(&result);
        assert!(
            text.contains("Applied face.set_profile (log #7, transaction 4, document version 12): changed 1 value."),
            "{text}"
        );
        assert!(text.contains("history_undo") && text.contains("render_preview"));
        assert_eq!(result.structured_content.unwrap()["log"], json!({"seq": 7, "tx": 4, "version": 12}));
    }

    #[test]
    fn edits_inside_a_transaction_remind_the_agent_to_commit() {
        let tx = OpenTx { id: TxId(4), label: "oval face".into(), cause: None };
        let outcome = Outcome::new(Plan::edit(patch(1))).with_applied(record());
        let text = text(&success(&Call { tx: Some(&tx), ..call(Mode::Apply) }, &outcome));
        assert!(text.contains("open transaction 4 (\"oval face\")") && text.contains("tx_commit"));
        assert!(!text.contains("history_undo"));
    }

    #[test]
    fn jobs_and_queries_describe_a_preview_in_their_own_terms() {
        let job = Call { kind: OpKind::Job, ..call(Mode::DryRun) };
        let plan =
            Plan { cost: Some(CostHint { seconds: Some(12.0), gpu_mib: None, usd: Some(0.5) }), ..Plan::default() };
        let preview = text(&success(&job, &Outcome::new(plan)));
        assert!(preview.starts_with("Dry run of face.set_profile: nothing was started."), "{preview}");
        assert!(preview.contains("Estimated cost: about 12 s, $0.50."));

        let query = Call { kind: OpKind::Query, ..call(Mode::DryRun) };
        let answer = text(&success(&query, &Outcome::new(Plan::answer(json!(1)))));
        assert_eq!(answer, "Ran face.set_profile (read-only).\nOutput:\n1", "dry_run changes nothing for a query");
    }

    #[test]
    fn queries_put_the_answer_in_the_text() {
        let outcome = Outcome::new(Plan::answer(json!({"outline": [1, 2]})));
        let query = Call { kind: OpKind::Query, ..call(Mode::Apply) };
        let text = text(&success(&query, &outcome));
        assert_eq!(text, "Ran face.set_profile (read-only).\nOutput:\n{\n  \"outline\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn queries_report_neither_mode_nor_applied() {
        let outcome = Outcome::new(Plan::answer(json!(1)));
        let data = success(&Call { kind: OpKind::Query, ..call(Mode::Apply) }, &outcome).structured_content.unwrap();
        assert_eq!(data, json!({"ok": true, "op": "face.set_profile", "output": 1}));
    }

    #[test]
    fn answers_are_never_cut_and_a_huge_one_is_announced_in_the_first_line() {
        let big = json!({"parts": (0..20_000).map(|i| format!("part-{i:05}")).collect::<Vec<_>>()});
        let outcome = Outcome::new(Plan::answer(big.clone()));
        let result = success(&Call { kind: OpKind::Query, ..call(Mode::Apply) }, &outcome);
        let text = text(&result);
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("Ran face.set_profile (read-only). The output is large ("), "{first}");
        let (_, json_part) = text.split_once("Output:\n").unwrap();
        assert_eq!(serde_json::from_str::<Value>(json_part).unwrap(), big, "the whole answer is in the text");
        assert_eq!(result.content.len(), 1);

        let small = text_of_small_answer();
        assert!(!small.contains("large"), "{small}");
    }

    fn text_of_small_answer() -> String {
        let outcome = Outcome::new(Plan::answer(json!({"a": 1})));
        text(&success(&Call { kind: OpKind::Query, ..call(Mode::Apply) }, &outcome))
    }

    #[test]
    fn a_patch_is_listed_in_full_and_oversized_values_get_a_full_second_block() {
        let result = success(&call(Mode::DryRun), &Outcome::new(Plan::edit(patch(30))));
        assert_eq!(result.content.len(), 1, "a short, small patch needs no second block");
        assert!(text(&result).contains("  a/29: 29 -> 30"));

        let big = Patch::new().with(DocPath::parse("x").unwrap(), None, Some(json!("y".repeat(500))));
        let result = success(&call(Mode::DryRun), &Outcome::new(Plan::edit(big)));
        assert!(text(&result).contains("... (502 chars)"));
        let full = result.content[1].as_text().unwrap().text.clone();
        assert!(full.starts_with("Full changes (the list above is shortened):") && full.contains(&"y".repeat(500)));

        let long = success(&call(Mode::DryRun), &Outcome::new(Plan::edit(patch(130))));
        assert!(text(&long).contains("... and 30 more changes (listed in full below)"));
        assert!(long.content[1].as_text().unwrap().text.contains("a/129"));
        assert_eq!(long.structured_content.unwrap()["changes"].as_array().unwrap().len(), 130);
    }

    #[test]
    fn argument_decode_failures_point_at_the_expected_call_not_the_cli() {
        let err = OpError::decode_failed("x.y", "args", "bad");
        let result = failure("x.y", &err, Some("x.y(a: number)"));
        let text = text(&result);
        assert!(text.contains("Hint: match the `Expected:` call below") && text.contains("Expected: x.y(a: number)"));
        assert!(!text.contains("omuzaki help"));
        assert_eq!(
            result.structured_content.unwrap()["hint"],
            "match the `Expected:` call below; the message says what didn't fit"
        );
    }

    #[test]
    fn errors_carry_code_field_hint_and_the_expected_signature() {
        let err = OpError::invalid("blend", "2 is out of range").hint("use 0..1");
        let result = failure("face.set_profile", &err, Some("face.set_profile(blend: number)"));
        assert_eq!(result.is_error, Some(true));
        let text = text(&result);
        assert_eq!(
            text,
            "face.set_profile failed [invalid] (argument `blend`): 2 is out of range\nHint: use 0..1\nExpected: face.set_profile(blend: number)"
        );
        let data = result.structured_content.unwrap();
        assert_eq!(
            (data["code"].clone(), data["field"].clone(), data["hint"].clone()),
            (json!("invalid"), json!("blend"), json!("use 0..1"))
        );
    }

    #[test]
    fn errors_without_a_hint_get_a_default_one_and_only_invalid_shows_the_signature() {
        let result = failure("x.y", &OpError::denied("needs publish"), Some("x.y()"));
        let text = text(&result);
        assert!(text.contains("tell the user") && !text.contains("Expected"));
    }

    #[test]
    fn a_missing_service_is_reported_as_unavailable_with_the_ops_own_hint_or_a_default() {
        let err = OpError::unavailable("dyn omuzaki_render_api::RenderService");
        let data = failure("render.preview", &err, None).structured_content.unwrap();
        assert_eq!(data["code"], "unavailable");
        assert!(data["message"].as_str().unwrap().contains("dyn omuzaki_render_api::RenderService"), "{data}");
        assert_eq!(data["hint"].as_str(), err.hint.as_deref());

        let bare = OpError { hint: None, ..err };
        let hint = failure("render.preview", &bare, None).structured_content.unwrap()["hint"].clone();
        assert!(hint.as_str().unwrap().contains("service"), "{hint}");
    }

    #[test]
    fn whole_argument_mismatches_do_not_name_a_fake_argument() {
        let err = OpError::decode_failed("x.y", "args", "expected an object");
        let text = text(&failure("x.y", &err, Some("x.y(a: number)")));
        assert!(text.starts_with("x.y failed [invalid]: arguments for x.y"), "{text}");
    }

    #[test]
    fn unknown_tools_suggest_neighbours() {
        let text = text(&unknown_tool("set_profile", &["face.set_profile"]));
        assert!(text.contains("[not_found]") && text.contains("did you mean `face.set_profile`?"));
    }

    /// A report of one change by `Actor::System`, for a session that is somebody else.
    fn heads_up() -> Report {
        let entries = VecDeque::from([Entry { n: 1, event: Event::op_applied(record()), version: None }]);
        let me = Actor::Agent { id: "claude#1".into(), model: None };
        summarise(&entries, 1, 0, &me, &crate::changes::OwnTxs::default()).unwrap()
    }

    #[test]
    fn the_heads_up_leads_the_text_and_joins_the_structured_result() {
        let outcome = Outcome::new(Plan::edit(patch(1))).with_applied(record());
        let plain = success(&call(Mode::Apply), &outcome);
        let result = with_heads_up(plain.clone(), &heads_up());

        let (before, after) = (text(&plain), text(&result));
        let expected_start =
            "Heads-up: since your last call, others changed the project (1 change; the log is now at n=1).";
        assert!(after.starts_with(expected_start), "{after}");
        assert!(after.ends_with(&format!("\n\n{before}")), "the result itself follows unchanged:\n{after}");
        assert_eq!(result.content.len(), 1, "same text block, so a client that shows only the first block shows it");

        let (mut data, original) = (result.structured_content.unwrap(), plain.structured_content.unwrap());
        assert_eq!(data["since_your_last_call"]["total"], 1);
        data.as_object_mut().unwrap().remove("since_your_last_call");
        assert_eq!(data, original, "nothing else in the structured result changes");
    }

    #[test]
    fn errors_carry_the_heads_up_too() {
        let failed = failure("x.y", &OpError::conflict("the face changed").hint("re-plan"), None);
        let result = with_heads_up(failed, &heads_up());
        assert_eq!(result.is_error, Some(true));
        let text = text(&result);
        assert!(
            text.starts_with("Heads-up:") && text.contains("\n\nx.y failed [conflict]: the face changed"),
            "{text}"
        );
        assert!(result.structured_content.unwrap()["since_your_last_call"].is_object());
    }

    #[test]
    fn history_results_say_what_changed_or_that_nothing_did() {
        assert_eq!(
            text(&history(Builtin::Undo, Some("oval face"))),
            "Undid \"oval face\". To reverse this, call history_redo."
        );
        assert_eq!(
            text(&history(Builtin::Redo, Some("oval face"))),
            "Redid \"oval face\". To reverse this, call history_undo."
        );
        assert_eq!(text(&history(Builtin::Undo, None)), "Nothing to undo.");
    }
}
