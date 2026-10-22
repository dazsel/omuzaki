//! A scripted [`PadSource`] for the tests: the gilrs event list the acceptance asks for, fed from the test thread,
//! with a handshake so a test knows its events were handed on before it drains a feed.
#![cfg(feature = "test-hooks")]

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, SystemTime};

use gilrs::{Axis, Button};

use super::{PadInput, PadSource, Polled};

/// How long [`PadScript::sync`] waits for the capture thread before it says no: a ceiling for a failing test, never
/// an ordering.
const SYNC_CEILING: Duration = Duration::from_secs(10);

enum Step {
    Event(u64, PadInput, SystemTime),
    Sync(Sender<()>),
}

/// The test's end of a scripted gamepad backend: each call is one gilrs event, read by the backend's capture thread in
/// order.
#[derive(Debug)]
pub struct PadScript {
    steps: Sender<Step>,
}

impl PadScript {
    fn send(&self, pad: u64, input: PadInput) {
        let _sent = self.steps.send(Step::Event(pad, input, SystemTime::now()));
    }

    /// Pad `pad` connected.
    pub fn connect(&self, pad: u64) {
        self.send(pad, PadInput::Connected);
    }

    /// It left.
    pub fn disconnect(&self, pad: u64) {
        self.send(pad, PadInput::Disconnected);
    }

    /// A button went down.
    pub fn press(&self, pad: u64, button: Button) {
        self.send(pad, PadInput::Button(button, true));
    }

    /// A button went up.
    pub fn release(&self, pad: u64, button: Button) {
        self.send(pad, PadInput::Button(button, false));
    }

    /// A button's analog value (a trigger's).
    pub fn value(&self, pad: u64, button: Button, value: f32) {
        self.send(pad, PadInput::Value(button, value));
    }

    /// An axis moved.
    pub fn axis(&self, pad: u64, axis: Axis, value: f32) {
        self.send(pad, PadInput::Axis(axis, value));
    }

    /// Waits until the capture thread has handed on every event sent before this call; `false` if it did not within
    /// the ceiling (it parks while capture does not read pads).
    pub fn sync(&self) -> bool {
        let (done, wait) = channel();
        self.steps.send(Step::Sync(done)).is_ok() && wait.recv_timeout(SYNC_CEILING).is_ok()
    }
}

/// The capture thread's end.
pub(crate) struct ScriptSource {
    steps: Receiver<Step>,
    connected: BTreeSet<u64>,
    /// A sync read but not yet answered: it is answered when the loop next asks for an event, so everything the loop
    /// did with the events before it (a run's start included) is done.
    synced: Option<Sender<()>>,
}

/// A script and its source, with `connected` pads there from the start.
pub(crate) fn script(connected: &[u64]) -> (PadScript, ScriptSource) {
    let (steps, rx) = channel();
    (PadScript { steps }, ScriptSource { steps: rx, connected: connected.iter().copied().collect(), synced: None })
}

impl PadSource for ScriptSource {
    fn connected(&self) -> Vec<u64> {
        self.connected.iter().copied().collect()
    }

    fn next(&mut self, wait: Duration) -> Polled {
        if let Some(done) = self.synced.take() {
            let _acked = done.send(());
        }
        let step = if wait.is_zero() {
            match self.steps.try_recv() {
                Ok(step) => step,
                Err(TryRecvError::Empty) => return Polled::Quiet,
                Err(TryRecvError::Disconnected) => return Polled::Ended,
            }
        } else {
            match self.steps.recv_timeout(wait) {
                Ok(step) => step,
                Err(RecvTimeoutError::Timeout) => return Polled::Quiet,
                Err(RecvTimeoutError::Disconnected) => return Polled::Ended,
            }
        };
        match step {
            Step::Event(pad, input, at) => {
                match input {
                    PadInput::Connected => {
                        self.connected.insert(pad);
                    }
                    PadInput::Disconnected => {
                        self.connected.remove(&pad);
                    }
                    _ => {}
                }
                Polled::Event { pad, input, at }
            }
            Step::Sync(done) => {
                self.synced = Some(done);
                Polled::Quiet
            }
        }
    }
}
