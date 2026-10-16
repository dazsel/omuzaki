//! The Omuzaki desktop app: a window around the shell (`docs/design/ux/shell.md`), with the live slice running behind
//! it (`omuzaki_ui::live::wiring`): the perform thread, the op runner with the performer in its services, and the
//! output window.
//!
//! The shell is wired to the live slice's runner. If the live slice can't start, the reason is logged and the shell
//! runs on a runner with no document, so the window still opens.

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::Runner;
use omuzaki_render::WgpuRenderer;
use omuzaki_stage::frame::Stage;
use omuzaki_ui::aux_window::{Backend, Platform, SurfaceRoute, transparency};
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::live::viewport::{NativePresenter, Presenter, ViewportError, kit_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, NoTracker, RealClock};
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::{Shell, class};

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
fn start_live(cc: &eframe::CreationContext<'_>) -> Option<LiveApp> {
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
    let mut live = match LiveApp::start(Box::new(NoTracker), Box::new(RealClock::new()), transparency, base) {
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
    state: &eframe::egui_wgpu::RenderState,
) -> Result<(WgpuRenderer, Stage, Box<dyn Presenter>), ViewportError> {
    let mut renderer = WgpuRenderer::from_device(&state.adapter, state.device.clone(), state.queue.clone())?;
    let stage = kit_stage(&mut renderer, PROGRAM_SIZE)?;
    Ok((renderer, stage, Box::new(NativePresenter::new(state.clone()))))
}

fn main() -> eframe::Result {
    omuzaki_ui::link();
    let mut native = eframe::NativeOptions::default();
    native.viewport = native
        .viewport
        .with_inner_size([1920.0, 1080.0])
        .with_min_inner_size([class::MIN_WINDOW.x, class::MIN_WINDOW.y])
        .with_title("Omuzaki");
    eframe::run_native(
        "Omuzaki",
        native,
        Box::new(|cc| {
            // Applying before the first frame lets the bundled fonts activate on the second.
            apply_settings(&cc.egui_ctx, Settings::default());
            let live = start_live(cc);
            let runner: Arc<dyn Runner> = live.as_ref().map_or_else(|| Arc::new(NullRunner) as _, LiveApp::runner);
            Ok(Box::new(App { shell: Shell::new(runner, LayoutStore::in_user_config()), live }))
        }),
    )
}
