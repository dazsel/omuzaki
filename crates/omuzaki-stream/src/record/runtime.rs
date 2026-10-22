//! The recorder: an [`Output`] that writes a take to disk, video and audio tracks side by side
//! (`docs/design/record.md` D2, D6, D7, D10).
//!
//! `submit` copies the frame into a free buffer of a fixed pool and returns; encoder threads write the frames, so
//! the stream's pump never waits on a codec or the disk. A frame that finds every buffer busy is not encoded: it is
//! counted, and the take repeats the frame before it in its place, so audio stays in sync and the timeline exact
//! (the PNG sequence links the previous file; nothing is ever dropped silently). An audio thread drains each track's
//! source into its WAV file, writing silence for lost, muted and private stretches and listing them in the manifest.
//!
//! The take runs inside [`StreamService`]: [`StreamService::record_start`] builds and starts the recorder at the
//! program's size and rate, [`StreamService::record_stop`] stops it and returns the take's report, which is also
//! written beside the take as `<stem>.take.json`.
#![allow(
    clippy::disallowed_methods,
    reason = "the recorder writes only inside the take folder under `record.dir`, a Machine setting no agent may write; no op passes a path in"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::Image;
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{
    AlphaNeed, Lanes, Output, OutputConfig, OutputInfo, OutputState, OutputStatus, SubmitReport,
};
use omuzaki_stream_api::time::Ticks;
use omuzaki_update_api::hold::Hold;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::disk::{DiskSlot, DiskState, GuardPlan, estimate_rate};
use super::settings::RecordFormat;
use super::take::{Gap, GapCause, StopCause, TakeReport, TrackReport};
use crate::i420;
use crate::service::{OutputId, StreamService};
use crate::sinks::png::PngSink;
use crate::sinks::wav::WavSink;
use crate::sinks::webm::{WebmSink, WebmTrack};
use crate::timeline::AudioSource;
use crate::vp9::{self, EncoderSettings, I420, Vp9Encoder};

/// Frames a recording may hold in its queue before it repeats one (D2: 0.53 s at 30 fps).
pub const QUEUE_FRAMES: usize = 16;
/// The most PNG encoder threads a take uses.
pub const MAX_PNG_THREADS: usize = 4;
/// How often the audio thread drains the tracks.
const AUDIO_POLL: Duration = Duration::from_millis(10);
/// Samples the audio thread reads per call.
const AUDIO_CHUNK: usize = 4096;
/// The registered kind of the recorder output.
pub const KIND: &str = "recorder";

/// One audio track to record.
pub struct AudioTrack {
    /// The track's name, which becomes its file's suffix (`mic` → `<stem>_mic.wav`).
    pub name: String,
    /// Where its samples come from.
    pub source: Box<dyn AudioSource>,
    /// Samples per second per channel.
    pub rate: u32,
    /// Interleaved channels.
    pub channels: u16,
    /// Which stream position lines up with the first video frame.
    pub start: TrackStart,
}

/// Which stream position of a track (in samples, as [`AudioSource`] counts them) lines up with the take's first video
/// frame. Earlier samples are dropped, and silence fills up to it if the source starts later.
pub enum TrackStart {
    /// The source's first sample.
    First,
    /// This stream position.
    Sample(u64),
    /// The position this gives for the first frame's capture time: for an engine tap, the engine clock's sample at
    /// that time plus the tap's capture offset. The track waits (its tap buffers) until the first frame arrives.
    FirstFrame(Align),
}

/// Maps the first frame's capture time to a track's stream position.
pub type Align = Box<dyn FnOnce(Ticks) -> u64 + Send>;

/// A track's start before the first frame has fixed it.
const START_PENDING: u64 = u64::MAX;

/// What to record, with the format already chosen.
pub struct RecordPlan {
    /// The folder the take goes into.
    pub dir: PathBuf,
    /// The take's stem (`take::stem`, made unique in `dir`).
    pub stem: String,
    /// The format: never [`RecordFormat::Auto`], which the caller resolves.
    pub format: RecordFormat,
    /// The audio tracks.
    pub tracks: Vec<AudioTrack>,
    /// The update hold the take keeps until it stops or fails, so the app never restarts into an update mid-take.
    pub hold: Option<Hold>,
    /// The disk guard: refuse to start, and stop cleanly, before the disk fills. `None` runs unguarded.
    pub guard: Option<GuardPlan>,
}

