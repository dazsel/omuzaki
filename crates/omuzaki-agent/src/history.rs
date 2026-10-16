//! The server's op log as the host uses it: what others changed, told to the model exactly once, and the outcome of the
//! host's own transactions, read after the fact (D4).
//!
//! # Three cursors
//!
//! Every event in `omuzaki://log` has a number `n` that rises by one per event. Three positions in that numbering
//! matter, each with one owner, and each moves only past events actually delivered to whoever it speaks for.
//!
//! | Cursor | Owner | What it says | Moved by |
//! |---|---|---|---|
//! | the server's *seen* | `omuzaki-mcp`, per session | the newest event the server counts as told to this session | the server, on **every** tool result (to the log's head, putting what others did since in that result's heads-up, `since_your_last_call`) and on every log read (to the log's head, whatever `after_n` asked). The host cannot hold it back; it learns where it went from each heads-up's `up_to_n` and each read's `latest`. |
//! | [`LogState::read_to`] | the host's reconciliation | the newest event the host's own reads of the log have returned | a host read only, to the read's `latest`: every event above the cursor still in the log's window was in that read. The next read asks `after_n` = this cursor, so no event is examined twice and none is skipped. |
//! | [`LogState::told`] (with [`LogState::held`]) | the model | the event ranges whose changes by others the model has been told of (`told`), or will be told of in the next message's context (`held`) | a heads-up the model received (in a tool result, or `session_info`'s in the context block): its range goes to `told`; a heads-up the model did not receive (the reply of a host call such as `tx_commit`, a refusal the host acted on, a result that arrived after Stop): it is kept in `held`, with its range; a host read: the others' changes it returned that neither set covers are listed into `held`, and its range is covered; the next message's context: `held` is given to the model and moves to `told`. The model's own `history_log` read counts as told for the range it returned. |
//!
//! Because the server moves *seen* on each read, a host read uses up the heads-up the next tool result would have
//! carried. The host therefore lists what such a read returned and the model was not told, and hands it over in the next
//! context block; and because tool results carry their range, nothing the model already read is listed again. A heads-up
//! whose whole range is already covered (a late result whose events a host read listed first) is dropped, not repeated.
//!
//! **Where the numbering starts.** A session starts caught up: the server sets *seen* to the log's head when it opens
//! the session, and never reports what came before. The host has no way to learn that number, so the host thread's first
//! act is one read of the log's head (no events): it sets `read_to`, and `told` starts as everything up to it. What
//! others changed between the session's opening and that read (the moment it takes the app to start the host) is
//! therefore not listed; nothing of this session had been read by the model by then, and its first context block
//! describes the project as it is. If that read fails, `told` starts empty and the first host read lists every change by
//! others still in the window: an old change told twice is the lesser harm than a new one never told.
//!
//! # Outcomes read after the fact
//!
//! A transaction the executor confirmed closed but whose outcome the host did not see (a commit Stop cut short, a step
//! the executor closed by itself after Stop) is *retained* ([`Retained`], see `tx.rs`) until the log shows
//! `tx_committed` or `tx_rolled_back` with its id: only a commit becomes a `Step`. The server's worker thread fills the
//! log a moment after the fact, so the host never waits for it before `Done`: retained outcomes are reconciled
//! asynchronously, at the start of each message (one read, before its context block) and while the host thread is idle
//! (on the host's clock: 20 ms after the close, then backing off to once a second). Never mid-answer: a read there would use up the
//! heads-up the answer's next tool result needs. An entry leaves the list only when its event is seen. (Should the event
//! leave the log's 256-event window unread, the entry stays, read for once a second while the host is idle, until the
//! host stops; the server moves an event into the log within milliseconds, so this needs 256 events in that time.)

use omuzaki_ops_api::{SessionTx, TxId};
use serde_json::Value;

use crate::content::{plain_text, wrap_data};
use crate::link::ToolReply;
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

/// Changes of others kept for the next message's context, and the event range they account for.
#[derive(Clone, Debug)]
struct Held {
    after: u64,
    upto: u64,
    lines: Vec<String>,
}

/// The host's cursors in the op log (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct LogState {
    /// The newest event number the host's own reads have returned; `None` until the first one succeeds.
    pub read_to: Option<u64>,
    /// The ranges whose changes by others the model has been told of.
    pub told: Ranges,
    /// The changes of others the model is told of in the next message's context.
    held: Vec<Held>,
    /// The ranges `held` accounts for (kept beside it so a lookup does not walk the lines).
    held_ranges: Ranges,
    /// When the log is next read for retained outcomes, on the host's clock; `None` when nothing is retained.
    next_poll: Option<f64>,
    /// The wait after that read.
    backoff: f64,
}

