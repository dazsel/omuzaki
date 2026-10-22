//! [`SpeechHost`]: audio in, speech events and transcripts out, on two worker threads.
//!
//! ```text
//! audio callback          VAD thread                           ASR thread
//! AudioInput::push -----> ring --> Silero VAD --> Segmenter    jobs --> Whisper --> Final / Partial
//!                                       |             \--utterance--> bounded queue (drop oldest)
//!                                       \--> SpeechStarted / SpeechEnded ------------> event queue (drop oldest)
//! ```
//!
//! # Threads and real-time rules
//! - **Audio thread:** only [`AudioInput::push`] (also the engine's [`TapSink`] for the `listen` voice tap): copies into
//!   a preallocated lock-free ring and wakes the VAD thread. No allocation, no locks, no blocking, no model work.
//! - **VAD thread:** reads the ring in 512-sample frames, runs Silero VAD and the segmenter, keeps the audio of the
//!   utterance in progress, and hands each finished utterance (one allocation per utterance) to the ASR thread.
//! - **ASR thread:** runs Whisper, one utterance at a time. It may take longer than the speech lasted; that is what
//!   the queue and its policy are for.
//!
//! # Demand-driven transcription
//! Listening (the VAD) is the baseline cost; Whisper is a feature cost. The ASR thread works only while at least one
//! consumer holds a [`TranscriptDemand`] from [`SpeechHost::request_transcripts`] (captions, the conversation loop, the
//! AI performer). With none, speech start/end events still flow (barge-in, the speaking indicator) but no audio is
//! copied out or queued, and [`HostStats::utterances_skipped`] counts what was not transcribed.
//!
//! # Back-pressure (nothing ever blocks the audio thread)
//! | Where | Full means | Policy | Reported as |
//! |---|---|---|---|
//! | audio ring (`ring_seconds`) | the VAD thread stalled | drop the newest samples | [`HostStats::samples_dropped`] |
//! | a session's "no answer this time" | the slot is switching models | treat the frame as silence, skip the utterance | [`HostStats::vad_unavailable`], [`HostStats::asr_unavailable`] |
//! | utterance queue (`utterance_queue`) | Whisper is slower than speech | drop the *oldest* utterance | [`SpeechEvent::UtteranceDropped`] |
//! | event queue (`event_queue`) | the application stopped polling | drop the oldest event | [`HostStats::events_dropped`] |
//! | partial transcripts | an older partial is still waiting | replace it | [`HostStats::partials_replaced`] |

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use omuzaki_ai_api::Session;
use omuzaki_audio_api::ring::{self, Consumer, Producer};
use omuzaki_audio_api::TapSink;
use omuzaki_audio_api::tap::DropPolicy;

use crate::asr::{Recognizer, Whisper};
use crate::queue::DropOldestQueue;
use crate::segmenter::{Event, FRAME, Segmenter, SegmenterConfig, ms_to_samples};
use crate::vad::{SileroVad, VoiceActivity, is_unavailable};
use crate::{SAMPLE_RATE, SpeechError};

/// How the VAD thread sleeps when the ring is empty. The audio thread also unparks it, so this only bounds the wait
/// if a wake-up is missed.
const IDLE_WAIT: Duration = Duration::from_millis(10);

/// Settings for a [`SpeechHost`].
#[derive(Clone, Debug, PartialEq)]
pub struct HostConfig {
    /// Speech start/end hysteresis, padding and the maximum utterance length. `min_silence_ms` is the hangover that
    /// trades Whisper CPU against latency; see [`DEFAULT_HANGOVER_MS`].
    pub segmenter: SegmenterConfig,
    /// Whisper language code (`Some("en")` for English-only models), or `None` to detect it on multilingual models.
    /// Changeable while running with [`SpeechHost::set_language`].
    pub language: Option<String>,
    /// Capacity of the audio ring in seconds: how long the VAD thread may stall before samples are dropped.
    pub ring_seconds: f32,
    /// Utterances that may wait for Whisper; one more evicts the oldest.
    pub utterance_queue: usize,
    /// Events that may wait for the application; one more evicts the oldest.
    pub event_queue: usize,
    /// While speech is in progress, transcribe the utterance so far every this many milliseconds and emit
    /// [`SpeechEvent::Partial`]. Zero (the default) turns partials off: each one is a full Whisper pass over the audio
    /// so far, so they cost CPU in proportion to utterance length.
    pub partial_interval_ms: u32,
}

