//! A scripted [`MidiSource`] for the tests: ports plugged and unplugged and messages sent from the test thread, with a
//! handshake so a test knows its messages were handed on before it polls a feed.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{MESSAGE, MidiSource, Polled};
use crate::shared::lock;

/// How long [`MidiScript::sync`] waits for the capture thread before it says no: a ceiling for a failing test, never
/// an ordering.
const SYNC_CEILING: Duration = Duration::from_secs(10);

enum Step {
    Message(u64, [u8; MESSAGE], u8),
    Unplug(u64),
    Sync(Sender<()>),
}

/// What the test sees of the source.
#[derive(Debug, Default)]
struct Seen {
    /// Whether the ports are open now, and how many times they were opened.
    open: AtomicBool,
    opens: AtomicU32,
    /// The ports plugged in.
    ports: Mutex<BTreeSet<u64>>,
    /// Whether `open` fails as a machine with no MIDI system would.
    no_midi: AtomicBool,
}

/// The test's end of a scripted MIDI backend.
#[derive(Debug)]
pub struct MidiScript {
    steps: Sender<Step>,
    seen: Arc<Seen>,
}

impl MidiScript {
    /// Port `port` is plugged in.
    pub fn plug(&self, port: u64) {
        lock(&self.seen.ports).insert(port);
    }

    /// It is unplugged.
    pub fn unplug(&self, port: u64) {
        lock(&self.seen.ports).remove(&port);
        let _sent = self.steps.send(Step::Unplug(port));
    }

    /// Port `port` sends `bytes` (at most three; more are cut off). Nothing is read while the ports are closed: a
    /// message sent then is lost, as on a real port.
    pub fn send(&self, port: u64, bytes: &[u8]) {
        if !self.is_open() {
            return;
        }
        let mut message = [0; MESSAGE];
        let len = bytes.len().min(MESSAGE);
        if let (Some(into), Some(from)) = (message.get_mut(..len), bytes.get(..len)) {
            into.copy_from_slice(from);
        }
        let _sent = self.steps.send(Step::Message(port, message, u8::try_from(len).unwrap_or(0)));
    }

    /// Whether the ports are open now (capture reads MIDI).
    pub fn is_open(&self) -> bool {
        self.seen.open.load(Ordering::Acquire)
    }

    /// Waits until the ports are open (`true`) or closed (`false`); `false` if they were not within the ceiling. The
    /// handshake for a pause or a start, while the loop parks and [`sync`](Self::sync) cannot answer.
    pub fn wait_open(&self, open: bool) -> bool {
        let deadline = std::time::Instant::now() + SYNC_CEILING;
        while self.is_open() != open {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// How many times the ports were opened.
    pub fn opens(&self) -> u32 {
        self.seen.opens.load(Ordering::Acquire)
    }

    /// Makes the next open fail as a machine with no MIDI system would.
    pub fn without_midi(&self) {
        self.seen.no_midi.store(true, Ordering::Release);
    }

    /// Waits until the capture thread has handed on every step sent before this call; `false` if it did not within
    /// the ceiling (it parks while capture does not read MIDI).
    pub fn sync(&self) -> bool {
        let (done, wait) = channel();
        self.steps.send(Step::Sync(done)).is_ok() && wait.recv_timeout(SYNC_CEILING).is_ok()
    }
}

/// The capture thread's end.
pub(crate) struct ScriptSource {
    steps: Receiver<Step>,
    seen: Arc<Seen>,
    /// A sync read but not yet answered: it is answered when the loop next asks, so everything it did before is done.
    synced: Option<Sender<()>>,
}

/// A script and its source.
pub(crate) fn script() -> (MidiScript, ScriptSource) {
    let (steps, rx) = channel();
    let seen = Arc::new(Seen::default());
    (MidiScript { steps, seen: Arc::clone(&seen) }, ScriptSource { steps: rx, seen, synced: None })
}

impl MidiSource for ScriptSource {
    fn open(&mut self) -> Result<(), &'static str> {
        if self.seen.no_midi.load(Ordering::Acquire) {
            return Err("this machine has no MIDI system");
        }
        self.seen.open.store(true, Ordering::Release);
        self.seen.opens.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn close(&mut self) {
        self.seen.open.store(false, Ordering::Release);
    }

    fn next(&mut self, wait: Duration) -> Polled {
        if let Some(done) = self.synced.take() {
            let _acked = done.send(());
        }
        match self.steps.recv_timeout(wait) {
            // A closed port reads nothing, and a port not plugged in sends nothing.
            Ok(Step::Message(port, bytes, len)) => {
                if self.seen.open.load(Ordering::Acquire) && lock(&self.seen.ports).contains(&port) {
                    Polled::Message { port, bytes, len }
                } else {
                    Polled::Quiet
                }
            }
            Ok(Step::Unplug(port)) => Polled::Gone(port),
            Ok(Step::Sync(done)) => {
                self.synced = Some(done);
                Polled::Quiet
            }
            Err(RecvTimeoutError::Timeout) => Polled::Quiet,
            Err(RecvTimeoutError::Disconnected) => Polled::Ended,
        }
    }
}
