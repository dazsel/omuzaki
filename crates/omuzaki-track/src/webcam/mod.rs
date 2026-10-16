//! The webcam [`Tracker`]: a camera thread takes camera frames, converts them and publishes `TrackFrame`s to the
//! mailbox (spec v3 §22 PF-01: off the main thread, newest frame wins); the face tracker runs on a thread of its own,
//! so a slow or stuck tracker cannot hold up the lost frame or `stop`. An optional preview tap serves the UI.
//!
//! ```text
//!  camera thread                                     tracker thread
//!  next_frame ─▶ ClockMap ─▶ convert ─▶ jobs (1) ─▶  FrameTracker::track
//!                  ▲                                        │
//!                  └── publish ◀── recv_timeout(tick) ◀── done (1)
//! ```
//!
//! | Module | What it does |
//! |---|---|
//! | `convert` | [`FrameConverter`]: any [`PixelFormat`] to packed RGB8 in a reused buffer |
//! | `preview` | [`PreviewFrame`] and the tap that fills it, only while it is on |
//! | `thread` | the camera thread's loop, the tracker's thread and the latency counters |
//!
//! The camera thread alone publishes, so publication takes no lock. It blocks only inside
//! [`CameraStream::next_frame`] and in a bounded wait, a tick at a time, for the tracker's answer; its one lock is a
//! `try_lock` on the preview slot. The two threads exchange frames over one-slot channels, which allocate nothing per
//! frame, and neither allocates once its buffers have grown (MJPG excepted, see [`FrameConverter`]).

mod convert;
mod preview;
mod thread;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_anim_api::{FrameSource, TrackFrame};
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_track_api::camera::{Camera, CameraFormatRequest, CameraId, CameraInfo, CameraStream, PixelFormat};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::{MailboxReader, mailbox};
use omuzaki_track_api::tracker::{LatencyStats, QualityTier, Tracker, TrackerConfig};

pub use convert::FrameConverter;
pub use preview::{PREVIEW_MAX_WIDTH, PreviewFrame};

use crate::error::FaceTrackError;
use crate::frame::RgbFrame;
use crate::pipeline::FaceTracker;
use crate::sample::{FaceSample, StageTimings};
use preview::PreviewTap;
use thread::{JOIN_GRACE, Outcome, POLL, Run, STOP_SLACK, Stats, Worker};

/// What turns an RGB frame into a face sample. [`FaceTracker`] implements it; tests script it.
pub trait FrameTracker: Send {
    /// Tracks the face in `frame`, captured at `time` (strictly increasing). `None` means no face, which is not an
    /// error.
    ///
    /// # Errors
    /// Whatever [`FaceTracker::track`] returns. The camera thread publishes a lost frame and carries on.
    fn track(
        &mut self,
        frame: &RgbFrame<'_>,
        time: Seconds,
    ) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError>;

    /// Forgets the tracked face, as for a camera that restarts.
    fn reset(&mut self);
}

impl FrameTracker for FaceTracker {
    fn track(
        &mut self,
        frame: &RgbFrame<'_>,
        time: Seconds,
    ) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError> {
        FaceTracker::track(self, frame, time)
    }

    fn reset(&mut self) {
        FaceTracker::reset(self);
    }
}

/// How the webcam is opened.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct WebcamConfig {
    /// A [`CameraInfo`] id for [`WebcamTracker::default_device`]; `None` means the first camera.
    pub camera: Option<String>,
    /// The mode [`tracker_config`](WebcamTracker::tracker_config) asks of the camera, in pixels; `start` itself
    /// honours the [`TrackerConfig`] it is given. The camera gives the nearest mode it has.
    pub width: u32,
    /// See [`width`](Self::width).
    pub height: u32,
    /// Frames per second asked of the camera.
    pub fps: u32,
    /// Format preference, best first; the first one the camera opens with wins. Used when the [`TrackerConfig`]
    /// given to `start` names no format of its own.
    pub formats: Vec<PixelFormat>,
}

impl Default for WebcamConfig {
    /// 1280 x 720 at 60 fps (as `CameraFormatRequest::default()`: a camera that offers 30 and 60 gives 60), the first
    /// camera, and [Rgb8, Bgra8, Nv12, Yuy2, Mjpg] (`PixelFormat::convert_cost`'s order): the raw formats first, cheapest to convert first. MJPG is last,
    /// chosen only when the camera has no raw format for the requested mode, because decoding it allocates inside
    /// the JPEG library on every frame (`FrameConverter`).
    fn default() -> Self {
        Self {
            camera: None,
            width: 1280,
            height: 720,
            fps: 60,
            formats: vec![
                PixelFormat::Rgb8,
                PixelFormat::Bgra8,
                PixelFormat::Nv12,
                PixelFormat::Yuy2,
                PixelFormat::Mjpg,
            ],
        }
    }
}

