//! The answer's transaction: one user message is one undo step (D2.5), and the state machine that closes it.
//!
//! # A transaction's states
//!
//! The session has at most one open transaction, and the host tracks the one it opened ([`HostTx`], in
//! `Worker::tx`) from `tx_begin` until the executor confirms it closed. It belongs to the answer that opened it and
//! carries that answer's text, the rationale of the step it becomes.
//!
//! | State | Meaning |
//! |---|---|
//! | **Open** | `tx_begin` succeeded; the answer's edits and driver jobs join it. |
//! | **Closing** | A close was asked (`tx_commit`, `tx_rollback`, or `Sessions::stop`) or a commit was cut short (Stop cancelled the call, the link failed), so the executor may have applied it. Not confirmed yet. |
//! | **Retained** | The executor confirmed it no longer holds it (`SessionInfo::open_tx` names another or none), but not whether it became a step. Stored as an `Entry::Retained` and kept in `Worker::retained` until the log shows its outcome; a reopened host takes it back from the store (`history.rs`). |
//! | **Committed** (final) | It is an undo step: its `Step` entry is recorded, once, with its rationale. |
//! | **Rolled back** (final) | Its edits were reverted; no step. |
//! | **Unclosed** (the failed state) | Every close the host tried was refused and the executor still holds it open. The person is told; the host keeps it, and the next message closes it before anything else. |
//!
//! What moves it, and the only things that do:
//!
//! | From | Confirmation | To |
//! |---|---|---|
//! | none | `tx_begin` returns success | Open |
//! | Open | the answer ends and the host sends `tx_commit`; Stop: `Sessions::stop` is accepted; `discard_changes` sends `tx_rollback` | Closing |
//! | Open | the session ended (the executor rolls an open transaction back) | Rolled back |
//! | Closing | the close's own reply is a success: `tx_commit` | Committed |
//! | Closing | the close's own reply is a success: `tx_rollback` | Rolled back |
//! | Closing | `Sessions::stop` returns `Committed(id)` with its id | Committed |
//! | Closing | a refused `tx_rollback` from `discard_changes` (the executor keeps it open) | Open |
//! | Closing | a close is refused or unanswered, and `open_tx` no longer names it | Retained |
//! | Closing | a close is refused or unanswered, `open_tx` still names it, and a close is left to try | Closing (the next close) |
//! | Closing | the same, and no close is left to try | Unclosed |
//! | Retained | the log shows `tx_committed` with its id | Committed |
//! | Retained | the log shows `tx_rolled_back` with its id | Rolled back |
//! | Unclosed | the next message arrives | Closing |
//! | Unclosed | the session ended | Rolled back |
//!
//! Which closes are tried: at the end of an answer, the commit only, so changes a refused commit leaves open are kept for
//! the next message rather than lost; after Stop, the executor's own close (it commits what is left), and if the executor
//! refuses the Stop, the host's commit and then its rollback (each waited for, bounded in real time: nothing else would
//! close it); for an Unclosed transaction at the next message, the commit and then the rollback, which Stop cuts short
//! like any call of the answer ([`CloseCalls`]): the transaction stays Closing with its own rationale and Stop's ending
//! closes it, so `Done` never waits on MCP's journal.
//!
//! **The invariant** (`history.rs`): a mark advances only after what it stands for is stored where every later path
//! reads it. For a transaction: it leaves `Worker::tx` only once its outcome is stored (a `Step`), confirmed to need
//! none (rolled back), or stored as owed (`Retained`); a host being dropped retains one whose close was sent rather than
//! forget it.
//!
//! **How an answer ends.** With `Done` when the transaction it leaves is none, Committed, Rolled back or Retained: its
//! closure is confirmed, and a Retained step is recorded when the log shows it, asynchronously, so a stopped answer still
//! reports `Done` within D4's 100 ms. With [`Unclosed`](omuzaki_agent_api::event::AssistantEvent::Unclosed) when it is
//! Unclosed: every close failed, `Error` events have said why, and the panel shows the failed state. The check is made
//! where the answer ends (`Worker::finish`), so no path can report `Done` with a transaction the executor still holds.

use omuzaki_agent_api::entry::AnswerId;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_ops_api::SessionTx;
use serde_json::json;

use crate::content::plain_text;
use crate::link::{ToolInfo, ToolKind, ToolReply};
use crate::run::{AnswerRun, Flow};
use crate::worker::Worker;