/// What the video half of a take does with frames.
trait VideoWriter: Send {
    /// Queues frame `index` (straight alpha); `false` when the queue is full and the frame was not taken.
    fn submit(&mut self, index: u64, image: &Image) -> Result<bool, StreamError>;
    /// How full the queue is, `0` to `1`.
    fn queue_fill(&self) -> f32;
    /// Writes the rest and closes the video, `frames` long.
    fn finish(self: Box<Self>, frames: u64) -> Result<VideoDone, StreamError>;
}

/// A finished video.
struct VideoDone {
    path: PathBuf,
    encoder: String,
    bytes: u64,
    filled: u64,
}

/// The PNG sequence: a pool of frame buffers and encoder threads writing one file per frame.
struct PngWriter {
    sink: PngSink,
    queue: Option<SyncSender<(u64, Image)>>,
    free: Receiver<Image>,
    workers: Vec<JoinHandle<Result<u64, StreamError>>>,
    queued: Arc<AtomicU64>,
    error: Arc<Mutex<Option<StreamError>>>,
}

impl PngWriter {
    fn start(sink: PngSink) -> Result<Self, StreamError> {
        let (queue_tx, queue_rx) = sync_channel::<(u64, Image)>(QUEUE_FRAMES);
        let (free_tx, free_rx) = sync_channel::<Image>(QUEUE_FRAMES);
        for _ in 0..QUEUE_FRAMES {
            let empty =
                Image { size: UVec2::ZERO, alpha: omuzaki_render_api::AlphaEncoding::Straight, rgba8: Vec::new() };
            free_tx.try_send(empty).map_err(|_| StreamError::Io("the frame buffers could not be made".into()))?;
        }
        let queue_rx = Arc::new(Mutex::new(queue_rx));
        let queued = Arc::new(AtomicU64::new(0));
        let error = Arc::new(Mutex::new(None));
        let threads =
            std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1)).clamp(1, MAX_PNG_THREADS);
        let mut workers = Vec::with_capacity(threads);
        for n in 0..threads {
            let (rx, free, sink, queued, error) =
                (Arc::clone(&queue_rx), free_tx.clone(), sink.clone(), Arc::clone(&queued), Arc::clone(&error));
            let worker = std::thread::Builder::new()
                .name(format!("omuzaki-record-png-{n}"))
                .spawn(move || png_worker(&rx, &free, &sink, &queued, &error))
                .map_err(|e| StreamError::Io(format!("a PNG encoder thread could not start: {e}")))?;
            workers.push(worker);
        }
        Ok(Self { sink, queue: Some(queue_tx), free: free_rx, workers, queued, error })
    }
}

fn png_worker(
    rx: &Mutex<Receiver<(u64, Image)>>,
    free: &SyncSender<Image>,
    sink: &PngSink,
    queued: &AtomicU64,
    error: &Mutex<Option<StreamError>>,
) -> Result<u64, StreamError> {
    let mut bytes = 0;
    loop {
        // Hold the lock only to take a job, so the threads encode in parallel.
        let job = lock(rx).recv();
        let Ok((index, image)) = job else { return Ok(bytes) };
        let result = sink.write(index, image.size, &image.rgba8);
        queued.fetch_sub(1, Ordering::Relaxed);
        // The pool's channel has room for every buffer, so returning one never fails while the writer lives.
        let _ = free.try_send(image);
        match result {
            Ok(n) => bytes += n,
            Err(e) => {
                lock(error).get_or_insert(e.clone());
                return Err(e);
            }
        }
    }
}

impl VideoWriter for PngWriter {
    fn submit(&mut self, index: u64, image: &Image) -> Result<bool, StreamError> {
        if let Some(e) = lock(&self.error).clone() {
            return Err(e);
        }
        let Ok(mut buffer) = self.free.try_recv() else { return Ok(false) };
        buffer.size = image.size;
        buffer.alpha = image.alpha;
        buffer.rgba8.clear();
        buffer.rgba8.extend_from_slice(&image.rgba8);
        let queue = self.queue.as_ref().ok_or(StreamError::Stopped)?;
        self.queued.fetch_add(1, Ordering::Relaxed);
        // As many buffers as queue places, so a buffer taken from the pool always has a place.
        queue.try_send((index, buffer)).map_err(|_| StreamError::Encode("the PNG encoders stopped".into()))?;
        Ok(true)
    }

    #[allow(clippy::cast_precision_loss)] // At most `QUEUE_FRAMES`.
    fn queue_fill(&self) -> f32 {
        self.queued.load(Ordering::Relaxed) as f32 / QUEUE_FRAMES as f32
    }

