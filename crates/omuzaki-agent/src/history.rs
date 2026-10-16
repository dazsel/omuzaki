//! The server's op log as the host uses it: what others changed, told to the model exactly once, and the outcome of the
//! host's own transactions, read after the fact (D4).
//!
//! # The invariant
//!
//! **A cursor or a delivered mark advances only after the content it stands for is stored where every later path
//! reads it.** The later paths are the next request (the model's history), a compaction (it rebuilds the history from
//! the transcript), Stop (it cuts the history and records its cutoff), and a reopened host (it reads the store). So the
//! store, mirrored by the transcript, holds everything owed: what the model is owed ([`Entry::Notice`], late results),
//! what it was given ([`Entry::Context`], tool results), and the undo steps still to be read from the log
//! ([`Entry::Retained`]). Memory holds only caches of these, rebuilt from the store when a host opens. Every path that
//! moves a mark, and what it stores first:
//!
//! | Mark | Moved when | Stored first |
//! |---|---|---|
//! | [`LogState::read_to`] | a host read returned events | a `Notice` for what in them is news to the model |
//! | [`LogState::told`], by a tool result | the model's call returns | the `ToolResult` entry, whose text holds the heads-up (`Worker::execute`) |
//! | `told`, by the context block (its heads-up, the notices and late results it carries) | the message is admitted | the `Context` entry, recorded under the record lock with the owed entries it carries (`Shared::record_context`) |
//! | a heads-up leaving `pending` undelivered | at each step of the host thread (`settle_pending`) | a `Notice` |
//! | a late result's heads-up | the callback runs | the `:late` result and a `Notice`, together |
//! | a transaction leaving `Worker::tx` without a confirmed outcome | the executor confirmed it closed | `Retained` (`retain`) |
//! | an outcome leaving `Worker::retained` | the log shows it | the `Step`, or `Settled` |
//! | the project summary counted as given (`last_describe`) | the context is admitted | the `Context` entry; a compaction forgets the mark, as the rebuilt history drops the text |
//! | Stop's cut of the model's history | Stop's ending | the `Stopped` cutoff (`stop.rs`) |
//! | the history replaced by a compaction | the summary is written | the `Compacted` entry (`compact.rs`) |
//!
//! A call abandoned after the server may have answered it (Stop cancelled it, a bounded wait ran out) can have moved
//! the server's *seen* cursor without the host seeing the heads-up: such a call marks the log [`LogState::dirty`], and
//! the next message reads it from `read_to` before its context block, so what the lost heads-up reported becomes a
//! `Notice`.
//!
//! # Three cursors
//!
//! Every event in `omuzaki://log` has a number `n` that rises by one per event. Three positions in that numbering
//! matter, each with one owner.
//!
//! | Cursor | Owner | What it says | Moved by |
//! |---|---|---|---|
//! | the server's *seen* | `omuzaki-mcp`, per session | the newest event the server counts as told to this session | the server, on **every** tool result (to the log's head, putting what others did since in that result's heads-up, `since_your_last_call`) and on every log read (to the log's head, whatever `after_n` asked). The host cannot hold it back; it learns where it went from each heads-up's `up_to_n` and each read's `latest`. |
//! | [`LogState::read_to`] | the host's reads | the newest event the host's own reads of the log have returned | a host read only, to the read's `latest`, after storing what in it is news. The next read asks `after_n` = this cursor, so no event is examined twice and none is skipped. |
//! | [`LogState::told`] | the model | the event ranges whose changes by others the model has been given | the rows above: a heads-up in a result the model received, the context block, the model's own `history_log` read. What the model is owed but not yet given is the `Notice` entries since the last `Context` entry. |
//!
//! Because the server moves *seen* on each read, a host read uses up the heads-up the next tool result would have
//! carried. The host therefore lists what such a read returned that the model was neither told nor owed, as a `Notice`;
//! and because tool results carry their range, nothing the model already read is listed again. A notice whose whole range
//! is already told or carried by an earlier notice (a late result's heads-up whose events a host read listed first) is
//! not given twice.
//!
//! **Where the numbering starts.** A session starts caught up: the server sets *seen* to the log's head when it opens
//! the session, and never reports what came before. The host has no way to learn that number, so the host thread's first
//! act is one read of the log's head (no events), which sets `read_to` and [`LogState::origin`]: nothing up to the origin
//! is listed. What others changed between the session's opening and that read (the moment it takes the app to start the
//! host) is therefore not listed; nothing of this session had been read by the model by then, and its first context block
//! describes the project as it is. If that read fails there is no origin, and the first host read lists every change by
//! others still in the window: an old change told twice is the lesser harm than a new one never told. A reopened host
//! is a new session with its own origin, and still gives the model the notices its store says are owed.
//!
//! # Outcomes read after the fact
//!
//! A transaction the executor confirmed closed but whose outcome the host did not see (a commit Stop cut short, a step
//! the executor closed by itself after Stop) is *retained* (see `tx.rs`) until the log shows `tx_committed` or
//! `tx_rolled_back` with its id: only a commit becomes a `Step`. The server's worker thread fills the log a moment after
//! the fact, so the host never waits for it before `Done`: retained outcomes are reconciled asynchronously, at the start
//! of each message (one read, before its context block) and while the host thread is idle (on the host's clock: 20 ms
//! after the close, then backing off to once a second). Never mid-answer: a read there would use up the heads-up the
//! answer's next tool result needs. An entry leaves the list only when its event is seen. A host reopened before then
//! takes the `Retained` entries that have no `Step` or `Settled` yet from its store, and reads the log from where each
//! was retained; if the log is behind that point the server has restarted and the outcome can no longer be read, so
//! the entry is `Settled` without a step. (Should the event leave the log's 256-event window unread, the entry stays,
//! read for once a second while the host is idle; the server moves an event into the log within milliseconds, so this
//! needs 256 events in that time.)