/// The label prefix of an assistant step in the history.
const LABEL_PREFIX: &str = "Assistant: ";
/// The most characters of the person's first line in the label.
pub(crate) const LABEL_CHARS: usize = 60;
/// The most characters of the person's first line in the cause.
pub(crate) const CAUSE_CHARS: usize = 200;
/// How long a close Stop cannot cut short waits for its reply, in real time.
const CALL_AFTER_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Added to the label of a step that Stop closed.
pub(crate) const STOPPED_SUFFIX: &str = " (stopped)";

/// What a call needs of the transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TxUse {
    /// Opens the answer's transaction if it is not open, and joins it.
    Join,
    /// Never in a transaction.
    Outside,
}

/// Where a tracked transaction is (see the module docs). Committed and Rolled back are not states it is kept in: the
/// host records them and lets go; Retained ones move to `Worker::retained`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TxState {
    Open,
    Closing,
    Unclosed,
}

/// The transaction the host opened and the executor may still hold.
#[derive(Clone, Debug)]
pub(crate) struct HostTx {
    pub tx: SessionTx,
    /// The answer that opened it.
    pub answer: AnswerId,
    /// What that answer's model wrote, set when the answer ends: the rationale of the step.
    pub rationale: String,
    /// Added to the step's label.
    pub suffix: &'static str,
    pub state: TxState,
}

/// One way the host closes a transaction itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Close {
    Commit,
    Rollback,
}

impl Close {
    fn tool(self) -> &'static str {
        match self {
            Self::Commit => "tx_commit",
            Self::Rollback => "tx_rollback",
        }
    }
}

/// How the closes the host tried ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Closed {
    Committed,
    RolledBack,
    Retained,
    Unclosed,
    /// Stop was pressed during an interruptible close: the transaction is Closing, with its rationale, for Stop's
    /// ending to settle.
    Interrupted,
}

/// Whether Stop can cut the closes short.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseCalls {
    /// The next message's close of an Unclosed transaction: Stop abandons the call at once, and its ending (the
    /// executor's Stop) closes the transaction instead, within D4's 100 ms even while MCP waits on the journal.
    Interruptible,
    /// The host's own closes after the executor refused Stop: nothing else would close the transaction, so each is
    /// waited for, bounded in real time.
    Bounded,
}

/// A tool's kind. A tool with no kind (an older server) counts as an edit, or as a query when it says it is read-only.
pub(crate) fn kind_of(info: Option<&ToolInfo>) -> ToolKind {
    match info {
        None => ToolKind::Edit,
        Some(t) if t.kind == ToolKind::Unknown => {
            if t.read_only {
                ToolKind::Query
            } else {
                ToolKind::Edit
            }
        }
        Some(t) => t.kind,
    }
}

/// How a call of this kind relates to the answer's transaction.
pub(crate) fn tx_use(kind: ToolKind) -> TxUse {
    match kind {
        ToolKind::Edit | ToolKind::DriverJob => TxUse::Join,
        ToolKind::Query | ToolKind::Job | ToolKind::Builtin | ToolKind::Unknown => TxUse::Outside,
    }
}

/// The first line of the person's message, trimmed.
pub(crate) fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