impl WebcamConfig {
    /// Uses the camera with this id.
    #[must_use]
    pub fn with_camera(mut self, id: impl Into<String>) -> Self {
        self.camera = Some(id.into());
        self
    }

    /// Asks the camera for this mode.
    #[must_use]
    pub fn with_mode(mut self, width: u32, height: u32, fps: u32) -> Self {
        (self.width, self.height, self.fps) = (width, height, fps);
        self
    }

    /// The camera request this configuration describes, with no format named.
    pub const fn request(&self) -> CameraFormatRequest {
        CameraFormatRequest::new(self.width, self.height, self.fps)
    }

    /// Sets the format preference, best first.
    #[must_use]
    pub fn with_formats(mut self, formats: Vec<PixelFormat>) -> Self {
        self.formats = formats;
        self
    }
}

/// Frames the camera thread discarded or could not use, apart from [`LatencyStats::dropped`], which stays the count of
/// camera frames skipped for lack of time. Of the last session.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Failures {
    /// Frames dropped because their device time was before the last one accepted.
    pub backwards: u64,
    /// Frames the converter refused; each one went out as a lost frame.
    pub conversion: u64,
    /// Frames the [`FrameTracker`] failed on; each one went out as a lost frame.
    pub tracking: u64,
}

impl Failures {
    /// The three counts.
    pub const fn new(backwards: u64, conversion: u64, tracking: u64) -> Self {
        Self { backwards, conversion, tracking }
    }
}

/// The camera size a lowered tier asks for, in pixels (the design's lever for a weak machine: `track-api.md` §9).
const REDUCED_SIZE: (u32, u32) = (640, 480);

/// The camera request `tier` makes of `request`. `Full` asks for what was asked. `Balanced` and `Light` cap the size
/// at [`REDUCED_SIZE`], which cuts the conversion and the crop work for every frame; the rate and the format stay.
/// The design leaves a tier's settings to this tracker; this is the cheapest mapping that needs nothing from the
/// face tracker itself, whose own reduced settings (blendshapes on alternate frames) would live in `pipeline.rs`.
pub fn tier_camera(tier: QualityTier, request: &CameraFormatRequest) -> CameraFormatRequest {
    let (w, h) = REDUCED_SIZE;
    match tier {
        QualityTier::Full => *request,
        QualityTier::Balanced | QualityTier::Light => {
            let reduced = u64::from(request.width) * u64::from(request.height) > u64::from(w) * u64::from(h);
            let (width, height) = if reduced { (w, h) } else { (request.width, request.height) };
            CameraFormatRequest::new(width, height, request.fps).with_pixel(request.pixel)
        }
    }
}

/// How many camera frames go by per frame tracked at `tier`: every frame, then every frame, then every second one
/// (`Light` tracks at half the camera's rate; the frames between are not converted, tracked or published).
pub const fn tier_stride(tier: QualityTier) -> u32 {
    match tier {
        QualityTier::Full | QualityTier::Balanced => 1,
        QualityTier::Light => 2,
    }
}

/// One running session: its thread and the flags that end it.
struct Session {
    camera: JoinHandle<Option<Outcome>>,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

/// The source `start` returns: the mailbox, until the session is stopped, then `None` for ever.
struct Source {
    reader: MailboxReader,
    live: Arc<AtomicBool>,
}

impl FrameSource for Source {
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        if self.live.load(Ordering::Relaxed) { self.reader.poll(now) } else { None }
    }
}