    fn finish(mut self: Box<Self>, frames: u64) -> Result<VideoDone, StreamError> {
        self.queue = None;
        let mut bytes = 0;
        let mut first_error = None;
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(Ok(n)) => bytes += n,
                Ok(Err(e)) => first_error = first_error.or(Some(e)),
                Err(_) => {
                    first_error = first_error.or(Some(StreamError::Encode("a PNG encoder thread panicked".into())));
                }
            }
        }
        if let Some(e) = first_error {
            return Err(e);
        }
        let filled = self.sink.fill_gaps(frames)?;
        Ok(VideoDone { path: self.sink.dir().to_owned(), encoder: "png".into(), bytes, filled })
    }
}

/// The colour's constant-quality level (0 best, 63 worst): visually clean for a character on a transparent stage.
pub const VP9_COLOUR_CQ: u32 = 14;
/// The alpha's level: edges matter more than colour, so it is coded finer (mean edge error under one level).
pub const VP9_ALPHA_CQ: u32 = 6;
/// libvpx's real-time speed (5 slowest to 9 fastest). Measured at 1080p30 on a 4-core VM: 2.5 ms to convert and
/// 12 ms to encode both pictures, against a 33 ms frame.
pub const VP9_CPU_USED: i32 = 6;
/// Threads converting each frame to the two pictures.
const CONVERT_THREADS: usize = 4;
/// Seconds between keyframes, which is how much a crash can lose and how finely a take can be cut.
pub const VP9_KEY_SECONDS: u32 = 2;

/// What the encoder thread hands back when its queue closes.
struct WebmDone {
    sink: WebmSink,
    encoded: u64,
}

/// `WebM` VP9 with alpha: a pool of frame buffers and one encoder thread running the colour and alpha encoders.
struct WebmWriter {
    queue: Option<SyncSender<(u64, Image)>>,
    free: Receiver<Image>,
    worker: Option<JoinHandle<Result<WebmDone, StreamError>>>,
    queued: Arc<AtomicU64>,
    error: Arc<Mutex<Option<StreamError>>>,
    path: PathBuf,
    encoder: String,
}

impl WebmWriter {
    fn start(path: &std::path::Path, cfg: &OutputConfig) -> Result<Self, StreamError> {
        let settings = |cq_level, studio_range| EncoderSettings {
            width: cfg.size.x,
            height: cfg.size.y,
            fps: cfg.fps,
            cq_level,
            cpu_used: VP9_CPU_USED,
            threads: vp9::default_threads(),
            studio_range,
            realtime: true,
        };
        // Both encoders start here, so a refusal fails `record.start` instead of the first frame.
        let colour = Vp9Encoder::new(settings(VP9_COLOUR_CQ, true))?;
        let alpha = Vp9Encoder::new(settings(VP9_ALPHA_CQ, false))?;
        let sink =
            WebmSink::create(path, WebmTrack { width: cfg.size.x, height: cfg.size.y, fps: cfg.fps, alpha: true })?;
        let (queue_tx, queue_rx) = sync_channel::<(u64, Image)>(QUEUE_FRAMES);
        let (free_tx, free_rx) = sync_channel::<Image>(QUEUE_FRAMES);
        for _ in 0..QUEUE_FRAMES {
            let empty =
                Image { size: UVec2::ZERO, alpha: omuzaki_render_api::AlphaEncoding::Straight, rgba8: Vec::new() };
            free_tx.try_send(empty).map_err(|_| StreamError::Io("the frame buffers could not be made".into()))?;
        }
        let queued = Arc::new(AtomicU64::new(0));
        let error = Arc::new(Mutex::new(None));
        let key_every = u64::from(VP9_KEY_SECONDS) * u64::from(cfg.fps.0) / u64::from(cfg.fps.1.max(1));
        let job = WebmJob { colour, alpha, sink, key_every: key_every.max(1) };
        let (thread_queued, thread_error) = (Arc::clone(&queued), Arc::clone(&error));
        let worker = std::thread::Builder::new()
            .name("omuzaki-record-vp9".into())
            .spawn(move || job.run(&queue_rx, &free_tx, &thread_queued, &thread_error))
            .map_err(|e| StreamError::Io(format!("the VP9 encoder thread could not start: {e}")))?;
        Ok(Self {
            queue: Some(queue_tx),
            free: free_rx,
            worker: Some(worker),
            queued,
            error,
            path: path.to_owned(),
            encoder: format!("libvpx-vp9 {}", vp9::version().unwrap_or_default()),
        })
    }
}

/// The encoder thread's state.
struct WebmJob {
    colour: Vp9Encoder,
    alpha: Vp9Encoder,
    sink: WebmSink,
    key_every: u64,
}

