//! The midir side of the MIDI backend: one input connection per port, each handing its messages to the capture thread
//! through a bounded queue (midir calls back on its own thread), and a look for new and departed ports every second.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection};
use omuzaki_input_api::capture::{CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};

use super::{MESSAGE, MidiSource, Polled, run_with};
use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::RawSink;

const MIDI: InputBackend = InputBackend {
    name: "midi.midir",
    classes: DeviceMask::NONE.with(DeviceClass::Midi),
    caps,
    run: Some(run),
    chosen: Chosen::Always,
};
register_input_backend!(MIDI);

/// The client name the OS lists the connections under.
const CLIENT: &str = "Omuzaki";
/// How often the ports are looked for again.
const LOOK: Duration = Duration::from_secs(1);
/// The most messages queued between the callbacks and the capture thread; the thread drains them within milliseconds.
const QUEUE: usize = 4096;
/// The fixed reason the backend fails with where the OS has no MIDI system (on Linux, no ALSA sequencer).
const NO_MIDI: &str = "this machine has no MIDI system";

fn caps() -> CaptureCaps {
    CaptureCaps { midi: Support::Available, ..CaptureCaps::default() }
}

fn run(sink: RawSink) {
    run_with(sink, Midir::new());
}

/// A message as queued: the port's handle, its bytes and their count.
type Queued = (u64, [u8; MESSAGE], u8);

/// One open port.
struct Open {
    handle: u64,
    id: String,
    _connection: MidiInputConnection<()>,
}

/// midir's ports as a [`MidiSource`].
pub(crate) struct Midir {
    /// The connection the ports are listed through, while open.
    lister: Option<MidiInput>,
    open: Vec<Open>,
    /// Every port id seen, in order: a port's handle is its place here plus one, the same each time it comes back.
    seen: Vec<String>,
    /// Handles open in the last run, until this run's first look finds them again or reports them gone.
    stale: Vec<u64>,
    gone: VecDeque<u64>,
    looked: Option<Instant>,
    send: SyncSender<Queued>,
    queue: Receiver<Queued>,
}

impl Midir {
    pub(crate) fn new() -> Self {
        let (send, queue) = sync_channel(QUEUE);
        Self {
            lister: None,
            open: Vec::new(),
            seen: Vec::new(),
            stale: Vec::new(),
            gone: VecDeque::new(),
            looked: None,
            send,
            queue,
        }
    }

    fn handle(&mut self, id: &str) -> u64 {
        let at = self.seen.iter().position(|s| s == id).unwrap_or_else(|| {
            self.seen.push(id.to_owned());
            self.seen.len() - 1
        });
        u64::try_from(at).map_or(u64::MAX, |at| at + 1)
    }

    /// Opens the ports not open yet and reports the ones that left.
    fn look(&mut self) {
        self.looked = Some(Instant::now());
        let Some(lister) = &self.lister else { return };
        let ids: Vec<String> = lister.ports().iter().map(midir::MidiInputPort::id).collect();
        let (kept, left): (Vec<Open>, Vec<Open>) = std::mem::take(&mut self.open).into_iter().partition(|o| ids.contains(&o.id));
        self.open = kept;
        self.gone.extend(left.iter().map(|o| o.handle));
        for id in ids {
            if self.open.iter().any(|o| o.id == id) {
                continue;
            }
            let handle = self.handle(&id);
            if let Some(connection) = self.connect(&id, handle) {
                self.open.push(Open { handle, id, _connection: connection });
            }
        }
        let open: Vec<u64> = self.open.iter().map(|o| o.handle).collect();
        self.gone.extend(self.stale.drain(..).filter(|h| !open.contains(h)));
    }

    /// A connection to port `id` whose messages are queued under `handle`; `None` if it left before it opened.
    fn connect(&self, id: &str, handle: u64) -> Option<MidiInputConnection<()>> {
        let mut input = MidiInput::new(CLIENT).ok()?;
        input.ignore(Ignore::All);
        let port = input.find_port_by_id(id)?;
        let send = self.send.clone();
        let deliver = move |_stamp: u64, bytes: &[u8], (): &mut ()| {
            let Some(len) = u8::try_from(bytes.len()).ok().filter(|n| usize::from(*n) <= MESSAGE) else { return };
            let mut message = [0; MESSAGE];
            if let Some(into) = message.get_mut(..bytes.len()) {
                into.copy_from_slice(bytes);
            }
            // A full queue means the capture thread stalled for thousands of messages; the excess is dropped.
            let _queued = send.try_send((handle, message, len));
        };
        input.connect(&port, CLIENT, deliver, ()).ok()
    }
}

impl MidiSource for Midir {
    fn open(&mut self) -> Result<(), &'static str> {
        let mut lister = MidiInput::new(CLIENT).map_err(|_| NO_MIDI)?;
        lister.ignore(Ignore::All);
        self.lister = Some(lister);
        while self.queue.try_recv().is_ok() {}
        self.look();
        Ok(())
    }

    fn close(&mut self) {
        self.stale.extend(self.open.drain(..).map(|o| o.handle));
        self.lister = None;
        self.looked = None;
        while self.queue.try_recv().is_ok() {}
    }

    fn next(&mut self, wait: Duration) -> Polled {
        if let Some(port) = self.gone.pop_front() {
            return Polled::Gone(port);
        }
        if self.lister.is_some() && self.looked.is_none_or(|at| at.elapsed() >= LOOK) {
            self.look();
            if let Some(port) = self.gone.pop_front() {
                return Polled::Gone(port);
            }
        }
        match self.queue.recv_timeout(wait) {
            Ok((port, bytes, len)) => Polled::Message { port, bytes, len },
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Polled::Quiet,
        }
    }
}
