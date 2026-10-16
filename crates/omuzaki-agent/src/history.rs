//! What the server's op log says about the host's own transactions (D4), and the changes of others that reading it
//! uses up.
//!
//! The host asks the log when it cannot tell from the replies whether a transaction became an undo step: a commit that
//! Stop cancelled in flight, a transaction the executor closed by itself. Two things about that read shape this module.
//!
//! - The server feeds the log from a worker thread, so an event can appear a moment after the commit it records. One
//!   read that does not find the transaction therefore proves nothing: the question is asked for a short while, and
//!   an outcome still unknown is kept (`Worker::unresolved`) and asked again before the next message.
//! - Reading the log tells the server this session has seen everything in it, so the "since your last call" heads-up
//!   the next tool result would carry is spent. What others changed is kept from each read (`Worker::missed`) and
//!   given to the model in the next message's context.

use std::time::{Duration, Instant};

use omuzaki_ops_api::{SessionTx, TxId};
use serde_json::Value;

use crate::content::{plain_text, wrap_data};
use crate::worker::Worker;

/// How long Stop's ending waits for the log to show a commit.
const CONFIRM_WAIT: Duration = Duration::from_millis(300);
/// How often the log is read while waiting.
const CONFIRM_POLL: Duration = Duration::from_millis(20);
/// The most changes of others the next message is told of, newest last; the rest are counted.
const MISSED_MAX: usize = 20;
/// The kinds of event that change the document, as the log names them.
const DOCUMENT_EVENTS: [&str; 5] = ["op_applied", "tx_committed", "tx_rolled_back", "undone", "redone"];

/// A transaction whose commit the host could not confirm: it is asked again before the next message.
pub(crate) struct Unresolved {
    pub tx: SessionTx,
    /// What the answer that made it wrote: the rationale of the step it becomes.
    pub rationale: String,
    pub suffix: &'static str,
}

impl Worker {
    /// The log's events, or `None` if it cannot be read. Everything others changed that was not seen before is kept for
    /// the model.
    fn read_log(&mut self) -> Option<Vec<Value>> {
        let reply = self.call_after_stop("history_log").filter(|reply| !reply.is_error)?;
        let log: Value = serde_json::from_str(&plain_text(&reply.blocks)).ok()?;
        let events = log.get("events")?.as_array()?.clone();
        self.keep_changes_of_others(&events);
        Some(events)
    }

    fn keep_changes_of_others(&mut self, events: &[Value]) {
        let me = self.session_info().and_then(|info| serde_json::to_value(info.actor).ok());
        for entry in events {
            let n = entry.get("n").and_then(Value::as_u64).unwrap_or(0);
            if n <= self.log_seen {
                continue;
            }
            self.log_seen = n;
            let Some(event) = entry.get("event") else { continue };
            let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
            let actor = event.get("actor").or_else(|| event.get("record").and_then(|r| r.get("actor")));
            if !DOCUMENT_EVENTS.contains(&kind) || actor == me.as_ref() {
                continue;
            }
            let what = event
                .get("record")
                .and_then(|r| r.get("op"))
                .or_else(|| event.get("label"))
                .and_then(Value::as_str)
                .map_or_else(|| kind.to_owned(), |name| format!("{kind} {name}"));
            self.missed.push(format!("{what} by {}", actor_name(actor)));
        }
    }

    /// Whether transaction `tx` became an undo step, by its id in the log. Asked for a short while, because the log
    /// lags the commit; `false` means not seen yet, not never.
    pub(crate) fn tx_committed(&mut self, tx: TxId) -> bool {
        let started = Instant::now();
        loop {
            if self.log_shows_commit(tx) {
                return true;
            }
            if started.elapsed() >= CONFIRM_WAIT {
                return false;
            }
            std::thread::sleep(CONFIRM_POLL);
        }
    }

    fn log_shows_commit(&mut self, tx: TxId) -> bool {
        self.read_log().is_some_and(|events| {
            events.iter().any(|entry| {
                let event = entry.get("event");
                event.and_then(|e| e.get("event")).and_then(Value::as_str) == Some("tx_committed")
                    && event.and_then(|e| e.get("tx")).and_then(Value::as_u64) == Some(tx.0)
            })
        })
    }

    /// Asks again about every commit still unconfirmed and records the steps that turn out to exist. A log that cannot
    /// be read keeps them for the next time.
    pub(crate) fn reconcile_unresolved(&mut self) {
        if self.unresolved.is_empty() {
            return;
        }
        let Some(events) = self.read_log() else { return };
        for u in std::mem::take(&mut self.unresolved) {
            let committed = events.iter().any(|entry| {
                let event = entry.get("event");
                event.and_then(|e| e.get("event")).and_then(Value::as_str) == Some("tx_committed")
                    && event.and_then(|e| e.get("tx")).and_then(Value::as_u64) == Some(u.tx.tx.0)
            });
            if committed {
                self.record_step(&u.tx, &u.rationale, u.suffix);
            }
        }
    }

    /// What others changed that the host's own log reads used up, once, for the next message's context.
    pub(crate) fn missed_note(&mut self) -> Option<String> {
        if self.missed.is_empty() {
            return None;
        }
        let mut lines = std::mem::take(&mut self.missed);
        let more = lines.len().saturating_sub(MISSED_MAX);
        lines.drain(..more);
        let tail = if more > 0 { format!("\n({more} earlier changes not listed)") } else { String::new() };
        Some(wrap_data(
            "history_log",
            &format!("[others changed the document while you were stopped:\n{}{tail}]", lines.join("\n")),
        ))
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
