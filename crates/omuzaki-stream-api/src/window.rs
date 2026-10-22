//! The clean output window's contract: the window host and the cast's palette as services, and the data they pass.
//!
//! The Live UI implements [`WindowHost`] (it owns the OS surface) and the stage implements [`PaletteSource`]; the
//! `output.window.*` ops in `omuzaki-stream` drive them and decide what the window looks like with pure code that
//! needs no display. Types and traits only, so the crate keeps building for `wasm32-unknown-unknown`.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::Service;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The window title when none is asked for. It never changes: capture tools re-find the window by it.
pub const DEFAULT_TITLE: &str = "Omuzaki Output";
/// The default key colour: chroma green, `#00b140`.
pub const DEFAULT_KEY: Colour = Colour::hex(0x0000_b140);
/// The shortest title, in characters.
pub const MIN_TITLE_CHARS: usize = 1;
/// The longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 64;
/// The faintest a desktop window may be ([`DesktopSettings::opacity`]): fainter, a person can lose it.
pub const MIN_OPACITY: f32 = 0.1;
/// The window scale a desktop window opens at when none is asked for: half the output, a character beside the
/// person's work rather than over all of it.
pub const DESKTOP_SCALE: f32 = 0.5;
/// The smallest display scale factor `plan` (in `omuzaki-stream`) accepts.
pub const MIN_SCALE_FACTOR: f64 = 0.5;
/// The largest display scale factor `plan` (in `omuzaki-stream`) accepts.
pub const MAX_SCALE_FACTOR: f64 = 4.0;
/// The smallest [`SizePolicy::Scale`] factor.
pub const MIN_SCALE: f32 = 0.1;
/// The largest [`SizePolicy::Scale`] factor.
pub const MAX_SCALE: f32 = 2.0;

/// What the window shows behind the character. Non-exhaustive, so a later backdrop is additive (ADR 0054): a match
/// outside this crate keeps a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Backdrop {
    /// A transparent surface. Only some capture tools and some hosts support it ([`WindowCaps::transparent`]).
    Transparent,
    /// A flat key colour, which every capture tool can remove.
    Key(Colour),
}

/// How big the window's client area is. Non-exhaustive, so a later policy is additive (ADR 0054): a match outside this
/// crate keeps a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SizePolicy {
    /// Exactly the output size in physical pixels, at any DPI scale factor.
    Output,
    /// The output size times a factor ([`MIN_SCALE`] to [`MAX_SCALE`]), each side rounded: below `1` a smaller window
    /// (a preview), above `1` a request for a larger one. `plan` (in `omuzaki-stream`) caps each side at its `MAX_SIDE`
    /// with the aspect ratio kept, so an output already at or past the cap comes out at the cap.
    Scale(f32),
}

/// Where the character is shown. Nothing here is a use case: a stream, a companion on the desktop and an AI co-host
/// are the same modes mixed with other blocks. Non-exhaustive, so a later mode is additive (ADR 0054).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WindowMode {
    /// Only in the studio's own viewport: no output window.
    Studio,
    /// The clean window a capture tool grabs: borderless, an exact size, in the taskbar, a key colour or transparent
    /// backdrop. The default.
    #[default]
    Capture,
    /// The character alone on the desktop: transparent, above other windows, out of the taskbar and Alt-Tab,
    /// clicked through as [`DesktopSettings::click_through`] says, and moved by dragging the character.
    Desktop,
}

/// Which clicks on a desktop window reach the windows under it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClickThrough {
    /// None: the whole window takes clicks.
    Off,
    /// Clicks on transparent pixels pass through; the character itself takes them (to drag it, or open its menu).
    /// The default.
    #[default]
    Transparent,
    /// Every click passes through, for a character over a game or a document. Only a person turns it on: the way back
    /// is the studio's toggle and its shortcut, never the character.
    Full,
}

/// How a desktop window looks and takes clicks.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DesktopSettings {
    /// How opaque the character is, [`MIN_OPACITY`] to 1.
    pub opacity: f32,
    /// Which clicks pass through.
    pub click_through: ClickThrough,
}

impl Default for DesktopSettings {
    /// Fully opaque; clicks on transparent pixels pass through.
    fn default() -> Self {
        Self { opacity: 1.0, click_through: ClickThrough::Transparent }
    }
}

impl DesktopSettings {
    /// Settings of `opacity` and `click_through`. [`check`](Self::check) says whether they are in range.
    #[must_use]
    pub const fn new(opacity: f32, click_through: ClickThrough) -> Self {
        Self { opacity, click_through }
    }

    /// Whether the opacity is a number from [`MIN_OPACITY`] to 1.
    ///
    /// # Errors
    /// [`WindowError::Opacity`] otherwise.
    pub fn check(&self) -> Result<(), WindowError> {
        if (MIN_OPACITY..=1.0).contains(&self.opacity) { Ok(()) } else { Err(WindowError::Opacity(self.opacity)) }
    }
}