impl LogState {
    /// Whether the model has been told of, or will next be told of, event `n`.
    fn accounted(&self, n: u64) -> bool {
        self.told.contains(n) || self.held_ranges.contains(n)
    }

    /// Keeps a heads-up the model did not receive for the next message's context, unless every event it reports is
    /// already accounted for.
    pub(crate) fn hold(&mut self, heads_up: HeadsUp) {
        if self.told.covers(heads_up.after, heads_up.upto) || self.held_ranges.covers(heads_up.after, heads_up.upto) {
            return;
        }
        self.held_ranges.insert(heads_up.after, heads_up.upto);
        self.held.push(Held { after: heads_up.after, upto: heads_up.upto, lines: vec![heads_up.json] });
    }

    /// The model received a reply carrying `heads_up`: its range is told, and it is no longer held.
    pub(crate) fn delivered(&mut self, heads_up: &HeadsUp) {
        self.told.insert(heads_up.after, heads_up.upto);
        if let Some(at) = self.held.iter().position(|h| h.after == heads_up.after && h.upto == heads_up.upto) {
            self.held.remove(at);
            self.held_ranges = Ranges::default();
            for h in &self.held {
                self.held_ranges.insert(h.after, h.upto);
            }
        }
    }

    /// Takes in one host read of the log: the events above `after` up to `latest`. Every document change by an actor
    /// other than `me` that the model has not been told of, and will not be, is listed for the next message; the whole
    /// range is accounted for from now on.
    fn take_in(&mut self, after: u64, latest: u64, events: &[Value], me: Option<&Value>) {
        let mut lines = Vec::new();
        for entry in events {
            let n = entry.get("n").and_then(Value::as_u64).unwrap_or(0);
            if n <= after || self.accounted(n) {
                continue;
            }
            if let Some(line) = change_of_other(entry.get("event"), me) {
                lines.push(line);
            }
        }
        if latest > after {
            if lines.is_empty() {
                // Nothing here is news to the model: the range is told.
                self.told.insert(after, latest);
            } else {
                self.held_ranges.insert(after, latest);
                self.held.push(Held { after, upto: latest, lines });
            }
        }
        self.read_to = Some(self.read_to.map_or(latest, |r| r.max(latest)));
    }

    /// The note for the next message's context: every change held, once. What it lists is told from then on.
    fn take_note(&mut self) -> Option<String> {
        if self.held.is_empty() {
            return None;
        }
        let mut lines = Vec::new();
        for held in std::mem::take(&mut self.held) {
            self.told.insert(held.after, held.upto);
            lines.extend(held.lines);
        }
        self.held_ranges = Ranges::default();
        let more = lines.len().saturating_sub(MISSED_MAX);
        lines.drain(..more);
        let tail = if more > 0 { format!("\n({more} earlier changes not listed)") } else { String::new() };
        Some(wrap_data(
            "history_log",
            &format!("[others changed the document, and no tool result has told you yet:\n{}{tail}]", lines.join("\n")),
        ))
    }
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

/// A transaction confirmed closed whose outcome the log has not shown yet (see `tx.rs`).
#[derive(Clone, Debug)]
pub(crate) struct Retained {
    pub tx: SessionTx,
    /// What the answer that made it wrote: the rationale of the step it becomes.
    pub rationale: String,
    /// Added to the label of the step (" (stopped)" for one Stop closed).
    pub suffix: &'static str,
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

impl Worker {
    /// The events numbered above `after`, and the log's newest number, read with a call Stop cannot cut short.
    fn read_log_after(&mut self, after: u64) -> Option<(Vec<Value>, u64)> {
        let args = serde_json::json!({ "after_n": after }).as_object().cloned();
        let reply = self.call_after_stop("history_log", args).filter(|reply| !reply.is_error)?;
        let log: Value = serde_json::from_str(&plain_text(&reply.blocks)).ok()?;
        let latest = log.get("latest").and_then(Value::as_u64).unwrap_or(0);
        Some((log.get("events")?.as_array()?.clone(), latest))
    }

    /// The host thread's first act: where this session's numbering starts (see the module docs). Reads no events.
    pub(crate) fn start_log(&mut self) {
        if let Some((_, latest)) = self.read_log_after(u64::MAX) {
            self.log.read_to = Some(latest);
            self.log.told.insert(0, latest);
        }
    }

    /// One read of the log from where the host's reads left off; `None` if it cannot be read. What others changed in it
    /// is taken in for the model.
    fn read_log(&mut self) -> Option<Vec<Value>> {
        let after = self.log.read_to.unwrap_or(0);
        let (events, latest) = self.read_log_after(after)?;
        let me = self.session_info().and_then(|info| serde_json::to_value(info.actor).ok());
        self.log.take_in(after, latest, &events, me.as_ref());
        Some(events)
    }

