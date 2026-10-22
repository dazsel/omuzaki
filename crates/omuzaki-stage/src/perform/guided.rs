//! The guided calibration's run (spec v3 PF-20): about forty seconds of prompts ("relax", "big smile", "brows up" …),
//! timed by the tracked frames themselves, fitted by `omuzaki-track`'s [`GuidedCalibrator`] and applied to the
//! performer through [`PerformHost::set_calibration`], where the app's save picks it up like any other calibration.
//!
//! # Threads
//!
//! The perform thread is never involved beyond copying each frame it polls into a [`FrameTap`] (a wait-free, newest-wins
//! mailbox publish). A run has its own thread, which polls the tap, places each frame in the prompt its capture time
//! falls in, and fits at the end; the fit allocates, so it stays off the perform thread. [`GuidedRun::status`] is what
//! the UI shows: the prompt, the seconds left in it and the frames kept.
//!
//! The routine's clock is the frames' capture times, so the prompts and what is measured never drift apart: a frame
//! belongs to the prompt that was showing when it was captured. A tracker publishes a frame even with no face (a lost
//! frame), so the routine moves on when the face is out of view, and that prompt is reported as skipped. With no
//! frames at all for [`STALL`], the run gives up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Calibration, CalibrationPrompt, Calibrator, FrameSource, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track::guided::GuidedCalibrator;
use omuzaki_track_api::mailbox::{MailboxReader, MailboxWriter, mailbox};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::host::PerformHost;

/// How long a run waits for frames before it gives up (no camera, or the capture was stopped).
pub const STALL: Duration = Duration::from_secs(10);

/// How often the run's thread looks for a new frame: well under a 60 fps frame, so the newest-wins tap loses few.
const POLL: Duration = Duration::from_millis(2);

/// Where a guided calibration stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GuidedStatus {
    /// None has run since the app started.
    #[default]
    Idle,
    /// Waiting for the first frame from the running capture.
    Waiting,
    /// A prompt is showing.
    Running {
        /// What the person is asked to do now.
        prompt: CalibrationPrompt,
        /// Its place in the routine, from 1.
        step: usize,
        /// How many prompts the routine has.
        steps: usize,
        /// Seconds left in this prompt.
        prompt_left_s: f32,
        /// Seconds left in the whole routine.
        left_s: f32,
    },
    /// The routine finished and its calibration is in use.
    Done {
        /// Prompts with too few tracked frames: their channels kept their earlier range.
        skipped: Vec<CalibrationPrompt>,
    },
    /// The routine could not make a calibration; the earlier one stays.
    Failed {
        /// Why, in plain words.
        reason: String,
    },
    /// The person stopped it; the earlier calibration stays.
    Cancelled,
}

impl GuidedStatus {
    /// Whether a run is under way.
    pub const fn is_running(&self) -> bool {
        matches!(self, Self::Waiting | Self::Running { .. })
    }

    /// A routine `prompt` with `step`, `steps`, `prompt_left_s` and `left_s` (see the variant).
    pub const fn running(prompt: CalibrationPrompt, step: usize, steps: usize, prompt_left_s: f32, left_s: f32) -> Self {
        Self::Running { prompt, step, steps, prompt_left_s, left_s }
    }
}

/// Where the perform side copies the frames it polls, for the cold-side readers: a guided run and the gesture watch
/// ([`super::gesture`]) each have their own mailbox. [`tee`](Self::tee) wraps each source the input posts. The perform
/// thread only publishes into newest-wins mailboxes (wait-free, no allocation); the readers sit behind locks on the
/// cold side, replaced with each new source.
#[derive(Default)]
pub struct FrameTap {
    calibration: Mutex<Option<MailboxReader>>,
    gestures: Mutex<Option<MailboxReader>>,
}

impl FrameTap {
    /// `source`, copying every frame it gives into this tap from now on.
    pub fn tee(&self, source: Box<dyn FrameSource>) -> Box<dyn FrameSource> {
        let (calibration, calibration_reader) = mailbox();
        let (gestures, gestures_reader) = mailbox();
        *self.calibration.lock().unwrap_or_else(PoisonError::into_inner) = Some(calibration_reader);
        *self.gestures.lock().unwrap_or_else(PoisonError::into_inner) = Some(gestures_reader);
        Box::new(Tee { source, writers: [calibration, gestures] })
    }

    /// The newest frame since the last poll, from the source posted last, for a guided run.
    fn poll(&self) -> Option<TrackFrame> {
        self.calibration.lock().unwrap_or_else(PoisonError::into_inner).as_mut()?.poll(Seconds(0.0))
    }

    /// The same, for the gesture watch.
    pub(super) fn poll_gestures(&self) -> Option<TrackFrame> {
        self.gestures.lock().unwrap_or_else(PoisonError::into_inner).as_mut()?.poll(Seconds(0.0))
    }
}

impl std::fmt::Debug for FrameTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameTap").finish_non_exhaustive()
    }
}

/// A source that copies what it gives into a tap.
struct Tee {
    source: Box<dyn FrameSource>,
    writers: [MailboxWriter; 2],
}

impl FrameSource for Tee {
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        let frame = self.source.poll(now)?;
        for writer in &mut self.writers {
            writer.publish(&frame);
        }
        Some(frame)
    }
}

