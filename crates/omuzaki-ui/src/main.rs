//! The Omuzaki desktop app: a window around the shell (`docs/design/ux/shell.md`).
//!
//! The shell is wired to a runner with no document until the app builds the real executor.

use std::sync::Arc;

use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::{Shell, class};

struct App {
    shell: Shell,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.ui(ui);
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
            Ok(Box::new(App { shell: Shell::new(Arc::new(NullRunner), LayoutStore::in_user_config()) }))
        }),
    )
}