use omuzaki_agent_api::entry::Entry;
use omuzaki_ops_api::TxId;
use serde_json::Value;

use crate::content::{plain_text, wrap_data};
use crate::link::ToolReply;
use crate::shared::is_late;
use crate::store::cut_rationale;
use crate::worker::Worker;

/// The most changes of others the next message is told of, newest last; the rest are counted.
const MISSED_MAX: usize = 20;
/// The kinds of event that change the document, as the log names them.
const DOCUMENT_EVENTS: [&str; 5] = ["op_applied", "tx_committed", "tx_rolled_back", "undone", "redone"];
/// The key the server puts its heads-up under in a structured result.
const HEADS_UP_KEY: &str = "since_your_last_call";
/// The first wait before the log is read again for a retained outcome, in seconds of the host's clock.
const POLL_FIRST: f64 = 0.02;
/// The longest wait between reads for retained outcomes.
const POLL_MAX: f64 = 1.0;

/// Event numbers as a union of ranges `(after, upto]`, kept sorted and merged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    /// Adds the events numbered above `after` up to `upto`.
    pub(crate) fn insert(&mut self, after: u64, upto: u64) {
        if upto <= after {
            return;
        }
        let (mut lo, mut hi) = (after, upto);
        // Every range that overlaps or touches `(lo, hi]` is merged into it.
        self.0.retain(|&(a, b)| {
            let apart = b < lo || a > hi;
            if !apart {
                lo = lo.min(a);
                hi = hi.max(b);
            }
            apart
        });
        let at = self.0.partition_point(|&(a, _)| a < lo);
        self.0.insert(at, (lo, hi));
    }

    /// Whether event `n` is covered.
    pub(crate) fn contains(&self, n: u64) -> bool {
        self.0.iter().any(|&(a, b)| a < n && n <= b)
    }

    /// The covered ranges, in order.
    pub(crate) fn spans(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.0.iter().copied()
    }

    /// Whether every event of `(after, upto]` is covered.
    pub(crate) fn covers(&self, after: u64, upto: u64) -> bool {
        upto <= after || self.0.iter().any(|&(a, b)| a <= after && upto <= b)
    }
}

/// A heads-up a server result carried: the range of events it reports, and its structured form.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeadsUp {
    pub after: u64,
    pub upto: u64,
    pub json: String,
}

/// The heads-up in `reply`, if it carries one.
pub(crate) fn heads_up_of(reply: &ToolReply) -> Option<HeadsUp> {
    let data = reply.structured.as_ref()?.get(HEADS_UP_KEY)?;
    let after = data.get("after_n").and_then(Value::as_u64)?;
    let upto = data.get("up_to_n").and_then(Value::as_u64)?;
    Some(HeadsUp { after, upto, json: data.to_string() })
}

/// What a reply tells the model when the model receives it: its heads-up, and for the model's own read of the log,
/// the range that read returned. Applied with [`Worker::deliver`] once the reply is stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Delivery {
    heads_up: Option<HeadsUp>,
    log_range: Option<(u64, u64)>,
}

