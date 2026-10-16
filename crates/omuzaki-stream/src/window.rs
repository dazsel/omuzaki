//! The clean output window's decisions as pure code: the exact physical size at any DPI, and a key colour that cannot
//! be confused with the cast.
//!
//! Window capture is the fallback for capture tools with no Spout, NDI or camera route (BR-01, BR-02, BR-04), so the
//! default backdrop is a **key colour**, which every tool can capture, not transparency, which only some can. The window
//! itself (the OS surface) belongs to the Live UI; it reaches this module through [`WindowHost`](crate::service::WindowHost)
//! and is told what to build by a [`WindowPlan`], so the part a person's capture tool depends on is tested without a
//! display:
//!
//! - [`plan`]: with [`SizePolicy::Output`] the client area is exactly the output size in physical pixels at any DPI scale
//!   factor, so a capture tool grabs one pixel per output pixel;
//! - [`pick_key`]: the first of three keying colours that stays farthest from every colour in the cast's palette;
//! - [`check_title`]: capture tools find the window again by its title, so a title is short, printable and stable
//!   ([`DEFAULT_TITLE`] never changes).

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use serde::{Deserialize, Serialize};

/// The window title when none is asked for. It never changes: capture tools re-find the window by it.
pub const DEFAULT_TITLE: &str = "Omuzaki Output";
/// The default key colour: chroma green, `#00b140`.
pub const DEFAULT_KEY: Colour = Colour::hex(0x0000_b140);
/// The shortest title, in characters.
pub const MIN_TITLE_CHARS: usize = 1;
/// The longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 64;
/// The smallest display scale factor [`plan`] accepts.
pub const MIN_SCALE_FACTOR: f64 = 0.5;
/// The largest display scale factor [`plan`] accepts.
pub const MAX_SCALE_FACTOR: f64 = 4.0;
/// The smallest [`SizePolicy::Scale`] factor.
pub const MIN_SCALE: f32 = 0.1;
/// The largest [`SizePolicy::Scale`] factor.
pub const MAX_SCALE: f32 = 2.0;
/// The most physical pixels a scaled window has per side.
pub const MAX_SIDE: u32 = 8192;
/// The key candidates [`pick_key`] chooses from, in order of preference: chroma green, magenta, chroma blue.
pub const KEY_CANDIDATES: [Colour; 3] = [DEFAULT_KEY, Colour::hex(0x00ff_00ff), Colour::hex(0x0000_47bb)];
/// A key whose [`KeyChoice::margin`] is below this distance is too close to a palette colour to key cleanly.
pub const COLLISION_MARGIN: f32 = 120.0;

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
    /// The output size times a factor (`0.1` to `2.0`), rounded: a smaller preview-sized window.
    Scale(f32),
}

/// What to open: the title, the backdrop, the size policy and the cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSettings {
    /// The window title; see [`check_title`].
    pub title: String,
    /// What shows behind the character.
    pub backdrop: Backdrop,
    /// The client area size.
    pub size: SizePolicy,
    /// Hide the mouse cursor over the window, so it is not captured.
    pub hide_cursor: bool,
}

impl Default for WindowSettings {
    /// `"Omuzaki Output"`, the `#00b140` key, exact output size, cursor hidden.
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_owned(),
            backdrop: Backdrop::Key(DEFAULT_KEY),
            size: SizePolicy::Output,
            hide_cursor: true,
        }
    }
}

/// The exact size to give the window, computed by [`plan`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// The host could not open the window; the text says why. For [`WindowHost`](crate::service::WindowHost)
    /// implementers.
    #[error("the window could not be opened: {0}")]
    Host(String),
}

/// Plans the window: the physical size, the logical size and the scale factor it was made for.
///
/// With [`SizePolicy::Output`] the physical size is `output` itself for any `scale_factor` from
/// [`MIN_SCALE_FACTOR`] to [`MAX_SCALE_FACTOR`]; with [`SizePolicy::Scale`] it is `round(output * s)`, at least `1` and
/// at most [`MAX_SIDE`] per side (a scale that would pass it is lowered for both sides, so the aspect ratio holds). The logical size is `physical / scale_factor`.
///
/// # Errors
/// [`WindowError::ScaleFactor`] for a NaN, zero or out-of-range factor, [`WindowError::Scale`] for a bad
/// [`SizePolicy::Scale`], [`WindowError::EmptyOutput`] for a zero side.
pub fn plan(output: UVec2, policy: SizePolicy, scale_factor: f64) -> Result<WindowPlan, WindowError> {
    if !(MIN_SCALE_FACTOR..=MAX_SCALE_FACTOR).contains(&scale_factor) {
        return Err(WindowError::ScaleFactor(scale_factor));
    }
    if output.x == 0 || output.y == 0 {
        return Err(WindowError::EmptyOutput(output.x, output.y));
    }
    let physical = match policy {
        SizePolicy::Output => output,
        SizePolicy::Scale(s) if (MIN_SCALE..=MAX_SCALE).contains(&s) => scaled_size(output, s),
        SizePolicy::Scale(s) => return Err(WindowError::Scale(s)),
    };
    Ok(WindowPlan {
        physical,
        logical: (f64::from(physical.x) / scale_factor, f64::from(physical.y) / scale_factor),
        scale_factor,
    })
}

