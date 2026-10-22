//! The Omuzaki desktop app: a window around the shell (`docs/design/ux/shell.md`), with the live slice running behind
//! it (`omuzaki_ui::live::wiring`): the perform thread, the op runner with the performer in its services, and the
//! output window.
//!
//! The shell is wired to the live slice's runner over the project folder (`--project`, or the app's own), opened on
//! its character (`omuzaki_ui::studio`): the sample is made when the project has none, so the first thing on screen is
//! a character. If the folder can't be opened the app runs on an in-memory document, and if the live slice can't start
//! the shell runs on a plain runner, so the window still opens; each reason is logged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use omuzaki_char_api::{BuildServices, CharId, CharacterDef, RegistryBuilder};

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::Runner;
use omuzaki_render::WgpuRenderer;
use omuzaki_script::ScriptStore;
use omuzaki_settings_api::service::Settings as SettingsService;
use omuzaki_stage::frame::Stage;
use omuzaki_theatre::TheatreBook;
use omuzaki_ui::aux_window::{Backend, Platform, SurfaceRoute, transparency};
use omuzaki_ui::ds::script_fonts::ScriptFonts;
use omuzaki_ui::ds::settings_keys::SettingsLink;
use omuzaki_ui::generate::Generation;
use omuzaki_ui::live::viewport::{NativePresenter, Presenter, ViewportError, kit_stage, project_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::setup::FirstRun;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::script::Scripting;
use omuzaki_ui::script::consent::ConsentBook;
use omuzaki_ui::script::store::DirScriptStore;
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::{Shell, class};
use omuzaki_ui::theatre::Theatre;
use omuzaki_ui::wiring::{self, Core, Document};
use omuzaki_ui::{studio, wiring::default_project_dir};

/// The studio's command line.
#[derive(Debug, Parser)]
#[command(name = "omuzaki-ui", about = "The Omuzaki studio")]
struct Args {
    /// The project folder to open; it is made there when it holds no project. Without it the studio keeps its own.
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,
}

struct App {
    shell: Shell,
    live: Option<LiveApp>,
    /// The look follows the `ui.*` settings, whoever changes them.
    look: Option<SettingsLink>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(look) = self.look.as_mut() {
            look.sync(ui.ctx());
        }
        // The stage first: its picture is what the stage panel and the output window draw this frame.
        if let Some(live) = self.live.as_mut() {
            live.show(ui.ctx());
        }
        self.shell.ui(ui);
        // The keyboard last: a key the Live panel took to bind, or a text field took to type, is no longer in the input.
        if let Some(live) = self.live.as_mut() {
            live.keys(ui.ctx());
        }
    }

    fn on_exit(&mut self) {
        // The quit order: the live slice stops its threads before the shell and the runner go.
        if let Some(mut live) = self.live.take()
            && let Err(error) = live.shutdown()
        {
            tracing::warn!(%error, "the live slice stopped badly");
        }
    }
}

/// The program frame's size: what the output window shows exactly (`output.window`, 1920 x 1080 physical pixels).
const PROGRAM_SIZE: UVec2 = UVec2::new(1920, 1080);

/// Starts the live slice for the GPU backend eframe chose, with the project's character on that same device (the one the
/// studio opens on, read before the executor takes the document, since the performer is built on its bind pose; the
/// kit character when it can't be read). The webcam tracker isn't linked in yet (#378), so the performer runs on the
/// life layer alone. If the stage can't start (no GPU state, or the character failing to build) the slice still runs
/// without a picture, and the reason is logged.
fn start_live(
    cc: &eframe::CreationContext<'_>,
    document: Document,
    settings: Option<Arc<dyn SettingsService>>,
) -> Option<LiveApp> {
    let character = match studio::character_to_stage(&document) {
        Ok(character) => Some(character),
        Err(error) => {
            tracing::error!(%error, "the project's character can't be staged: the stage shows the kit character");
            None
        }
    };
    let backend = cc
        .wgpu_render_state
        .as_ref()
        .map_or(Backend::Unknown, |state| Backend::from_name(state.adapter.get_info().backend.to_str()));
    let transparency = transparency(Platform::current(), backend, SurfaceRoute::EguiViewport);
    let stage = cc.wgpu_render_state.as_ref().and_then(|state| match start_stage(state, character.as_ref()) {
        Ok(stage) => Some(stage),
        Err(error) => {
            tracing::error!(%error, "the stage did not start");
            None
        }
    });
    let base = stage.as_ref().map_or(HumanoidPose::REST, |(_, stage, _)| LiveScene::base_pose(stage));
    let mut live = match LiveApp::start_studio(
        document,
        &install_dir(),
        models_dir().as_deref(),
        settings,
        Box::new(RealClock::new()),
        transparency,
        base,
    ) {
        Ok(live) => live,
        Err(error) => {
            tracing::error!(%error, "the live slice did not start");
            return None;
        }
    };
    if let (Some((renderer, stage, presenter)), Some(_)) = (stage, cc.wgpu_render_state.as_ref())
        && let Err(error) = live.attach_viewport(renderer, stage, PROGRAM_SIZE, presenter)
    {
        tracing::error!(%error, "the stage was not attached");
    }
    Some(live)
}

