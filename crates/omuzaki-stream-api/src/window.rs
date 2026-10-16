//! The clean output window's contract: the window host and the cast's palette as services, and the data they pass.
//!
//! The Live UI implements [`WindowHost`] (it owns the OS surface) and the stage implements [`PaletteSource`]; the
//! `output.window.*` ops in `omuzaki-stream` drive them and decide what the window looks like with pure code that
//! needs no display. Types and traits only, so the crate keeps building for `wasm32-unknown-unknown`.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::Service;
use serde::{Deserialize, Serialize};

/// The window title when none is asked for. It never changes: capture tools re-find the window by it.
pub const DEFAULT_TITLE: &str = "Omuzaki Output";
/// The default key colour: chroma green, `#00b140`.
pub const DEFAULT_KEY: Colour = Colour::hex(0x0000_b140);
/// The shortest title, in characters.
pub const MIN_TITLE_CHARS: usize = 1;
/// The longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 64;
/// The smallest display scale factor `plan` (in `omuzaki-stream`) accepts.
pub const MIN_SCALE_FACTOR: f64 = 0.5;
/// The largest display scale factor `plan` (in `omuzaki-stream`) accepts.
pub const MAX_SCALE_FACTOR: f64 = 4.0;
/// The smallest [`SizePolicy::Scale`] factor.
pub const MIN_SCALE: f32 = 0.1;
/// The largest [`SizePolicy::Scale`] factor.
pub const MAX_SCALE: f32 = 2.0;

/// What the window shows behind the character.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backdrop {
    /// A transparent surface. Only some capture tools and some hosts support it ([`WindowCaps::transparent`]).
    Transparent,
    /// A flat key colour, which every capture tool can remove.
    Key(Colour),
}

/// How big the window's client area is.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizePolicy {
    /// Exactly the output size in physical pixels, at any DPI scale factor.
    Output,
    /// The output size times a factor ([`MIN_SCALE`] to [`MAX_SCALE`]), each side rounded: below `1` a smaller window
    /// (a preview), above `1` a request for a larger one. `plan` (in `omuzaki-stream`) caps each side at its `MAX_SIDE`
    /// with the aspect ratio kept, so an output already at or past the cap comes out at the cap.
    Scale(f32),
}

/// What to open: the title, the backdrop, the size policy, the cursor and the stacking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WindowSettings {
    /// The window title; see `check_title` (in `omuzaki-stream`).
    pub title: String,
    /// What shows behind the character.
    pub backdrop: Backdrop,
    /// The client area size.
    pub size: SizePolicy,
    /// Hide the mouse cursor over the window, so it is not captured.
    pub hide_cursor: bool,
    /// Keep the window above other windows. Off unless the person turns it on, and `true` only when
    /// [`WindowCaps::always_on_top`] allows it (the ops resolve it, as they do the backdrop).
    #[serde(default)]
    pub always_on_top: bool,
}

impl Default for WindowSettings {
    /// `"Omuzaki Output"`, the `#00b140` key, exact output size, cursor hidden, not on top.
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_owned(),
            backdrop: Backdrop::Key(DEFAULT_KEY),
            size: SizePolicy::Output,
            hide_cursor: true,
            always_on_top: false,
        }
    }
}

/// The exact size to give the window, computed by `plan` (in `omuzaki-stream`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WindowPlan {
    /// The client area in physical pixels: what a capture tool grabs.
    pub physical: UVec2,
    /// The same size in logical units (`physical / scale_factor`), for window systems that size in logical units.
    pub logical: (f64, f64),
    /// The display scale factor the plan was made for.
    pub scale_factor: f64,
}

/// Why a window cannot be planned or opened.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WindowError {
    /// The display scale factor is NaN or outside [`MIN_SCALE_FACTOR`] to [`MAX_SCALE_FACTOR`].
    #[error("the display scale factor {0} must be {MIN_SCALE_FACTOR} to {MAX_SCALE_FACTOR}")]
    ScaleFactor(f64),
    /// The [`SizePolicy::Scale`] factor is NaN or outside [`MIN_SCALE`] to [`MAX_SCALE`].
    #[error("the window scale {0} must be {MIN_SCALE} to {MAX_SCALE}")]
    Scale(f32),
    /// The output has a zero side.
    #[error("the output size {0}x{1} must be at least 1x1")]
    EmptyOutput(u32, u32),
    /// The title is empty, too long or holds a control character.
    #[error("the title must be {MIN_TITLE_CHARS} to {MAX_TITLE_CHARS} characters with no control characters")]
    Title,
    /// The host could not open the window; the text says why. For [`WindowHost`]
    /// implementers.
    #[error("the window could not be opened: {0}")]
    Host(String),
}

