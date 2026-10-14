//! The design system: tokens, themes, components and the gallery (`docs/design/ux/design-system.md`).
//!
//! Start with [`theme::apply`] (once, before the first frame), then use the widgets in [`widgets`]. Panels never
//! write a raw colour or size: they use the tokens in [`tokens`] and the components, and the gallery shows each one.

// Pixel and index arithmetic converts between small integers and `f32` throughout (a row index to a y position, a
// 0..1 fraction to a byte). Every such cast is on a value bounded by the screen, a handful of items or a colour
// channel, so none can lose anything that matters; checking each one with `try_from` would bury the layout code.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

pub mod a11y;
#[cfg(any(test, feature = "capture"))]
pub mod audit;
pub mod clock;
pub mod confirm;
pub mod contrast;
pub mod expr;
pub mod focus;
pub mod fonts;
pub mod gallery;
pub mod icons;
pub mod input;
pub mod os_prefs;
pub mod paint;
pub mod strings;
#[cfg(any(test, feature = "capture"))]
pub mod testing;
pub mod text;
pub mod theme;
pub mod tokens;
pub mod widgets;

pub use strings::{English, Msg, Pseudo, Strings};
pub use theme::{Ds, Settings, apply, apply_settings, ds, set_strings};
pub use tokens::{Density, Scale, Status, Theme, ThemeKind};
