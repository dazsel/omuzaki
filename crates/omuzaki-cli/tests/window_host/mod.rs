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
    OutputSurface, PaletteSource, WindowCaps, WindowError, WindowHost, WindowPlan, WindowSettings, WindowState,
};

/// A 1920 by 1080 output on a display at 100%, on a host without a transparent surface (so the transparent example
/// takes the fallback path) that can keep a window on top.
struct IdleWindow(Mutex<WindowState>);

impl WindowHost for IdleWindow {
    fn capabilities(&self) -> WindowCaps {
        WindowCaps::new(false, true)
    }

    fn surface(&self) -> OutputSurface {
        OutputSurface::new(UVec2::new(1920, 1080), 1.0)
    }

    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = WindowState::open(plan.physical, settings.backdrop);
        Ok(())
    }

    fn close(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = WindowState::Closed;
    }

    fn state(&self) -> WindowState {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A scene with no cast: an empty palette, for which `output.window.suggest_key` picks the default key.
struct NoCast;

impl PaletteSource for NoCast {
    fn colours(&self, _scene: Option<&str>) -> Vec<Colour> {
        Vec::new()
    }
}

/// `services` with a closed [`IdleWindow`] and the [`NoCast`] palette added.
pub fn with_window(services: ServicesBuilder) -> ServicesBuilder {
    services
        .insert::<dyn WindowHost>(Arc::new(IdleWindow(Mutex::new(WindowState::Closed))))
        .insert::<dyn PaletteSource>(Arc::new(NoCast))
}
