//! [`LiveApp`]: the live slice as the app starts it. The perform thread, the op runner with the performer in its
//! services, and the output window, started together and stopped in the quit order of [`super`].

use std::sync::Arc;

use egui::Context;
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_core::glam::UVec2;
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::{Actor, Runner, ServicesBuilder};
use omuzaki_render::WgpuRenderer;
use omuzaki_stage::frame::Stage;
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_stage::perform::input::{InputHost, NoSource};
use omuzaki_stream_api::window::WindowHost;
use omuzaki_track_api::camera::{CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

use super::clock::{ClockObserver, PerformClock};
use super::mailbox::FrameReader;
use super::rig::{LiveRig, live_performer};
use super::thread::{LoopReport, WiringError};
use crate::aux_window::{PhysicalSize, Transparency};
use crate::live::hotkeys::Hotkeys;
use crate::live::viewport::{LiveViewport, Presenter, SharedTracker, ViewportError};
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
    clock: Option<ClockObserver>,
    tracker: SharedTracker,
    viewport: Option<LiveViewport>,
    hotkeys: Hotkeys,
}

impl LiveApp {
    /// Starts the perform thread and the life layer, timed by `clock`, and the tracking input over `tracker` (idle until
    /// the `input.*` ops bind a camera and start the mirror); gives the triggers the starter expressions until a
    /// character brings its own; and builds the runner: every op linked into the app, over an empty document, with the
    /// performer, the input and the output window's host in its services.
    /// The output window starts closed, on a platform where a transparent surface is `transparency`. `base` is the pose
    /// the performer is built on: the character's bind pose ([`LiveScene::base_pose`](super::LiveScene::base_pose)),
    /// or [`HumanoidPose::REST`] with no stage.
    ///
    /// # Errors
    /// [`StartError`]; nothing is left running then.
    pub fn start(
        tracker: Box<dyn Tracker>,
        clock: Box<dyn PerformClock>,
        transparency: Transparency,
        base: HumanoidPose,
    ) -> Result<Self, StartError> {
        let performer = live_performer(Box::new(NoSource), ChannelBuf::NEUTRAL, base);
        // The viewport reads the tracker's latency while the input owns the tracker: two handles to one.
        let tracker = SharedTracker::new(tracker);
        let observer = clock.observer();
        let (rig, reader) = LiveRig::start(Box::new(tracker.clone()), TrackerConfig::default(), performer, clock)
            .map_err(StartError::Wiring)?;
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
            clock: observer,
            tracker,
            viewport: None,
            hotkeys: Hotkeys::new(),
        })
    }

    /// Starts drawing the stage: `stage` (the kit's, from [`kit_stage`](crate::live::viewport::kit_stage), made on
    /// `renderer` at `size`) takes the perform thread's frames from now on, and `presenter` shows each picture to egui.
    ///
    /// # Errors
    /// [`ViewportError::Stopped`] after [`shutdown`](Self::shutdown).
    pub fn attach_viewport(
        &mut self,
        mut renderer: WgpuRenderer,
        stage: Stage,
        size: UVec2,
        presenter: Box<dyn Presenter>,
    ) -> Result<(), ViewportError> {
        let Some(reader) = self.reader.take() else {
            if let Err(error) = stage.release(&mut renderer) {
                tracing::warn!(%error, "releasing a stage nothing could draw failed");
            }
            return Err(ViewportError::Stopped);
        };
        let view =
            LiveViewport::new(renderer, stage, size, reader, presenter, self.clock.clone(), Some(self.tracker.clone()));
        self.viewport = Some(view);
        Ok(())
    }

    /// The stage view, once [`attach_viewport`](Self::attach_viewport) has run.
    pub fn viewport_mut(&mut self) -> Option<&mut LiveViewport> {
        self.viewport.as_mut()
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

    /// Draws this frame: the stage first, if one is attached (its picture is published for the stage panel, so call this
    /// before the shell draws), then the output window when it is open, showing that same picture or its backdrop alone
    /// until there is one. Requests the ops queued on the host are carried out first, and what the window is is
    /// published after, for `output.window.status`. Call once per frame from the main viewport.
    pub fn show(&mut self, ctx: &Context) {
        // An open output window shows the program over its own backdrop (the key, or the desktop), so while one is open
        // the program carries the character alone, with alpha. Requests the ops queued are carried out first so the
        // frame drawn now already follows them; a context that can't open an OS window keeps the window closed.
        self.host.take_requests(&mut self.output);
        let clean = self.output.status().open && !ctx.embed_viewports();
        let program = self.viewport.as_mut().and_then(|view| {
            view.set_clean(clean);
            match view.frame(ctx, ctx.input(|i| i.unstable_dt)) {
                Ok(program) => program,
                Err(error) => {
                    tracing::warn!(%error, "the stage did not draw this frame");
                    None
                }
            }
        });
        if self.viewport.is_some() {
            // The perform thread steps at 60 Hz whether or not anything is moving on screen.
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
        self.show_program(ctx, program);
    }

    /// Sends this frame's key presses and releases to the triggers they are bound to (`trigger.press`). Call it once per
    /// frame, after the shell has drawn: the Live panel's key capture and a focused text field take their keys from the
    /// input first, so a key that binds or types never also fires.
    pub fn keys(&mut self, ctx: &Context) {
        self.hotkeys.dispatch(ctx, &*self.runner, &Actor::Person { name: "you".to_owned() });
    }

    /// [`show`](Self::show) with the program frame given: for a caller that renders elsewhere (the output window's
    /// tests).
    pub fn show_program(&mut self, ctx: &Context, program: Option<Program>) {
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
        if let Some(view) = self.viewport.take()
            && let Err(error) = view.release()
        {
            tracing::warn!(%error, "the stage did not release cleanly");
        }
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
