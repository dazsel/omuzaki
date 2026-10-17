//! The op event feed. Ops fire `Runner` events (op applied, transaction committed, undo ...); agents learn of them
//! through one resource, `omuzaki://log`:
//!
//! - **read** it for the most recent events, each with a sequence number `n`;
//! - **subscribe** to it (`resources/subscribe`, or `subscriptions/listen` on protocol 2026-07-28) and the server sends
//!   `notifications/resources/updated` whenever an event arrives.
//!
//! Why a resource and not `notifications/message` (logging) or progress: logging is deprecated from protocol
//! 2026-07-28 and progress only exists inside a request. Resource updates are the one push channel every protocol
//! version supports. Bursts are coalesced: a subscriber is told "something changed", then reads the log.
//!
//! Most clients never show those notifications to the model, so the same log also feeds the pull side: each tool
//! result starts with what *other* actors changed since the session's last call (see `changes`).
//!
//! An event reaches the log a moment after the op that caused it (a worker thread moves it over from the runner), so
//! a result may leave out a change made a few microseconds earlier. It is then reported by the next result: positions
//! are event numbers, so nothing is lost or repeated.
//!
//! # Consumers and cursors
//!
//! The ring is shared; every consumer of it keeps a cursor of its own, and reporting to one never moves another's:
//!
//! - **the session's results** (a blocking call, `tasks/get`, `approval_wait`): the session cursors in `State`
//!   (`seen` for document changes, which is also the stale-write guard's `base`; the approval cursor). They are
//!   *consuming*: what a result reports is marked told, because the agent has it;
//! - **a task's pushes** (status notifications): a `PushCursor` owned by that task's watcher. A client may ignore
//!   notifications, so a push must not consume what a later poll or call has yet to deliver; the watcher reports from
//!   its own position and leaves the session's alone. It *starts* as a copy of the session's cursors at the moment the
//!   task is created, not at the task's start: whatever arrived since the session's last result and has not been
//!   reported to the agent is in the first push as well as in the poll, so the two say the same thing;
//! - **resource subscribers** (`Updates`): one position each.
//!
//! A cursor can fall behind the ring (256 entries). A consumer that must not miss an event checks
//! [`EventHub::lost_after`] and refreshes from the authority instead of waiting for an event that is gone: a task
//! watcher re-reads the job from `Jobs::job`; a document position reads as version 0 (see [`EventHub::version_at`]).
//! An approval decision that left the ring is no longer in the feed, and `approval_wait` then answers from the
//! registry's pending list.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use omuzaki_doc_api::{DocPath, DocVersion};
use omuzaki_ops_api::{Actor, Event, Grants, Runner};
use serde::Serialize;
use tokio::sync::watch;

use crate::changes::{self, OwnTxs, Report};
use crate::error::ServerError;
use crate::floats;
use crate::resources::Providers;

/// The resource that carries op events.
pub(crate) const LOG_URI: &str = "omuzaki://log";

/// Events kept for readers. The runner owns the full op log; this is a window for catching up after a notification.
pub(crate) const LOG_CAPACITY: usize = 256;

/// How often the worker checks whether the server has been dropped. The std channel offers no way to wake a blocked
/// `recv` when the server goes away, and the runner (which owns the sender) may outlive the server.
const SHUTDOWN_POLL: Duration = Duration::from_millis(500);

/// Locks a mutex, recovering the data if another thread panicked while holding it: the feed is append-only, so a
/// half-finished push can't leave it inconsistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One event and its number in the log (`n`, rising by one per event).
#[derive(Serialize)]
pub(crate) struct Entry {
    pub(crate) n: u64,
    pub(crate) event: Event,
    /// The newest document version any event up to this one reported. Not part of the log's JSON: it is how a session's
    /// position becomes the `base` of its writes.
    #[serde(skip)]
    pub(crate) version: Option<DocVersion>,
}

/// The JSON body of `omuzaki://log` and of `history_log`.
#[derive(Serialize)]
struct LogView<'a> {
    latest: u64,
    events: Vec<&'a Entry>,
    note: String,
}

