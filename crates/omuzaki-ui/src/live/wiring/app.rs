//! [`LiveApp`]: the live slice as the app starts it. The perform thread, the op runner with the performer in its
//! services, and the output window, started together and stopped in the quit order of [`super`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::Context;
use omuzaki_ai_api::{ModelRegistry, NoConsentRecords};
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_engine::{CpalBackend, LiveEngine};
use omuzaki_connect::{ConnectorSet, Host, HostParts, SettingsConfig, ThreadPace};
use omuzaki_connect_api::Connectors;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_face_api::ExpressionSet;
use omuzaki_http::{ConnectHttp, HttpFetcher};
use omuzaki_input::{BindingDispatcher, BindingRunner, DispatchReport, Hub, HubConfig};
use omuzaki_input_api::feed::{InputControl, InputHub, IntentFeed, MotionFeed};
use omuzaki_live::Automation;
use omuzaki_live::approvals::DiskApprovals;
use omuzaki_live::vars::DiskVariables;
use omuzaki_live_api::event::EventSink;
use omuzaki_live_api::services::Approvals;
use omuzaki_obs::service::ObsService;
use omuzaki_ops_api::{Actor, Runner, ServicesBuilder};
use omuzaki_render::WgpuRenderer;
use omuzaki_secrets::KeychainSecrets;
use omuzaki_secrets_api::prelude::SecretStore;
use omuzaki_settings_api::service::Settings;
use omuzaki_stage::effects::emotes::EmoteSource;
use omuzaki_stage::effects::log::EffectLimits;
use omuzaki_stage::effects::service::StageEffects;
use omuzaki_stage::frame::Stage;
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_stage::perform::input::{InputHost, NoSource};
use omuzaki_stage_api::EffectService;
use omuzaki_store::DiskStore;
use omuzaki_store_api::Store;
use omuzaki_stream::feed::ProgramFeed;
use omuzaki_stream::service::{Program as StreamProgram, StreamService};
use omuzaki_stream_api::window::{ClickThrough, WindowHost};
use omuzaki_theatre::TheatreBook;
use omuzaki_track_api::camera::{CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};
use omuzaki_track_net::PhoneSettings;
use omuzaki_twitch::TwitchService;
use omuzaki_vts::client::SecretTokens;
use omuzaki_vts::service::VtsService;

use omuzaki_metrics_api::MetricsReader;

use super::clock::{ClockObserver, PerformClock};
use super::inputs::{model_registry, studio_inputs};
use super::mailbox::FrameReader;
use super::rig::{Extras, LiveRig, live_performer};
use super::thread::{LoopReport, WiringError};
use crate::aux_window::{PhysicalSize, Transparency};
use crate::live::channels::ChannelFeed;
use crate::live::health::HealthPublisher;
use crate::live::hotkeys::Hotkeys;
use crate::live::viewport::{LiveViewport, Presenter, SharedTracker, ViewportError};
use crate::output_window::{OutputHost, OutputPlan, OutputWindow, Program};
use crate::wiring::{self, Core, Document, ExecutorError};

/// The studio's shortcut that turns a desktop character's full click-through off.
pub const WAY_BACK: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL.plus(egui::Modifiers::ALT), egui::Key::M);

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
    /// What this machine approved for each show; only the approval card and Settings write it (never an op).
    approvals: Option<Arc<DiskApprovals>>,
    /// The OS input hub and the dispatcher that runs its bindings; `None` if the hub's store could not be read.
    input: Option<InputWiring>,
    /// The OS keychain the connectors keep their secrets in; `None` without a data folder.
    secrets: Option<Arc<dyn SecretStore>>,
    /// The stage's particle effects: the `stage.effect.*` ops queue into it, and the viewport's stage draws it.
    effects: Arc<StageEffects>,
    /// The project folder's real path, which keys a show's approvals as the engine does.
    project: String,
    /// The Build Theatre's book: the `theatre.*` ops write it, its watcher follows the runner.
    theatre: TheatreBook,
    /// The store renders and connector outputs land in, for the Theatre's evidence and references.
    renders: Option<Arc<dyn Store>>,
    /// My scripts.
    scripts: Option<Arc<dyn omuzaki_script::ScriptStore>>,
    /// Writes the live-metrics snapshot the Health panel reads (#1348).
    health: LiveHealth,
}