/// What a window host can do.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WindowCaps {
    /// It can make a transparent surface ([`Backdrop::Transparent`]).
    pub transparent: bool,
    /// It can keep the window above others.
    pub always_on_top: bool,
}

/// The output the window shows and the display it opens on: what `plan` (in `omuzaki-stream`) needs and the ops cannot know.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OutputSurface {
    /// The output size in pixels.
    pub size: UVec2,
    /// The scale factor of the display the window opens on.
    pub scale_factor: f64,
}

/// Whether the window is open and what it looks like. Non-exhaustive, so a later state (a window being created, say)
/// is additive: a match outside this crate keeps a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WindowState {
    /// No window.
    Closed,
    /// The window is open. Build it with [`WindowState::open`].
    #[non_exhaustive]
    Open {
        /// Its client area in physical pixels.
        physical: UVec2,
        /// What it shows behind the character.
        backdrop: Backdrop,
        /// It is minimised. A minimised window presents nothing, so no capture tool can grab it; a person or an
        /// agent restores it rather than trusting that it is on air.
        #[serde(default)]
        minimised: bool,
    },
}

impl WindowPlan {
    /// A plan of `physical` pixels (`logical` in logical units) for a display with `scale_factor`.
    #[must_use]
    pub const fn new(physical: UVec2, logical: (f64, f64), scale_factor: f64) -> Self {
        Self { physical, logical, scale_factor }
    }
}

impl WindowCaps {
    /// A host that can make a transparent surface if `transparent`, and keep the window on top if `always_on_top`.
    #[must_use]
    pub const fn new(transparent: bool, always_on_top: bool) -> Self {
        Self { transparent, always_on_top }
    }
}

impl OutputSurface {
    /// An output of `size` pixels shown on a display with `scale_factor`.
    #[must_use]
    pub const fn new(size: UVec2, scale_factor: f64) -> Self {
        Self { size, scale_factor }
    }
}

impl WindowState {
    /// An open window, not minimised, with a client area of `physical` pixels showing `backdrop`.
    #[must_use]
    pub const fn open(physical: UVec2, backdrop: Backdrop) -> Self {
        Self::Open { physical, backdrop, minimised: false }
    }

    /// The same state, minimised or not; a closed window stays closed.
    #[must_use]
    pub const fn with_minimised(self, minimised: bool) -> Self {
        match self {
            Self::Open { physical, backdrop, .. } => Self::Open { physical, backdrop, minimised },
            Self::Closed => Self::Closed,
        }
    }
}

/// The host of the clean output window: the part that owns the OS surface. Implemented by the Live UI; the
/// `output.window.*` ops drive it and decide nothing a display is needed to test (`omuzaki-stream`'s `window` module).
///
/// All methods take `&self` and may be called from any thread; an implementation synchronises itself and must not
/// block on the render loop. `open` and `close` run on the thread that executes the op.
pub trait WindowHost: Service {
    /// What this host can do. Constant for the life of the host.
    fn capabilities(&self) -> WindowCaps;

    /// The output size and the scale factor of the display the window would open on, now. `plan` (in `omuzaki-stream`)
    /// turns it into the window's exact size.
    fn surface(&self) -> OutputSurface;

    /// Opens the window with exactly `plan.physical` pixels of client area. Opening while a window is open replaces
    /// it. `settings.backdrop` and `settings.always_on_top` are already resolved: `Transparent` and `true` only where
    /// [`capabilities`](Self::capabilities) allows them.
    ///
    /// A host whose windows belong to a UI thread may queue the request and return `Ok` at once: the window then
    /// appears on that thread's next frame, and [`state`](Self::state) reports `Open` from then on. A surface that
    /// fails after the queueing shows as `state` staying `Closed`.
    ///
    /// # Errors
    /// [`WindowError::Host`] when the surface cannot be made.
    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError>;

    /// Closes the window. Closing a closed window does nothing.
    fn close(&self);

    /// Whether a window is open, and its size, backdrop and whether it is minimised.
    fn state(&self) -> WindowState;
}

/// The colours of the cast, for choosing a key that cannot be confused with the character. The stage implements it
/// later.
pub trait PaletteSource: Service {
    /// The palette of `scene`, or of the scene on air when `None`. Empty when the scene has no cast.
    fn colours(&self, scene: Option<&str>) -> Vec<Colour>;
}