/// What `reply` to a call of `tool` with `args` tells the model that receives it.
pub(crate) fn delivery_of(tool: &str, args: Option<&Value>, reply: &ToolReply) -> Delivery {
    let log_range = (tool == "history_log" && !reply.is_error)
        .then(|| {
            let after = args.and_then(|a| a.get("after_n")).and_then(Value::as_u64).unwrap_or(0);
            let log = serde_json::from_str::<Value>(&plain_text(&reply.blocks)).ok()?;
            log.get("latest").and_then(Value::as_u64).map(|latest| (after, latest))
        })
        .flatten();
    Delivery { heads_up: heads_up_of(reply), log_range }
}

/// The host's positions in the op log (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct LogState {
    /// Where this session's numbering starts: events up to it are not this session's news. `None` if unknown.
    pub origin: Option<u64>,
    /// The newest event number the host's own reads have returned; `None` until the first one succeeds.
    pub read_to: Option<u64>,
    /// The ranges whose changes by others the model has been given.
    pub told: Ranges,
    /// Heads-ups of replies the host received and has not yet given to the model or stored as owed.
    pending: Vec<HeadsUp>,
    /// A call was abandoned after the server may have answered it: the next message reads the log first.
    pub dirty: bool,
    /// When the log is next read for retained outcomes, on the host's clock; `None` when nothing is retained.
    next_poll: Option<f64>,
    /// The wait after that read.
    backoff: f64,
}

impl LogState {
    /// Whether event `n` is news to the model: after the origin, and neither told nor owed (`owed` are the ranges of the
    /// owed notices).
    fn is_news(&self, n: u64, owed: &Ranges) -> bool {
        self.origin.is_none_or(|origin| n > origin) && !self.told.contains(n) && !owed.contains(n)
    }

    /// The model was given what `delivery` stands for.
    fn apply(&mut self, delivery: &Delivery) {
        if let Some(heads_up) = &delivery.heads_up {
            self.told.insert(heads_up.after, heads_up.upto);
            self.pending.retain(|p| p != heads_up);
        }
        if let Some((after, latest)) = delivery.log_range {
            self.told.insert(after, latest);
        }
    }
}

/// The `Notice` entries among `owed`, with the ranges they account for.
fn owed_ranges(owed: &[Entry]) -> Ranges {
    let mut ranges = Ranges::default();
    for entry in owed {
        if let Entry::Notice { after, upto, .. } = entry {
            ranges.insert(*after, *upto);
        }
    }
    ranges
}

/// A short line for an event that changed the document, if an actor other than `me` made it.
fn change_of_other(event: Option<&Value>, me: Option<&Value>) -> Option<String> {
    let event = event?;
    let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
    let actor = event.get("actor").or_else(|| event.get("record").and_then(|r| r.get("actor")));
    if !DOCUMENT_EVENTS.contains(&kind) || actor == me {
        return None;
    }
    let what = event
        .get("record")
        .and_then(|r| r.get("op"))
        .or_else(|| event.get("label"))
        .and_then(Value::as_str)
        .map_or_else(|| kind.to_owned(), |name| format!("{kind} {name}"));
    Some(format!("{what} by {}", actor_name(actor)))
}

/// A short name for an actor as the log serialises it.
fn actor_name(actor: Option<&Value>) -> String {
    match actor {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Object(fields)) => fields
            .get("name")
            .or_else(|| fields.get("id"))
            .and_then(Value::as_str)
            .or_else(|| fields.get("kind").and_then(Value::as_str))
            .unwrap_or("someone")
            .to_owned(),
        _ => "someone".to_owned(),
    }
}

/// What the log says became of transaction `tx`: `Some(true)` committed, `Some(false)` rolled back, `None` nothing yet.
fn outcome_in(events: &[Value], tx: TxId) -> Option<bool> {
    events.iter().find_map(|entry| {
        let event = entry.get("event")?;
        if event.get("tx").and_then(Value::as_u64) != Some(tx.0) {
            return None;
        }
        match event.get("event").and_then(Value::as_str) {
            Some("tx_committed") => Some(true),
            Some("tx_rolled_back") => Some(false),
            _ => None,
        }
    })
}

/// A transaction whose outcome the log has not shown yet: the memory side of its stored `Retained` entry.
#[derive(Clone, Debug)]
pub(crate) struct Retained {
    pub tx: TxId,
    /// The label of the step it becomes.
    pub label: String,
    /// The rationale of the step it becomes, already cut.
    pub rationale: String,
}

