//! The Omuzaki desktop app: a window around the shell (`docs/design/ux/shell.md`), with the live slice running behind
//! it (`omuzaki_ui::live::wiring`): the perform thread, the op runner with the performer in its services, and the
//! output window.
//!
//! The shell is wired to the live slice's runner. If the live slice can't start, the reason is logged and the shell
//! runs on a runner with no document, so the window still opens.

use std::sync::Arc;

use omuzaki_ops_api::Runner;
use omuzaki_ui::aux_window::{Backend, Platform, SurfaceRoute, transparency};
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::live::wiring::{LiveApp, NoTracker, RealClock};
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::{Shell, class};

struct App {
    shell: Shell,
    live: Option<LiveApp>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.ui(ui);
        if let Some(live) = self.live.as_mut() {
            // The viewport does not render the scene yet, so the output window shows its backdrop alone.
            live.show(ui.ctx(), None);
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

/// Starts the live slice for the GPU backend eframe chose. The webcam tracker isn't linked in yet (#378), so the
/// performer runs on the life layer alone.
fn start_live(cc: &eframe::CreationContext<'_>) -> Option<LiveApp> {
    let backend = cc
        .wgpu_render_state
        .as_ref()
        .map_or(Backend::Unknown, |state| Backend::from_name(state.adapter.get_info().backend.to_str()));
    let transparency = transparency(Platform::current(), backend, SurfaceRoute::EguiViewport);
    match LiveApp::start(Box::new(NoTracker), Box::new(RealClock::new()), transparency) {
        Ok(live) => Some(live),
        Err(error) => {
            tracing::error!(%error, "the live slice did not start");
            None
        }
    }
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