/// The host's end-of-speech hangover: how long the VAD must hear silence before an utterance closes and goes to
/// Whisper. Longer than the segmenter's own 200 ms on purpose: every Whisper call costs a fixed encoder pass over
/// a 30 s window however short the audio, so merging phrases separated by a short pause into one call cuts the CPU.
/// Measured with `host_bench` on `base.en`, 4 threads, a read-aloud clip: 600 ms gave 10 calls instead of 16 and
/// 6.7 to 7.7 % of 8 cores instead of 12.3 to 15.4 %, for about 0.3 to 0.5 s more latency from the last spoken sound to
/// the text (research `2026-10-speech-host-cpu.md`). Lower it where turn-taking matters more than CPU (a conversation
/// loop on a GPU recogniser, where calls are cheap); it never delays [`SpeechEvent::SpeechStarted`].
pub const DEFAULT_HANGOVER_MS: u32 = 600;

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            segmenter: SegmenterConfig { min_silence_ms: DEFAULT_HANGOVER_MS, ..SegmenterConfig::default() },
            language: Some("en".into()),
            ring_seconds: 10.0,
            utterance_queue: 4,
            event_queue: 256,
            partial_interval_ms: 0,
        }
    }
}

/// Wall-clock timings of one final transcript. Add the segmenter's `min_silence_ms` (the end-of-speech hangover,
/// [`DEFAULT_HANGOVER_MS`] by default) to get from the last spoken sound to the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Latency {
    /// From the utterance being queued (speech end detected) until Whisper picked it up.
    pub queue_wait: Duration,
    /// Whisper's own time.
    pub asr: Duration,
    /// From speech end detected until this event: `queue_wait + asr` plus thread hand-off.
    pub detect_to_final: Duration,
}

/// What the host reports. Times are seconds since the first sample pushed.
#[derive(Clone, Debug, PartialEq)]
pub enum SpeechEvent {
    /// Speech began (first frame over the threshold, no padding). The earliest signal, for barge-in.
    SpeechStarted {
        /// When.
        at: f64,
    },
    /// An utterance ended; it was queued for Whisper if a consumer wanted transcripts (see [`TranscriptDemand`]).
    SpeechEnded {
        /// Its number, counting up from zero; the same id appears on its transcripts.
        utterance: u64,
        /// Start, padding included.
        start: f64,
        /// End, padding included.
        end: f64,
    },
    /// The speech that started was shorter than `min_speech_ms` (a cough, a click): no utterance will follow.
    SpeechRejected {
        /// Start.
        start: f64,
        /// End.
        end: f64,
    },
    /// A provisional transcript of an utterance still in progress (only when `partial_interval_ms` is set).
    Partial {
        /// The utterance it belongs to.
        utterance: u64,
        /// Start, padding included.
        start: f64,
        /// How far the audio went when this was taken.
        end: f64,
        /// The text so far.
        text: String,
    },
    /// The transcript of a finished utterance.
    Final {
        /// The utterance.
        utterance: u64,
        /// Start, padding included.
        start: f64,
        /// End, padding included.
        end: f64,
        /// The text; empty when Whisper heard nothing.
        text: String,
        /// How long it took.
        latency: Latency,
    },
    /// Whisper could not keep up and this utterance was evicted from the full queue unprocessed.
    UtteranceDropped {
        /// The utterance.
        utterance: u64,
        /// Start.
        start: f64,
        /// End.
        end: f64,
    },
    /// A model call failed. The worker carries on with the next frame or utterance.
    Error {
        /// The utterance involved, if any.
        utterance: Option<u64>,
        /// The error's message.
        message: String,
    },
}