#[derive(Default)]
struct Ring {
    /// Sequence number of the last event pushed (0 = none yet).
    last: u64,
    /// The newest document version any event so far reported (or the feed's starting version).
    version: Option<DocVersion>,
    /// The document's version when the feed started.
    start_version: Option<DocVersion>,
    /// The newest op-log record any `OpApplied` event so far carried, or the log's end when the feed started.
    log_seq: u64,
    entries: VecDeque<Entry>,
}

/// The document version an event reports, if it carries one.
fn version_of(event: &Event) -> Option<DocVersion> {
    match event {
        Event::OpApplied { record, .. } => Some(record.version),
        Event::TxRolledBack { version, .. } | Event::Undone { version, .. } | Event::Redone { version, .. } => {
            Some(*version)
        }
        _ => None,
    }
}

/// Collects the runner's events and tells subscribers when one arrives.
pub(crate) struct EventHub {
    ring: Mutex<Ring>,
    /// The runner accepted the subscription, so events are being delivered. Without it (the server's grants lack `read`)
    /// the log stays empty and there is nothing to catch up with.
    fed: AtomicBool,
    /// The sequence number of the newest event; watchers wake on every change.
    latest: watch::Sender<u64>,
}

impl EventHub {
    /// A hub with no events; `fed` says whether a runner is going to deliver any.
    fn empty(fed: bool) -> Self {
        Self { ring: Mutex::default(), fed: AtomicBool::new(fed), latest: watch::Sender::new(0) }
    }

    /// Subscribes to `runner` now (so no event is missed) and starts the worker that drains it. Events carry document
    /// values, so the feed needs `grants` to hold `read`: without it the runner refuses the subscription and the log
    /// stays empty, never leaking what the server's agents may not see.
    ///
    /// # Errors
    /// [`ServerError::EventFeed`] if the worker thread can't start.
    pub(crate) fn start(runner: &dyn Runner, grants: &Grants) -> Result<Arc<Self>, ServerError> {
        let hub = Arc::new(Self::empty(false));
        let Ok(events) = runner.subscribe(grants) else { return Ok(hub) };
        // After the subscription, so a record appended in between is also on its way as an event: the feed starts at
        // the log's end, and only records past it are ones the feed owes us (see `Session::catch_up`).
        let end = latest_log_seq(runner, grants);
        // The document's version as the feed starts, so a session's first write is guarded against whatever changes after
        // it connected, before any event has told it a version. An empty log is the document as it was opened.
        let version = runner
            .log(end.saturating_sub(1), 1, grants)
            .ok()
            .and_then(|records| records.first().map(|r| r.version))
            .filter(|_| end > 0)
            .unwrap_or(DocVersion(0));
        {
            let mut ring = lock(&hub.ring);
            ring.log_seq = end;
            ring.start_version = Some(version);
            ring.version = Some(version);
        }
        hub.fed.store(true, Ordering::Release);
        let weak = Arc::downgrade(&hub);
        std::thread::Builder::new()
            .name("omuzaki-mcp-events".to_owned())
            .spawn(move || drain(&events, &weak))
            .map_err(ServerError::EventFeed)?;
        Ok(hub)
    }

    fn push(&self, event: Event) {
        let n = {
            let mut ring = lock(&self.ring);
            ring.last += 1;
            let n = ring.last;
            ring.version = version_of(&event).or(ring.version);
            if let Event::OpApplied { record, .. } = &event {
                ring.log_seq = ring.log_seq.max(record.seq);
            }
            let version = ring.version;
            ring.entries.push_back(Entry { n, event, version });
            if ring.entries.len() > LOG_CAPACITY {
                ring.entries.pop_front();
            }
            n
        };
        // After the lock is released: a woken reader must find the event already in the ring.
        self.latest.send_replace(n);
    }

    /// A receiver that wakes whenever a new event arrives.
    pub(crate) fn watch(&self) -> watch::Receiver<u64> {
        self.latest.subscribe()
    }

    /// The number of the newest event so far (0 = none yet): where a session starts, caught up with the log.
    pub(crate) fn latest(&self) -> u64 {
        lock(&self.ring).last
    }

    /// Whether the runner is delivering events at all.
    pub(crate) fn is_fed(&self) -> bool {
        self.fed.load(Ordering::Acquire)
    }

