//! The loader thread of one slot session: loads replacements and drops retired sessions, so neither ever happens on
//! the thread that calls `run`.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use omuzaki_ai_api::{ModelRef, Session, SessionInfo, SlotRef};

use crate::registry::{Chosen, Registry};

/// Work for the loader thread.
pub(super) enum Job {
    /// Load whatever the slot should run now.
    Load(Box<Request>),
    /// Drop a session that was swapped out (releasing a runtime's session can take milliseconds).
    Retire(Box<dyn Session>),
}

/// One load.
pub(super) struct Request {
    /// The model running now, if one is: loading is skipped when the chain's best choice is that same model.
    pub current: Option<ModelRef>,
    /// The model that failed in `run` and must be switched off for this app session, with the reason.
    pub failed: Option<(ModelRef, String)>,
    /// The model the replacement will stand in for if the chain doesn't name it itself: the failed one.
    pub stands_in_for: Option<ModelRef>,
    /// The failed session, dropped here and not on the thread that ran it.
    pub retire: Option<Box<dyn Session>>,
}

/// The loader's answer to a [`Request`].
pub(super) struct Done {
    /// The registry's generation the load was based on: the state read was at least this new.
    pub generation: u64,
    pub outcome: Outcome,
}

pub(super) enum Outcome {
    /// What runs now is still the best choice.
    Keep,
    /// A replacement is ready.
    Swap { session: Box<dyn Session>, info: SessionInfo },
    /// Nothing could be loaded; the slot keeps what it has (nothing, after a crash).
    Failed,
}

/// The sending side of the loader thread, owned by the slot session. Dropping it ends the thread.
pub(super) struct Loader {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

/// Why the loader can't be reached.
#[derive(Debug)]
pub(super) struct Gone;

impl Loader {
    /// Starts the thread for `slot`'s session.
    pub fn spawn(registry: Registry, slot: SlotRef, explicit: Option<ModelRef>) -> io::Result<Self> {
        let (jobs, job_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        thread::Builder::new()
            .name(format!("slot-session {slot}"))
            .spawn(move || work(&registry, &slot, explicit.as_ref(), &job_rx, &done_tx))?;
        Ok(Self { jobs, done })
    }

    /// Queues `job`. Never blocks: the channel is unbounded.
    pub fn send(&self, job: Job) -> Result<(), Gone> {
        self.jobs.send(job).map_err(|_| Gone)
    }

    /// The next answer, if one is ready. Never blocks and takes no lock.
    pub fn try_recv(&self) -> Result<Option<Done>, Gone> {
        match self.done.try_recv() {
            Ok(done) => Ok(Some(done)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(Gone),
        }
    }
}

fn work(registry: &Registry, slot: &SlotRef, explicit: Option<&ModelRef>, jobs: &Receiver<Job>, done: &Sender<Done>) {
    for job in jobs {
        match job {
            Job::Retire(session) => drop(session),
            Job::Load(request) => {
                if done.send(load(registry, slot, explicit, *request)).is_err() {
                    break;
                }
            }
        }
    }
}

fn load(registry: &Registry, slot: &SlotRef, explicit: Option<&ModelRef>, request: Request) -> Done {
    let Request { current, failed, stands_in_for, retire } = request;
    drop(retire);
    if let Some((model, reason)) = &failed {
        tracing::warn!(%slot, %model, "{reason}; switching it off until Omuzaki restarts and loading the next model");
        registry.disable_for_session(model, reason);
    }
    // After the switch-off, which counts as a change itself, and before the chain is read: whatever changes later
    // bumps the generation again and is picked up by the next check.
    let generation = registry.generation();
    let outcome = match registry.load_chain(slot, explicit, current.as_ref()) {
        Ok(Chosen::Keep) => Outcome::Keep,
        Ok(Chosen::Loaded { session, model, wanted }) => {
            let fallback_for = [wanted, stands_in_for].into_iter().flatten().find(|m| m != &model);
            if let Some(original) = &fallback_for {
                tracing::warn!(%slot, %model, %original, "the slot runs a fallback");
            }
            let mut info = session.info();
            info.fallback_for = fallback_for;
            Outcome::Swap { session, info }
        }
        Err(e) => {
            tracing::warn!(%slot, "no model could be loaded: {e}");
            Outcome::Failed
        }
    };
    Done { generation, outcome }
}
