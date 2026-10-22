//! The Omuzaki desktop app: a window around the shell (`docs/design/ux/shell.md`), with the live slice running behind
//! it (`omuzaki_ui::live::wiring`): the perform thread, the op runner with the performer in its services, and the
//! output window.
//!
//! The shell is wired to the live slice's runner over the project folder (`--project`, or the app's own), opened on
//! its character (`omuzaki_ui::studio`): the sample is made when the project has none, so the first thing on screen is
//! a character. If the folder can't be opened the app runs on an in-memory document, and if the live slice can't start
//! the shell runs on a plain runner, so the window still opens; each reason is logged.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use omuzaki_char_api::{BuildServices, RegistryBuilder};

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::Runner;
use omuzaki_render::WgpuRenderer;
use omuzaki_stage::frame::Stage;
use omuzaki_ui::aux_window::{Backend, Platform, SurfaceRoute, transparency};
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::live::viewport::{NativePresenter, Presenter, ViewportError, kit_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, NoTracker, RealClock};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::{Shell, class};
use omuzaki_ui::wiring::{self, Document};
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

/// Starts the live slice for the GPU backend eframe chose, with the kit character on that same device. The webcam
/// tracker isn't linked in yet (#378), so the performer runs on the life layer alone. If the stage can't start (no
/// GPU state, or the kit character failing to build) the slice still runs without a picture, and the reason is logged.
fn start_live(cc: &eframe::CreationContext<'_>, document: Document) -> Option<LiveApp> {
    let backend = cc
        .wgpu_render_state
        .as_ref()
        .map_or(Backend::Unknown, |state| Backend::from_name(state.adapter.get_info().backend.to_str()));
    let transparency = transparency(Platform::current(), backend, SurfaceRoute::EguiViewport);
    let stage = cc.wgpu_render_state.as_ref().and_then(|state| match start_stage(state) {
        Ok(stage) => Some(stage),
        Err(error) => {
            tracing::error!(%error, "the stage did not start");
            None
        }
    });
    let base = stage.as_ref().map_or(HumanoidPose::REST, |(_, stage, _)| LiveScene::base_pose(stage));
    let mut live =
        match LiveApp::start_on(document, Box::new(NoTracker), Box::new(RealClock::new()), transparency, base) {
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

/// The renderer on eframe's own device, the kit stage on it, and the presenter that shares its picture with egui.
fn start_stage(
    gpu: &eframe::egui_wgpu::RenderState,
) -> Result<(WgpuRenderer, Stage, Box<dyn Presenter>), ViewportError> {
    let mut renderer = WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone())?;
    let stage = kit_stage(&mut renderer, PROGRAM_SIZE)?;
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

/// The runner the shell works through: the live slice's, or a plain one over the project when the slice did not start.
fn runner_of(live: Option<&LiveApp>, dir: Option<&PathBuf>) -> Arc<dyn Runner> {
    if let Some(live) = live {
        return live.runner();
    }
    match wiring::runner_on(open_document(dir), omuzaki_ops_api::Services::default()) {
        Ok(runner) => runner,
        Err(error) => {
            tracing::error!(%error, "the ops did not start: the studio has no document");
            Arc::new(NullRunner)
        }
    }
}

/// The shell over `runner`, opened on the project's character, with the device egui draws with for the viewport.
fn studio_shell(cc: &eframe::CreationContext<'_>, runner: Arc<dyn Runner>) -> Shell {
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
    shell
}

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
            apply_settings(&cc.egui_ctx, Settings::default());
            let live = start_live(cc, open_document(dir.as_ref()));
            let runner = runner_of(live.as_ref(), dir.as_ref());
            Ok(Box::new(App { shell: studio_shell(cc, runner), live }))
        }),
    )
}