/// The webcam [`Tracker`]. It owns its [`Camera`] and [`FrameTracker`]; `start` opens the stream and moves the tracker
/// onto the camera thread, `stop` joins the thread (within 100 ms) and takes the tracker back, so `stop` then `start`
/// works. Every frame the thread publishes is a [`TrackFrame`]: a face, or a lost frame when none is found, when a
/// frame cannot be converted or tracked, when the camera is silent for [`TrackerConfig::lost_after`] (once), or when
/// the stream fails.
///
/// Frames are timed on the `clock` given to [`new`](Self::new), the stage's own, so they line up with the rest of the
/// pipeline however far into a performance tracking starts. [`LatencyStats::dropped`] counts camera frames the thread
/// skipped (gaps in the camera's `seq`) and nothing else; [`failures`](Self::failures) counts the rest.
///
/// `start` honours [`TrackerConfig::camera`] (size, rate and, if it names one, the format; otherwise the
/// [`WebcamConfig`] preference is tried in order), [`TrackerConfig::lost_after`] and [`TrackerConfig::tier`], which
/// [`tier_camera`] and [`tier_stride`] map to concrete settings.
///
/// Two threads run: the camera thread, which captures, converts and publishes, and the tracker's own, which runs
/// `track`. The camera thread waits for each result a tick at a time, so when the tracker is slow, or stuck inside
/// `track`, it still publishes the one lost frame once nothing has gone out for [`TrackerConfig::lost_after`], and
/// still sees `stop`. It is the only thread that publishes. A lost frame that stands for no camera frame repeats the
/// last frame's `seq` (`track-api.md` §8). If the tracker does not return when `stop` asks, its thread is let go after
/// a short grace period and reported through [`error`](Self::error); the tracker is lost, as it is if its thread
/// panics, and every later `start` says so. A failure to open the camera or to start a thread, by contrast, hands the
/// tracker back, so `start` can be retried.
pub struct WebcamTracker {
    camera: Box<dyn Camera>,
    /// `None` while a session runs, or for good if the camera thread panicked.
    tracker: Option<Box<dyn FrameTracker>>,
    config: WebcamConfig,
    preview: Arc<PreviewTap>,
    clock: Arc<dyn Clock>,
    stats: Arc<Stats>,
    error: Arc<Mutex<Option<TrackError>>>,
    next_seq: u64,
    session: Option<Session>,
    /// Test seam: how many of the next thread spawns to fail.
    spawn_plan: std::collections::VecDeque<bool>,
    /// How long `stop` waits for the camera thread before detaching it; [`JOIN_GRACE`] unless a test says otherwise.
    join_grace: Duration,
}

