//! [`CohostService`]: the co-host as the app runs it, one at a time, behind the `performer.*` ops (`ai-performer.md`
//! §3, §7.3, §8). The app puts one in the runner's services with a [`CohostFactory`] that builds the real parts (the
//! speech host on the microphone, the TTS host on the mixer, the LLM); the ops start, stop and steer it, and read its
//! status and transcript, the same way for a person in the studio and an agent over MCP.
//!
//! The transcript stays in memory (§7.3: speech is personal data and never enters the op log): the last
//! [`TRANSCRIPT_KEPT`] lines, each with a sequence number a reader passes back to read only what is new.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_performer::addressing::Respond;
use omuzaki_performer::persona::Persona;
use omuzaki_performer_api::event::TranscriptLine;
use omuzaki_performer_api::intent::Intent;
use omuzaki_stage_api::ids::MemberId;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Cohost, CohostThread};

/// How many transcript lines the service keeps; older ones are dropped first.
pub const TRANSCRIPT_KEPT: usize = 512;

/// What a co-host is started with.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Booking {
    /// The character it performs, as the document names it (`characters/<id>`); `None` for the character on stage
    /// when the project has none.
    pub character: Option<String>,
    /// The cast member its intents target.
    pub member: MemberId,
    /// Who it is.
    pub persona: Persona,
    /// When it answers.
    pub respond: Respond,
}

impl Booking {
    /// A booking of `persona` on `member`, answering when addressed.
    pub fn new(member: MemberId, persona: Persona) -> Self {
        Self { character: None, member, persona, respond: Respond::Addressed }
    }

    /// The same, for the character `id`.
    #[must_use]
    pub fn character(mut self, id: impl Into<String>) -> Self {
        self.character = Some(id.into());
        self
    }

    /// The same, answering as `respond` says.
    #[must_use]
    pub fn respond(mut self, respond: Respond) -> Self {
        self.respond = respond;
        self
    }
}

/// A co-host built and ready to run.
#[non_exhaustive]
pub struct Built {
    /// The loop, its stage and its voice.
    pub cohost: Cohost,
    /// The program clock its parts keep time on.
    pub clock: Arc<dyn Clock>,
    /// What it runs on, for `performer.status`: the LLM, the models, where the voice goes. Never a key.
    pub about: Value,
    /// Stops what the factory started besides the co-host (the speech host, its microphone tap), after the co-host
    /// thread has stopped.
    pub on_stop: Option<Box<dyn FnOnce() + Send>>,
}

impl Built {
    /// A co-host on `clock`, with nothing else to stop.
    pub fn new(cohost: Cohost, clock: Arc<dyn Clock>) -> Self {
        Self { cohost, clock, about: Value::Null, on_stop: None }
    }

    /// The same, describing itself as `about`.
    #[must_use]
    pub fn about(mut self, about: Value) -> Self {
        self.about = about;
        self
    }

    /// The same, running `on_stop` when it stops.
    #[must_use]
    pub fn on_stop(mut self, on_stop: impl FnOnce() + Send + 'static) -> Self {
        self.on_stop = Some(Box::new(on_stop));
        self
    }
}

/// Why a co-host could not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// Something it needs is not set up on this machine (a model, the microphone, the LLM): `what` says which, `fix`
    /// says how a person sets it up.
    #[error("{what}")]
    NotReady {
        /// What is missing.
        what: String,
        /// How to set it up.
        fix: String,
    },
    /// A part failed to start.
    #[error("{0}")]
    Failed(String),
}

impl StartError {
    /// A missing piece and how to set it up.
    pub fn not_ready(what: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::NotReady { what: what.into(), fix: fix.into() }
    }

    /// A part that failed.
    pub fn failed(why: impl std::fmt::Display) -> Self {
        Self::Failed(why.to_string())
    }
}

/// Builds the co-host's parts. The app's factory builds real ones; tests build fakes.
pub trait CohostFactory: Send + Sync {
    /// The host the co-host would send what it hears to, if that host is not this machine: a person must start such
    /// a co-host (`performer.start` asks). `None` when everything stays local.
    fn sends_to(&self) -> Option<String> {
        None
    }

    /// What it would cost to run, when it bills by use (a hosted realtime model): per minute heard and spoken, and
    /// the show's cap. `None` when it costs nothing.
    fn costs(&self) -> Option<Value> {
        None
    }

    /// Whether it could start now, and if not, what is missing; asked by a dry run, so it starts nothing.
    ///
    /// # Errors
    /// [`StartError::NotReady`] naming the first missing piece.
    fn check(&self) -> Result<(), StartError> {
        Ok(())
    }

    /// Builds a co-host for `booking`.
    ///
    /// # Errors
    /// [`StartError`].
    fn build(&self, booking: &Booking) -> Result<Built, StartError>;
}

/// The running co-host.
struct Running {
    thread: CohostThread,
    lines: Receiver<TranscriptLine>,
    booking: Booking,
    about: Value,
    started: Seconds,
    on_stop: Option<Box<dyn FnOnce() + Send>>,
}

impl Running {
    fn stop(mut self) {
        self.thread.stop();
        if let Some(on_stop) = self.on_stop.take() {
            on_stop();
        }
    }
}

/// The transcript the service keeps: numbered lines, the oldest dropped first.
#[derive(Default)]
struct Kept {
    lines: VecDeque<(u64, TranscriptLine)>,
    next: u64,
}