impl WebmJob {
    fn run(
        mut self,
        rx: &Receiver<(u64, Image)>,
        free: &SyncSender<Image>,
        queued: &AtomicU64,
        error: &Mutex<Option<StreamError>>,
    ) -> Result<WebmDone, StreamError> {
        match self.encode_all(rx, free, queued) {
            Ok(encoded) => Ok(WebmDone { sink: self.sink, encoded }),
            Err(e) => {
                lock(error).get_or_insert(e.clone());
                Err(e)
            }
        }
    }

    fn encode_all(
        &mut self,
        rx: &Receiver<(u64, Image)>,
        free: &SyncSender<Image>,
        queued: &AtomicU64,
    ) -> Result<u64, StreamError> {
        let (mut colour, mut alpha) = (I420::default(), I420::default());
        let (mut colour_out, mut alpha_out) = (Vec::new(), Vec::new());
        let mut encoded = 0u64;
        let mut since_key = 0u64;
        while let Ok((index, image)) = rx.recv() {
            let converted = i420::rgba_to_i420_threads(
                &image.rgba8,
                image.size.x,
                image.size.y,
                &mut colour,
                &mut alpha,
                CONVERT_THREADS,
            );
            queued.fetch_sub(1, Ordering::Relaxed);
            // The pool's channel has room for every buffer, so returning one never fails while the writer lives.
            let _ = free.try_send(image);
            if !converted {
                return Err(StreamError::Config("a frame's pixels do not match its size".into()));
            }
            let key = since_key == 0 || since_key >= self.key_every || self.sink.needs_key(index);
            let pts = i64::try_from(index).unwrap_or(i64::MAX);
            colour_out.clear();
            alpha_out.clear();
            // The two pictures are independent: encode them side by side.
            let (colour_encoder, alpha_encoder) = (&mut self.colour, &mut self.alpha);
            let alpha_result = std::thread::scope(|scope| {
                let alpha_job = scope.spawn(|| alpha_encoder.encode(&alpha, pts, key, &mut alpha_out));
                colour_encoder.encode(&colour, pts, key, &mut colour_out)?;
                alpha_job.join().map_err(|_| StreamError::Encode("the alpha encoder panicked".into()))
            })?;
            alpha_result?;
            // No look-ahead and no frame dropping: one packet each per frame in.
            let (Some(c), Some(a)) = (colour_out.first(), alpha_out.first()) else {
                return Err(StreamError::Encode(format!("libvpx returned no packet for frame {index}")));
            };
            self.sink.write_frame(index, c.key, &c.data, Some(&a.data))?;
            since_key = if c.key { 1 } else { since_key + 1 };
            encoded += 1;
        }
        Ok(encoded)
    }
}

impl VideoWriter for WebmWriter {
    fn submit(&mut self, index: u64, image: &Image) -> Result<bool, StreamError> {
        if let Some(e) = lock(&self.error).clone() {
            return Err(e);
        }
        let Ok(mut buffer) = self.free.try_recv() else { return Ok(false) };
        buffer.size = image.size;
        buffer.alpha = image.alpha;
        buffer.rgba8.clear();
        buffer.rgba8.extend_from_slice(&image.rgba8);
        let queue = self.queue.as_ref().ok_or(StreamError::Stopped)?;
        self.queued.fetch_add(1, Ordering::Relaxed);
        queue.try_send((index, buffer)).map_err(|_| StreamError::Encode("the VP9 encoder stopped".into()))?;
        Ok(true)
    }

    #[allow(clippy::cast_precision_loss)] // At most `QUEUE_FRAMES`.
    fn queue_fill(&self) -> f32 {
        self.queued.load(Ordering::Relaxed) as f32 / QUEUE_FRAMES as f32
    }

    fn finish(mut self: Box<Self>, frames: u64) -> Result<VideoDone, StreamError> {
        self.queue = None;
        let worker = self.worker.take().ok_or(StreamError::Stopped)?;
        let done = worker.join().map_err(|_| StreamError::Encode("the VP9 encoder thread panicked".into()))??;
        let bytes = done.sink.finish(frames)?;
        Ok(VideoDone {
            path: self.path.clone(),
            encoder: self.encoder.clone(),
            bytes,
            filled: frames.saturating_sub(done.encoded),
        })
    }
}

/// The audio half of a take: one thread draining every track.
struct AudioPump {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<Vec<TrackReport>, StreamError>>,
}