/// Counters since the host started. Cheap to read at any time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostStats {
    /// VAD frames processed (32 ms each).
    pub frames: u64,
    /// Audio samples dropped because the ring was full.
    pub samples_dropped: u64,
    /// Utterances queued for Whisper (a consumer wanted transcripts).
    pub utterances: u64,
    /// Utterances evicted unprocessed because Whisper fell behind.
    pub utterances_dropped: u64,
    /// Utterances heard but not transcribed because no consumer held a [`TranscriptDemand`].
    pub utterances_skipped: u64,
    /// Events evicted because the application did not poll.
    pub events_dropped: u64,
    /// Partial transcripts replaced by a newer one before Whisper got to them.
    pub partials_replaced: u64,
    /// Frames the VAD failed to score (each was treated as silence). Only the first error of a run is reported as an
    /// event, so a model that fails on every frame cannot flood the event queue.
    pub vad_errors: u64,
    /// Frames the `vad@1` session had no answer for because its slot was switching models (each treated as silence,
    /// with no event).
    pub vad_unavailable: u64,
    /// Utterances the `asr@1` session had no answer for because its slot was switching models (skipped, with no
    /// event).
    pub asr_unavailable: u64,
    /// Worker threads that panicked. Non-zero means the host has stopped; a [`SpeechEvent::Error`] said why.
    pub workers_failed: u64,
    /// Time the VAD thread spent scoring frames (the CPU cost of listening).
    pub vad_busy: Duration,
    /// Time the ASR thread spent in Whisper (the CPU cost of transcribing, times its thread count).
    pub asr_busy: Duration,
}

#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    samples_dropped: AtomicU64,
    utterances: AtomicU64,
    utterances_dropped: AtomicU64,
    utterances_skipped: AtomicU64,
    events_dropped: AtomicU64,
    partials_replaced: AtomicU64,
    vad_errors: AtomicU64,
    vad_unavailable: AtomicU64,
    asr_unavailable: AtomicU64,
    workers_failed: AtomicU64,
    vad_busy_ns: AtomicU64,
    asr_busy_ns: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> HostStats {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        HostStats {
            frames: get(&self.frames),
            samples_dropped: get(&self.samples_dropped),
            utterances: get(&self.utterances),
            utterances_dropped: get(&self.utterances_dropped),
            utterances_skipped: get(&self.utterances_skipped),
            events_dropped: get(&self.events_dropped),
            partials_replaced: get(&self.partials_replaced),
            vad_errors: get(&self.vad_errors),
            vad_unavailable: get(&self.vad_unavailable),
            asr_unavailable: get(&self.asr_unavailable),
            workers_failed: get(&self.workers_failed),
            vad_busy: Duration::from_nanos(get(&self.vad_busy_ns)),
            asr_busy: Duration::from_nanos(get(&self.asr_busy_ns)),
        }
    }
}