    /// Keeps transaction `retained` until the log shows its outcome. The log is asked 20 ms later on the host's clock
    /// while the host is idle (the server's worker needs that moment to move the event in), or at the next message.
    pub(crate) fn retain(&mut self, retained: Retained) {
        self.retained.push(retained);
        self.log.next_poll = Some(self.now().0 + POLL_FIRST);
        self.log.backoff = POLL_FIRST * 2.0;
    }

    /// Reads the log once for the retained outcomes and records the steps it shows; what it does not show stays.
    pub(crate) fn reconcile_now(&mut self) {
        if self.retained.is_empty() {
            return;
        }
        let Some(events) = self.read_log() else { return };
        for r in std::mem::take(&mut self.retained) {
            match outcome_in(&events, r.tx.tx) {
                Some(true) => self.record_step(&r.tx, &r.rationale, r.suffix),
                Some(false) => {}
                None => self.retained.push(r),
            }
        }
        if self.retained.is_empty() {
            self.log.next_poll = None;
        }
    }

    /// Called while the host thread is idle: reads the log for retained outcomes when the next read is due.
    pub(crate) fn reconcile_when_due(&mut self) {
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

    /// Accounts for a reply the host received from the server: a heads-up in it is held for the model until a caller
    /// says the model received the reply ([`delivered`](Self::delivered)). Every reply passes here.
    pub(crate) fn took_reply(&mut self, reply: &ToolReply) {
        if let Some(heads_up) = heads_up_of(reply) {
            self.log.hold(heads_up);
        }
    }

    /// The model received `reply` as the result of its call to `tool` with `args`: what it reports is told. For the
    /// model's own read of the log, that is the range the read returned.
    pub(crate) fn delivered(&mut self, tool: &str, args: Option<&Value>, reply: &ToolReply) {
        if let Some(heads_up) = heads_up_of(reply) {
            self.log.delivered(&heads_up);
        }
        if tool == "history_log" && !reply.is_error {
            let after = args.and_then(|a| a.get("after_n")).and_then(Value::as_u64).unwrap_or(0);
            let latest = serde_json::from_str::<Value>(&plain_text(&reply.blocks))
                .ok()
                .and_then(|log| log.get("latest").and_then(Value::as_u64));
            if let Some(latest) = latest {
                self.log.told.insert(after, latest);
            }
        }
    }

    /// What others changed that no reply the model received has told it, once, for the next message's context.
    pub(crate) fn missed_note(&mut self) -> Option<String> {
        self.log.take_note()
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn change(n: u64, op: &str) -> Value {
        json!({"n": n, "event": {"event": "op_applied", "record": {"op": op, "actor": {"kind": "person", "name": "Sam"}}}})
    }

    fn heads_up(after: u64, upto: u64) -> HeadsUp {
        HeadsUp { after, upto, json: format!("{{\"after_n\":{after},\"up_to_n\":{upto}}}") }
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
    fn a_read_lists_only_what_the_model_was_not_told_and_a_later_read_lists_nothing_twice() {
        let mut log = LogState::default();
        log.told.insert(0, 2);
        log.delivered(&heads_up(3, 4));
        let events = [change(2, "seen.before"), change(3, "gap"), change(4, "in.a.heads.up"), change(5, "new")];
        log.take_in(1, 5, &events, None);
        let note = log.take_note().unwrap();
        assert!(note.contains("gap") && note.contains("new"), "{note}");
        assert!(!note.contains("seen.before") && !note.contains("in.a.heads.up"), "{note}");
        log.take_in(1, 5, &events, None);
        assert!(log.take_note().is_none(), "a second read of the same events lists nothing");
        assert_eq!(log.read_to, Some(5));
    }

    #[test]
    fn a_held_heads_up_is_told_once_whether_the_model_gets_it_or_the_next_context_does() {
        let mut log = LogState::default();
        log.hold(heads_up(3, 6));
        log.delivered(&heads_up(3, 6));
        assert!(log.take_note().is_none(), "the model read it in the result");
        log.hold(heads_up(6, 9));
        assert!(log.take_note().unwrap().contains("\"up_to_n\":9"));
        log.hold(heads_up(6, 9));
        assert!(log.take_note().is_none(), "a heads-up for events already told is dropped");
    }

    #[test]
    fn the_model_is_not_told_its_own_changes_nor_reads() {
        let me = json!({"kind": "agent", "id": "a"});
        let own = json!({"n": 3, "event": {"event": "op_applied", "record": {"op": "x", "actor": me}}});
        let read = json!({"n": 4, "event": {"event": "something_else", "actor": "Sam"}});
        let mut log = LogState::default();
        log.take_in(2, 4, &[own, read], Some(&me));
        assert!(log.take_note().is_none());
        assert!(log.told.covers(2, 4));
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
