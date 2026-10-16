//! [`LiveApp`]: the live slice as the app starts it. The perform thread, the op runner with the performer in its
//! services, and the output window, started together and stopped in the quit order of [`super`].

use std::sync::Arc;

use egui::Context;
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::{Runner, ServicesBuilder};
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_stage::perform::input::{InputHost, NoSource};
use omuzaki_stream_api::window::WindowHost;
use omuzaki_track_api::camera::{CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

use super::clock::PerformClock;
use super::mailbox::FrameReader;
use super::rig::{LiveRig, live_performer};
use super::thread::{LoopReport, WiringError};
use crate::aux_window::{PhysicalSize, Transparency};
use crate::output_window::{OutputHost, OutputPlan, OutputWindow, Program};
use crate::wiring::{self, ExecutorError};

/// Why the live slice did not start.
#[derive(Debug)]
#[non_exhaustive]
pub enum StartError {
    /// The perform thread could not start.
    Wiring(WiringError),
    /// The performer refused the starter expressions.
    Perform(PerformError),
    /// The op registry is broken (two crates register one name): a bug in whichever crate registered them.
    Ops(ExecutorError),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wiring(error) => error.fmt(f),
            Self::Perform(error) => write!(f, "the performer refused the starter expressions: {error}"),
            Self::Ops(error) => write!(f, "the ops could not start: {error}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wiring(error) => Some(error),
            Self::Perform(error) => Some(error),
            Self::Ops(error) => Some(error),
        }
    }
}

/// The live slice of the running app.
pub struct LiveApp {
    rig: Option<LiveRig>,
    reader: Option<FrameReader>,
    runner: Arc<dyn Runner>,
    output: OutputWindow,
    host: OutputHost,
}

impl LiveApp {
    /// Starts the perform thread and the life layer, timed by `clock`, and the tracking input over `tracker` (idle until
    /// the `input.*` ops bind a camera and start the mirror); gives the triggers the starter expressions until a
    /// character brings its own; and builds the runner: every op linked into the app, over an empty document, with the
    /// performer, the input and the output window's host in its services.
    /// The output window starts closed, on a platform where a transparent surface is `transparency`.
    ///
    /// # Errors
    /// [`StartError`]; nothing is left running then.
    pub fn start(
        tracker: Box<dyn Tracker>,
        clock: Box<dyn PerformClock>,
        transparency: Transparency,
    ) -> Result<Self, StartError> {
        let performer = live_performer(Box::new(NoSource), ChannelBuf::NEUTRAL, HumanoidPose::REST);
        let (rig, reader) =
            LiveRig::start(tracker, TrackerConfig::default(), performer, clock).map_err(StartError::Wiring)?;
        let host = rig.host().map_err(StartError::Wiring)?;
        host.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).map_err(StartError::Perform)?;
        let window = OutputHost::new(transparency, PhysicalSize::FULL_HD);
        let input: Arc<dyn InputHost> = rig.input();
        let services = ServicesBuilder::new()
            .insert::<dyn PerformHost>(host)
            .insert::<dyn InputHost>(input)
            .insert::<dyn WindowHost>(Arc::new(window.clone()))
            .build();
        let runner = wiring::runner(services).map_err(StartError::Ops)?;
        Ok(Self {
            rig: Some(rig),
            reader: Some(reader),
            runner,
            output: OutputWindow::new(OutputPlan::default(), transparency),
            host: window,
        })
    }

    /// The runner the shell and the panels call.
    pub fn runner(&self) -> Arc<dyn Runner> {
        Arc::clone(&self.runner)
    }

    /// The rig: the input and the loop's counts. `None` after [`shutdown`](Self::shutdown).
    pub fn rig_mut(&mut self) -> Option<&mut LiveRig> {
        self.rig.as_mut()
    }

    /// The newest frames from the perform thread, for the render side. `None` after [`shutdown`](Self::shutdown).
    pub fn reader_mut(&mut self) -> Option<&mut FrameReader> {
        self.reader.as_mut()
    }

    /// The output window, which the `output.window.*` ops open and close through the runner.
    pub fn output_mut(&mut self) -> &mut OutputWindow {
        &mut self.output
    }

    /// Draws this frame's output window (when it is open): `program`, the viewport's rendered frame, or the backdrop
    /// alone until the viewport renders one. Requests the ops queued on the host are carried out first, and what the
    /// window is is published after, for `output.window.status`. Call once per frame from the main viewport.
    pub fn show(&mut self, ctx: &Context, program: Option<Program>) {
        self.host.take_requests(&mut self.output);
        self.output.show(ctx, program);
        self.host.publish(&self.output, f64::from(ctx.pixels_per_point()));
    }

    /// The quit order (`live::wiring`): the output window closes and the render side lets go of its frames, then the
    /// rig stops the tracker and joins the perform thread. The runner keeps its handle, which answers `Disconnected`
    /// from then on, until the app drops it. Calling it again does nothing.
    ///
    /// # Errors
    /// [`WiringError::Panicked`] if the perform thread had panicked.
    pub fn shutdown(&mut self) -> Result<LoopReport, WiringError> {
        self.output.set_open(false);
        self.host.publish(&self.output, WindowHost::surface(&self.host).scale_factor);
        self.reader = None;
        match self.rig.take() {
            Some(rig) => rig.shutdown(),
            None => Ok(LoopReport::default()),
        }
    }
}

impl Drop for LiveApp {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!(%error, "the live slice stopped badly");
        }
    }
}

impl std::fmt::Debug for LiveApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveApp").field("running", &self.rig.is_some()).finish_non_exhaustive()
    }
}

/// A tracker with no cameras: what the app performs with until the webcam tracker (#378) is linked in. The life layer
/// keeps the character moving, and picking a camera says there is none.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTracker;

impl Tracker for NoTracker {
    fn devices(&self) -> Vec<CameraInfo> {
        Vec::new()
    }

    fn start(
        &mut self,
        device: &CameraId,
        _config: &TrackerConfig,
    ) -> Result<Box<dyn omuzaki_anim_api::FrameSource>, TrackError> {
        Err(TrackError::NoDevice(device.clone()))
    }

    fn stop(&mut self) {}

    fn latency(&self) -> LatencyStats {
        LatencyStats::default()
    }
}