/// One track while it records.
struct TrackState {
    track: AudioTrack,
    sink: WavSink,
    gaps: Vec<Gap>,
    /// Interleaved samples written, silence included.
    written: u64,
    /// Whether the first sample has been lined up with `start_sample`.
    aligned: bool,
    /// The stream position that lines up with the first frame, if fixed.
    start_sample: Option<u64>,
    /// Set by the recorder at the first frame, for a [`TrackStart::FirstFrame`] track: [`START_PENDING`] until then.
    pending: Option<Arc<AtomicU64>>,
}

impl TrackState {
    #[allow(clippy::cast_precision_loss)] // Seconds for a manifest: microsecond precision is ample.
    fn seconds(&self, samples: u64) -> f64 {
        samples as f64 / (f64::from(self.track.rate) * f64::from(self.track.channels))
    }

    /// Writes `samples` interleaved samples of silence for `cause`, merging with a gap of the same cause just before.
    fn silence(&mut self, samples: u64, cause: GapCause) -> Result<(), StreamError> {
        if samples == 0 {
            return Ok(());
        }
        let at_s = self.seconds(self.written);
        let len_s = self.seconds(samples);
        match self.gaps.last_mut() {
            Some(last) if last.cause == cause && (last.at_s + last.len_s - at_s).abs() < 1e-9 => last.len_s += len_s,
            _ => self.gaps.push(Gap { at_s, len_s, cause }),
        }
        self.sink.write_silence(samples / u64::from(self.track.channels))?;
        self.written += samples;
        Ok(())
    }

    /// Reads everything the source has now into the file. A track still waiting for the first frame reads nothing,
    /// unless this is the `last` pass (a take with no frames), which takes the source from its first sample.
    fn drain(&mut self, buf: &mut [f32], last: bool) -> Result<(), StreamError> {
        if let Some(pending) = &self.pending {
            match pending.load(Ordering::Acquire) {
                START_PENDING if !last => return Ok(()),
                START_PENDING => {}
                s0 => self.start_sample = Some(s0),
            }
            self.pending = None;
        }
        loop {
            let run = self.track.source.read(buf);
            if run.len == 0 {
                return Ok(());
            }
            let mut skip = 0usize;
            if self.aligned {
                self.silence(run.gap_before, GapCause::Lost)?;
            } else if let Some(s0) = self.start_sample {
                let end = run.start_sample + run.len as u64;
                if end <= s0 {
                    continue;
                }
                if run.start_sample < s0 {
                    skip = usize::try_from(s0 - run.start_sample).unwrap_or(run.len);
                } else {
                    self.silence(run.start_sample - s0, GapCause::Lost)?;
                }
                self.aligned = true;
            } else {
                self.aligned = true;
            }
            let samples = &buf[skip..run.len];
            if run.private {
                self.silence(samples.len() as u64, GapCause::Private)?;
            } else if run.muted {
                self.silence(samples.len() as u64, GapCause::Muted)?;
            } else {
                self.sink.write(samples)?;
                self.written += samples.len() as u64;
            }
        }
    }

    fn finish(self) -> Result<TrackReport, StreamError> {
        let frames = self.sink.frames();
        let files = self.sink.finish()?;
        Ok(TrackReport { name: self.track.name, files, frames, rate: self.track.rate, gaps: self.gaps })
    }
}

impl AudioPump {
    fn start(states: Vec<TrackState>) -> Result<Self, StreamError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("omuzaki-record-audio".into())
            .spawn(move || {
                let mut states = states;
                let mut buf = vec![0.0f32; AUDIO_CHUNK];
                loop {
                    let last = flag.load(Ordering::Acquire);
                    for state in &mut states {
                        state.drain(&mut buf, last)?;
                    }
                    if last {
                        break;
                    }
                    std::thread::sleep(AUDIO_POLL);
                }
                states.into_iter().map(TrackState::finish).collect()
            })
            .map_err(|e| StreamError::Io(format!("the audio recording thread could not start: {e}")))?;
        Ok(Self { stop, thread })
    }

    fn finish(self) -> Result<Vec<TrackReport>, StreamError> {
        self.stop.store(true, Ordering::Release);
        self.thread.join().map_err(|_| StreamError::Io("the audio recording thread panicked".into()))?
    }
}

/// The take's report, filled in when the recorder stops.
pub type ReportSlot = Arc<Mutex<Option<Result<TakeReport, StreamError>>>>;