/// The folder the app is installed in: the executable's, where the release puts the face models and natives.
fn install_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)).unwrap_or_default()
}

/// The faces Japanese, Korean and Chinese text needs, from the stage's pinned list through the download cache models
/// share (#205); `None` when this machine has no cache folder, and such text then falls back to egui's own faces.
fn script_fonts() -> Option<ScriptFonts> {
    let dir = omuzaki_http::app_cache_dir()
        .map_err(
            |error| tracing::warn!(%error, "no download cache: Japanese, Korean and Chinese faces can't be fetched"),
        )
        .ok()?;
    let cache = Arc::new(omuzaki_ai::VerifiedCache::new(dir, omuzaki_http::HttpFetcher::new()));
    Some(ScriptFonts::new(Arc::new(omuzaki_stage::sheet::layout::PinnedFonts::new(cache))))
}

/// Where the model registry keeps its state: `models` beside the studio's own project folder.
fn models_dir() -> Option<PathBuf> {
    default_project_dir()?.parent().map(|data| data.join("models"))
}

/// The renderer on eframe's own device, `character` (else the kit's) staged on it, and the presenter that shares its
/// picture with egui.
fn start_stage(
    gpu: &eframe::egui_wgpu::RenderState,
    character: Option<&(CharId, CharacterDef)>,
) -> Result<(WgpuRenderer, Stage, Box<dyn Presenter>), ViewportError> {
    let mut renderer = WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone())?;
    let stage = match character {
        Some((id, def)) => project_stage(&mut renderer, id, def, PROGRAM_SIZE)?,
        None => kit_stage(&mut renderer, PROGRAM_SIZE)?,
    };
    Ok((renderer, stage, Box::new(NativePresenter::new(gpu.clone()))))
}

/// The project folder `args` names, or the studio's own; `None` when there is no folder to keep one in.
fn project_dir(args: &Args) -> Option<PathBuf> {
    args.project.clone().or_else(default_project_dir)
}

/// The document over the project folder, or an in-memory one (logged) when the folder can't be opened.
fn open_document(dir: Option<&PathBuf>) -> Document {
    let Some(dir) = dir else {
        tracing::warn!("no folder to keep the project in: changes last until the studio quits");
        return Document::empty();
    };
    match Document::project(dir) {
        Ok(document) => document,
        Err(error) => {
            tracing::error!(%error, dir = %dir.display(), "the project did not open: changes last until the studio quits");
            Document::empty()
        }
    }
}

/// The studio's settings, in the OS's config and local data folders (or `OMUZAKI_CONFIG_DIR` and `OMUZAKI_DATA_DIR`);
/// `None`, logged, when the OS gives no home folder: choices then last until the studio quits.
fn studio_settings() -> Option<Arc<dyn SettingsService>> {
    match omuzaki_settings::Paths::from_env() {
        Ok(paths) => Some(Arc::new(omuzaki_settings::Store::open(&paths))),
        Err(error) => {
            tracing::warn!(%error, "no folder for the settings: choices last until the studio quits");
            None
        }
    }
}

/// The executor the shell works through and the Theatre over it: the live slice's, or a plain one over the project
/// when the slice did not start; `None` when not even that could start (the shell then runs on a runner with no
/// document).
fn core_of(
    live: Option<&LiveApp>,
    dir: Option<&PathBuf>,
    settings: Option<Arc<dyn SettingsService>>,
    scripts: Option<Arc<dyn ScriptStore>>,
) -> Option<(Core, Theatre)> {
    if let Some(live) = live {
        return Some((live.core(), live.theatre()));
    }
    let book = dir.map_or_else(TheatreBook::new, |dir| TheatreBook::for_project(dir));
    let mut services = omuzaki_ops_api::ServicesBuilder::new().insert::<TheatreBook>(Arc::new(book.clone()));
    if let Some(settings) = settings {
        services = services.insert::<dyn SettingsService>(settings);
    }
    if let Some(scripts) = scripts {
        services = services.insert::<dyn ScriptStore>(scripts);
    }
    match wiring::core_on(open_document(dir), services.build()) {
        Ok(core) => {
            if let Err(error) = book.attach(&core.runner, Arc::clone(&core.sessions)) {
                tracing::warn!(%error, "the Theatre's book does not follow the runner: its steps stay empty");
            }
            Some((core, Theatre::new(book, None)))
        }
        Err(error) => {
            tracing::error!(%error, "the ops did not start: the studio has no document");
            None
        }
    }
}