/// The sections a context block gives for what is owed: one listing every change by others not yet given (newest
/// last, at most [`MISSED_MAX`]), and one per late result. Also the ranges they give. `tool_of` names the tool a call
/// id belongs to.
pub(crate) fn owed_sections(
    owed: &[Entry],
    told: &Ranges,
    tool_of: &dyn Fn(&str) -> Option<String>,
) -> (Vec<String>, Ranges) {
    let mut given = Ranges::default();
    let mut lines: Vec<String> = Vec::new();
    let mut late = Vec::new();
    for entry in owed {
        match entry {
            Entry::Notice { after, upto, text, .. } => {
                if told.covers(*after, *upto) || given.covers(*after, *upto) {
                    continue;
                }
                given.insert(*after, *upto);
                lines.extend(text.lines().map(str::to_owned));
            }
            Entry::ToolResult { id, ok, blocks, .. } if is_late(entry) => {
                let call = id.trim_end_matches(":late");
                let tool = tool_of(call).unwrap_or_else(|| call.to_owned());
                let text: Vec<&str> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        omuzaki_agent_api::entry::StoredBlock::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                let first = text.join("\n");
                let outcome =
                    if *ok { "applied".to_owned() } else { format!("failed: {}", crate::tx::first_line(&first)) };
                // The outcome can carry the first line of a server's result, so the whole note is data.
                late.push(wrap_data(
                    "late_result",
                    &format!("[your call `{tool}` finished after the stop: {outcome}]"),
                ));
            }
            _ => {}
        }
    }
    let mut sections = late;
    if !lines.is_empty() {
        let more = lines.len().saturating_sub(MISSED_MAX);
        lines.drain(..more);
        let tail = if more > 0 { format!("\n({more} earlier changes not listed)") } else { String::new() };
        sections.push(wrap_data(
            "history_log",
            &format!("[others changed the document, and no tool result has told you yet:\n{}{tail}]", lines.join("\n")),
        ));
    }
    (sections, given)
}

impl Worker {
    /// The events numbered above `after`, and the log's newest number, read with a call Stop cannot cut short. A read
    /// that fails after the server may have answered it leaves the log dirty: `read_to` has not moved, so the next read
    /// covers the same events.
    fn read_log_after(&mut self, after: u64) -> Option<(Vec<Value>, u64)> {
        let args = serde_json::json!({ "after_n": after }).as_object().cloned();
        let reply = self.call_after_stop("history_log", args).filter(|reply| !reply.is_error)?;
        let log: Value = serde_json::from_str(&plain_text(&reply.blocks)).ok()?;
        let latest = log.get("latest").and_then(Value::as_u64).unwrap_or(0);
        Some((log.get("events")?.as_array()?.clone(), latest))
    }

    /// The host thread's first acts: where this session's numbering starts (see the module docs; reads no events), and
    /// the outcomes its store says are still to be read, from `history`.
    pub(crate) fn start_log(&mut self, history: &[Entry]) {
        if let Some((_, latest)) = self.read_log_after(u64::MAX) {
            self.log.read_to = Some(latest);
            self.log.origin = Some(latest);
        }
        self.recover_retained(history);
    }

    /// Takes the `Retained` entries of `history` that no `Step` or `Settled` followed, to read their outcome again.
    fn recover_retained(&mut self, history: &[Entry]) {
        let closed = |tx: TxId| {
            history.iter().any(|e| matches!(e, Entry::Step { tx: t, .. } | Entry::Settled { tx: t, .. } if *t == tx))
        };
        for entry in history {
            let Entry::Retained { tx, label, rationale, after, .. } = entry else { continue };
            if closed(*tx) || self.retained.iter().any(|r| r.tx == *tx) {
                continue;
            }
            if self.log.origin.is_some_and(|origin| origin < *after) {
                // The log is behind where it was when this was retained: the server restarted, and lost it.
                self.shared.record(Entry::settled(*tx));
                continue;
            }
            self.retained.push(Retained { tx: *tx, label: label.clone(), rationale: rationale.clone() });
            // Its outcome is above `after`: the next read starts no later.
            self.log.read_to = Some(self.log.read_to.map_or(*after, |r| r.min(*after)));
        }
        if !self.retained.is_empty() {
            self.log.next_poll = Some(self.now().0);
            self.log.backoff = POLL_FIRST;
        }
    }