/// The recorder output.
pub struct Recorder {
    dir: PathBuf,
    stem: String,
    format: RecordFormat,
    tracks: Vec<AudioTrack>,
    /// The first-frame starts still to fix, each with the cell its track reads.
    aligners: Vec<(Align, Arc<AtomicU64>)>,
    cfg: Option<OutputConfig>,
    state: OutputState,
    video: Option<Box<dyn VideoWriter>>,
    audio: Option<AudioPump>,
    /// One past the highest frame index submitted: the take's length in frames.
    frames: u64,
    written: u64,
    full: u64,
    last_error: Option<StreamError>,
    report: ReportSlot,
    /// Released when the take stops, fails or is dropped.
    hold: Option<Hold>,
    guard: Option<GuardPlan>,
    /// The take's planning rate, bytes per second, fixed at start.
    rate: u64,
    disk: DiskSlot,
    next_disk_check: Option<Instant>,
}

impl Recorder {
    /// A recorder for `plan`, not started yet.
    pub fn new(plan: RecordPlan) -> Self {
        Self {
            dir: plan.dir,
            stem: plan.stem,
            format: plan.format,
            tracks: plan.tracks,
            aligners: Vec::new(),
            cfg: None,
            state: OutputState::Idle,
            video: None,
            audio: None,
            frames: 0,
            written: 0,
            full: 0,
            last_error: None,
            report: Arc::new(Mutex::new(None)),
            hold: plan.hold,
            guard: plan.guard,
            rate: 0,
            disk: Arc::new(Mutex::new(None)),
            next_disk_check: None,
        }
    }

    /// Where the take's disk state appears while it records.
    pub fn disk_slot(&self) -> DiskSlot {
        Arc::clone(&self.disk)
    }

    /// Asks the disk, records the answer and returns it; `None` without a guard or when the disk can't be asked.
    fn check_disk(&mut self, now: Instant) -> Option<DiskState> {
        let guard = self.guard.as_ref()?;
        self.next_disk_check = Some(now + guard.every);
        let state = guard.check(&self.dir, self.rate);
        *lock(&self.disk) = state;
        state
    }

    /// Closes the take with `cause` and keeps its report.
    fn close(&mut self, cause: StopCause) {
        let result = self.finish(cause);
        if let Err(e) = &result {
            self.last_error = Some(e.clone());
        }
        self.state = if result.is_ok() { OutputState::Stopped } else { OutputState::Failed };
        *lock(&self.report) = Some(result);
        self.hold = None;
    }

    /// Where the take's report appears once the recorder stops.
    pub fn report_slot(&self) -> ReportSlot {
        Arc::clone(&self.report)
    }

    /// The path the take's video will have.
    pub fn video_path(&self) -> PathBuf {
        match self.format {
            RecordFormat::WebmVp9Alpha => self.dir.join(format!("{}.webm", self.stem)),
            _ => self.dir.join(&self.stem),
        }
    }

    fn open_video(&self, cfg: &OutputConfig) -> Result<Box<dyn VideoWriter>, StreamError> {
        match self.format {
            RecordFormat::PngSequence => {
                Ok(Box::new(PngWriter::start(PngSink::create(&self.video_path(), &self.stem)?)?))
            }
            RecordFormat::WebmVp9Alpha => Ok(Box::new(WebmWriter::start(&self.video_path(), cfg)?)),
            _ => Err(StreamError::Config(format!("{:?} is not a format a take can be written in", self.format))),
        }
    }

    fn finish(&mut self, cause: StopCause) -> Result<TakeReport, StreamError> {
        let cfg = self.cfg.clone().ok_or(StreamError::Stopped)?;
        let tracks = match self.audio.take() {
            Some(audio) => audio.finish()?,
            None => Vec::new(),
        };
        let video = self.video.take().ok_or(StreamError::Stopped)?.finish(self.frames)?;
        let mut bytes = video.bytes;
        for track in &tracks {
            for file in &track.files {
                bytes += std::fs::metadata(file).map_or(0, |m| m.len());
            }
        }
        let duration_s = seconds_of(self.frames, cfg.fps);
        let stop_cause = if self.last_error.is_some() { StopCause::WriteFailed } else { cause };
        let report = TakeReport {
            kind: "take".into(),
            stem: self.stem.clone(),
            format: self.format,
            video: video.path,
            encoder: video.encoder,
            size: [cfg.size.x, cfg.size.y],
            fps: [cfg.fps.0, cfg.fps.1],
            frames: self.frames,
            dropped_frames: video.filled,
            duration_s,
            bytes,
            tracks,
            stop_cause,
            manifest: self.dir.join(format!("{}.take.json", self.stem)),
        };
        report.write()?;
        Ok(report)
    }
}

fn video_fps(cfg: Option<&OutputConfig>) -> (u32, u32) {
    cfg.map_or((30, 1), |c| c.fps)
}