/// Whether this machine can show a [`WindowMode`], and why not when it can't, in words a person can act on ("Desktop
/// mode needs a compositor on this Linux session").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ModeSupport {
    /// The mode.
    pub mode: WindowMode,
    /// Whether it can be opened here.
    pub available: bool,
    /// Why not, when it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ModeSupport {
    /// `mode` can be opened.
    #[must_use]
    pub const fn available(mode: WindowMode) -> Self {
        Self { mode, available: true, reason: None }
    }

    /// `mode` can't be opened, because `reason`.
    #[must_use]
    pub fn unavailable(mode: WindowMode, reason: impl Into<String>) -> Self {
        Self { mode, available: false, reason: Some(reason.into()) }
    }
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
    /// Where the window shows the character; never [`WindowMode::Studio`], which opens no window. A host opening a
    /// [`WindowMode::Desktop`] window gets a transparent backdrop, `always_on_top` and the cursor shown, already
    /// resolved by the ops.
    #[serde(default)]
    pub mode: WindowMode,
    /// How a desktop window looks and takes clicks; ignored in [`WindowMode::Capture`].
    #[serde(default)]
    pub desktop: DesktopSettings,
}

impl Default for WindowSettings {
    /// `"Omuzaki Output"`, the `#00b140` key, exact output size, cursor hidden, not on top, for capture.
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_owned(),
            backdrop: Backdrop::Key(DEFAULT_KEY),
            size: SizePolicy::Output,
            hide_cursor: true,
            always_on_top: false,
            mode: WindowMode::Capture,
            desktop: DesktopSettings::default(),
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
    /// A desktop window's opacity is NaN or outside [`MIN_OPACITY`] to 1.
    #[error("the opacity {0} must be {MIN_OPACITY} to 1")]
    Opacity(f32),
    /// A [`SizePolicy`] or [`Backdrop`] newer than the code that was asked to use it (both are non-exhaustive); the
    /// text names it.
    #[error("this build does not support the {0}")]
    Unsupported(String),
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
        /// Where it shows the character.
        #[serde(default)]
        mode: WindowMode,
        /// How a desktop window looks and takes clicks; `None` in [`WindowMode::Capture`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desktop: Option<DesktopSettings>,
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
        Self::Open { physical, backdrop, minimised: false, mode: WindowMode::Capture, desktop: None }
    }

    /// The same open window, showing the character as `settings` say: their mode, and their desktop settings in
    /// [`WindowMode::Desktop`]. A closed window stays closed.
    #[must_use]
    pub const fn with_mode(self, settings: &WindowSettings) -> Self {
        match self {
            Self::Open { physical, backdrop, minimised, .. } => {
                let desktop = if matches!(settings.mode, WindowMode::Desktop) { Some(settings.desktop) } else { None };
                Self::Open { physical, backdrop, minimised, mode: settings.mode, desktop }
            }
            Self::Closed => Self::Closed,
        }
    }

    /// The same open desktop window with `desktop` instead; any other state is returned as it is.
    #[must_use]
    pub const fn with_desktop(self, desktop: DesktopSettings) -> Self {
        match self {
            Self::Open { physical, backdrop, minimised, mode: WindowMode::Desktop, .. } => {
                Self::Open { physical, backdrop, minimised, mode: WindowMode::Desktop, desktop: Some(desktop) }
            }
            other => other,
        }
    }

    /// The same state, minimised or not; a closed window stays closed.
    #[must_use]
    pub const fn with_minimised(self, minimised: bool) -> Self {
        match self {
            Self::Open { physical, backdrop, mode, desktop, .. } => {
                Self::Open { physical, backdrop, minimised, mode, desktop }
            }
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

    /// Which [`WindowMode`]s this machine can show, each with the reason when it can't. The default: the studio and
    /// capture always, and the desktop never, since a host that hasn't built it would open a capture window in its
    /// place.
    fn modes(&self) -> Vec<ModeSupport> {
        vec![
            ModeSupport::available(WindowMode::Studio),
            ModeSupport::available(WindowMode::Capture),
            ModeSupport::unavailable(WindowMode::Desktop, "This build can't show the character on the desktop yet"),
        ]
    }

    /// Changes an open desktop window's opacity and click-through at once, without reopening it. `desktop` is already
    /// checked.
    ///
    /// # Errors
    /// [`WindowError::Unsupported`] by default, and from a host whose window is not open in [`WindowMode::Desktop`];
    /// [`WindowError::Host`] when the OS refuses.
    fn set_desktop(&self, desktop: &DesktopSettings) -> Result<(), WindowError> {
        let _ = desktop;
        Err(WindowError::Unsupported("desktop window".into()))
    }
}

/// The colours of the cast, for choosing a key that cannot be confused with the character. The stage implements it
/// later.
pub trait PaletteSource: Service {
    /// The palette of `scene`, or of the scene on air when `None`. Empty when the scene has no cast.
    fn colours(&self, scene: Option<&str>) -> Vec<Colour>;
}
