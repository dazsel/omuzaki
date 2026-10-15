//! [`SlotSession`]: the session `ModelRegistry::open` returns. It follows its slot instead of one model (design
//! `docs/design/ai-api.md` section 7, ADR 0017):
//!
//! - a new selection, an install or a removal is noticed between runs and swapped in once it has loaded;
//! - a crash, a stall or any runtime error switches the model off for the app session and falls back down the
//!   slot's chain.
//!
//! **`run` never waits.** It reads the registry's generation (an atomic), polls a channel without blocking and calls
//! the current session. Loading, switching models off (which writes the state) and dropping retired sessions all
//! happen on the session's loader thread (`loader`). Until a replacement is ready the old session keeps answering,
//! or `RunError::Unavailable` does when there is none.

mod loader;

use omuzaki_ai_api::{
    ModelRef, RegistryError, RunError, Session, SessionInfo, Signature, SlotRef, TensorBuf, TensorRef, find_slot,
};

use crate::registry::Registry;
use loader::{Done, Job, Loader, Outcome, Request};

/// A session that follows its slot: see the [module documentation](self).
pub(crate) struct SlotSession {
    registry: Registry,
    slot: SlotRef,
    /// The model `open` was asked for, if any: it stays first in the chain for the session's life.
    explicit: Option<ModelRef>,
    signature: Signature,
    /// What runs now; while there is no session, what ran last.
    info: SessionInfo,
    active: Option<Box<dyn Session>>,
    /// The registry generation the current choice was made at.
    seen: u64,
    /// Started by `open`, not by the first change: starting a thread is too slow for `run`. `None` only if the
    /// thread couldn't be started or has died, and then `hand_over` tries again.
    loader: Option<Loader>,
    /// Loads handed to the loader that haven't answered yet.
    in_flight: usize,
    /// A `reset` the next session to be swapped in hasn't had yet.
    reset_owed: bool,
}

impl SlotSession {
    /// Wraps `session`, which `registry` just loaded for `slot`, read at `generation`. `wanted` is the model asked
    /// for (explicit, else selected): if the session runs another one, it stands in for it.
    ///
    /// # Errors
    /// `UnknownSlot` if the slot isn't registered.
    pub fn start(
        registry: Registry,
        slot: &SlotRef,
        explicit: Option<ModelRef>,
        generation: u64,
        session: Box<dyn Session>,
        wanted: Option<ModelRef>,
    ) -> Result<Self, RegistryError> {
        let signature = find_slot(slot).ok_or_else(|| RegistryError::UnknownSlot(slot.clone()))?.signature();
        let mut info = session.info();
        info.fallback_for = wanted.filter(|w| w != &info.model);
        let mut this = Self {
            registry,
            slot: slot.clone(),
            explicit,
            signature,
            info,
            active: Some(session),
            seen: generation,
            loader: None,
            in_flight: 0,
            reset_owed: false,
        };
        this.loader = this.spawn_loader();
        Ok(this)
    }

    fn spawn_loader(&self) -> Option<Loader> {
        Loader::spawn(self.registry.handle(), self.slot.clone(), self.explicit.clone())
            .inspect_err(|e| tracing::warn!(slot = %self.slot, "couldn't start a loader thread: {e}"))
            .ok()
    }

    /// Takes in the loader's answers and, if the registry changed since the last choice, asks for a new one.
    fn poll(&mut self) {
        self.receive();
        if self.in_flight == 0 && self.registry.generation() != self.seen {
            let request = Request {
                current: self.active.is_some().then(|| self.info.model.clone()),
                failed: None,
                stands_in_for: self.info.fallback_for.clone(),
                retire: None,
            };
            self.hand_over(request);
        }
    }

    /// Applies every answer that is ready.
    fn receive(&mut self) {
        while self.in_flight > 0 {
            let Some(loader) = &self.loader else { return };
            match loader.try_recv() {
                Ok(Some(done)) => self.apply(done),
                Ok(None) => return,
                Err(_) => self.lose_loader(),
            }
        }
    }

    fn apply(&mut self, done: Done) {
        self.in_flight -= 1;
        self.seen = done.generation;
        if let Outcome::Swap { mut session, info } = done.outcome {
            if self.reset_owed {
                session.reset();
            }
            self.info = info;
            if let Some(old) = self.active.replace(session) {
                self.retire(old);
            }
        }
        // Whatever was running has been reset already; only a slot with nothing running still owes it.
        if self.in_flight == 0 && self.active.is_some() {
            self.reset_owed = false;
        }
    }

    fn retire(&mut self, session: Box<dyn Session>) {
        if let Some(loader) = &self.loader
            && loader.send(Job::Retire(session)).is_err()
        {
            self.lose_loader();
        }
    }

    /// Queues `request`. If the loader can't be reached, the slot keeps what it has and tries again at the next
    /// change.
    fn hand_over(&mut self, request: Request) {
        if self.loader.is_none() {
            self.loader = self.spawn_loader();
        }
        match self.loader.as_ref().map(|l| l.send(Job::Load(Box::new(request)))) {
            Some(Ok(())) => self.in_flight += 1,
            _ => self.lose_loader(),
        }
    }

    /// The loader thread died (a panic in a runtime): forget its queue. The next change starts a new one.
    fn lose_loader(&mut self) {
        tracing::warn!(slot = %self.slot, "the loader thread stopped; the slot keeps its current model");
        self.loader = None;
        self.in_flight = 0;
        self.seen = self.registry.generation();
    }

    /// The running model failed: take it out, answer `Unavailable` from now on, and let the loader switch it off and
    /// load the next one in the chain.
    fn fail(&mut self, error: &RunError) {
        let failed = self.info.model.clone();
        let request = Request {
            current: None,
            stands_in_for: self.info.fallback_for.clone().or_else(|| Some(failed.clone())),
            failed: Some((failed, format!("it stopped working ({error})"))),
            retire: self.active.take(),
        };
        self.hand_over(request);
    }
}

impl Session for SlotSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.poll();
        let Some(session) = self.active.as_mut() else { return Err(RunError::Unavailable) };
        match session.run(inputs, outputs) {
            Err(error @ (RunError::Crashed | RunError::Failed(_))) => {
                self.fail(&error);
                Err(RunError::Unavailable)
            }
            other => other,
        }
    }

    fn reset(&mut self) {
        self.receive();
        if let Some(session) = self.active.as_mut() {
            session.reset();
        }
        self.reset_owed = self.in_flight > 0 || self.active.is_none();
    }

    /// While the slot has no session (after a crash with nothing to fall back to), the model that ran last.
    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}
