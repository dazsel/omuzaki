//! [`LiveApp`]: the live slice as the app starts it. The perform thread, the op runner with the performer in its
//! services, and the output window, started together and stopped in the quit order of [`super`].

use std::path::Path;
use std::sync::Arc;

use egui::Context;
use omuzaki_ai_api::ModelRegistry;
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_engine::{CpalBackend, LiveEngine};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_face_api::ExpressionSet;
use omuzaki_http::ConnectHttp;
use omuzaki_live::Automation;
use omuzaki_live::approvals::DiskApprovals;
use omuzaki_live::vars::DiskVariables;
use omuzaki_live_api::event::EventSink;
use omuzaki_obs::service::ObsService;
use omuzaki_ops_api::{Actor, Runner, ServicesBuilder};
use omuzaki_render::WgpuRenderer;
use omuzaki_secrets::KeychainSecrets;
use omuzaki_secrets_api::prelude::SecretStore;
use omuzaki_stage::frame::Stage;
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_stage::perform::input::{InputHost, NoSource};
use omuzaki_stream::feed::ProgramFeed;
use omuzaki_stream::service::{Program as StreamProgram, StreamService};
use omuzaki_stream_api::window::WindowHost;
use omuzaki_track_api::camera::{CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};
use omuzaki_track_net::PhoneSettings;
use omuzaki_twitch::TwitchService;
use omuzaki_vts::client::SecretTokens;
use omuzaki_vts::service::VtsService;

use super::clock::{ClockObserver, PerformClock};
use super::inputs::{model_registry, studio_inputs};
use super::mailbox::FrameReader;
use super::rig::{Extras, LiveRig, live_performer};
use super::thread::{LoopReport, WiringError};
use crate::aux_window::{PhysicalSize, Transparency};
use crate::live::channels::ChannelFeed;
use crate::live::hotkeys::Hotkeys;
use crate::live::viewport::{LiveViewport, Presenter, SharedTracker, ViewportError};
use crate::output_window::{OutputHost, OutputPlan, OutputWindow, Program};
use crate::wiring::{self, Core, Document, ExecutorError};

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
    /// A reader of its own for the Channels panel, which keeps reading after the viewport takes `reader`.
    channel_reader: FrameReader,
    core: Core,
    output: OutputWindow,
    host: OutputHost,
    clock: Option<ClockObserver>,
    tracker: SharedTracker,
    viewport: Option<LiveViewport>,
    hotkeys: Hotkeys,
    /// Feeds the program frame to the recorder, the replay buffer and the live outputs while any of them runs.
    feed: Option<ProgramFeed>,
    /// Runs the shows' rules (the `automation.*` and `show.*` ops reach it through the runner).
    automation: Arc<Automation>,
}

