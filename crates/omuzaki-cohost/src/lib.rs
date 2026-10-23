//! The AI co-host on stage (CLAUDE.md section 6: "the AI co-host hears the person, answers aloud ... and its face and
//! head move with the reply on a character in the viewport").
//!
//! ```text
//!  Ears (speech host) ─▶ Show (omuzaki-performer) ─▶ Voice (TTS host) ─▶ ai_voice feeds ─▶ program
//!                          │ intents                    │ lead PCM
//!                          ▼                            ▼
//!                     PerformStage ─▶ triggers     LeadAnalysis ─▶ voice mailbox ─▶ LipSync + life cues
//!                          └──────────────── PerformHost (the perform thread) ◀───────────────┘
//! ```
//!
//! - [`stage::PerformStage`]: the performer's `Stage` over a `PerformHost`.
//! - [`attach_voice`]: connects the AI's voice to the host's lip sync, so its mouth and speech nods follow its words.
//! - [`Cohost`]: one step of the loop, the stage and the voice; [`CohostThread`] runs it every few milliseconds on the
//!   program clock, and hands out the transcript.
//! - [`service::CohostService`]: the running co-host behind the `performer.*` ops ([`ops`]), built by a
//!   [`service::CohostFactory`]; [`local::LocalFactory`] builds it from the app's own models, microphone and the
//!   `performer.llm` setting ([`settings`]), and [`local::MicMouth`] gives the mouth to the microphone whenever the
//!   co-host is not speaking.
#![deny(missing_docs)]

omuzaki_core::define_link! {
    omuzaki_anim::link();
    omuzaki_llm::link();
    omuzaki_performer::link();
    omuzaki_performer_api::link();
    omuzaki_speech::link();
    omuzaki_stage::link();
    omuzaki_stage_api::link();
    omuzaki_track_voice::link();
}

pub mod local;
pub mod ops;
pub mod service;
pub mod settings;
pub mod stage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_performer::show::Show;
use omuzaki_performer_api::event::TranscriptLine;
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_track_voice::{LeadAnalysis, LipSyncConfig, VowelProfile, voice_mailbox};

use crate::stage::PerformStage;

/// How often the thread steps: the fake harness's step, well inside the 165 ms barge-in budget.
pub const STEP_EVERY: Duration = Duration::from_millis(5);

/// Connects a new voice analysis to `host`'s lip sync and returns it: give one clone to the TTS host as its
/// `VoiceLead`, and the other to [`Cohost::new`].
///
/// # Errors
/// The host's, when it can't take the voice.
pub fn attach_voice(host: &dyn PerformHost, config: LipSyncConfig) -> Result<LeadAnalysis, PerformError> {
    let (writer, reader) = voice_mailbox();
    host.set_voice(Some(reader), config)?;
    Ok(LeadAnalysis::new(writer, VowelProfile::default()))
}

/// The co-host: the turn loop, its stage and its voice, stepped together.
pub struct Cohost {
    show: Show,
    stage: Arc<PerformStage>,
    voice: Option<LeadAnalysis>,
}

impl Cohost {
    /// A co-host of `show` acting through `stage` (the same one the show's parts hold) and moving its mouth through
    /// `voice`.
    pub fn new(show: Show, stage: Arc<PerformStage>, voice: Option<LeadAnalysis>) -> Self {
        Self { show, stage, voice }
    }

    /// One step at program time `now`.
    pub fn step(&mut self, now: Seconds) {
        self.show.step(now);
        self.stage.tick(now);
        if let Some(voice) = &self.voice {
            voice.poll(now);
        }
    }

    /// The loop, for `say`, `intent`, `stop`, `status` and settings.
    pub fn show(&mut self) -> &mut Show {
        &mut self.show
    }

    /// Stops the turn and frees the stage's trigger keys.
    pub fn stop(&mut self, now: Seconds) {
        self.show.stop(now);
        self.stage.clear();
    }
}

impl std::fmt::Debug for Cohost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cohost").field("show", &self.show).finish_non_exhaustive()
    }
}

/// A [`Cohost`] on its own thread, stepped every [`STEP_EVERY`] on `clock`; its transcript lines go to `lines`.
pub struct CohostThread {
    cohost: Arc<Mutex<Cohost>>,
    clock: Arc<dyn Clock>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CohostThread {
    /// Starts the thread.
    ///
    /// # Errors
    /// When the OS can't start a thread.
    pub fn start(cohost: Cohost, clock: Arc<dyn Clock>, lines: Sender<TranscriptLine>) -> std::io::Result<Self> {
        let cohost = Arc::new(Mutex::new(cohost));
        let running = Arc::new(AtomicBool::new(true));
        let thread = {
            let (cohost, clock, running) = (Arc::clone(&cohost), Arc::clone(&clock), Arc::clone(&running));
            std::thread::Builder::new().name("omuzaki-cohost".into()).spawn(move || {
                while running.load(Ordering::Acquire) {
                    let out = {
                        let mut c = lock(&cohost);
                        c.step(clock.now());
                        c.show.drain()
                    };
                    for line in out {
                        if lines.send(line).is_err() {
                            break;
                        }
                    }
                    std::thread::sleep(STEP_EVERY);
                }
            })?
        };
        Ok(Self { cohost, clock, running, thread: Some(thread) })
    }

    /// Runs `f` on the co-host between steps (`say`, `intent`, settings).
    pub fn with<R>(&self, f: impl FnOnce(&mut Cohost, Seconds) -> R) -> R {
        let now = self.clock.now();
        f(&mut lock(&self.cohost), now)
    }

    /// Stops the co-host and its thread.
    pub fn stop(&mut self) {
        self.with(Cohost::stop);
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CohostThread {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for CohostThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CohostThread").field("running", &self.running.load(Ordering::Relaxed)).finish_non_exhaustive()
    }
}

fn lock(m: &Mutex<Cohost>) -> MutexGuard<'_, Cohost> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