impl WebcamTracker {
    /// A stopped tracker over `camera` and `tracker`, timing frames on the stage's `clock`. Nothing is opened until
    /// [`start`](Tracker::start).
    pub fn new(
        camera: Box<dyn Camera>,
        tracker: Box<dyn FrameTracker>,
        config: WebcamConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            camera,
            tracker: Some(tracker),
            config,
            clock,
            preview: Arc::new(PreviewTap::default()),
            stats: Arc::new(Stats::new()),
            error: Arc::default(),
            next_seq: 0,
            session: None,
            spawn_plan: std::collections::VecDeque::new(),
            join_grace: JOIN_GRACE,
        }
    }

    /// The camera [`WebcamConfig::camera`] names, or the first one; `None` if there is no such camera.
    pub fn default_device(&self) -> Option<CameraId> {
        let cameras = self.camera.enumerate();
        match &self.config.camera {
            Some(id) => cameras.into_iter().find(|c| c.id.0 == *id),
            None => cameras.into_iter().next(),
        }
        .map(|c| c.id)
    }

    /// A [`TrackerConfig`] asking for the mode in the [`WebcamConfig`], for `start`.
    pub fn tracker_config(&self) -> TrackerConfig {
        TrackerConfig::default().with_camera(self.config.request())
    }

    /// Sets how long `stop` waits for the camera thread to come back before it detaches it. The default is short
    /// enough for `stop`'s 100 ms budget; a test on a loaded machine may want it long, so that a slow thread is not
    /// mistaken for a stuck one.
    #[doc(hidden)]
    pub fn set_join_grace(&mut self, grace: Duration) {
        self.join_grace = grace;
    }

    /// Plans the next thread spawns: one entry per spawn, `true` to fail it as if the OS refused. A start makes two (the
    /// tracker's thread, then the camera thread). To test that `start` hands everything back and can be retried.
    #[doc(hidden)]
    pub fn fail_spawns(&mut self, plan: impl IntoIterator<Item = bool>) {
        self.spawn_plan = plan.into_iter().collect();
    }

    /// Frames discarded or lost to a failure in the current (or last) session.
    pub fn failures(&self) -> Failures {
        self.stats.failures()
    }

    /// Turns the preview tap on or off (off by default; costs nothing while off).
    pub fn set_preview(&self, on: bool) {
        self.preview.set(on);
    }

    /// Copies the newest preview into `out`; false if none is new since the last call, or if the camera thread is
    /// swapping a frame in right now (the next call gets it). Never waits, so it is safe on a render loop.
    pub fn preview(&self, out: &mut PreviewFrame) -> bool {
        self.preview.take(out)
    }

    /// Why the last session's camera thread ended by itself, if it did (the stream error it stopped on), or why it
    /// was lost (a panic, or a tracker that did not return when `stop` asked, so that its thread was let go).
    pub fn error(&self) -> Option<TrackError> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Starts a named thread, unless a test has asked for the next spawn to fail.
    fn spawn<T: Send + 'static>(
        &mut self,
        name: &str,
        body: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<JoinHandle<T>> {
        if self.spawn_plan.pop_front() == Some(true) {
            drop(body);
            return Err(std::io::Error::other("injected spawn failure"));
        }
        std::thread::Builder::new().name(name.to_owned()).spawn(body)
    }

    /// Opens `device` as `request` asks: the format it names, or else the first of the preference the camera accepts.
    fn open(&mut self, device: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        let preference: Vec<PixelFormat> = request.pixel.map_or_else(|| self.config.formats.clone(), |p| vec![p]);
        let mut last = TrackError::FormatUnsupported(*request);
        for pixel in preference {
            match self.camera.open(device, &request.with_pixel(Some(pixel))) {
                Ok(stream) => return Ok(stream),
                Err(e @ TrackError::FormatUnsupported(_)) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

impl Tracker for WebcamTracker {
    fn devices(&self) -> Vec<CameraInfo> {
        self.camera.enumerate()
    }

    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        self.stop();
        let mut tracker = self.tracker.take().ok_or_else(|| {
            TrackError::Backend(
                "the face tracker was lost: its thread panicked, or did not return when stopped".to_owned(),
            )
        })?;
        let stream = match self.open(device, &tier_camera(config.tier, &config.camera)) {
            Ok(stream) => stream,
            Err(e) => {
                self.tracker = Some(tracker);
                return Err(e);
            }
        };
        tracker.reset();
        let (writer, reader) = mailbox();
        let (stop, live) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        // The new session's numbers are installed only once both threads are running, so a start that fails leaves the
        // last session's counters and error where they were.
        let (stats, error) = (Arc::new(Stats::new()), Arc::<Mutex<Option<TrackError>>>::default());
        // Inference gets a thread of its own, so that the camera thread, which publishes, keeps time while the tracker
        // works, and can say so if it takes longer than `lost_after` or never returns.
        let worker = match Worker::start(tracker, |body| self.spawn("omuzaki-webcam-track", body)) {
            Ok(worker) => worker,
            Err((e, tracker)) => {
                // No thread ran: the tracker goes back where a camera-open failure puts it, and the stream is
                // dropped, which releases the camera.
                self.tracker = Some(tracker);
                return Err(TrackError::Backend(format!("tracker thread: {e}")));
            }
        };
        // The camera thread is handed its `Run` through a cell, so that if it cannot be started, the closure's drop
        // does not take the worker (and the tracker in it) or the camera stream with it.
        let cell = Arc::new(Mutex::new(Some(Run {
            stream,
            writer,
            clock: Arc::clone(&self.clock),
            first_seq: self.next_seq,
            lost_after: config.lost_after,
            stride: tier_stride(config.tier),
            grace: self.join_grace,
            stop: Arc::clone(&stop),
            preview: Arc::clone(&self.preview),
            stats: Arc::clone(&stats),
            error: Arc::clone(&error),
            worker,
        })));
        let body = {
            let cell = Arc::clone(&cell);
            move || cell.lock().unwrap_or_else(PoisonError::into_inner).take().map(Run::run)
        };
        let camera = match self.spawn("omuzaki-webcam", body) {
            Ok(handle) => handle,
            Err(e) => {
                let run = cell.lock().unwrap_or_else(PoisonError::into_inner).take();
                // No job was given to the tracker's thread, so wait for it, however long: the tracker must come back.
                self.tracker = run.and_then(|run| run.worker.recover());
                return Err(TrackError::Backend(format!("camera thread: {e}")));
            }
        };
        (self.stats, self.error) = (stats, error);
        self.session = Some(Session { camera, stop, live: Arc::clone(&live) });
        Ok(Box::new(Source { reader, live }))
    }

    fn stop(&mut self) {
        let Some(Session { camera, stop, live }) = self.session.take() else { return };
        stop.store(true, Ordering::Relaxed);
        live.store(false, Ordering::Relaxed);
        // The camera thread notices `stop` within a poll and then gives the tracker's thread `join_grace`: with the
        // slack, that is the whole of the stop budget when `join_grace` is the default.
        let give_up = Instant::now() + self.join_grace + POLL + STOP_SLACK;
        while !camera.is_finished() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(1));
        }
        let failure = if camera.is_finished() {
            match camera.join() {
                Ok(Some(Outcome { tracker, next_seq })) => {
                    self.next_seq = next_seq;
                    self.tracker = tracker;
                    self.tracker.is_none().then(|| {
                        format!(
                            "the face tracker did not return within {:?} of stop; its thread was let go",
                            self.join_grace
                        )
                    })
                }
                // `None` cannot happen: the cell is only emptied by the thread that runs. Treated like a panic.
                Ok(None) | Err(_) => Some("the camera thread panicked".to_owned()),
            }
        } else {
            // Dropping the handle lets the thread go; it ends when the camera lets it, because `stop` is set.
            drop(camera);
            Some("the camera thread did not stop in time and was let go".to_owned())
        };
        if let Some(why) = failure {
            *self.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(TrackError::Backend(why));
        }
    }

    fn latency(&self) -> LatencyStats {
        self.stats.snapshot()
    }
}

impl Drop for WebcamTracker {
    fn drop(&mut self) {
        self.stop();
    }
}