/// The live-metrics snapshot's writer, its reader, and the parts it samples that only the app holds.
struct LiveHealth {
    publisher: HealthPublisher,
    reader: MetricsReader,
    /// The snapshot's clock: seconds since the slice started.
    started: std::time::Instant,
    /// The performer, for its status.
    perform: Arc<dyn PerformHost>,
    /// The audio engine, for its health.
    audio: Option<Arc<dyn AudioEngine>>,
}

impl LiveHealth {
    fn new(perform: Arc<dyn PerformHost>, audio: Option<Arc<dyn AudioEngine>>) -> Self {
        let (publisher, reader) = HealthPublisher::with_reader();
        Self { publisher, reader, started: std::time::Instant::now(), perform, audio }
    }
}

/// The OS input hub (ADR 0060) as the app runs it: its control is in the runner's services (the `input.*` capture and
/// binding ops), and the dispatcher runs each binding's action as the person (#469). Capture stays off until a person
/// starts it from a drawn indicator.
struct InputWiring {
    /// Runs the bindings' actions; dropped first, so no action runs while the hub stops.
    dispatcher: Option<BindingDispatcher>,
    /// The motion and intent feeds, held for the stage until it reads input motion (ST-1, ST-2).
    _feeds: Option<(Box<dyn MotionFeed>, Box<dyn IntentFeed>)>,
    hub: Hub,
}

impl InputWiring {
    /// The hub, stamping input on the perform loop's clock as the trackers stamp their frames, and keeping its bindings
    /// and protected programs in the app's data folder; `None` (with a warning) if its store could not be read.
    fn hub(clock: &dyn PerformClock, data: Option<PathBuf>) -> Option<Hub> {
        let clock: Arc<dyn Clock> = match clock.observer() {
            Some(observer) => Arc::new(ObservedClock(observer)),
            None => Arc::new(SystemClock::new()),
        };
        Hub::new(HubConfig::new(clock, data))
            .inspect_err(
                |error| tracing::warn!(%error, "the input hub did not start: OS input and bindings are unavailable"),
            )
            .ok()
    }

    /// Puts the hub's control in the runner's services, for the `input.*` ops.
    fn serve(services: ServicesBuilder, hub: Option<&Hub>) -> ServicesBuilder {
        match hub {
            Some(hub) => services.insert::<dyn InputControl>(hub.control()),
            None => services,
        }
    }