    /// One read of the log from where the host's reads left off; `None` if it cannot be read. What in it is news to the
    /// model is stored as a `Notice` before `read_to` moves past it.
    fn read_log(&mut self) -> Option<Vec<Value>> {
        let after = self.log.read_to.unwrap_or(0);
        let Some((events, latest)) = self.read_log_after(after) else {
            self.log.dirty = true;
            return None;
        };
        let me = self.session_info().and_then(|info| serde_json::to_value(info.actor).ok());
        let owed = owed_ranges(&self.shared.owed());
        let lines: Vec<String> = events
            .iter()
            .filter(|entry| {
                let n = entry.get("n").and_then(Value::as_u64).unwrap_or(0);
                n > after && self.log.is_news(n, &owed)
            })
            .filter_map(|entry| change_of_other(entry.get("event"), me.as_ref()))
            .collect();
        // Nothing else is news: told already, owed by a stored notice, or not a change by others.
        if latest > after && !lines.is_empty() {
            self.shared.record(Entry::notice(after, latest, lines.join("\n")));
        }
        self.log.read_to = Some(self.log.read_to.map_or(latest, |r| r.max(latest)));
        self.log.dirty = false;
        Some(events)
    }

    /// Keeps a transaction until the log shows its outcome: its `Retained` entry is stored first. The log is asked 20 ms
    /// later on the host's clock while the host is idle (the server's worker needs that moment), or at the next message.
    pub(crate) fn retain(&mut self, tx: &omuzaki_ops_api::SessionTx, rationale: &str, suffix: &str) {
        let label = format!("{}{suffix}", tx.label);
        let rationale = cut_rationale(rationale.trim());
        let after = self.log.read_to.unwrap_or(0);
        self.shared.record(Entry::retained(tx.tx, label.clone(), rationale.clone(), after));
        self.retained.push(Retained { tx: tx.tx, label, rationale });
        self.log.next_poll = Some(self.now().0 + POLL_FIRST);
        self.log.backoff = POLL_FIRST * 2.0;
    }

    /// Reads the log once if an outcome is retained or a call was abandoned, and records what it shows: a `Step` for a
    /// commit, `Settled` for a rollback, each before the outcome leaves the list. What it does not show stays.
    pub(crate) fn reconcile_now(&mut self) {
        if self.retained.is_empty() && !self.log.dirty {
            return;
        }
        let Some(events) = self.read_log() else { return };
        for r in std::mem::take(&mut self.retained) {
            match outcome_in(&events, r.tx) {
                Some(true) => self.shared.record(Entry::step(r.tx, r.label, r.rationale)),
                Some(false) => self.shared.record(Entry::settled(r.tx)),
                None => self.retained.push(r),
            }
        }
        if self.retained.is_empty() {
            self.log.next_poll = None;
        }
    }

    /// Called while the host thread is idle: stores the heads-ups no one was given, and reads the log for retained
    /// outcomes when the next read is due.
    pub(crate) fn reconcile_when_due(&mut self) {
        self.settle_pending();
        let Some(due) = self.log.next_poll else { return };
        let now = self.now().0;
        if self.retained.is_empty() || now < due {
            return;
        }
        self.reconcile_now();
        if !self.retained.is_empty() {
            self.log.next_poll = Some(now + self.log.backoff);
            self.log.backoff = (self.log.backoff * 2.0).min(POLL_MAX);
        }
    }

    /// Accounts for a reply the host received from the server: a heads-up in it waits in `pending` until the model is
    /// given the reply ([`deliver`](Self::deliver)) or it is stored as owed ([`settle_pending`](Self::settle_pending)).
    /// Every reply passes here.
    pub(crate) fn took_reply(&mut self, reply: &ToolReply) {
        if let Some(heads_up) = heads_up_of(reply) {
            self.log.pending.push(heads_up);
        }
    }

    /// The model was given a reply, already stored: what it carried is told.
    pub(crate) fn deliver(&mut self, delivery: &Delivery) {
        self.log.apply(delivery);
    }

    /// Stores every heads-up still pending as a `Notice` the next context gives, unless the model was told it already.
    pub(crate) fn settle_pending(&mut self) {
        let pending = std::mem::take(&mut self.log.pending);
        let notices: Vec<Entry> = pending
            .into_iter()
            .filter(|h| !self.log.told.covers(h.after, h.upto))
            .map(|h| Entry::notice(h.after, h.upto, h.json))
            .collect();
        if !notices.is_empty() {
            self.shared.record_all(notices);
        }
    }

