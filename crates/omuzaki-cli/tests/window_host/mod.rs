//! The output window the conformance rig's ops run against (#2198): a [`WindowHost`] and a [`PaletteSource`] with no
//! display behind them, so the `output.window.*` examples plan and dry-run as they would in the app.
//!
//! The host keeps only the state the ops read back. A dry-run never reaches `open` or `close`, and the window ops are
//! `Undo::None`, so the suite stops at the dry-run (`check_example`); the state is kept honest anyway, in case a later
//! rule applies them.

use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::ServicesBuilder;
use omuzaki_stream_api::window::{
    Backdrop, DesktopSettings, ModeSupport, OutputSurface, PaletteSource, WindowCaps, WindowError, WindowHost,
    WindowMode, WindowPlan, WindowSettings, WindowState,
};

/// A 1920 by 1080 output on a display at 100% that can keep a window on top. For most ops the host has no transparent
/// surface, so the transparent example takes the fallback path; for the ops whose examples show the character on the
/// desktop (`output.window.open` and `output.window.set`) it can, and shows desktop mode.
struct IdleWindow {
    state: Mutex<WindowState>,
    desktop: bool,
}

impl WindowHost for IdleWindow {
    fn capabilities(&self) -> WindowCaps {
        WindowCaps::new(self.desktop, true)
    }

    fn modes(&self) -> Vec<ModeSupport> {
        let desktop = if self.desktop {
            ModeSupport::available(WindowMode::Desktop)
        } else {
            ModeSupport::unavailable(WindowMode::Desktop, "the conformance rig shows no desktop window for this op")
        };
        vec![ModeSupport::available(WindowMode::Studio), ModeSupport::available(WindowMode::Capture), desktop]
    }

    fn set_desktop(&self, desktop: &DesktopSettings) -> Result<(), WindowError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *state = state.with_desktop(*desktop);
        Ok(())
    }

    fn surface(&self) -> OutputSurface {
        OutputSurface::new(UVec2::new(1920, 1080), 1.0)
    }

    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError> {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) =
            WindowState::open(plan.physical, settings.backdrop).with_mode(settings);
        Ok(())
    }

    fn close(&self) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = WindowState::Closed;
    }

    fn state(&self) -> WindowState {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A scene with no cast: an empty palette, for which `output.window.suggest_key` picks the default key.
struct NoCast;

impl PaletteSource for NoCast {
    fn colours(&self, _scene: Option<&str>) -> Vec<Colour> {
        Vec::new()
    }
}

/// `services` with an [`IdleWindow`] for `op` and the [`NoCast`] palette added. The window is closed, except for
/// `output.window.set`, which changes a window already on the desktop.
pub fn with_window(services: ServicesBuilder, op: &str) -> ServicesBuilder {
    let desktop = matches!(op, "output.window.open" | "output.window.set");
    let state = if op == "output.window.set" {
        let mut settings = WindowSettings::default();
        settings.mode = WindowMode::Desktop;
        WindowState::open(UVec2::new(960, 540), Backdrop::Transparent).with_mode(&settings)
    } else {
        WindowState::Closed
    };
    services
        .insert::<dyn WindowHost>(Arc::new(IdleWindow { state: Mutex::new(state), desktop }))
        .insert::<dyn PaletteSource>(Arc::new(NoCast))
}