fn add_time(counter: &AtomicU64, elapsed: Duration) {
    counter.fetch_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// Where the workers publish events; counts what the bounded queue had to evict.
#[derive(Clone)]
struct Emitter {
    queue: Arc<DropOldestQueue<SpeechEvent>>,
    counters: Arc<Counters>,
}

impl Emitter {
    fn emit(&self, event: SpeechEvent) {
        if self.queue.push(event).is_some() {
            self.counters.events_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// How many consumers currently want transcripts. Shared by the host, its workers and every [`TranscriptDemand`].
#[derive(Default)]
struct Demand(AtomicUsize);

impl Demand {
    fn wanted(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }
}

/// A consumer's claim on transcripts: while any of these exists, the host runs Whisper. Cloning adds a claim; dropping
/// one releases it. Hold it exactly as long as the captions (or the conversation, or the AI performer) are in use, so
/// that an idle host costs only the VAD.
#[must_use = "dropping the demand at once turns transcription off again"]
pub struct TranscriptDemand {
    demand: Arc<Demand>,
}

impl TranscriptDemand {
    fn new(demand: &Arc<Demand>) -> Self {
        demand.0.fetch_add(1, Ordering::AcqRel);
        Self { demand: Arc::clone(demand) }
    }

    /// Keeps transcription on for as long as the host lives, for a tool that always wants text.
    pub fn keep(self) {
        std::mem::forget(self);
    }
}

impl Clone for TranscriptDemand {
    fn clone(&self) -> Self {
        Self::new(&self.demand)
    }
}

impl Drop for TranscriptDemand {
    fn drop(&mut self) {
        self.demand.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Partial,
    Final,
}

/// One piece of audio for Whisper.
struct Job {
    kind: JobKind,
    utterance: u64,
    start: f64,
    end: f64,
    samples: Vec<f32>,
    /// When the VAD thread queued it, for the latency report.
    queued: Instant,
}

/// The write end of the host's audio ring. Move it into the audio callback and call [`push`](Self::push) with each
/// block of 16 kHz mono samples, or hand it to the audio engine as the [`TapSink`] of a 16 kHz voice tap.
pub struct AudioInput {
    producer: Producer,
    wake: Thread,
}

impl AudioInput {
    /// Hands a block to the VAD thread. Never allocates, takes a lock or blocks (it makes one non-blocking `unpark`
    /// wake-up call); returns how many samples were accepted. The
    /// rest were dropped because the ring was full and are counted in [`HostStats::samples_dropped`].
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let accepted = self.producer.push(samples);
        // `unpark` is a futex wake when the VAD thread is parked: a syscall, but it never blocks and never allocates,
        // which is what a real-time callback needs. The VAD thread's 10 ms poll is the backstop if a wake is missed.
        self.wake.unpark();
        accepted
    }

    /// How many samples fit in the ring right now. Live capture never needs this (a full ring means the host is
    /// stalled and dropping is the policy); a feeder replaying a file waits on it so that nothing is dropped.
    #[must_use]
    pub fn free(&self) -> usize {
        self.producer.free()
    }
}

impl TapSink for AudioInput {
    fn push(&mut self, samples: &[f32]) -> usize {
        AudioInput::push(self, samples)
    }
}

/// A running speech host. [`stop`](Self::stop) drains the workers; dropping it without that aborts them.
pub struct SpeechHost {
    events: Arc<DropOldestQueue<SpeechEvent>>,
    jobs: Arc<DropOldestQueue<Job>>,
    counters: Arc<Counters>,
    demand: Arc<Demand>,
    language: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    vad_wake: Thread,
    vad_thread: Option<JoinHandle<()>>,
    asr_thread: Option<JoinHandle<()>>,
}

impl SpeechHost {
    /// Starts the workers over a `vad@1` and an `asr@1` session (the registry's slot sessions, which follow the
    /// person's model choice and fall back on their own). Returns the host and the [`AudioInput`] for the audio
    /// callback. The sessions run on the host's worker threads, never on the caller's or the audio thread.
    ///
    /// # Errors
    /// [`SpeechError::Signature`] when a session presents another slot's signature, [`SpeechError::Spawn`] when a
    /// thread cannot be started.
    pub fn open(
        vad: Box<dyn Session>,
        asr: Box<dyn Session>,
        config: &HostConfig,
    ) -> Result<(Self, AudioInput), SpeechError> {
        Self::start(config, Box::new(SileroVad::new(vad)?), Box::new(Whisper::new(asr)?))
    }

    /// Starts the workers over any VAD and recogniser; [`open`](Self::open) is this with the real ones.
    ///
    /// # Errors
    /// [`SpeechError::Spawn`] when a thread cannot be started.
    pub fn start(
        config: &HostConfig,
        vad: Box<dyn VoiceActivity>,
        recognizer: Box<dyn Recognizer>,
    ) -> Result<(Self, AudioInput), SpeechError> {
        let ring_samples = (config.ring_seconds.max(0.1) * SAMPLE_RATE as f32) as usize;
        let (producer, consumer) = ring::ring(ring_samples, DropPolicy::DropNewest, 0);
        let counters = Arc::new(Counters::default());
        let events = Arc::new(DropOldestQueue::new(config.event_queue));
        let jobs = Arc::new(DropOldestQueue::new(config.utterance_queue));
        let emitter = Emitter { queue: Arc::clone(&events), counters: Arc::clone(&counters) };
        let demand = Arc::new(Demand::default());
        let language = Arc::new(Mutex::new(config.language.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(AtomicBool::new(false));

        let worker_shared = Arc::new(WorkerShared {
            emitter: emitter.clone(),
            counters: Arc::clone(&counters),
            stop: Arc::clone(&stop),
            jobs: Arc::clone(&jobs),
            demand: Arc::clone(&demand),
        });
        let asr_thread = {
            let worker = AsrWorker {
                recognizer,
                jobs: Arc::clone(&jobs),
                emitter: emitter.clone(),
                counters: Arc::clone(&counters),
                demand: Arc::clone(&demand),
                language: Arc::clone(&language),
                abort: Arc::clone(&abort),
            };
            let shared = Arc::clone(&worker_shared);
            spawn("omuzaki-asr", move || guarded("asr", &shared, move || worker.run()))?
        };
        let vad_thread = {
            let worker = VadWorker::new(vad, consumer, config, &worker_shared);
            let shared = Arc::clone(&worker_shared);
            spawn("omuzaki-vad", move || guarded("vad", &shared, move || worker.run()))?
        };
        let vad_wake = vad_thread.thread().clone();
        let input = AudioInput { producer, wake: vad_wake.clone() };
        let host = Self {
            events,
            jobs,
            counters,
            demand,
            language,
            stop,
            abort,
            vad_wake,
            vad_thread: Some(vad_thread),
            asr_thread: Some(asr_thread),
        };
        Ok((host, input))
    }

    /// The next event if one is waiting.
    #[must_use]
    pub fn try_next_event(&self) -> Option<SpeechEvent> {
        self.events.try_pop()
    }

    /// Waits up to `timeout` for the next event. `None` on timeout, or once the host has stopped and all events were
    /// read.
    #[must_use]
    pub fn next_event_timeout(&self, timeout: Duration) -> Option<SpeechEvent> {
        self.events.pop_timeout(timeout)
    }

    /// Asks for transcripts: Whisper runs for utterances that end while the returned claim (or a clone) is alive.
    /// Without any claim the host only listens.
    pub fn request_transcripts(&self) -> TranscriptDemand {
        TranscriptDemand::new(&self.demand)
    }

    /// Changes the Whisper language for utterances that start transcribing from now on.
    pub fn set_language(&self, language: Option<String>) {
        *self.language.lock().unwrap_or_else(PoisonError::into_inner) = language;
    }

    /// False once the host has stopped, whether by [`stop`](Self::stop) or because a worker panicked (see
    /// [`HostStats::workers_failed`]).
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
    }

    /// The counters so far.
    #[must_use]
    pub fn stats(&self) -> HostStats {
        self.counters.snapshot()
    }

    /// Finishes cleanly: processes the audio already pushed, closes a speech in progress, transcribes every queued
    /// utterance and joins the workers, returning the final counters. Events stay readable until the queue is empty.
    /// Calling it again is harmless. It waits for the Whisper call in flight and for every queued utterance, so it can
    /// take seconds on a busy backlog; drop the host instead to abandon them.
    pub fn stop(&mut self) -> HostStats {
        self.shutdown(false);
        self.counters.snapshot()
    }

    fn shutdown(&mut self, abort: bool) {
        self.abort.store(abort, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.vad_wake.unpark();
        // A worker that panicked has already stopped; there is nothing more to do with its payload.
        if let Some(handle) = self.vad_thread.take() {
            let _ = handle.join();
        }
        if abort {
            self.jobs.clear();
        }
        self.jobs.close();
        if let Some(handle) = self.asr_thread.take() {
            let _ = handle.join();
        }
        self.events.close();
    }
}

/// Dropping aborts: queued utterances are discarded, but the Whisper call already running cannot be interrupted, so
/// `drop` still waits for it (about 0.5 to 3 s on `base.en`).
impl Drop for SpeechHost {
    fn drop(&mut self) {
        self.shutdown(true);
    }
}

/// What a panicking worker needs to report itself and stop the host.
struct WorkerShared {
    emitter: Emitter,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
    jobs: Arc<DropOldestQueue<Job>>,
    demand: Arc<Demand>,
}

/// Runs a worker body, turning a panic (whisper.cpp, ONNX Runtime, a bug) into one `Error` event and a stopped host
/// instead of a silently dead thread.
fn guarded(name: &'static str, shared: &WorkerShared, body: impl FnOnce()) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        let reason = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        shared.counters.workers_failed.fetch_add(1, Ordering::Relaxed);
        // Stop first, then report: whoever reads the error event must already see a stopped host (`is_running`
        // false, no more utterances accepted). The other worker has nothing useful left to do.
        shared.stop.store(true, Ordering::Release);
        shared.jobs.close();
        shared
            .emitter
            .emit(SpeechEvent::Error { utterance: None, message: format!("{name} thread panicked: {reason}") });
    }
}

fn spawn(name: &'static str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, SpeechError> {
    thread::Builder::new().name(name.into()).spawn(body).map_err(|source| SpeechError::Spawn { name, source })
}

fn seconds(sample: usize) -> f64 {
    sample as f64 / f64::from(SAMPLE_RATE)
}

/// The VAD thread's state.
struct VadWorker {
    vad: Box<dyn VoiceActivity>,
    consumer: Consumer,
    segmenter: Segmenter,
    jobs: Arc<DropOldestQueue<Job>>,
    emitter: Emitter,
    counters: Arc<Counters>,
    demand: Arc<Demand>,
    stop: Arc<AtomicBool>,
    pad: usize,
    partial_interval: usize,
    /// Audio from `history_base` on: the utterance in progress plus the padding before it.
    history: VecDeque<f32>,
    history_base: usize,
    /// Samples received so far (the stream position after the last whole frame).
    total: usize,
    /// Start of the speech in progress and the id its utterance will get.
    speech_start: Option<usize>,
    next_utterance: u64,
    last_partial: usize,
    /// True while the VAD keeps failing, so only the first error of a run becomes an event.
    vad_failing: bool,
}

impl VadWorker {
    fn new(vad: Box<dyn VoiceActivity>, consumer: Consumer, config: &HostConfig, shared: &WorkerShared) -> Self {
        let cfg = config.segmenter;
        let capacity = ms_to_samples(cfg.max_speech_ms) + 2 * ms_to_samples(cfg.pad_ms) + 2 * FRAME;
        Self {
            vad,
            consumer,
            segmenter: Segmenter::new(cfg),
            jobs: Arc::clone(&shared.jobs),
            emitter: shared.emitter.clone(),
            counters: Arc::clone(&shared.counters),
            demand: Arc::clone(&shared.demand),
            stop: Arc::clone(&shared.stop),
            pad: ms_to_samples(cfg.pad_ms),
            partial_interval: ms_to_samples(config.partial_interval_ms),
            history: VecDeque::with_capacity(capacity),
            history_base: 0,
            total: 0,
            speech_start: None,
            next_utterance: 0,
            last_partial: 0,
            vad_failing: false,
        }
    }

    fn run(mut self) {
        let mut frame = [0.0f32; FRAME];
        let mut filled = 0;
        loop {
            // Read the stop flag *before* draining: samples pushed before stop() are then guaranteed to be seen.
            let stopping = self.stop.load(Ordering::Acquire);
            let read = self.consumer.pop_into(&mut frame[filled..]);
            filled += read;
            self.counters.samples_dropped.store(self.consumer.dropped(), Ordering::Relaxed);
            if filled == FRAME {
                self.frame(&frame);
                filled = 0;
            } else if read == 0 {
                if stopping {
                    break;
                }
                thread::park_timeout(IDLE_WAIT);
            }
        }
        if filled > 0 {
            // The tail of the stream: pad the last partial frame with silence so no speech is lost.
            frame[filled..].fill(0.0);
            self.frame(&frame);
        }
        if let Some(event) = self.segmenter.finish() {
            self.handle(event);
        }
    }

    fn frame(&mut self, frame: &[f32; FRAME]) {
        let started = Instant::now();
        self.history.extend(frame.iter().copied());
        self.total += FRAME;
        let probability = match self.vad.probability(frame) {
            Ok(probability) => {
                self.vad_failing = false;
                probability
            }
            Err(error) if is_unavailable(&error) => {
                // The slot is swapping models: no answer this time. Counted, not reported, and heard as silence.
                self.counters.vad_unavailable.fetch_add(1, Ordering::Relaxed);
                0.0
            }
            Err(error) => {
                self.counters.vad_errors.fetch_add(1, Ordering::Relaxed);
                // Report the first error of a run only: a model that fails on every frame would otherwise emit ~31
                // events a second and evict the real ones from the bounded queue. It re-arms after a good frame.
                if !self.vad_failing {
                    self.vad_failing = true;
                    self.emitter.emit(SpeechEvent::Error { utterance: None, message: error.to_string() });
                }
                // Treat the unscorable frame as silence so a speech in progress still ends instead of hanging open.
                0.0
            }
        };
        let event = self.segmenter.push(probability);
        if let Some(event) = event {
            self.handle(event);
        }
        self.maybe_partial();
        self.trim_history();
        self.counters.frames.fetch_add(1, Ordering::Relaxed);
        add_time(&self.counters.vad_busy_ns, started.elapsed());
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Start { at } => {
                self.speech_start = Some(at);
                self.last_partial = at;
                self.emitter.emit(SpeechEvent::SpeechStarted { at: seconds(at) });
            }
            Event::End(segment) => {
                let utterance = self.next_utterance;
                self.next_utterance += 1;
                self.speech_start = None;
                let (start, end) = (seconds(segment.start), seconds(segment.end.min(self.total)));
                self.emitter.emit(SpeechEvent::SpeechEnded { utterance, start, end });
                if !self.demand.wanted() {
                    self.counters.utterances_skipped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                self.counters.utterances.fetch_add(1, Ordering::Relaxed);
                let samples = self.copy_out(segment.start, segment.end);
                let job = Job { kind: JobKind::Final, utterance, start, end, samples, queued: Instant::now() };
                if let Some(evicted) = self.jobs.push(job) {
                    self.counters.utterances_dropped.fetch_add(1, Ordering::Relaxed);
                    self.emitter.emit(SpeechEvent::UtteranceDropped {
                        utterance: evicted.utterance,
                        start: evicted.start,
                        end: evicted.end,
                    });
                }
            }
            Event::Rejected(segment) => {
                self.speech_start = None;
                let end = segment.end.min(self.total);
                self.emitter.emit(SpeechEvent::SpeechRejected { start: seconds(segment.start), end: seconds(end) });
            }
        }
    }

    /// Queues a provisional transcription of the speech so far, if partials are on and the interval has passed.
    fn maybe_partial(&mut self) {
        let Some(start) = self.speech_start else { return };
        if self.partial_interval == 0 || !self.demand.wanted() || self.total - self.last_partial < self.partial_interval
        {
            return;
        }
        self.last_partial = self.total;
        let from = start.saturating_sub(self.pad);
        let job = Job {
            kind: JobKind::Partial,
            utterance: self.next_utterance,
            start: seconds(from),
            end: seconds(self.total),
            samples: self.copy_out(from, self.total),
            queued: Instant::now(),
        };
        if self.jobs.set_ephemeral(job).is_some() {
            self.counters.partials_replaced.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A copy of stream samples `start..end`, clamped to what the history still holds.
    fn copy_out(&self, start: usize, end: usize) -> Vec<f32> {
        let from = start.max(self.history_base) - self.history_base;
        let to = end.min(self.total).saturating_sub(self.history_base).max(from);
        self.history.range(from..to).copied().collect()
    }

    /// Forgets audio no segment can need any more: before the speech in progress (less its padding), or, when idle,
    /// everything but the last frame plus the padding a speech starting on the next frame would want.
    fn trim_history(&mut self) {
        let keep_from = match self.speech_start {
            Some(start) => start.saturating_sub(self.pad),
            None => self.total.saturating_sub(FRAME + self.pad),
        };
        if keep_from > self.history_base {
            self.history.drain(..keep_from - self.history_base);
            self.history_base = keep_from;
        }
    }
}

/// The ASR thread's state.
struct AsrWorker {
    recognizer: Box<dyn Recognizer>,
    jobs: Arc<DropOldestQueue<Job>>,
    emitter: Emitter,
    counters: Arc<Counters>,
    demand: Arc<Demand>,
    language: Arc<Mutex<Option<String>>>,
    abort: Arc<AtomicBool>,
}

impl AsrWorker {
    fn run(mut self) {
        while let Some(job) = self.jobs.pop() {
            if self.abort.load(Ordering::Acquire) {
                break;
            }
            // The last consumer may have left while this waited in the queue: nobody wants the text any more.
            if !self.demand.wanted() {
                if job.kind == JobKind::Final {
                    self.counters.utterances_skipped.fetch_add(1, Ordering::Relaxed);
                }
                continue;
            }
            let queue_wait = job.queued.elapsed();
            let language = self.language.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let started = Instant::now();
            let result = self.recognizer.transcribe(&job.samples, language.as_deref());
            add_time(&self.counters.asr_busy_ns, started.elapsed());
            let event = match (result, job.kind) {
                (Ok(t), JobKind::Final) => SpeechEvent::Final {
                    utterance: job.utterance,
                    start: job.start,
                    end: job.end,
                    text: t.text,
                    latency: Latency { queue_wait, asr: t.elapsed, detect_to_final: job.queued.elapsed() },
                },
                (Ok(t), JobKind::Partial) => {
                    SpeechEvent::Partial { utterance: job.utterance, start: job.start, end: job.end, text: t.text }
                }
                (Err(error), _) if is_unavailable(&error) => {
                    // The slot is swapping models: this utterance gets no text. Counted, not an error.
                    self.counters.asr_unavailable.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                (Err(error), _) => SpeechEvent::Error { utterance: Some(job.utterance), message: error.to_string() },
            };
            self.emitter.emit(event);
        }
    }
}