/// The [`SizePolicy::Scale`] window: `output * scale`, with one common scale that keeps the longer side within
/// [`MAX_SIDE`] so the aspect ratio survives, then each side rounded to at least `1`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=MAX_SIDE before the cast
fn scaled_size(output: UVec2, scale: f32) -> UVec2 {
    let longest = f64::from(output.x.max(output.y));
    let scale = f64::from(scale).min(f64::from(MAX_SIDE) / longest);
    let side = |side: u32| (f64::from(side) * scale).round().clamp(1.0, f64::from(MAX_SIDE)) as u32;
    UVec2::new(side(output.x), side(output.y))
}

/// Checks a window title: [`MIN_TITLE_CHARS`] to [`MAX_TITLE_CHARS`] characters, none a control character.
///
/// # Errors
/// [`WindowError::Title`] when it is not.
pub fn check_title(title: &str) -> Result<(), WindowError> {
    let chars = title.chars().count();
    if (MIN_TITLE_CHARS..=MAX_TITLE_CHARS).contains(&chars) && !title.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(WindowError::Title)
    }
}

/// The key [`pick_key`] chose and how safe it is.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyChoice {
    /// The key colour to use.
    pub colour: Colour,
    /// The redmean distance from `colour` to the nearest palette colour; infinite for an empty palette.
    pub margin: f32,
    /// `margin` is below [`COLLISION_MARGIN`]: every candidate is close to something in the palette, so keying will
    /// eat part of the character.
    pub collides: bool,
}

/// Picks the key colour that is farthest (redmean distance) from every colour in `palette`.
///
/// The candidates are [`KEY_CANDIDATES`], and a tie goes to the earlier one, so an empty palette, or one that touches
/// all three, picks chroma green. Alpha is ignored: a translucent palette colour still tints what the capture sees.
pub fn pick_key(palette: &[Colour]) -> KeyChoice {
    let margin_of = |key: Colour| palette.iter().map(|c| redmean(key, *c)).fold(f32::INFINITY, f32::min);
    let mut best = KeyChoice { colour: KEY_CANDIDATES[0], margin: margin_of(KEY_CANDIDATES[0]), collides: false };
    for key in &KEY_CANDIDATES[1..] {
        let margin = margin_of(*key);
        if margin > best.margin {
            best = KeyChoice { colour: *key, margin, collides: false };
        }
    }
    best.collides = best.margin < COLLISION_MARGIN;
    best
}

/// The redmean colour distance (a cheap perceptual metric): with `r = (R1 + R2) / 2`,
/// `sqrt((2 + r/256) dR² + 4 dG² + (2 + (255 - r)/256) dB²)`.
fn redmean(a: Colour, b: Colour) -> f32 {
    let (ar, br) = (f64::from(a.r), f64::from(b.r));
    let r = f64::midpoint(ar, br);
    let (dr, dg, db) = (ar - br, f64::from(a.g) - f64::from(b.g), f64::from(a.b) - f64::from(b.b));
    let squared = (2.0 + r / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - r) / 256.0) * db * db;
    #[allow(clippy::cast_possible_truncation)] // a distance below 766 fits an f32
    let distance = squared.sqrt() as f32;
    distance
}

/// What a window host can do.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCaps {
    /// It can make a transparent surface ([`Backdrop::Transparent`]).
    pub transparent: bool,
    /// It can keep the window above others.
    pub always_on_top: bool,
}

/// The output the window shows and the display it opens on: what [`plan`] needs and the ops cannot know.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputSurface {
    /// The output size in pixels.
    pub size: UVec2,
    /// The scale factor of the display the window opens on.
    pub scale_factor: f64,
}

/// Whether the window is open and what it looks like.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WindowState {
    /// No window.
    Closed,
    /// The window is open.
    Open {
        /// Its client area in physical pixels.
        physical: UVec2,
        /// What it shows behind the character.
        backdrop: Backdrop,
    },
}
