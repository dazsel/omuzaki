//! The clean output window's decisions as pure code: the exact physical size at any DPI, and a key colour that cannot
//! be confused with the cast.
//!
//! Window capture is the fallback for capture tools with no Spout, NDI or camera route (BR-01, BR-02, BR-04), so the
//! default backdrop is a **key colour**, which every tool can capture, not transparency, which only some can. The window
//! itself (the OS surface) belongs to the Live UI; it reaches this module through [`WindowHost`](omuzaki_stream_api::window::WindowHost)
//! and is told what to build by a [`WindowPlan`], so the part a person's capture tool depends on is tested without a
//! display:
//!
//! - [`plan`]: with [`SizePolicy::Output`] the client area is exactly the output size in physical pixels at any DPI scale
//!   factor, so a capture tool grabs one pixel per output pixel;
//! - [`pick_key`]: the first of three keying colours that stays farthest from every colour in the cast's palette;
//! - [`check_title`]: capture tools find the window again by its title, so a title is short, printable and stable
//!   ([`DEFAULT_TITLE`](omuzaki_stream_api::window::DEFAULT_TITLE) never changes).

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::window::{
    DEFAULT_KEY, MAX_SCALE, MAX_SCALE_FACTOR, MAX_TITLE_CHARS, MIN_SCALE, MIN_SCALE_FACTOR, MIN_TITLE_CHARS,
    SizePolicy, WindowError, WindowPlan,
};
use serde::{Deserialize, Serialize};

/// The most physical pixels a scaled window has per side.
pub const MAX_SIDE: u32 = 8192;
/// The key candidates [`pick_key`] chooses from, in order of preference: chroma green, magenta, chroma blue.
pub const KEY_CANDIDATES: [Colour; 3] = [DEFAULT_KEY, Colour::hex(0x00ff_00ff), Colour::hex(0x0000_47bb)];
/// A key whose [`KeyChoice::margin`] is below this distance is too close to a palette colour to key cleanly.
pub const COLLISION_MARGIN: f32 = 120.0;

/// Plans the window: the physical size, the logical size and the scale factor it was made for.
///
/// With [`SizePolicy::Output`] the physical size is `output` itself for any `scale_factor` from
/// [`MIN_SCALE_FACTOR`] to [`MAX_SCALE_FACTOR`]; with [`SizePolicy::Scale`] it is `round(output * s)`, at least `1` and
/// at most [`MAX_SIDE`] per side (a scale that would pass it is lowered for both sides, so the aspect ratio holds). The logical size is `physical / scale_factor`.
///
/// # Errors
/// [`WindowError::ScaleFactor`] for a NaN, zero or out-of-range factor, [`WindowError::Scale`] for a bad
/// [`SizePolicy::Scale`], [`WindowError::EmptyOutput`] for a zero side, [`WindowError::Unsupported`] for a policy
/// newer than this code.
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
        other => return Err(WindowError::Unsupported(format!("size policy {other:?}"))),
    };
    Ok(WindowPlan::new(
        physical,
        (f64::from(physical.x) / scale_factor, f64::from(physical.y) / scale_factor),
        scale_factor,
    ))
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