    /// Takes the hub's feeds and starts the dispatcher on `runner`, as the person at the app.
    fn start(hub: Hub, runner: Arc<dyn Runner>) -> Self {
        let Some((motion, intents, bindings)) = hub.take_feeds() else {
            return Self { dispatcher: None, _feeds: None, hub };
        };
        let person = Actor::Person { name: "you".to_owned() };
        let dispatcher = match BindingRunner::new(bindings, runner, hub.control(), person).spawn() {
            Ok(dispatcher) => Some(dispatcher),
            Err(error) => {
                tracing::warn!(%error, "the binding dispatcher did not start: bindings do nothing");
                None
            }
        };
        Self { dispatcher, _feeds: Some((motion, intents)), hub }
    }
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
    /// `settings` is the app's settings store, for the `settings.*` ops and the ops that save a choice; without it
    /// they answer that it is unavailable, and choices last until the studio quits.
    ///
    /// # Errors
    /// [`StartError`]; nothing is left running then.
    #[allow(clippy::too_many_arguments)] // Each is one of the app's own services or facts; a struct would only rename them.
    pub fn start_studio(
        document: Document,
        install: &Path,
        models: Option<&Path>,
        settings: Option<Arc<dyn Settings>>,
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
        let extras = Extras {
            phone: Some(inputs.phone),
            pose_out: Some(inputs.pose_out),
            models,
            secrets,
            data,
            tap: None,
            settings,
            scripts: crate::script::store::DirScriptStore::in_user_config()
                .map(|store| Arc::new(store) as Arc<dyn omuzaki_script::ScriptStore>),
        };
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
        let hub = InputWiring::hub(clock.as_ref(), extras.data.clone());
        let performer = live_performer(Box::new(NoSource), ChannelBuf::NEUTRAL, *base);
        // The viewport reads the tracker's latency while the input owns the tracker: two handles to one.
        let tracker = SharedTracker::new(tracker);
        let observer = clock.observer();
        let models = extras.models.clone();
        let secrets = extras.secrets.clone();
        let settings = extras.settings.clone();
        let scripts = extras.scripts.clone();
        let data = extras.data.clone();
        // The shows: the automation engine runs their rules through this app's runner, and Twitch's events reach it.
        let automation = Arc::new(Automation::new());
        let twitch = TwitchService::new(Arc::new(ConnectHttp::new()), Arc::clone(&automation) as Arc<dyn EventSink>);
        let vts = vts_bridge(secrets.as_ref(), &mut extras);
        let (rig, reader) =
            LiveRig::start_with(Box::new(tracker.clone()), extras, TrackerConfig::default(), performer, clock)
                .map_err(StartError::Wiring)?;
        let host = rig.host().map_err(StartError::Wiring)?;
        let perform = Arc::clone(&host);
        host.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).map_err(StartError::Perform)?;
        let window = OutputHost::new(transparency, PhysicalSize::FULL_HD);
        let input: Arc<dyn InputHost> = rig.input();
        let effects = Arc::new(session_effects(settings.as_ref()));
        let mut services = ServicesBuilder::new()
            .insert::<dyn EffectService>(Arc::clone(&effects) as Arc<dyn EffectService>)
            .insert::<dyn PerformHost>(host)
            .insert::<dyn InputHost>(input)
            .insert::<dyn WindowHost>(Arc::new(window.clone()))
            .insert::<ObsService>(Arc::new(ObsService::new()));
        if let Some(models) = models {
            services = services.insert::<dyn ModelRegistry>(models);
        }
        if let Some(secrets) = &secrets {
            services = services.insert::<dyn SecretStore>(Arc::clone(secrets));
        }
        if let Some(settings) = &settings {
            services = services.insert::<dyn Settings>(Arc::clone(settings));
        }
        if let Some(scripts) = &scripts {
            services = services.insert::<dyn omuzaki_script::ScriptStore>(Arc::clone(scripts));
        }
        services = services.insert::<Automation>(Arc::clone(&automation)).insert::<TwitchService>(Arc::new(twitch));
        if let Some(vts) = vts {
            services = services.insert::<VtsService>(vts);
        }
        services = InputWiring::serve(services, hub.as_ref());
        let (with, renders) = with_connectors(services, data.as_deref(), secrets.as_ref(), settings.as_ref());
        // The Theatre's book is kept beside the project, so a reopened project shows its plans and cards again.
        let theatre = document.dir().map_or_else(TheatreBook::new, TheatreBook::for_project);
        services = with.insert::<TheatreBook>(Arc::new(theatre.clone()));
        // Frames and the microphone count from one zero, so a take's audio lines up with its video.
        let epoch = std::time::Instant::now();
        let stream = start_stream(epoch);
        if let Some(stream) = &stream {
            services = services.insert::<StreamService>(Arc::clone(stream));
        }
        let audio = start_audio(epoch);
        if let Some(audio) = &audio {
            services = services.insert::<dyn AudioEngine>(Arc::clone(audio));
        }
        let services = services.build();
        let project = document.dir().map(|dir| dir.to_string_lossy().into_owned());
        let project_key = project.clone().unwrap_or_default();
        let core = wiring::core_on(document, services).map_err(StartError::Ops)?;
        automation.attach(&core.runner);
        if let Some(audio) = &audio {
            follow_project(Arc::clone(audio), &core.runner);
        }
        automation.attach_sessions(&core.sessions);
        if let Err(error) = theatre.attach(&core.runner, Arc::clone(&core.sessions)) {
            tracing::warn!(%error, "the Theatre's book does not follow the runner: its steps stay empty");
        }
        // What this machine approved and the shows' variables live in the app's data folder, never in the project.
        // Without one, nothing is approved and variables last one run.
        let approvals = data.as_ref().map(|data| {
            automation.set_variables(Arc::new(DiskVariables::new(data)), project);
            let approvals = Arc::new(DiskApprovals::new(data));
            automation.set_approvals(Arc::clone(&approvals) as Arc<dyn Approvals>);
            approvals
        });
        Ok(Self {
            rig: Some(rig),
            channel_reader: reader.clone(),
            reader: Some(reader),
            // Before `core` moves: the dispatcher runs on its runner.
            input: hub.map(|hub| InputWiring::start(hub, Arc::clone(&core.runner))),
            core,
            output: OutputWindow::new(OutputPlan::default(), transparency),
            host: window,
            clock: observer,
            tracker,
            viewport: None,
            hotkeys: Hotkeys::new(),
            feed: stream.map(ProgramFeed::new),
            automation,
            approvals,
            secrets,
            effects,
            project: project_key,
            theatre,
            renders,
            scripts,
            health: LiveHealth::new(perform, audio),
        })
    }

    /// The automation engine: the approval card allows a show's needs through its approvals store and calls
    /// [`Automation::rearm`], which no op can do.
    pub fn automation(&self) -> &Arc<Automation> {
        &self.automation
    }

    /// The approvals store the approval card writes a person's answer to (then [`Automation::rearm`]); `None` without
    /// a data folder, when nothing can be approved.
    pub fn approvals(&self) -> Option<&Arc<DiskApprovals>> {
        self.approvals.as_ref()
    }

    /// The approval card's "Allow on this machine" (`crate::broadcast::rules`): the approvals store and a re-arm of
    /// the running show, as the person; `None` without a data folder, when nothing can be approved.
    pub fn show_approver(&self) -> Option<crate::broadcast::ShowApprover> {
        let approvals = Arc::clone(self.approvals.as_ref()?);
        let approver =
            crate::broadcast::DiskApprover::new(Arc::clone(&self.automation), approvals, self.project.clone());
        Some(crate::broadcast::ShowApprover(Arc::new(approver)))
    }

    /// The Theatre as the studio's panels read it: the core's book and the render store.
    pub fn theatre(&self) -> crate::theatre::Theatre {
        crate::theatre::Theatre::new(self.theatre.clone(), self.renders.clone())
    }

    /// My scripts, for the Script workspace; `None` when the OS gives no config folder.
    pub fn script_store(&self) -> Option<Arc<dyn omuzaki_script::ScriptStore>> {
        self.scripts.clone()
    }

    /// The store generated files land in, for the Generated tray; `None` without a connector host.
    pub fn generated_store(&self) -> Option<Arc<dyn Store>> {
        self.renders.clone()
    }

    /// The keychain, for the Settings window's OBS password (which no op carries); `None` without a data folder.
    pub fn keychain(&self) -> Option<crate::settings::connectors::Keychain> {
        self.secrets.clone().map(crate::settings::connectors::Keychain)
    }

    /// Starts drawing the stage: `stage` (from [`project_stage`](crate::live::viewport::project_stage), made on
    /// `renderer` at `size`) takes the perform thread's frames from now on, and `presenter` shows each picture to egui.
    ///
    /// # Errors
    /// [`ViewportError::Stopped`] after [`shutdown`](Self::shutdown).
    pub fn attach_viewport(
        &mut self,
        mut renderer: WgpuRenderer,
        mut stage: Stage,
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
        stage.attach_effects(Arc::clone(&self.effects), EffectLimits::live());
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

    /// The OS input hub, for the shell's capture indicator and the binding editor (its `IndicatorHost`, `ChordStore` and
    /// `ProtectList`). `None` if it did not start, or after [`shutdown`](Self::shutdown).
    pub fn input_hub(&self) -> Option<&Hub> {
        self.input.as_ref().map(|input| &input.hub)
    }

    /// What the bindings' actions did (ops run and failed, trigger edges, the latest failure), for the binding editor.
    /// `None` if the dispatcher did not start, or after [`shutdown`](Self::shutdown).
    pub fn binding_report(&self) -> Option<DispatchReport> {
        self.input.as_ref()?.dispatcher.as_ref().map(BindingDispatcher::report)
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
        self.publish_health();
        // The desktop character's per-pixel click-through reads the frame's alpha, a few times a second at most and
        // only while the pointer is over it.
        if self.output.wants_mask()
            && let Some(view) = self.viewport.as_mut()
        {
            match view.snapshot() {
                Ok(frame) => self.output.set_mask(&frame),
                Err(error) => tracing::warn!(%error, "the program frame could not be read for click-through"),
            }
        }
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

    /// The live-metrics snapshot the Health panel and the health strip read.
    pub fn metrics(&self) -> MetricsReader {
        self.health.reader.clone()
    }

    /// Writes this frame into the snapshot: the stage's frame every call, the performer, the tracker and the audio at
    /// 10 Hz. A part that is not running publishes nothing, so it reads as absent rather than zero.
    fn publish_health(&mut self) {
        let health = &mut self.health;
        let now = health.started.elapsed().as_secs_f64();
        if let Some(view) = self.viewport.as_mut() {
            health.publisher.frame(now, view.take_rendered());
        }
        if !health.publisher.slow_due(now) {
            return;
        }
        if let Some(rig) = &self.rig {
            health.publisher.perform(now, &health.perform.status(), rig.report().skipped);
            health.publisher.tracker(now, &self.tracker.latency());
        }
        if let Some(audio) = &health.audio {
            health.publisher.audio(now, &audio.status(), &audio.report());
        }
    }

    /// Sends this frame's key presses and releases to the triggers they are bound to (`trigger.press`). Call it once per
    /// frame, after the shell has drawn: the Live panel's key capture and a focused text field take their keys from the
    /// input first, so a key that binds or types never also fires.
    pub fn keys(&mut self, ctx: &Context) {
        let you = Actor::Person { name: "you".to_owned() };
        self.way_back(ctx, &you);
        self.hotkeys.dispatch(ctx, &*self.core.runner, &you);
    }

    /// The way back from a character every click passes through: [`WAY_BACK`] in the studio lets it take its own clicks
    /// again (`output.window.set`, as the person), whatever panel is showing.
    fn way_back(&mut self, ctx: &Context, you: &Actor) {
        let through = self.output.plan().desktop.is_some_and(|d| d.click_through == ClickThrough::Full);
        // In the studio as a shortcut, or anywhere on the desktop through the output window's probe.
        let pressed = ctx.input_mut(|i| i.consume_shortcut(&WAY_BACK)) | self.output.take_way_back();
        if !through || !pressed {
            return;
        }
        let args = crate::live::model::object(&[("click_through", "transparent".into())]);
        if let Err(error) = crate::live::model::run(&*self.core.runner, you, "output.window.set", args) {
            tracing::warn!(%error, "the character could not take its clicks back");
        }
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
        // Capture ends and no binding runs from here on.
        self.input = None;
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

/// The stream service counting from `epoch`, or `None` with a warning when it can't start.
fn start_stream(epoch: std::time::Instant) -> Option<Arc<StreamService>> {
    match StreamService::with_epoch(epoch) {
        Ok(stream) => Some(Arc::new(stream)),
        Err(error) => {
            tracing::warn!(%error, "the stream service did not start: recording and the live outputs are unavailable");
            None
        }
    }
}

/// The stage's effect service with a seed per session (bursts asked for without one differ from run to run, and a take
/// records each seed), fetching chat emotes' pictures (particles.md D13): HTTPS with a 5 s timeout and no redirects,
/// so nothing leaves the host list, cached in the app's cache folder, and switched by `effects.chat_emotes`.
fn session_effects(settings: Option<&Arc<dyn Settings>>) -> StageEffects {
    #[allow(clippy::cast_possible_truncation, reason = "the clock's low 64 bits are the seed")]
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let five = std::time::Duration::from_secs(5);
    let fetcher = HttpFetcher::builder().connect_timeout(five).read_timeout(five).max_redirects(0).retries(1).build();
    let mut source = EmoteSource::new(Arc::new(fetcher));
    if let Some(settings) = settings {
        source = source.with_settings(Arc::clone(settings));
    }
    if let Ok(cache) = omuzaki_http::app_cache_dir() {
        source = source.with_cache(cache.join("chat-emotes"));
    }
    StageEffects::new(seed).with_emotes(source)
}

/// The live audio engine on this machine's devices, its clock counting from `epoch`; `None` (with a warning) when it
/// can't start.
fn start_audio(epoch: std::time::Instant) -> Option<Arc<dyn AudioEngine>> {
    match LiveEngine::with_epoch(Arc::new(CpalBackend::new(epoch)), None, epoch) {
        Ok(audio) => Some(Arc::new(audio)),
        Err(error) => {
            tracing::warn!(%error, "the audio engine did not start: the microphone is unavailable");
            None
        }
    }
}

/// `services` with the connector host (as `dyn Connectors` and as itself) and the asset store, when they start; and
/// the store, which the Theatre reads renders from.
fn with_connectors(
    services: ServicesBuilder,
    data: Option<&Path>,
    secrets: Option<&Arc<dyn SecretStore>>,
    settings: Option<&Arc<dyn Settings>>,
) -> (ServicesBuilder, Option<Arc<dyn Store>>) {
    match connector_host(data, secrets, settings) {
        Some((store, host)) => (
            services
                .insert::<dyn Store>(Arc::clone(&store))
                .insert::<dyn Connectors>(Arc::clone(&host) as Arc<dyn Connectors>)
                .insert::<Host>(host),
            Some(store),
        ),
        None => (services, None),
    }
}

/// The connector host (`connect.*`) over every linked connector, and the asset store its outputs land in: the store
/// lives in the app's data folder, beside the keychain's lock file, so candidates outlast a project. `None` (with a
/// warning) without a data folder, a keychain or settings: a connector needs a person's settings to be enabled and
/// the keychain for its key.
fn connector_host(
    data: Option<&Path>,
    secrets: Option<&Arc<dyn SecretStore>>,
    settings: Option<&Arc<dyn Settings>>,
) -> Option<(Arc<dyn Store>, Arc<Host>)> {
    let (Some(data), Some(secrets), Some(settings)) = (data, secrets, settings) else {
        tracing::warn!("no data folder, keychain or settings: the `connect.*` ops are unavailable");
        return None;
    };
    let store: Arc<dyn Store> = match DiskStore::open(data.join("store")) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            tracing::warn!(%error, "the asset store did not open: the `connect.*` ops are unavailable");
            return None;
        }
    };
    let set = match ConnectorSet::from_registered() {
        Ok(set) => set,
        Err(errors) => {
            for error in &errors {
                tracing::warn!(%error, "a connector did not load");
            }
            return None;
        }
    };
    // Connectors that need an App service (an LLM provider's settings) reach these.
    let services = ServicesBuilder::new()
        .insert::<dyn Settings>(Arc::clone(settings))
        .insert::<dyn SecretStore>(Arc::clone(secrets))
        .build();
    let host = Host::new(HostParts {
        set,
        config: Arc::new(SettingsConfig::new(Arc::clone(settings))),
        secrets: Arc::clone(secrets),
        store: Arc::clone(&store),
        http: Arc::new(ConnectHttp::new()),
        pace: Arc::new(ThreadPace),
        clock: Arc::new(SystemClock::new()),
        consent: Arc::new(NoConsentRecords),
        services,
    });
    Some((store, Arc::new(host)))
}

/// The `VTube Studio` bridge: the perform thread feeds it every step's channels through `extras`' tap; the `vts.*` ops
/// connect it. `None`, with a warning, when it can't start.
fn vts_bridge(secrets: Option<&Arc<dyn SecretStore>>, extras: &mut Extras) -> Option<Arc<VtsService>> {
    match VtsService::new() {
        Ok(service) => {
            let service = match secrets {
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
    }
}

/// Keeps the mixer on the project's graph: the `audio.bus.*` ops edit it, undo included.
fn follow_project(audio: Arc<dyn AudioEngine>, runner: &Arc<dyn Runner>) {
    if let Err(error) = omuzaki_audio_engine::follow(audio, runner) {
        tracing::warn!(error = %error.message, "the audio mixer will not follow the project");
    }
}