/// The frame rate the program is fed to the outputs at (spec §10's budget scene).
const STREAM_FPS: (u32, u32) = (30, 1);

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
        Self::start_on(Document::empty(), tracker, clock, transparency, base)
    }

    /// [`start`](Self::start), with the runner over `document` (the open project) instead of an empty one.
    ///
    /// # Errors
    /// [`StartError`]; nothing is left running then.
    pub fn start_on(
        document: Document,
        tracker: Box<dyn Tracker>,
        clock: Box<dyn PerformClock>,
        transparency: Transparency,
        base: HumanoidPose,
    ) -> Result<Self, StartError> {
        Self::start_with(document, tracker, Extras::default(), clock, transparency, &base)
    }

    /// [`start_on`](Self::start_on), with the studio's real inputs: the webcam tracker over the install's face models,
    /// the phone link and VMC out ([`studio_inputs`]), timed on `clock`. When the webcam can't track (no face
    /// models in `install`, no camera backend on this platform) it still lists its cameras and says why on start; the
    /// reason is logged here. The model registry keeps its state in `models` and puts the `model.*` ops in the
    /// runner's services; with no folder, or one that can't be opened, those ops answer that they're unavailable.
    ///
    /// # Errors
    /// [`StartError`]; nothing is left running then.
    pub fn start_studio(
        document: Document,
        install: &Path,
        models: Option<&Path>,
        clock: Box<dyn PerformClock>,
        transparency: Transparency,
        base: HumanoidPose,
    ) -> Result<Self, StartError> {
        // The trackers stamp frames on the perform loop's own clock, so frames and steps share one time line.
        let tracking: Arc<dyn Clock> = match clock.observer() {
            Some(observer) => Arc::new(ObservedClock(observer)),
            None => Arc::new(SystemClock::new()),
        };
        let inputs = studio_inputs(install, tracking, PhoneSettings::default());
        if let Some(problem) = &inputs.webcam_problem {
            tracing::warn!(%problem, "the webcam can't track yet");
        }
        let models_dir = models;
        let models = models.ok_or_else(|| "the app has no data folder".to_owned()).and_then(model_registry);
        let models = match models {
            Ok(registry) => Some(Arc::new(registry) as Arc<dyn ModelRegistry>),
            Err(problem) => {
                tracing::warn!(%problem, "the model registry did not open: the `model.*` ops are unavailable");
                None
            }
        };
        // The app's data folder is the models folder's parent: the keychain keeps its label list's lock file there, and
        // the shows keep what this machine approved and their variables.
        let data = models_dir.and_then(Path::parent).map(Path::to_path_buf);
        let secrets = data.as_ref().map(|data| Arc::new(KeychainSecrets::new(data)) as Arc<dyn SecretStore>);
        let extras =
            Extras { phone: Some(inputs.phone), pose_out: Some(inputs.pose_out), models, secrets, data, tap: None };
        Self::start_with(document, inputs.webcam, extras, clock, transparency, &base)
    }

    fn start_with(
        document: Document,
        tracker: Box<dyn Tracker>,
        mut extras: Extras,
        clock: Box<dyn PerformClock>,
        transparency: Transparency,
        base: &HumanoidPose,
    ) -> Result<Self, StartError> {
        let performer = live_performer(Box::new(NoSource), ChannelBuf::NEUTRAL, *base);
        // The viewport reads the tracker's latency while the input owns the tracker: two handles to one.
        let tracker = SharedTracker::new(tracker);
        let observer = clock.observer();
        let models = extras.models.clone();
        let secrets = extras.secrets.clone();
        let data = extras.data.clone();
        // The shows: the automation engine runs their rules through this app's runner, and Twitch's events reach it.
        let automation = Arc::new(Automation::new());
        let twitch = TwitchService::new(Arc::new(ConnectHttp::new()), Arc::clone(&automation) as Arc<dyn EventSink>);
        // The VTube Studio bridge: the perform thread feeds it every step's channels; the `vts.*` ops connect it.
        let vts = match VtsService::new() {
            Ok(service) => {
                let service = match &secrets {
                    Some(store) => service.with_tokens(Arc::new(SecretTokens(Arc::clone(store)))),
                    None => service,
                };
                let service = Arc::new(service);
                let mut feed = service.feed();
                let face = |t: Tracking| t == Tracking::Tracking;
                extras.tap.get_or_insert(Box::new(move |frame| feed.push(&frame.channels, face(frame.tracking))));
                Some(service)
            }
            Err(error) => {
                tracing::warn!(%error, "the VTube Studio bridge did not start: the `vts.*` ops are unavailable");
                None
            }
        };
        let (rig, reader) =
            LiveRig::start_with(Box::new(tracker.clone()), extras, TrackerConfig::default(), performer, clock)
                .map_err(StartError::Wiring)?;
        let host = rig.host().map_err(StartError::Wiring)?;
        host.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).map_err(StartError::Perform)?;
        let window = OutputHost::new(transparency, PhysicalSize::FULL_HD);
        let input: Arc<dyn InputHost> = rig.input();
        let mut services = ServicesBuilder::new()
            .insert::<dyn PerformHost>(host)
            .insert::<dyn InputHost>(input)
            .insert::<dyn WindowHost>(Arc::new(window.clone()))
            .insert::<ObsService>(Arc::new(ObsService::new()));
        if let Some(models) = models {
            services = services.insert::<dyn ModelRegistry>(models);
        }
        if let Some(secrets) = secrets {
            services = services.insert::<dyn SecretStore>(secrets);
        }
        services = services.insert::<Automation>(Arc::clone(&automation)).insert::<TwitchService>(Arc::new(twitch));
        if let Some(vts) = vts {
            services = services.insert::<VtsService>(vts);
        }
        // Frames and the microphone count from one zero, so a take's audio lines up with its video.
        let epoch = std::time::Instant::now();
        let stream = match StreamService::with_epoch(epoch) {
            Ok(stream) => Some(Arc::new(stream)),
            Err(error) => {
                tracing::warn!(%error, "the stream service did not start: recording and the live outputs are unavailable");
                None
            }
        };
        if let Some(stream) = &stream {
            services = services.insert::<StreamService>(Arc::clone(stream));
        }
        match LiveEngine::with_epoch(Arc::new(CpalBackend::new(epoch)), None, epoch) {
            Ok(audio) => services = services.insert::<dyn AudioEngine>(Arc::new(audio)),
            Err(error) => tracing::warn!(%error, "the audio engine did not start: the microphone is unavailable"),
        }
        let services = services.build();
        let project = document.dir().map(|dir| dir.to_string_lossy().into_owned());
        let core = wiring::core_on(document, services).map_err(StartError::Ops)?;
        automation.attach(&core.runner);
        automation.attach_sessions(&core.sessions);
        // What this machine approved and the shows' variables live in the app's data folder, never in the project.
        // Without one, nothing is approved and variables last one run.
        if let Some(data) = &data {
            automation.set_variables(Arc::new(DiskVariables::new(data)), project);
            automation.set_approvals(Arc::new(DiskApprovals::new(data)));
        }
        Ok(Self {
            rig: Some(rig),
            channel_reader: reader.clone(),
            reader: Some(reader),
            core,
            output: OutputWindow::new(OutputPlan::default(), transparency),
            host: window,
            clock: observer,
            tracker,
            viewport: None,
            hotkeys: Hotkeys::new(),
            feed: stream.map(ProgramFeed::new),
            automation,
        })
    }

    /// The automation engine: the approval card allows a show's needs through its approvals store and calls
    /// [`Automation::rearm`], which no op can do.
    pub fn automation(&self) -> &Arc<Automation> {
        &self.automation
    }

    /// Starts drawing the stage: `stage` (from [`project_stage`](crate::live::viewport::project_stage), made on
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
        if let Some(feed) = &self.feed {
            feed.service().set_program(StreamProgram { size, fps: STREAM_FPS });
        }
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
        Arc::clone(&self.core.runner)
    }

    /// The app's executor: the runner, and the agent sessions on it.
    pub fn core(&self) -> Core {
        self.core.clone()
    }

    /// The rig: the input and the loop's counts. `None` after [`shutdown`](Self::shutdown).
    pub fn rig_mut(&mut self) -> Option<&mut LiveRig> {
        self.rig.as_mut()
    }

    /// The newest frames from the perform thread, for the render side. `None` after [`shutdown`](Self::shutdown).
    pub fn reader_mut(&mut self) -> Option<&mut FrameReader> {
        self.reader.as_mut()
    }

    /// The perform thread's frames for the Channels panel, to put in the shell's views.
    pub fn channel_feed(&self) -> ChannelFeed {
        ChannelFeed::new(self.channel_reader.clone())
    }

    /// The stream service the recorder, the replay buffer and the live outputs run in (the `record.*` and `output.*`
    /// ops reach it through the runner). `None` when it could not start, or after [`shutdown`](Self::shutdown).
    pub fn stream(&self) -> Option<&Arc<StreamService>> {
        self.feed.as_ref().map(ProgramFeed::service)
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
        // A take, the replay buffer or a live output carries the character alone with alpha too.
        let streaming = self.feed.as_ref().is_some_and(ProgramFeed::wanted);
        let clean = (self.output.status().open && !ctx.embed_viewports()) || streaming;
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
        if let (Some(feed), Some(view)) = (self.feed.as_mut(), self.viewport.as_mut())
            && let Err(error) = feed.offer(|| view.snapshot())
        {
            tracing::warn!(%error, "the program frame could not be read back for the outputs");
        }
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
        self.hotkeys.dispatch(ctx, &*self.core.runner, &Actor::Person { name: "you".to_owned() });
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
        // A take still recording is finished (its manifest written) before the frames stop.
        if let Some(feed) = self.feed.take() {
            feed.service().shutdown();
        }
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

/// A [`PerformClock`]'s time as the trackers' [`Clock`].
struct ObservedClock(ClockObserver);

impl Clock for ObservedClock {
    fn now(&self) -> Seconds {
        (self.0)()
    }
}

/// A tracker with no cameras: what a test performs with, and the app when it has no inputs. The life layer
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