impl Kept {
    fn push(&mut self, line: TranscriptLine) {
        if self.lines.len() == TRANSCRIPT_KEPT {
            self.lines.pop_front();
        }
        self.next += 1;
        self.lines.push_back((self.next, line));
    }
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    kept: Kept,
}

impl State {
    /// Moves what the co-host wrote since the last read into the kept transcript.
    fn collect(&mut self) {
        if let Some(running) = &self.running {
            for line in running.lines.try_iter() {
                self.kept.push(line);
            }
        }
    }
}

/// One line as `performer.transcript` answers it.
#[derive(Serialize)]
struct Numbered<'a> {
    seq: u64,
    #[serde(flatten)]
    line: &'a TranscriptLine,
}

/// The co-host behind the `performer.*` ops: at most one runs at a time.
pub struct CohostService {
    factory: Arc<dyn CohostFactory>,
    state: Mutex<State>,
}

impl CohostService {
    /// A service that builds its co-host with `factory`.
    pub fn new(factory: Arc<dyn CohostFactory>) -> Self {
        Self { factory, state: Mutex::default() }
    }

    /// The factory, for the ops' checks.
    pub fn factory(&self) -> &dyn CohostFactory {
        self.factory.as_ref()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a co-host runs.
    pub fn is_running(&self) -> bool {
        self.state().running.is_some()
    }

    /// Builds and starts a co-host for `booking`. A running one is stopped first, so a second start replaces it.
    ///
    /// # Errors
    /// [`StartError`] from the factory, or when the co-host thread cannot start; nothing is left running then.
    pub fn start(&self, booking: Booking) -> Result<Value, StartError> {
        self.stop();
        let built = self.factory.build(&booking)?;
        let (sender, lines) = channel();
        let started = built.clock.now();
        let thread = CohostThread::start(built.cohost, built.clock, sender)
            .map_err(|e| StartError::failed(format!("the co-host thread did not start: {e}")));
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                if let Some(on_stop) = built.on_stop {
                    on_stop();
                }
                return Err(error);
            }
        };
        let running = Running { thread, lines, booking, about: built.about, started, on_stop: built.on_stop };
        let mut state = self.state();
        state.running = Some(running);
        Ok(Self::describe(&state))
    }

    /// Stops the co-host: cuts its turn, frees its expression triggers and stops its parts. Answers whether one ran.
    pub fn stop(&self) -> bool {
        let running = {
            let mut state = self.state();
            state.collect();
            state.running.take()
        };
        // Stopped outside the lock: the thread's last step may still be writing lines.
        running.map(Running::stop).is_some()
    }

    /// Runs `f` on the running co-host.
    fn with<R>(&self, f: impl FnOnce(&mut Cohost, Seconds) -> R) -> Option<R> {
        let state = self.state();
        state.running.as_ref().map(|running| running.thread.with(f))
    }

    /// Speaks `text` (inline tags allowed) as its own turn, cutting a running one. `None` when no co-host runs.
    pub fn say(&self, text: &str) -> Option<()> {
        self.with(|cohost, now| cohost.show().say(now, text))
    }

    /// Applies `intent` now, checked and limited like the model's. `None` when no co-host runs.
    pub fn intent(&self, intent: &Intent) -> Option<()> {
        self.with(|cohost, now| cohost.show().intent(now, intent))
    }

    /// Changes when it answers, from the next utterance. `None` when no co-host runs.
    pub fn set_respond(&self, respond: Respond) -> Option<()> {
        let done = self.with(|cohost, _| cohost.show().set_respond(respond));
        if done.is_some()
            && let Some(running) = &mut self.state().running
        {
            running.booking.respond = respond;
        }
        done
    }

    /// The `performer.status` answer: whether it runs, and if so on what, for whom, and where its turn is.
    pub fn status(&self) -> Value {
        let mut state = self.state();
        state.collect();
        Self::describe(&state)
    }

    fn describe(state: &State) -> Value {
        let Some(running) = &state.running else {
            return json!({ "running": false, "transcript_seq": state.kept.next });
        };
        let (status, now) = running.thread.with(|cohost, now| (cohost.show().status(now), now));
        json!({
            "running": true,
            "character": running.booking.character,
            "member": running.booking.member.to_string(),
            "persona": running.booking.persona.name,
            "persona_revision": running.booking.persona.revision().to_string(),
            "respond": running.booking.respond,
            "for_s": (now.0 - running.started.0).max(0.0),
            "status": status,
            "runs_on": running.about,
            "transcript_seq": state.kept.next,
        })
    }

    /// The transcript lines after `after` (0 for all that are kept), at most `limit`, oldest first; `next` is what to
    /// pass as `after` to read on.
    pub fn transcript(&self, after: u64, limit: usize) -> Value {
        let mut state = self.state();
        state.collect();
        let lines: Vec<Numbered<'_>> = state
            .kept
            .lines
            .iter()
            .filter(|(seq, _)| *seq > after)
            .take(limit)
            .map(|(seq, line)| Numbered { seq: *seq, line })
            .collect();
        let next = lines.last().map_or(after.min(state.kept.next), |l| l.seq);
        let dropped = state.kept.lines.front().is_some_and(|(first, _)| *first > after + 1);
        json!({ "lines": lines, "next": next, "dropped_before": dropped, "running": state.running.is_some() })
    }
}

impl Drop for CohostService {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for CohostService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CohostService").field("running", &self.is_running()).finish_non_exhaustive()
    }
}