    /// The newest op-log record the feed has delivered (or the log's end when it started): the runner's log holds
    /// records past this only while events are still on their way.
    pub(crate) fn log_seq(&self) -> u64 {
        lock(&self.ring).log_seq
    }

    /// The document version a reader that has been told of every event up to `seen` knows, to send as a write's `base`.
    /// The feed starts at the document's version when it began, so this is `None` only when the runner delivers no events. A position older than the
    /// window can't be mapped; it reads as version 0, which makes the executor refuse a write over anything another
    /// actor has changed, so the agent re-reads (and its position catches up) rather than overwrite what it never saw.
    pub(crate) fn version_at(&self, seen: u64) -> Option<DocVersion> {
        let ring = lock(&self.ring);
        match ring.entries.iter().rev().find(|e| e.n <= seen) {
            Some(entry) => entry.version,
            None if ring.entries.front().is_some_and(|e| e.n > seen + 1) => Some(DocVersion(0)),
            None => ring.start_version,
        }
    }

    /// Whether events after number `n` have already left the window, so a consumer at `n` has missed some.
    pub(crate) fn lost_after(&self, n: u64) -> bool {
        let ring = lock(&self.ring);
        ring.last > n && ring.entries.front().is_none_or(|e| e.n > n + 1)
    }

    /// The events after number `n`, oldest first (those still in the window), each with its number.
    pub(crate) fn entries_after(&self, n: u64) -> Vec<(u64, Event)> {
        lock(&self.ring).entries.iter().filter(|e| e.n > n).map(|e| (e.n, e.event.clone())).collect()
    }

    /// The log's JSON body, and the newest event number it shows (the reader is now caught up to it). `after` keeps
    /// only the events numbered above it; `None` keeps every event still in the window.
    ///
    /// # Errors
    /// Only if an event can't be serialised (a bug in an event type).
    pub(crate) fn read(&self, after: Option<u64>) -> Result<(String, u64), serde_json::Error> {
        let ring = lock(&self.ring);
        let note = format!(
            "the last {LOG_CAPACITY} events since this server started, oldest first; `n` rises by one per event"
        );
        let events = ring.entries.iter().filter(|entry| after.is_none_or(|after| entry.n > after)).collect();
        // Shown without the digits an `f32` widened to `f64` brings (see `floats`).
        let view = serde_json::to_value(&LogView { latest: ring.last, events, note })?;
        let body = serde_json::to_string(&floats::tidied(view))?;
        Ok((body, ring.last))
    }

    /// What actors other than `me` changed after event `seen`, and the newest event number it covers (the reader's
    /// new position). `None` when nothing by others happened. `own` is the transactions `me` made.
    pub(crate) fn changes_since(&self, seen: u64, me: &Actor, own: &OwnTxs) -> (Option<Report>, u64) {
        let ring = lock(&self.ring);
        (changes::summarise(&ring.entries, ring.last, seen, me, own), ring.last)
    }

    /// Resolves once the log holds event `n` (or a later one), at once if it already does. It waits on the `latest`
    /// watch, so it wakes on each push instead of polling. The worker thread fills the log on its own schedule, so
    /// a caller that needs an event must await this rather than count virtual time.
    pub(crate) async fn wait_for(&self, n: u64) {
        // The sender lives in `self`, so the watch cannot close while this future borrows it.
        let _ = self.watch().wait_for(|seen| *seen >= n).await;
    }
}

/// Turns the event feed into the resource URIs a subscriber is told changed. One per subscriber (a legacy session's
/// feed task, or a 2026 `subscriptions/listen` stream), because each has its own position in the log.
pub(crate) struct Updates {
    hub: Arc<EventHub>,
    providers: Providers,
    arrivals: watch::Receiver<u64>,
    /// The newest event already turned into notifications.
    after: u64,
}

impl Updates {
    /// Starts at the log's current end: only events from now on are reported.
    pub(crate) fn new(hub: &Arc<EventHub>, providers: &Providers) -> Self {
        // Watch before reading the position: an event in between is at worst reported as an empty wake-up.
        let arrivals = hub.watch();
        Self { hub: Arc::clone(hub), providers: providers.clone(), arrivals, after: hub.latest() }
    }