/// The shell over `core`, opened on the project's character, with the device egui draws with for the viewport and
/// History's pictures, and the agent sessions for the Agent panel; on the first start, in the setup flow.
fn studio_shell(cc: &eframe::CreationContext<'_>, core: Option<Core>, dir: Option<&PathBuf>) -> Shell {
    let runner: Arc<dyn Runner> = core.as_ref().map_or_else(|| Arc::new(NullRunner) as _, |c| Arc::clone(&c.runner));
    let character = match studio::open_character(runner.as_ref()) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::error!(%error, "no character could be opened");
            None
        }
    };
    let mut shell = Shell::new(runner, LayoutStore::in_user_config());
    let focus = EditFocus::default();
    focus.open(character);
    shell.insert_view(focus);
    shell.insert_view(ViewportServices {
        gpu: cc.wgpu_render_state.clone(),
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    if let (Some(core), Some(dir), Some(gpu)) = (&core, dir, cc.wgpu_render_state.as_ref()) {
        match history_pictures(core, dir, gpu) {
            Ok(pictures) => shell.insert_view(pictures),
            Err(error) => tracing::warn!(%error, "History shows no pictures"),
        }
    }
    if let Some(core) = core {
        shell.insert_view(Agents(core.sessions));
    }
    // The first start opens the setup flow beside the stage (shell.md section 14).
    if let Some(dir) = omuzaki_ui::shell::persist::user_config_dir() {
        shell.keep_palette_recents_in(dir.join("palette-recents.json"));
    }
    let first_run = FirstRun::in_user_config();
    first_run.open_if_new(&shell.requests());
    shell.insert_view(first_run);
    shell
}

/// History's pictures of the project in `dir`, rendered on egui's own device by a renderer of their own.
fn history_pictures(
    core: &Core,
    dir: &Path,
    gpu: &eframe::egui_wgpu::RenderState,
) -> Result<omuzaki_ui::shell::pictures::HistoryPictures, String> {
    let renderer =
        WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone()).map_err(|e| e.to_string())?;
    omuzaki_ui::shell::pictures::taken(Arc::clone(&core.runner), Box::new(renderer), dir)
}

/// Every allocation goes through the scripts' memory budget, so a script run can be held to its memory limit (a run
/// refuses to start without it).
#[global_allocator]
static ALLOCATOR: omuzaki_script::MemoryBudget = omuzaki_script::MemoryBudget::system();

fn main() -> eframe::Result {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    omuzaki_ui::link();
    let dir = project_dir(&args);
    let mut native = eframe::NativeOptions::default();
    native.viewport = native
        .viewport
        .with_inner_size([1920.0, 1080.0])
        .with_min_inner_size([class::MIN_WINDOW.x, class::MIN_WINDOW.y])
        .with_title("Omuzaki");
    eframe::run_native(
        "Omuzaki",
        native,
        Box::new(move |cc| {
            // Applying before the first frame lets the bundled fonts activate on the second.
            let settings = studio_settings();
            let mut look = settings.clone().map(|store| {
                let link = SettingsLink::new(store);
                match script_fonts() {
                    Some(fonts) => link.with_fonts(fonts),
                    None => link,
                }
            });
            match look.as_mut() {
                Some(look) => look.sync(&cc.egui_ctx),
                None => omuzaki_ui::ds::apply_settings(&cc.egui_ctx, omuzaki_ui::ds::Settings::default()),
            }
            let live = start_live(cc, open_document(dir.as_ref()), settings.clone());
            // My scripts: the live slice's, or a folder of their own when it did not start.
            let mine = match live.as_ref() {
                Some(live) => live.script_store(),
                None => DirScriptStore::in_user_config().map(|store| Arc::new(store) as Arc<dyn ScriptStore>),
            };
            let (core, theatre) = core_of(live.as_ref(), dir.as_ref(), settings, mine.clone()).unzip();
            let scripting = core.as_ref().map(|core| {
                let agreed = omuzaki_ui::shell::persist::user_config_dir()
                    .map_or_else(ConsentBook::in_memory, |config| {
                        ConsentBook::in_file(config.join("scripts-agreed.json"))
                    });
                let project = dir.as_ref().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
                Scripting::new(Arc::clone(&core.runner), mine, project, agreed)
            });
            let generation = core.as_ref().map(|core| Generation {
                runner: Arc::clone(&core.runner),
                store: live.as_ref().and_then(LiveApp::generated_store),
            });
            let mut shell = studio_shell(cc, core, dir.as_ref());
            if let Some(generation) = generation {
                shell.insert_view(generation);
            }
            if let Some(scripting) = scripting {
                shell.insert_view(scripting);
            }
            if let Some(theatre) = theatre {
                shell.insert_view(theatre);
            }
            if let Some(live) = live.as_ref() {
                shell.insert_view(live.channel_feed());
                shell.insert_view(live.metrics());
                if let Some(approver) = live.show_approver() {
                    shell.insert_view(approver);
                }
                if let Some(keychain) = live.keychain() {
                    shell.insert_view(keychain);
                }
            }
            Ok(Box::new(App { shell, live, look }))
        }),
    )
}
