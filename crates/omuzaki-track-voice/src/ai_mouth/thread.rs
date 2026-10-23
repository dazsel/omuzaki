//! The drive on its own thread: [`MouthHandle`] is the TTS host's [`VoiceLead`], [`MouthThread`] polls the drive every
//! [`POLL`] of the program clock and hands each released item to the stage.
//!
//! Both sides take one short lock: the pump's push copies the PCM into the drive's buffers, the poll runs the model
//! hand-off and the release. Neither is the audio thread, which never waits on either (it only writes the play log).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_anim_api::Layer;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_performer_api::voice::{CutProbe, VoiceLead};

use super::{MouthDrive, MouthStats};

/// Where released items go: the stage's `PlayoutDelay` for the AI mouth.
pub type MouthOut = Box<dyn FnMut(Seconds, &Layer) + Send>;

/// How often the release thread polls: twice per 5 ms engine block, four times per 1,024-frame block.
pub const POLL: Duration = Duration::from_micros(2_500);

fn lock(drive: &Mutex<MouthDrive>) -> MutexGuard<'_, MouthDrive> {
    // A panic while holding the lock leaves the drive as it was between two calls; keep going.
    drive.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pump's end of the drive. Cloning shares the drive.
#[derive(Clone)]
pub struct MouthHandle(Arc<Mutex<MouthDrive>>);

impl MouthHandle {
    /// A handle over `drive`.
    pub fn new(drive: MouthDrive) -> Self {
        Self(Arc::new(Mutex::new(drive)))
    }

    /// The drive's counters.
    pub fn stats(&self) -> MouthStats {
        lock(&self.0).stats()
    }

    /// Runs one poll at `now` (what the thread does every [`POLL`]; tests step it themselves).
    pub fn poll(&self, now: Seconds, out: &mut dyn FnMut(Seconds, &Layer)) {
        lock(&self.0).poll(now, out);
    }
}

impl VoiceLead for MouthHandle {
    fn attach(&mut self, probe: CutProbe) {
        lock(&self.0).attach(probe);
    }

    fn push(&mut self, at: Seconds, feed_sample: u64, rate: u32, pcm: &[f32], private: bool) {
        lock(&self.0).push(at, feed_sample, rate, pcm, private);
    }

    fn cut(&mut self, at: Seconds) {
        lock(&self.0).cut(at);
    }
}

/// The release thread. Dropping it stops the thread.
pub struct MouthThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MouthThread {
    /// Polls `handle`'s drive every [`POLL`] of `clock`, handing each `(stamp, layer)` to `out` (the stage's
    /// `PlayoutDelay` for the AI mouth). Cold.
    ///
    /// # Errors
    /// The OS refused a thread.
    pub fn start(handle: &MouthHandle, clock: Arc<dyn Clock>, mut out: MouthOut) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let drive = Arc::clone(&handle.0);
        let thread = std::thread::Builder::new().name("omuzaki-ai-mouth".into()).spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                lock(&drive).poll(clock.now(), &mut *out);
                std::thread::sleep(POLL);
            }
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A panic on the thread has ended it already; there is nothing to recover here.
            let _ = thread.join();
        }
    }
}

impl Drop for MouthThread {
    fn drop(&mut self) {
        self.stop();
    }
}