    /// Waits for an event. `false` when the hub is gone.
    pub(crate) async fn arrived(&mut self) -> bool {
        self.arrivals.changed().await.is_ok()
    }

    /// The URIs in `wanted` that the events since the last call changed, each once however many events there were
    /// (a burst is one notification, then the client reads).
    ///
    /// - the log changed whenever any event arrived;
    /// - an applied op touches what the providers say its patch paths affect;
    /// - a rollback, undo or redo carries no paths, so it touches everything wanted, as does a window that moved on
    ///   past events this subscriber never saw.
    ///
    /// The log comes last, so a subscriber to it knows the rest of that wake has already been sent.
    pub(crate) fn changed(&mut self, wanted: &BTreeSet<String>) -> Vec<String> {
        let entries = self.hub.entries_after(self.after);
        let (Some(&(first, _)), Some(&(last, _))) = (entries.first(), entries.last()) else { return Vec::new() };
        let mut everything = first > self.after + 1;
        self.after = last;
        let mut paths: Vec<DocPath> = Vec::new();
        for (_, event) in &entries {
            match event {
                Event::OpApplied { record, .. } => paths.extend(record.patch.changes.iter().map(|c| c.path.clone())),
                Event::TxRolledBack { .. } | Event::Undone { .. } | Event::Redone { .. } => everything = true,
                _ => {}
            }
        }
        let touched = if everything { Vec::new() } else { self.providers.touched(&paths) };
        let is_touched = |uri: &String| {
            everything || touched.iter().any(|t| t == uri || (t.ends_with('/') && uri.starts_with(t.as_str())))
        };
        let mut uris: Vec<String> = wanted.iter().filter(|u| *u != LOG_URI && is_touched(u)).cloned().collect();
        if wanted.contains(LOG_URI) {
            uris.push(LOG_URI.to_owned());
        }
        uris
    }
}

/// The number of the newest record in the runner's op log (0 when it is empty or `grants` can't read it).
///
/// Records are numbered from 1 without gaps, so this finds the end by doubling and then halving, asking for one record at
/// a time: a log of a million records costs forty calls, not a copy.
pub(crate) fn latest_log_seq(runner: &dyn Runner, grants: &Grants) -> u64 {
    let exists = |n: u64| runner.log(n - 1, 1, grants).is_ok_and(|records| !records.is_empty());
    if !exists(1) {
        return 0;
    }
    let (mut have, mut missing) = (1, 2);
    while exists(missing) {
        have = missing;
        missing = missing.saturating_mul(2);
        if missing == u64::MAX {
            return have;
        }
    }
    while missing - have > 1 {
        let middle = have + (missing - have) / 2;
        if exists(middle) {
            have = middle;
        } else {
            missing = middle;
        }
    }
    have
}

