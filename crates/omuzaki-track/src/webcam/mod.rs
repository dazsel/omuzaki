//! The webcam [`Tracker`]: one camera thread runs camera frame, pixel conversion, face tracker and `TrackFrame`
//! mailbox (spec v3 §22 PF-01: off the main thread, newest frame wins), with an optional preview tap for the UI.
//!
//! | Module | What it does |
//! |---|---|
//! | `convert` | [`FrameConverter`]: any [`PixelFormat`] to packed RGB8 in a reused buffer |
//! | `preview` | [`PreviewFrame`] and the tap that fills it, only while it is on |
//! | `thread` | the camera thread's loop, the stage-clock time and the latency counters |
//!
//! The camera thread never blocks except inside [`CameraStream::next_frame`], never locks except a `try_lock` on the
//! preview slot, and allocates nothing per frame once its buffers have grown (MJPG excepted, see [`FrameConverter`]).

mod convert;
mod preview;
mod thread;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use omuzaki_anim_api::{FrameSource, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::camera::{Camera, CameraFormatRequest, CameraId, CameraInfo, PixelFormat};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::{MailboxReader, mailbox};
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

pub use convert::FrameConverter;
pub use preview::{PREVIEW_MAX_WIDTH, PreviewFrame};

use crate::error::FaceTrackError;
use crate::frame::RgbFrame;
use crate::pipeline::FaceTracker;
use crate::sample::{FaceSample, StageTimings};
use preview::PreviewTap;
use thread::{Outcome, Run, Stats, stage_now};

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
    /// The mode asked of the camera, in pixels. The camera gives the nearest it has.
    pub width: u32,
    /// See [`width`](Self::width).
    pub height: u32,
    /// Frames per second asked of the camera.
    pub fps: u32,
    /// Format preference, best first; the first one the camera opens with wins.
    pub formats: Vec<PixelFormat>,
}

impl Default for WebcamConfig {
    /// 1280 x 720 at 30 fps, the first camera, and [Nv12, Yuy2, Mjpg, Bgra8, Rgb8] (the cheapest to convert first
    /// that webcams commonly offer).
    fn default() -> Self {
        Self {
            camera: None,
            width: 1280,
            height: 720,
            fps: 30,
            formats: vec![
                PixelFormat::Nv12,
                PixelFormat::Yuy2,
                PixelFormat::Mjpg,
                PixelFormat::Bgra8,
                PixelFormat::Rgb8,
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

    /// Sets the format preference, best first.
    #[must_use]
    pub fn with_formats(mut self, formats: Vec<PixelFormat>) -> Self {
        self.formats = formats;
        self
    }
}

/// One running session: its thread and the flags that end it.
struct Session {
    handle: JoinHandle<Outcome>,
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
/// onto the camera thread, `stop` joins the thread (within 200 ms) and takes the tracker back, so `stop` then `start`
/// works. Every frame the thread publishes is a [`TrackFrame`]: a face, or a lost frame when none is found, when a
/// frame cannot be converted or tracked, when the camera is silent for [`TrackerConfig::lost_after`] (once), or when
/// the stream fails.
///
/// [`LatencyStats::dropped`] counts camera frames the thread skipped (gaps in the camera's `seq`) plus frames it
/// could not convert or track. [`TrackerConfig::camera`] is not used: the request comes from [`WebcamConfig`],
/// which also carries the format preference.
pub struct WebcamTracker {
    camera: Box<dyn Camera>,
    /// `None` while a session runs, or for good if the camera thread panicked.
    tracker: Option<Box<dyn FrameTracker>>,
    config: WebcamConfig,
    preview: Arc<PreviewTap>,
    stats: Arc<Stats>,
    error: Arc<Mutex<Option<TrackError>>>,
    next_seq: u64,
    session: Option<Session>,
}

impl WebcamTracker {
    /// A stopped tracker over `camera` and `tracker`. Nothing is opened until [`start`](Tracker::start).
    pub fn new(camera: Box<dyn Camera>, tracker: Box<dyn FrameTracker>, config: WebcamConfig) -> Self {
        Self {
            camera,
            tracker: Some(tracker),
            config,
            preview: Arc::new(PreviewTap::default()),
            stats: Arc::new(Stats::new()),
            error: Arc::default(),
            next_seq: 0,
            session: None,
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

    /// Turns the preview tap on or off (off by default; costs nothing while off).
    pub fn set_preview(&self, on: bool) {
        self.preview.set(on);
    }

    /// Copies the newest preview into `out`; false if none is new since the last call. May wait for the camera thread
    /// to finish swapping a frame in (a pointer swap), never for a downscale.
    pub fn preview(&self, out: &mut PreviewFrame) -> bool {
        self.preview.take(out)
    }

    /// Why the last session's camera thread ended by itself, if it did: the stream error it stopped on.
    pub fn error(&self) -> Option<TrackError> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Opens `device` with the first format of the preference the camera accepts.
    fn open(&mut self, device: &CameraId) -> Result<Box<dyn omuzaki_track_api::camera::CameraStream>, TrackError> {
        let c = &self.config;
        let mut last = TrackError::FormatUnsupported(CameraFormatRequest::new(c.width, c.height, c.fps));
        for &pixel in &c.formats {
            let request = CameraFormatRequest::new(c.width, c.height, c.fps).with_pixel(Some(pixel));
            match self.camera.open(device, &request) {
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
            TrackError::Backend("the face tracker was lost when the camera thread panicked".to_owned())
        })?;
        let stream = match self.open(device) {
            Ok(stream) => stream,
            Err(e) => {
                self.tracker = Some(tracker);
                return Err(e);
            }
        };
        tracker.reset();
        stage_now(); // starts the clock here, not on the camera thread
        let (writer, reader) = mailbox();
        let (stop, live) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        self.stats = Arc::new(Stats::new());
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let run = Run {
            stream,
            tracker,
            writer,
            first_seq: self.next_seq,
            lost_after: config.lost_after,
            stop: Arc::clone(&stop),
            preview: Arc::clone(&self.preview),
            stats: Arc::clone(&self.stats),
            error: Arc::clone(&self.error),
        };
        // If the spawn fails the tracker goes with the closure; a later `start` says so.
        let handle = std::thread::Builder::new()
            .name("omuzaki-webcam".to_owned())
            .spawn(move || run.run())
            .map_err(|e| TrackError::Backend(format!("camera thread: {e}")))?;
        self.session = Some(Session { handle, stop, live: Arc::clone(&live) });
        Ok(Box::new(Source { reader, live }))
    }

    fn stop(&mut self) {
        let Some(Session { handle, stop, live }) = self.session.take() else { return };
        stop.store(true, Ordering::Relaxed);
        live.store(false, Ordering::Relaxed);
        match handle.join() {
            Ok(Outcome { tracker, next_seq }) => {
                self.tracker = Some(tracker);
                self.next_seq = next_seq;
            }
            Err(_) => {
                *self.error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(TrackError::Backend("the camera thread panicked".to_owned()));
            }
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