/// The routine's prompts on a clock from its start: which prompt `elapsed` seconds fall in, its index and the seconds
/// left in it; `None` once the routine is over.
fn prompt_at(elapsed: f64) -> Option<(usize, CalibrationPrompt, f64)> {
    let mut end = 0.0;
    for (i, prompt) in CalibrationPrompt::ROUTINE.into_iter().enumerate() {
        end += f64::from(prompt.seconds());
        if elapsed < end {
            return Some((i, prompt, end - elapsed));
        }
    }
    None
}

/// The routine's whole length, in seconds.
fn routine_secs() -> f64 {
    CalibrationPrompt::ROUTINE.iter().map(|p| f64::from(p.seconds())).sum()
}

/// One guided calibration at a time, and the status of the last one.
#[derive(Default)]
pub struct GuidedRun {
    status: Arc<Mutex<GuidedStatus>>,
    cancel: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for GuidedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuidedRun").field("status", &self.status()).finish_non_exhaustive()
    }
}

impl GuidedRun {
    /// Where the run stands.
    pub fn status(&self) -> GuidedStatus {
        self.status.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Starts a run over the frames `tap` gives, fitting on top of `base` and applying the result to `perform`. False
    /// if one is already running.
    ///
    /// # Errors
    /// The thread could not be started.
    pub fn start(
        &self,
        tap: Arc<FrameTap>,
        base: Calibration,
        perform: Arc<dyn PerformHost>,
    ) -> std::io::Result<bool> {
        let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if self.status().is_running() {
            return Ok(false);
        }
        if let Some(done) = thread.take() {
            // The last run has ended (its status says so); its thread is finishing or finished.
            done.join().ok();
        }
        // Frames from before the start belong to no prompt.
        while tap.poll().is_some() {}
        self.cancel.store(false, Ordering::Relaxed);
        self.set(GuidedStatus::Waiting);
        let (status, cancel) = (Arc::clone(&self.status), Arc::clone(&self.cancel));
        let body = move || {
            let end = routine(&tap, &base, &status, &cancel);
            let end = match end {
                Ok(outcome) => match perform.set_calibration(outcome.calibration) {
                    Ok(()) => GuidedStatus::Done { skipped: outcome.skipped },
                    Err(e) => GuidedStatus::Failed { reason: format!("the calibration could not be applied: {e}") },
                },
                Err(end) => end,
            };
            *status.lock().unwrap_or_else(PoisonError::into_inner) = end;
        };
        match std::thread::Builder::new().name("omuzaki-calibrate".to_owned()).spawn(body) {
            Ok(handle) => {
                *thread = Some(handle);
                Ok(true)
            }
            Err(e) => {
                self.set(GuidedStatus::Failed { reason: format!("the calibration could not start: {e}") });
                Err(e)
            }
        }
    }

    /// Stops a running routine; the earlier calibration stays. Returns whether one was running.
    pub fn cancel(&self) -> bool {
        let running = self.status().is_running();
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take() {
            thread.join().ok();
        }
        running
    }

    fn set(&self, status: GuidedStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

impl Drop for GuidedRun {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The routine on the run's thread: frames into prompts until the routine's time is up, then the fit. `Err` is the
/// status it ended on instead.
fn routine(
    tap: &FrameTap,
    base: &Calibration,
    status: &Mutex<GuidedStatus>,
    cancel: &AtomicBool,
) -> Result<omuzaki_anim_api::CalibrationOutcome, GuidedStatus> {
    let mut calibrator = GuidedCalibrator::new(*base);
    let (total, steps) = (routine_secs(), CalibrationPrompt::ROUTINE.len());
    let mut start = None::<f64>;
    let mut last_frame = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(GuidedStatus::Cancelled);
        }
        let Some(frame) = tap.poll() else {
            if last_frame.elapsed() > STALL {
                return Err(GuidedStatus::Failed {
                    reason: format!("no frames from the camera for {} s: is the preview running?", STALL.as_secs()),
                });
            }
            std::thread::sleep(POLL);
            continue;
        };
        last_frame = Instant::now();
        let elapsed = frame.time.0 - *start.get_or_insert(frame.time.0);
        let Some((i, prompt, prompt_left)) = prompt_at(elapsed) else { break };
        calibrator.push(prompt, &frame);
        #[allow(clippy::cast_possible_truncation)] // seconds within a forty-second routine
        let shown = GuidedStatus::running(prompt, i + 1, steps, prompt_left as f32, (total - elapsed) as f32);
        *status.lock().unwrap_or_else(PoisonError::into_inner) = shown;
    }
    calibrator.finish().map_err(|e| GuidedStatus::Failed { reason: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompts_follow_one_another_on_the_routines_clock() {
        assert_eq!(prompt_at(0.0).map(|(i, p, _)| (i, p)), Some((0, CalibrationPrompt::Rest)));
        let (i, prompt, left) = prompt_at(4.5).unwrap();
        assert_eq!((i, prompt), (1, CalibrationPrompt::BigSmile));
        assert!((left - 2.5).abs() < 1e-9);
        assert_eq!(prompt_at(routine_secs() - 0.01).map(|(_, p, _)| p), Some(CalibrationPrompt::LookAround));
        assert_eq!(prompt_at(routine_secs()), None);
        assert!((routine_secs() - 38.0).abs() < 1e-9);
    }
}