#[allow(clippy::cast_possible_truncation)] // Seconds of a take: far below 2^63 ns.
fn seconds_to_ticks(seconds: f64) -> i64 {
    (seconds * 1e9).round() as i64
}

#[allow(clippy::cast_precision_loss)] // Frame counts far below 2^52.
fn seconds_of(frames: u64, fps: (u32, u32)) -> f64 {
    frames as f64 * f64::from(fps.1) / f64::from(fps.0.max(1))
}

impl Output for Recorder {
    fn info(&self) -> OutputInfo {
        OutputInfo::new(KIND, AlphaNeed::Straight, Lanes { gpu: None, cpu: true })
    }

    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        if self.state == OutputState::Running {
            return Err(StreamError::Busy);
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| StreamError::Io(format!("could not create {}: {e}", self.dir.display())))?;
        self.rate = estimate_rate(self.format, cfg.size, cfg.fps);
        if self.check_disk(Instant::now()) == Some(DiskState::Stop) {
            let keep = self.guard.as_ref().map_or(0, |g| g.guard.min_free >> 20);
            self.hold = None;
            return Err(StreamError::Unavailable {
                what: "room on the recordings disk".into(),
                hint: format!(
                    "free space on the disk holding {} (a take keeps {keep} MiB free, record.min_free_mb), or choose another folder in record.dir",
                    self.dir.display()
                ),
            });
        }
        let video = self.open_video(cfg)?;
        let mut states = Vec::with_capacity(self.tracks.len());
        for mut track in self.tracks.drain(..) {
            let sink =
                WavSink::create(&self.dir.join(format!("{}_{}", self.stem, track.name)), track.rate, track.channels)?;
            let (start_sample, pending) = match std::mem::replace(&mut track.start, TrackStart::First) {
                TrackStart::First => (None, None),
                TrackStart::Sample(s) => (Some(s), None),
                TrackStart::FirstFrame(align) => {
                    let cell = Arc::new(AtomicU64::new(START_PENDING));
                    self.aligners.push((align, Arc::clone(&cell)));
                    (None, Some(cell))
                }
            };
            states.push(TrackState {
                track,
                sink,
                gaps: Vec::new(),
                written: 0,
                aligned: false,
                start_sample,
                pending,
            });
        }
        self.audio = if states.is_empty() { None } else { Some(AudioPump::start(states)?) };
        self.video = Some(video);
        self.cfg = Some(cfg.clone());
        self.state = OutputState::Running;
        Ok(())
    }

    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError> {
        if self.state != OutputState::Running {
            return Err(StreamError::Stopped);
        }
        let started = Instant::now();
        let image = frame.cpu.ok_or_else(|| StreamError::Config("the recorder reads the CPU image".into()))?;
        // Once a second by default: the disk about to fill ends the take cleanly, before this frame, with every file readable.
        if self.next_disk_check.is_some_and(|at| started >= at) && self.check_disk(started) == Some(DiskState::Stop) {
            self.close(StopCause::DiskLow);
            return Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, 0));
        }
        let video = self.video.as_mut().ok_or(StreamError::Stopped)?;
        // The first frame fixes where the waiting tracks start: frame 0 is the take's first frame (the pacer's zero).
        for (align, cell) in self.aligners.drain(..) {
            let ticks =
                Ticks(frame.capture_time.0 - seconds_to_ticks(seconds_of(frame.index, video_fps(self.cfg.as_ref()))));
            cell.store(align(ticks).min(START_PENDING - 1), Ordering::Release);
        }
        self.frames = self.frames.max(frame.index + 1);
        let dropped = match video.submit(frame.index, image) {
            Ok(true) => {
                self.written += 1;
                0
            }
            Ok(false) => {
                self.full += 1;
                1
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                self.state = OutputState::Failed;
                self.hold = None;
                return Err(e);
            }
        };
        Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, dropped))
    }

    fn status(&self) -> OutputStatus {
        let mut status = OutputStatus::default();
        status.state = self.state;
        status.frames = self.written;
        status.dropped = self.frames.saturating_sub(self.written);
        status.queue_fill = self.video.as_ref().map_or(0.0, |v| v.queue_fill());
        status.last_error.clone_from(&self.last_error);
        status
    }

    fn stop(&mut self) {
        if !matches!(self.state, OutputState::Running | OutputState::Failed) || self.video.is_none() {
            return;
        }
        self.close(StopCause::Requested);
    }
}

/// The take in progress.
pub(crate) struct ActiveTake {
    id: OutputId,
    stem: String,
    format: RecordFormat,
    video: PathBuf,
    started: Instant,
    report: ReportSlot,
    disk: DiskSlot,
}

