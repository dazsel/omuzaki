//! The binding dispatcher (#469, PF-05; design §5 "Consumers"): the ops side of the binding feed. It polls
//! `BindingFeed` and runs each hit's action:
//!
//! - **An op** runs through the executor on the binding's press, as the person, with `cause: binding` (the binding's
//!   name as the cause text) in the journal, so it lands in history like the person's own edit and undoes the same
//!   way. A repeat or a release runs nothing: an op is one step, and an action that lasts while held is a trigger.
//!   The action's scopes were checked when it was set (`input.binding.set_action`), so the person's grants apply.
//! - **A trigger** (PF-25, #478) goes to the trigger sink the App wires in, with every edge, so a held trigger ends on
//!   its release.
//! - **An intent** never arrives here: the feed routes it to the frame thread.
//!
//! An op that fails (a missing op, bad arguments, a conflict) is counted and kept as the last failure for the binding
//! editor to show; the dispatcher carries on with the next hit. Hits run one at a time in the feed's order, so a
//! press is never overtaken by a later one; while an op runs, later hits wait in the rings, whose drop policy keeps
//! releases (design §5).
//!
//! [`BindingRunner::pump`] runs what is queued on the caller's thread (the tests drive it that way, with no sleeps);
//! [`BindingRunner::spawn`] runs it on a thread of its own that looks every [`POLL`], so an action starts within a
//! frame of its press.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_input_api::binding::{BindingAction, BindingId, Edge};
use omuzaki_input_api::feed::{BindingFeed, InputControl};
use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, Request, Runner};

/// How often the dispatcher's thread looks at the feed: under a 60 Hz frame, and cheap when nothing is pressed.
pub const POLL: Duration = Duration::from_millis(8);

/// Where a trigger binding's edges go (PF-25's trigger slots): the slot, the edge and when it happened.
pub type TriggerSink = Box<dyn FnMut(u8, Edge, Seconds) + Send>;

/// An op a binding ran that failed.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BindingFailure {
    /// The binding.
    pub id: BindingId,
    /// The op it ran.
    pub op: String,
    /// Why it failed.
    pub error: OpError,
}

/// What the dispatcher has done so far.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct DispatchReport {
    /// Ops run and applied.
    pub ran: u64,
    /// Ops that failed.
    pub failed: u64,
    /// Trigger edges handed to the sink.
    pub triggers: u64,
    /// The latest failure.
    pub last_failure: Option<BindingFailure>,
}

fn lock(report: &Mutex<DispatchReport>) -> MutexGuard<'_, DispatchReport> {
    report.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs the binding feed's hits; see the module docs.
pub struct BindingRunner {
    feed: Box<dyn BindingFeed>,
    runner: Arc<dyn Runner>,
    control: Arc<dyn InputControl>,
    person: Actor,
    triggers: Option<TriggerSink>,
    report: Arc<Mutex<DispatchReport>>,
}

impl BindingRunner {
    /// A dispatcher over `feed` (the hub's, from `take_feeds`), running ops through `runner` as `person`. `control`
    /// names the bindings in the journal.
    pub fn new(
        feed: Box<dyn BindingFeed>,
        runner: Arc<dyn Runner>,
        control: Arc<dyn InputControl>,
        person: Actor,
    ) -> Self {
        Self { feed, runner, control, person, triggers: None, report: Arc::default() }
    }

    /// Hands trigger bindings' edges to `sink`; without one they are dropped.
    #[must_use]
    pub fn with_triggers(mut self, sink: TriggerSink) -> Self {
        self.triggers = Some(sink);
        self
    }

    /// What it has done so far.
    pub fn report(&self) -> DispatchReport {
        lock(&self.report).clone()
    }

    /// Runs every hit queued now, in order, and returns how many there were.
    pub fn pump(&mut self) -> usize {
        let mut count = 0;
        loop {
            // The action is copied out, so the feed's borrow ends before the op runs.
            let Some((id, action, edge, at)) = self.feed.poll().map(|hit| (hit.id, hit.action.clone(), hit.edge, hit.at))
            else {
                return count;
            };
            count += 1;
            self.run(id, action, edge, at);
        }
    }

    fn run(&mut self, id: BindingId, action: BindingAction, edge: Edge, at: Seconds) {
        match action {
            BindingAction::Op { op, args, .. } => {
                if edge == Edge::Press {
                    self.run_op(id, op, args);
                }
            }
            BindingAction::Trigger(slot) => {
                if let Some(sink) = &mut self.triggers {
                    lock(&self.report).triggers += 1;
                    sink(slot, edge, at);
                }
            }
            // Intents go to the frame thread; anything newer than this dispatcher is not its to run.
            _ => {}
        }
    }

    fn run_op(&self, id: BindingId, op: String, args: serde_json::Value) {
        let name = self.control.bindings().into_iter().find(|b| b.id == id).map(|b| b.name);
        let request = Request {
            op: op.clone(),
            args,
            mode: Mode::Apply,
            actor: self.person.clone(),
            grants: Grants::person(),
            cause: Some(Cause { kind: CauseKind::Binding, text: name }),
            tx: None,
            base: None,
        };
        let result = self.runner.run(request);
        let mut report = lock(&self.report);
        match result {
            Ok(_) => report.ran += 1,
            Err(error) => {
                report.failed += 1;
                report.last_failure = Some(BindingFailure { id, op, error });
            }
        }
    }

    /// Runs the dispatcher on a thread of its own until the returned handle is dropped.
    ///
    /// # Errors
    /// The OS could not start the thread.
    pub fn spawn(mut self) -> std::io::Result<BindingDispatcher> {
        let stop = Arc::new(AtomicBool::new(false));
        let report = Arc::clone(&self.report);
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("input bindings".to_owned()).spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                if self.pump() == 0 {
                    std::thread::park_timeout(POLL);
                }
            }
        })?;
        Ok(BindingDispatcher { stop, report, thread: Some(thread) })
    }
}

/// The running dispatcher; dropping it stops its thread.
pub struct BindingDispatcher {
    stop: Arc<AtomicBool>,
    report: Arc<Mutex<DispatchReport>>,
    thread: Option<JoinHandle<()>>,
}

impl BindingDispatcher {
    /// What it has done so far.
    pub fn report(&self) -> DispatchReport {
        lock(&self.report).clone()
    }
}

impl Drop for BindingDispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            // A panicking op already reported itself; there is nothing more to do with it here.
            let _joined = thread.join();
        }
    }
}
