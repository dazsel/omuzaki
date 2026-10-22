//! [`LiveRig`]: the perform thread and the tracking input, started and stopped together in the documented order.

use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_ai_api::ModelRegistry;
use omuzaki_anim::BusAnimator;
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{ChannelBuf, DeviceProfile, FrameSource, HumanoidPose, LifeConfig};
use omuzaki_ops_api::Mode;
use omuzaki_secrets_api::prelude::SecretStore;
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{InputHost as _, LiveInput, NoPrompt};
use omuzaki_stage::perform::{Performer, PerformerParts};
use omuzaki_track_api::pose_out::PoseOutput;
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};

use super::clock::PerformClock;
use super::mailbox::FrameReader;
use super::thread::{FrameTap, LoopReport, PerformThread, WiringError};

/// The life layer's seed: fixed, so a session's idle motion is the same on every run (spec §4, deterministic).
pub const LIFE_SEED: u64 = 7;

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

/// The inputs beside the webcam's tracker, when the App has them ([`studio_inputs`](super::inputs::studio_inputs) builds them).
#[derive(Default)]
pub struct Extras {
    /// The phone link: the iPhone source's tracker.
    pub phone: Option<Box<dyn Tracker>>,
    /// VMC out.
    pub pose_out: Option<Box<dyn PoseOutput>>,
    /// The model registry, for the `model.*` ops (the app puts it in the runner's services; the rig ignores it).
    pub models: Option<Arc<dyn ModelRegistry>>,
    /// The OS keychain, for the connectors' passwords and tokens (the app puts it in the runner's services; the rig
    /// ignores it).
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// The app's data folder, where the shows keep what this machine approved and their variables (the app uses it;
    /// the rig ignores it).
    pub data: Option<PathBuf>,
    /// Handed every frame the perform thread publishes (the `VTube Studio` bridge's feed).
    pub tap: Option<FrameTap>,
}

impl std::fmt::Debug for Extras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extras")
            .field("phone", &self.phone.is_some())
            .field("pose_out", &self.pose_out.is_some())
            .field("models", &self.models.is_some())
            .field("secrets", &self.secrets.is_some())
            .field("data", &self.data)
            .field("tap", &self.tap.is_some())
            .finish()
    }
}

/// The perform thread and the tracking input (the stage's [`LiveInput`], which owns the tracker). Dropping it runs
/// [`shutdown`](Self::shutdown)'s order and logs a failure.
pub struct LiveRig {
    perform: Option<PerformThread>,
    input: Arc<LiveInput>,
}

impl LiveRig {
    /// Starts the perform thread on `performer` (built over the stage's `NoSource`), timed by `clock`, and the input over
    /// `tracker` with `config`, idle: no camera bound, the mirror off, the life layer driving the face. The input posts
    /// each source it starts into the thread's slot. Returns the rig and the reader the render thread draws from.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the thread can't start.
    pub fn start(
        tracker: Box<dyn Tracker>,
        config: TrackerConfig,
        performer: Performer,
        clock: Box<dyn PerformClock>,
    ) -> Result<(Self, FrameReader), WiringError> {
        Self::start_with(tracker, Extras::default(), config, performer, clock)
    }

    /// [`start`](Self::start), with the phone link and the pose output in `extras` handed to the input as well.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the thread can't start.
    pub fn start_with(
        tracker: Box<dyn Tracker>,
        extras: Extras,
        config: TrackerConfig,
        performer: Performer,
        clock: Box<dyn PerformClock>,
    ) -> Result<(Self, FrameReader), WiringError> {
        let (perform, reader) = PerformThread::spawn_tapped(performer, clock, extras.tap)?;
        // Desktop Windows and Linux have no camera prompt; a platform that has one brings its own permission.
        let mut input = LiveInput::new(tracker, Box::new(NoPrompt), config, perform.sources(), perform.host());
        if let Some(phone) = extras.phone {
            input = input.with_phone(phone);
        }
        if let Some(pose_out) = extras.pose_out {
            input = input.with_pose_out(pose_out);
        }
        Ok((Self { perform: Some(perform), input: Arc::new(input) }, reader))
    }

    /// The tracking input as the `input.*` ops see it, for the runner's services: the camera list, the binding, the
    /// mirror and the permission all go through it.
    pub fn input(&self) -> Arc<LiveInput> {
        Arc::clone(&self.input)
    }

    /// The performer as the ops see it, for the runner's services.
    ///
    /// # Errors
    /// [`WiringError::Stopped`] after shutdown.
    pub fn host(&self) -> Result<Arc<dyn PerformHost>, WiringError> {
        self.perform.as_ref().map(PerformThread::host).ok_or(WiringError::Stopped)
    }

    /// The perform loop's counts so far.
    pub fn report(&self) -> LoopReport {
        self.perform.as_ref().map(PerformThread::report).unwrap_or_default()
    }

    /// The quit order (`live::wiring`): the mirror stops, so the tracker releases the camera, then the perform thread
    /// stops and is joined. The render side has already released its stage; the ops' handles answer `Disconnected`
    /// afterwards.
    ///
    /// # Errors
    /// [`WiringError::Panicked`] if the perform thread had panicked.
    pub fn shutdown(mut self) -> Result<LoopReport, WiringError> {
        self.stop_all()
    }

    fn stop_all(&mut self) -> Result<LoopReport, WiringError> {
        // Stopping the mirror never fails (`InputHost::preview`); were that to change, the thread still stops.
        if let Err(error) = self.input.preview(false, Mode::Apply) {
            tracing::warn!(%error, "the camera did not stop");
        }
        match self.perform.take() {
            Some(perform) => perform.stop(),
            None => Ok(LoopReport::default()),
        }
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
        f.debug_struct("LiveRig").field("report", &self.report()).finish_non_exhaustive()
    }
}
