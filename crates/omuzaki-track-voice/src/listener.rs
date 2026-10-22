//! [`VoiceListener`]: the analyser on its own thread, reading an audio-api tap ring (the `Voice(Mouth)` tap, PF-12)
//! and publishing each frame to the voice mailbox.
//!
//! The ring has no wake-up, so the thread reads what is there every [`POLL`] and sleeps between reads: the audio
//! callback never waits on it, and a 5 ms poll against 10 ms hops adds at most one poll of latency. A muted run
//! (the person muted the mic: nobody hears, `omuzaki-audio-api` taps) is analysed as silence, so the mouth closes; a
//! gap (the device stalled) is not filled.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_audio_api::tap::TapReader;

use crate::analyzer::VoiceAnalyzer;
use crate::mailbox::VoiceWriter;

/// How often the thread reads the ring.
pub const POLL: Duration = Duration::from_millis(5);

/// Samples read per call: 20 ms at 48 kHz, more than a poll's worth at any supported rate.
const CHUNK: usize = 1024;

/// The running listener. Dropping it stops the thread.
pub struct VoiceListener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl VoiceListener {
    /// Starts analysing `reader` with `analyzer` (made for the tap's rate), publishing into `writer`. Cold.
    ///
    /// # Errors
    /// The OS refused a thread.
    pub fn start(mut reader: TapReader, mut analyzer: VoiceAnalyzer, mut writer: VoiceWriter) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("omuzaki-voice".into()).spawn(move || {
            let mut buf = [0.0f32; CHUNK];
            while !flag.load(Ordering::Relaxed) && !reader.writer_gone() {
                loop {
                    let run = reader.read(&mut buf);
                    if run.len == 0 {
                        break;
                    }
                    let samples = &mut buf[..run.len];
                    if run.muted {
                        samples.fill(0.0);
                    }
                    analyzer.push(samples, |frame| writer.publish(frame));
                }
                std::thread::sleep(POLL);
            }
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }

    /// Stops the thread and waits for it (at most one [`POLL`] and one read).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A panic on the thread has ended it already; there is nothing to recover here.
            let _ = thread.join();
        }
    }
}

impl Drop for VoiceListener {
    fn drop(&mut self) {
        self.stop();
    }
}