/// Worker body: move events from the runner's channel into the hub until either side goes away.
fn drain(events: &Receiver<Event>, hub: &Weak<EventHub>) {
    loop {
        match events.recv_timeout(SHUTDOWN_POLL) {
            Ok(event) => match hub.upgrade() {
                Some(hub) => hub.push(event),
                None => return,
            },
            Err(RecvTimeoutError::Timeout) if hub.strong_count() > 0 => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use omuzaki_core::time::Seconds;
    use omuzaki_doc_api::{DocVersion, Patch};
    use omuzaki_ops_api::{Actor, OpRecord, TxId};
    use serde_json::json;

    use super::*;

    fn applied(path: &str) -> Event {
        let record = OpRecord::new(
            1,
            "face.set_blend",
            json!({}),
            Actor::System,
            TxId(1),
            Seconds(0.0),
            Patch::new().with(DocPath::parse(path).unwrap(), None, Some(json!(1))),
            DocVersion(1),
        );
        Event::op_applied(record)
    }

    fn wanted(uris: &[&str]) -> BTreeSet<String> {
        uris.iter().map(|u| (*u).to_owned()).collect()
    }

    fn quiet_hub() -> Arc<EventHub> {
        Arc::new(EventHub::empty(true))
    }

    #[test]
    fn updates_name_what_the_paths_touch_with_the_log_last() {
        let hub = quiet_hub();
        let mut updates = Updates::new(&hub, &Providers::new(&[]));
        let all = wanted(&["omuzaki://character/mira", "omuzaki://log", "omuzaki://project"]);
        hub.push(applied("characters/mira/face/blend"));
        assert_eq!(updates.changed(&all), ["omuzaki://character/mira", "omuzaki://project", "omuzaki://log"]);
        assert!(updates.changed(&all).is_empty(), "each event is reported once");
        hub.push(applied("characters/noor/face/blend"));
        assert_eq!(updates.changed(&all), ["omuzaki://project", "omuzaki://log"]);
    }

    #[test]
    fn replacing_or_removing_all_the_characters_reaches_each_subscribed_one() {
        let hub = quiet_hub();
        let mut updates = Updates::new(&hub, &Providers::new(&[]));
        let mira = wanted(&["omuzaki://character/mira"]);
        for path in ["characters", "name"] {
            hub.push(applied(path));
            let expect: &[&str] = if path == "characters" { &["omuzaki://character/mira"] } else { &[] };
            assert_eq!(updates.changed(&mira), expect, "{path}");
        }
    }

    #[test]
    fn a_rollback_or_a_missed_window_touches_everything_wanted() {
        let hub = quiet_hub();
        let mut updates = Updates::new(&hub, &Providers::new(&[]));
        let mira = wanted(&["omuzaki://character/mira"]);
        hub.push(Event::tx_rolled_back(TxId(1), Actor::System, DocVersion(1)));
        assert_eq!(updates.changed(&mira), ["omuzaki://character/mira"]);
        for _ in 0..=LOG_CAPACITY {
            hub.push(applied("characters/noor/x"));
        }
        assert_eq!(updates.changed(&mira), ["omuzaki://character/mira"], "events fell out of the window unseen");
    }

    fn committed(n: u64) -> Event {
        Event::tx_committed(TxId(n), format!("step {n}"), Actor::System)
    }

    #[test]
    fn push_numbers_events_and_keeps_a_bounded_window() {
        let hub = Arc::new(EventHub::empty(true));
        let watcher = hub.watch();
        for i in 1..=(LOG_CAPACITY as u64 + 10) {
            hub.push(committed(i));
        }
        let (body, latest) = hub.read(None).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(latest, LOG_CAPACITY as u64 + 10);
        assert_eq!(hub.latest(), latest);
        assert_eq!(body["latest"], LOG_CAPACITY + 10);
        assert_eq!(body["events"].as_array().unwrap().len(), LOG_CAPACITY);
        assert_eq!(body["events"][0]["n"], 11);
        assert_eq!(*watcher.borrow(), LOG_CAPACITY as u64 + 10);
    }

    #[tokio::test]
    async fn wait_for_stays_pending_until_its_event_arrives_from_another_thread() {
        let hub = Arc::new(EventHub::empty(true));
        let waiter = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.wait_for(2).await })
        };
        // Events reach the log from the worker thread, so push from a separate OS thread, as production does.
        let first = {
            let hub = hub.clone();
            std::thread::spawn(move || hub.push(committed(1)))
        };
        first.join().unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!waiter.is_finished(), "event 1 must not satisfy a wait for event 2");

        let second = {
            let hub = hub.clone();
            std::thread::spawn(move || hub.push(committed(2)))
        };
        second.join().unwrap();
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_returns_at_once_when_the_event_is_already_logged() {
        let hub = Arc::new(EventHub::empty(true));
        hub.push(committed(1));
        hub.push(committed(2));
        hub.wait_for(1).await;
        hub.wait_for(2).await;
    }

    #[test]
    fn the_worker_stops_when_the_hub_is_dropped() {
        let (tx, rx) = mpsc::channel();
        let hub = Arc::new(EventHub::empty(true));
        hub.fed.store(true, Ordering::Release);
        let weak = Arc::downgrade(&hub);
        let worker = std::thread::spawn(move || drain(&rx, &weak));
        tx.send(committed(1)).unwrap();
        drop(hub);
        worker.join().unwrap();
    }
}