    /// Takes the heads-up of `delivery` out of `pending`, so it is not stored as owed while the reply that carries it is
    /// about to be given to the model.
    pub(crate) fn hold_for_delivery(&mut self, delivery: &Delivery) {
        if let Some(heads_up) = &delivery.heads_up {
            self.log.pending.retain(|p| p != heads_up);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn notice(after: u64, upto: u64, text: &str) -> Entry {
        Entry::notice(after, upto, text)
    }

    #[test]
    fn ranges_merge_and_answer_what_they_cover() {
        let mut r = Ranges::default();
        r.insert(5, 8);
        r.insert(0, 3);
        r.insert(3, 5);
        assert_eq!(r, Ranges(vec![(0, 8)]), "touching ranges merge");
        r.insert(10, 12);
        assert!(r.contains(8) && !r.contains(9) && r.contains(11) && !r.contains(0));
        assert!(r.covers(2, 8) && !r.covers(7, 11) && r.covers(10, 12) && r.covers(4, 4));
    }

    #[test]
    fn news_is_after_the_origin_and_neither_told_nor_owed() {
        let mut log = LogState { origin: Some(2), ..LogState::default() };
        log.told.insert(4, 5);
        let mut owed = Ranges::default();
        owed.insert(6, 7);
        let news: Vec<u64> = (1..=8).filter(|n| log.is_news(*n, &owed)).collect();
        // (4, 5] is told and (6, 7] owed: 5 and 7 are not news, 4 and 6 are.
        assert_eq!(news, [3, 4, 6, 8]);
    }

    #[test]
    fn a_notice_is_given_once_and_not_when_its_range_was_told_or_given_in_the_same_context() {
        let mut told = Ranges::default();
        told.insert(0, 3);
        let owed = [
            notice(1, 3, "told.already"),
            notice(3, 9, "listed.by.a.read"),
            notice(4, 6, "late.heads.up.inside.it"),
            notice(9, 10, "new.one"),
        ];
        let (sections, given) = owed_sections(&owed, &told, &|_| None);
        let text = sections.join("\n");
        assert!(text.contains("listed.by.a.read") && text.contains("new.one"), "{text}");
        assert!(!text.contains("told.already") && !text.contains("late.heads.up.inside.it"), "{text}");
        assert!(given.covers(3, 10));
    }

    #[test]
    fn a_late_result_is_noted_with_its_tool_and_outcome() {
        let owed =
            [Entry::tool_result("c1:late", false, vec![omuzaki_agent_api::entry::StoredBlock::Text("boom".into())])];
        let (sections, _) = owed_sections(&owed, &Ranges::default(), &|id| (id == "c1").then(|| "render".to_owned()));
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("`render` finished after the stop: failed: boom"), "{sections:?}");
    }

    #[test]
    fn a_delivery_tells_the_heads_up_and_the_models_own_read() {
        let mut log = LogState::default();
        let heads_up = HeadsUp { after: 3, upto: 6, json: "{}".into() };
        log.pending.push(heads_up.clone());
        log.apply(&Delivery { heads_up: Some(heads_up), log_range: Some((0, 2)) });
        assert_eq!(log.pending, Vec::<HeadsUp>::new());
        assert!(log.told.covers(0, 2) && log.told.covers(3, 6));
    }

    #[test]
    fn the_model_is_not_told_its_own_changes_nor_reads() {
        let me = json!({"kind": "agent", "id": "a"});
        let own = json!({"event": "op_applied", "record": {"op": "x", "actor": me}});
        let read = json!({"event": "something_else", "actor": "Sam"});
        assert!(change_of_other(Some(&own), Some(&me)).is_none());
        assert!(change_of_other(Some(&read), Some(&me)).is_none());
        let other = json!({"event": "op_applied", "record": {"op": "x", "actor": {"kind": "person", "name": "Sam"}}});
        assert_eq!(change_of_other(Some(&other), Some(&me)).as_deref(), Some("op_applied x by Sam"));
    }

    #[test]
    fn only_a_commit_or_a_rollback_with_its_id_is_an_outcome() {
        let events = [
            json!({"n": 1, "event": {"event": "tx_committed", "tx": 30}}),
            json!({"n": 2, "event": {"event": "undone", "tx": 31}}),
            json!({"n": 3, "event": {"event": "tx_rolled_back", "tx": 32}}),
        ];
        assert_eq!(outcome_in(&events, TxId(30)), Some(true));
        assert_eq!(outcome_in(&events, TxId(31)), None);
        assert_eq!(outcome_in(&events, TxId(32)), Some(false));
    }
}
