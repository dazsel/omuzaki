//! The services the stream crate's ops call. The traits live here so the Live UI can implement them without depending
//! on the ops; the app inserts one implementation of each into its `Services` (ADR 0015).
//!
//! More services join this file with the stream service task; only the window ones exist so far.

use omuzaki_core::colour::Colour;
use omuzaki_ops_api::Service;

use crate::window::{OutputSurface, WindowCaps, WindowError, WindowPlan, WindowSettings, WindowState};

/// The host of the clean output window: the part that owns the OS surface. Implemented by the Live UI; the
/// `output.window.*` ops drive it and decide nothing a display is needed to test (`crate::window`).
///
/// All methods take `&self` and may be called from any thread; an implementation synchronises itself and must not
/// block on the render loop. `open` and `close` run on the thread that executes the op.
pub trait WindowHost: Service {
    /// What this host can do. Constant for the life of the host.
    fn capabilities(&self) -> WindowCaps;

    /// The output size and the scale factor of the display the window would open on, now. [`crate::window::plan`]
    /// turns it into the window's exact size.
    fn surface(&self) -> OutputSurface;

    /// Opens the window with exactly `plan.physical` pixels of client area. Opening while a window is open replaces
    /// it. `settings.backdrop` is already resolved: it is `Transparent` only if [`capabilities`](Self::capabilities)
    /// allows it.
    ///
    /// # Errors
    /// [`WindowError::Host`] when the surface cannot be made.
    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError>;

    /// Closes the window. Closing a closed window does nothing.
    fn close(&self);

    /// Whether a window is open and its size and backdrop.
    fn state(&self) -> WindowState;
}

/// The colours of the cast, for choosing a key that cannot be confused with the character. The stage implements it
/// later.
pub trait PaletteSource: Service {
    /// The palette of `scene`, or of the scene on air when `None`. Empty when the scene has no cast.
    fn colours(&self, scene: Option<&str>) -> Vec<Colour>;
}