/// What `record.start` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordStarted {
    /// The take's stem.
    pub stem: String,
    /// The format it is written in.
    pub format: RecordFormat,
    /// Where its video goes.
    pub video: PathBuf,
    /// Frame size in pixels.
    pub size: [u32; 2],
    /// Frames per second as `[numerator, denominator]`.
    pub fps: [u32; 2],
}

/// What `record.status` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordStatus {
    /// Whether a take is recording.
    pub recording: bool,
    /// The take's stem, while recording.
    pub stem: Option<String>,
    /// The format, while recording.
    pub format: Option<RecordFormat>,
    /// Where its video goes, while recording.
    pub video: Option<PathBuf>,
    /// Seconds since it started.
    pub elapsed_s: f64,
    /// Frames written.
    pub frames: u64,
    /// Frames repeated because the encoder fell behind.
    pub dropped_frames: u64,
    /// How full the encoder's queue is, `0` to `1`; above `0.5` the take is falling behind.
    pub queue_fill: f32,
    /// The take's last error, if any.
    pub error: Option<String>,
    /// The disk's state while a guarded take records: `low` gives the seconds of space left.
    pub disk: Option<DiskState>,
    /// Why the take ended on its own (the disk was about to fill, a write failed); `record.stop` returns its report.
    pub ended: Option<StopCause>,
}

impl StreamService {
    /// Starts a take of the program picture with `plan`.
    ///
    /// # Errors
    /// [`StreamError::Busy`] while another take records, or what [`Recorder`]'s start reports.
    pub fn record_start(&self, plan: RecordPlan) -> Result<RecordStarted, StreamError> {
        let mut active = lock(&self.recording);
        if let Some(take) = active.as_ref() {
            // A take that ended on its own (its report is in its manifest) makes way for the next.
            if lock(&take.report).is_none() {
                return Err(StreamError::Busy);
            }
            let id = take.id;
            *active = None;
            let _ = self.stop(id);
        }
        let program = self.program();
        let recorder = Recorder::new(plan);
        let (stem, format, video, report, disk) = (
            recorder.stem.clone(),
            recorder.format,
            recorder.video_path(),
            recorder.report_slot(),
            recorder.disk_slot(),
        );
        let cfg = OutputConfig::new(stem.clone(), program.size, program.fps)?;
        let id = self.start(Box::new(recorder), cfg)?;
        *active = Some(ActiveTake {
            id,
            stem: stem.clone(),
            format,
            video: video.clone(),
            started: Instant::now(),
            report,
            disk,
        });
        Ok(RecordStarted {
            stem,
            format,
            video,
            size: [program.size.x, program.size.y],
            fps: [program.fps.0, program.fps.1],
        })
    }

    /// Stops the take and returns its report, which is also written as `<stem>.take.json`.
    ///
    /// # Errors
    /// [`StreamError::Stopped`] when nothing records, or the error that ended the take's files.
    pub fn record_stop(&self) -> Result<TakeReport, StreamError> {
        let take = lock(&self.recording).take().ok_or(StreamError::Stopped)?;
        self.stop(take.id)?;
        let result = lock(&take.report).take();
        result.unwrap_or_else(|| Err(StreamError::Io("the take ended without a report".into())))
    }

    /// The take in progress, if any.
    pub fn record_status(&self) -> RecordStatus {
        let active = lock(&self.recording);
        let Some(take) = active.as_ref() else {
            return RecordStatus {
                recording: false,
                stem: None,
                format: None,
                video: None,
                elapsed_s: 0.0,
                frames: 0,
                dropped_frames: 0,
                queue_fill: 0.0,
                error: None,
                disk: None,
                ended: None,
            };
        };
        let status = self.outputs().into_iter().find(|o| o.id == take.id);
        let ended = match lock(&take.report).as_ref() {
            Some(Ok(report)) => Some(report.stop_cause),
            Some(Err(_)) => Some(StopCause::WriteFailed),
            None => None,
        };
        RecordStatus {
            recording: ended.is_none(),
            stem: Some(take.stem.clone()),
            format: Some(take.format),
            video: Some(take.video.clone()),
            elapsed_s: take.started.elapsed().as_secs_f64(),
            frames: status.as_ref().map_or(0, |s| s.frames),
            dropped_frames: status.as_ref().map_or(0, |s| s.dropped),
            queue_fill: status.as_ref().map_or(0.0, |s| s.queue_fill),
            error: status.and_then(|s| s.error),
            disk: *lock(&take.disk),
            ended,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