fn chars(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

/// `Assistant: <first line, 60 chars>`.
pub(crate) fn label(first_line: &str) -> String {
    format!("{LABEL_PREFIX}{}", chars(first_line, LABEL_CHARS))
}

/// The first line, 200 chars: the cause every request in the transaction records.
pub(crate) fn cause(first_line: &str) -> String {
    chars(first_line, CAUSE_CHARS)
}

/// The text the model gets for `history_undo` or `history_redo` while the answer's transaction is open (D2.5).
pub(crate) const UNDO_WHILE_OPEN: &str = "your changes in this answer are not a step yet; use `discard_changes`";

impl Worker {
    /// Whether the answer's transaction is open, so edits join it and undo is answered by the host.
    pub(crate) fn tx_open(&self) -> bool {
        self.tx.as_ref().is_some_and(|t| t.state == TxState::Open)
    }

    /// Tracks a transaction the executor holds that the host does not (a `tx_begin` Stop cut short after the executor
    /// applied it): the session is the assistant's own, so any transaction open in it is one the host asked for.
    pub(crate) fn adopt_open_tx(&mut self, state: TxState) {
        if self.tx.is_some() {
            return;
        }
        let Some(info) = self.session_info() else { return };
        if matches!(info.state, omuzaki_ops_api::SessionState::Ended { .. }) {
            return;
        }
        if let Some(tx) = info.open_tx {
            let answer = AnswerId(self.answers);
            self.tx = Some(HostTx { tx, answer, rationale: String::new(), suffix: "", state });
        }
    }

    /// Whether the executor still holds the tracked transaction open.
    pub(crate) fn executor_holds_tx(&self) -> bool {
        let Some(tracked) = self.tx.as_ref() else { return false };
        self.session_info().and_then(|info| info.open_tx).is_some_and(|open| open.tx == tracked.tx.tx)
    }

    /// Opens the answer's transaction with `tx_begin`. A refusal comes back for the model, as the result of the call that
    /// needed the transaction.
    pub(crate) fn begin_tx(&mut self, run: &mut AnswerRun) -> Result<Option<ToolReply>, Flow> {
        let first = run.first_line.clone();
        let args = json!({"label": label(&first), "cause": cause(&first)});
        let reply = self.call_plain("tx_begin", args.as_object().cloned())?;
        if reply.is_error {
            return Ok(Some(reply));
        }
        if let Some(tx) = self.session_info().and_then(|info| info.open_tx) {
            self.tx =
                Some(HostTx { tx, answer: run.answer, rationale: String::new(), suffix: "", state: TxState::Open });
        }
        Ok(None)
    }

    /// The tracked transaction is Committed: its step is recorded with its rationale, and the host lets go of it.
    pub(crate) fn tx_committed_now(&mut self) {
        if let Some(t) = self.tx.take() {
            self.record_step(&t.tx, &t.rationale, t.suffix);
        }
    }

    /// The executor no longer holds the tracked transaction, and its outcome is the log's to tell: Retained.
    pub(crate) fn retain_tx(&mut self) {
        if let Some(t) = self.tx.take() {
            self.retain(&t.tx, &t.rationale, t.suffix);
        }
    }

    /// After a close was refused or went unanswered: Retained if the executor no longer holds the transaction, else
    /// back to `still_open`.
    fn settle_unconfirmed(&mut self, still_open: TxState) {
        if self.executor_holds_tx() {
            if let Some(t) = self.tx.as_mut() {
                t.state = still_open;
            }
        } else {
            self.retain_tx();
        }
    }

    /// Commits the answer's transaction at its end, with a call Stop can cut short (the executor's Stop then closes it).
    /// A refusal is shown; the transaction is then Unclosed, kept for the next message.
    ///
    /// # Errors
    /// [`Flow::Stopped`] if Stop was pressed during the commit (the transaction is Closing, and Stop's ending settles
    /// it); `Revoked` or `Fail` if the session or the link went away (Closing too).
    pub(crate) fn commit_at_end(&mut self) -> Result<(), Flow> {
        match self.tx.as_mut() {
            Some(t) if t.state == TxState::Open => t.state = TxState::Closing,
            _ => return Ok(()),
        }
        let reply = self.call_plain("tx_commit", None)?;
        if reply.is_error {
            self.shared.emit(AssistantEvent::Error(HostError::Mcp(plain_text(&reply.blocks))));
            self.settle_unconfirmed(TxState::Unclosed);
        } else {
            self.tx_committed_now();
        }
        Ok(())
    }

    /// After a commit or rollback that ended in neither a reply nor a cancel (a failed link): the executor decides.
    pub(crate) fn settle_after_failed_close(&mut self) {
        if self.tx.as_ref().is_some_and(|t| t.state == TxState::Closing) {
            self.settle_unconfirmed(TxState::Unclosed);
        }
    }

    /// Tries `closes` in order on the tracked transaction until the executor confirms one or no longer holds the
    /// transaction. Refusals are shown. When every close fails and the executor still holds it, the transaction is
    /// Unclosed. With no tracked transaction there is nothing to close (`Retained`: the executor holds nothing of it).
    pub(crate) fn close_in_order(&mut self, closes: &[Close], calls: CloseCalls) -> Closed {
        let Some(t) = self.tx.as_mut() else { return Closed::Retained };
        t.state = TxState::Closing;
        for &close in closes {
            if !self.executor_holds_tx() {
                self.retain_tx();
                return Closed::Retained;
            }
            let reply = match calls {
                CloseCalls::Bounded => self.call_after_stop(close.tool(), None),
                CloseCalls::Interruptible => match self.call_plain(close.tool(), None) {
                    Ok(reply) => Some(reply),
                    Err(Flow::Stopped) => return Closed::Interrupted,
                    Err(_) => None,
                },
            };
            match reply {
                Some(reply) if !reply.is_error => {
                    return match close {
                        Close::Commit => {
                            self.tx_committed_now();
                            Closed::Committed
                        }
                        Close::Rollback => {
                            self.tx = None;
                            Closed::RolledBack
                        }
                    };
                }
                Some(reply) => {
                    self.shared.emit(AssistantEvent::Error(HostError::Mcp(plain_text(&reply.blocks))));
                }
                None => {}
            }
        }
        self.settle_unconfirmed(TxState::Unclosed);
        if self.tx.is_some() { Closed::Unclosed } else { Closed::Retained }
    }

    /// The session ended: the executor rolled an open transaction back; one whose close was already sent may have
    /// committed first, which only the log can tell.
    pub(crate) fn settle_ended_session(&mut self) {
        match self.tx.as_ref().map(|t| t.state) {
            Some(TxState::Closing) => self.retain_tx(),
            Some(TxState::Open | TxState::Unclosed) => self.tx = None,
            None => {}
        }
    }

    /// Calls a tool that a close needs where the usual cancel flag would end it at once: no cancel, bounded in real time
    /// instead. `None` if the call failed or took too long.
    pub(crate) fn call_after_stop(
        &mut self,
        name: &str,
        args: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Option<ToolReply> {
        let pending = self.link.call_tool(name, args, omuzaki_llm_api::stream::Cancel::new(), None);
        let started = std::time::Instant::now();
        let reply = loop {
            match pending.poll(std::time::Duration::from_millis(1)) {
                Some(Ok(Ok(reply))) => break Some(reply),
                Some(_) => break None,
                None if started.elapsed() > CALL_AFTER_STOP_WAIT => break None,
                None => {}
            }
        };
        match &reply {
            Some(reply) => self.took_reply(reply),
            // Abandoned or failed: the server may have answered (`history.rs`).
            None => self.log.dirty = true,
        }
        reply
    }

    /// `discard_changes`: rolls the answer's transaction back. `None` means none was open; a refusal comes back for the
    /// model, and the transaction stays open.
    ///
    /// # Errors
    /// As for any call; a rollback Stop cut short is Closing, for Stop's ending to settle.
    pub(crate) fn rollback_tx(&mut self) -> Result<Option<ToolReply>, Flow> {
        match self.tx.as_mut() {
            Some(t) if t.state == TxState::Open => t.state = TxState::Closing,
            _ => return Ok(None),
        }
        let reply = self.call_plain("tx_rollback", None)?;
        if reply.is_error {
            self.settle_unconfirmed(TxState::Open);
        } else {
            self.tx = None;
        }
        Ok(Some(reply))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(kind: ToolKind, read_only: bool) -> ToolInfo {
        ToolInfo { name: "t".into(), title: None, description: String::new(), input_schema: json!({}), kind, read_only }
    }

    #[test]
    fn a_seventy_character_line_gives_a_sixty_character_label_part_and_the_whole_cause() {
        let line = "x".repeat(70);
        assert_eq!(label(&line), format!("Assistant: {}", "x".repeat(60)));
        assert_eq!(cause(&line), line);
    }

    #[test]
    fn the_label_counts_characters_not_bytes() {
        let line = "é".repeat(80);
        assert_eq!(label(&line).chars().count(), LABEL_PREFIX.len() + 60);
    }

    #[test]
    fn kinds_decide_who_joins_the_transaction() {
        assert_eq!(tx_use(ToolKind::Edit), TxUse::Join);
        assert_eq!(tx_use(ToolKind::DriverJob), TxUse::Join);
        for kind in [ToolKind::Query, ToolKind::Job, ToolKind::Builtin] {
            assert_eq!(tx_use(kind), TxUse::Outside);
        }
    }

    #[test]
    fn a_tool_without_a_kind_is_an_edit_unless_it_is_read_only() {
        assert_eq!(kind_of(Some(&tool(ToolKind::Unknown, false))), ToolKind::Edit);
        assert_eq!(kind_of(Some(&tool(ToolKind::Unknown, true))), ToolKind::Query);
        assert_eq!(kind_of(None), ToolKind::Edit);
        assert_eq!(kind_of(Some(&tool(ToolKind::Job, false))), ToolKind::Job);
    }

    #[test]
    fn the_first_line_skips_blank_lines() {
        assert_eq!(first_line("\n  make her face more oval \nmore"), "make her face more oval");
    }
}
