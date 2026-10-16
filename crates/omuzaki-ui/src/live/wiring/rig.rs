//! [`LiveRig`]: the tracker and the perform thread, started and stopped together in the documented order.

use std::sync::Arc;

use omuzaki_anim::BusAnimator;
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{ChannelBuf, DeviceProfile, FrameSource, HumanoidPose, LifeConfig};
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::{Performer, PerformerParts};
use omuzaki_track_api::camera::{CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

use super::clock::PerformClock;
use super::mailbox::FrameReader;
use super::thread::{LoopReport, PerformThread, WiringError};

/// The life layer's seed: fixed, so a session's idle motion is the same on every run (spec §4, deterministic).
pub const LIFE_SEED: u64 = 7;

/// A frame source that never has a frame: what the performer reads before a camera starts and after one fails, so the
/// life layer keeps the character moving.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCamera;

impl FrameSource for NoCamera {
    fn poll(&mut self, _now: omuzaki_core::time::Seconds) -> Option<omuzaki_anim_api::TrackFrame> {
        None
    }
}

/// The app's performer: the webcam retarget, the life layer and the bus animator over `source`, showing `rest` on
/// `base` (the character's bind pose, [`LiveScene::base_pose`](super::LiveScene::base_pose)) until tracked.
pub fn live_performer(source: Box<dyn FrameSource>, rest: ChannelBuf, base: HumanoidPose) -> Performer {
    let parts = PerformerParts {
        source,
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(LIFE_SEED))),
        animator: Box::new(BusAnimator::default()),
    };
    Performer::new(parts, rest, base)
}

/// The tracker and the perform thread. Dropping it runs [`shutdown`](Self::shutdown)'s order and logs a failure.
pub struct LiveRig {
    tracker: Box<dyn Tracker>,
    config: TrackerConfig,
    perform: Option<PerformThread>,
    camera: Option<CameraId>,
}

impl LiveRig {
    /// Starts the perform thread on `performer` (built over [`NoCamera`]), timed by `clock`, with `tracker` idle.
    /// Returns the rig and the reader the render thread draws from.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the thread can't start.
    pub fn start(
        tracker: Box<dyn Tracker>,
        config: TrackerConfig,
        performer: Performer,
        clock: Box<dyn PerformClock>,
    ) -> Result<(Self, FrameReader), WiringError> {
        let (perform, reader) = PerformThread::spawn(performer, clock)?;
        Ok((Self { tracker, config, perform: Some(perform), camera: None }, reader))
    }

    /// The cameras the tracker can use. Cold.
    pub fn cameras(&self) -> Vec<CameraInfo> {
        self.tracker.devices()
    }

    /// The camera being tracked, if one is.
    pub fn camera(&self) -> Option<&CameraId> {
        self.camera.as_ref()
    }

    /// Starts tracking from `camera` and hands its frames to the performer; a camera already running is stopped first.
    /// Also the Retry after an unplug: starting the same camera again gives the performer a fresh source.
    ///
    /// # Errors
    /// [`LiveError::Track`] if the camera won't open (the performer stays on the life layer), or
    /// [`LiveError::Wiring`] if the perform thread can't take the source.
    pub fn use_camera(&mut self, camera: &CameraId) -> Result<(), LiveError> {
        self.camera = None;
        let source = self.tracker.start(camera, &self.config).map_err(LiveError::Track)?;
        self.thread()?.set_source(source).map_err(LiveError::Wiring)?;
        self.camera = Some(camera.clone());
        Ok(())
    }

    /// Stops the camera; the performer hands the face to the life layer.
    ///
    /// # Errors
    /// [`LiveError::Wiring`] if the perform thread is gone.
    pub fn stop_camera(&mut self) -> Result<(), LiveError> {
        self.tracker.stop();
        self.camera = None;
        self.thread()?.set_source(Box::new(NoCamera)).map_err(LiveError::Wiring)
    }

    /// The performer as the ops see it, for the runner's services.
    ///
    /// # Errors
    /// [`LiveError::Wiring`] with [`WiringError::Stopped`] after shutdown.
    pub fn host(&self) -> Result<Arc<dyn PerformHost>, LiveError> {
        self.perform.as_ref().map(PerformThread::host).ok_or(LiveError::Wiring(WiringError::Stopped))
    }

    /// Tracker latency (for the health meter).
    pub fn latency(&self) -> LatencyStats {
        self.tracker.latency()
    }

    /// The perform loop's counts so far.
    pub fn report(&self) -> LoopReport {
        self.perform.as_ref().map(PerformThread::report).unwrap_or_default()
    }

    /// The quit order (`live::wiring`): the tracker stops and releases the camera, then the perform thread stops and
    /// is joined. The render side has already released its stage; the ops' handles answer `Disconnected` afterwards.
    ///
    /// # Errors
    /// [`WiringError::Panicked`] if the perform thread had panicked.
    pub fn shutdown(mut self) -> Result<LoopReport, WiringError> {
        self.stop_all()
    }

    fn stop_all(&mut self) -> Result<LoopReport, WiringError> {
        self.tracker.stop();
        self.camera = None;
        match self.perform.take() {
            Some(perform) => perform.stop(),
            None => Ok(LoopReport::default()),
        }
    }

    fn thread(&self) -> Result<&PerformThread, LiveError> {
        self.perform.as_ref().ok_or(LiveError::Wiring(WiringError::Stopped))
    }
}

impl Drop for LiveRig {
    fn drop(&mut self) {
        if let Err(error) = self.stop_all() {
            tracing::warn!(%error, "the live rig did not shut down cleanly");
        }
    }
}

impl std::fmt::Debug for LiveRig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveRig").field("camera", &self.camera).field("report", &self.report()).finish_non_exhaustive()
    }
}

/// Why the rig refused a request.
#[derive(Debug)]
#[non_exhaustive]
pub enum LiveError {
    /// The tracker refused the camera.
    Track(TrackError),
    /// The perform thread refused the request.
    Wiring(WiringError),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Track(error) => write!(f, "the camera did not start: {error}"),
            Self::Wiring(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Track(error) => Some(error),
            Self::Wiring(error) => Some(error),
        }
    }
}
