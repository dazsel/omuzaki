//! The per-OS window layer: always-on-top, hiding from the taskbar and the window switcher, and click-through.
//!
//! winit covers the common part (transparent attribute, window level, a whole-window `set_cursor_hittest`). What it
//! cannot do (per-pixel click-through, the switcher, a Dock icon) is done here with the OS's own calls. Every feature
//! reports what happened as a [`Feature`], so a run on a real machine prints a table that can be pasted into the research
//! page.

use std::fmt;

use winit::dpi::PhysicalPosition;
use winit::event_loop::EventLoopBuilder;
use winit::window::{Window, WindowAttributes, WindowLevel};

use crate::scene::Mask;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

/// How much of the window lets the mouse through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickThrough {
    /// The whole window takes mouse input.
    Off,
    /// The whole window is click-through (winit's `set_cursor_hittest(false)`).
    Window,
    /// Only pixels with alpha below [`crate::scene::HIT_ALPHA`] let the mouse through.
    Pixel,
}

/// Which layer toggles click-through on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTestBackend {
    /// winit's `set_cursor_hittest`, which sets `WS_EX_LAYERED | WS_EX_TRANSPARENT`.
    Winit,
    /// Direct `SetWindowLongPtrW`, plus `SetLayeredWindowAttributes` so a `DirectComposition` window stays visible.
    Native,
}

/// Where the window sits in the stacking order (winit's `WindowLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// An ordinary window.
    Normal,
    /// Above other windows.
    Top,
    /// Below other windows.
    Bottom,
}

impl From<Level> for WindowLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::Normal => Self::Normal,
            Level::Top => Self::AlwaysOnTop,
            Level::Bottom => Self::AlwaysOnBottom,
        }
    }
}

/// What to ask of the window.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Stacking level.
    pub level: Level,
    /// Hide it from the taskbar or Dock and from Alt-Tab, Cmd-Tab or the pager.
    pub hide_from_switcher: bool,
    /// Click-through behaviour.
    pub click_through: ClickThrough,
    /// Windows only: how click-through is toggled.
    pub hit_test: HitTestBackend,
}

/// What came of one requested feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done; the text says how.
    Applied(String),
    /// This OS or windowing system has no such feature (or winit exposes none).
    Unsupported(&'static str),
    /// The call failed.
    Failed(String),
}

/// One feature and its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feature {
    /// Short name, such as `always_on_top`.
    pub name: &'static str,
    /// What happened.
    pub outcome: Outcome,
}

impl Feature {
    pub(crate) fn applied(name: &'static str, how: impl Into<String>) -> Self {
        Self { name, outcome: Outcome::Applied(how.into()) }
    }

    pub(crate) fn unsupported(name: &'static str, why: &'static str) -> Self {
        Self { name, outcome: Outcome::Unsupported(why) }
    }

    pub(crate) fn failed(name: &'static str, why: impl fmt::Display) -> Self {
        Self { name, outcome: Outcome::Failed(why.to_string()) }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.outcome {
            Outcome::Applied(how) => write!(f, "[ok]          {}: {how}", self.name),
            Outcome::Unsupported(why) => write!(f, "[unsupported] {}: {why}", self.name),
            Outcome::Failed(why) => write!(f, "[FAILED]      {}: {why}", self.name),
        }
    }
}

/// Window attributes shared by every OS, plus the OS's own.
pub fn window_attributes(size: (u32, u32), options: &Options) -> WindowAttributes {
    let attributes = WindowAttributes::default()
        .with_title("Omuzaki mascot window spike")
        .with_inner_size(winit::dpi::PhysicalSize::new(size.0, size.1))
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_window_level(options.level.into());
    os::attributes(attributes, *options)
}

/// OS-specific event loop settings (macOS activation policy).
pub fn configure_event_loop<T: 'static>(builder: &mut EventLoopBuilder<T>, options: &Options) {
    os::configure_event_loop(builder, *options);
}

/// Per-window state for the features that need more than a one-off call.
pub struct State {
    options: Options,
    os: os::State,
    /// Last value given to `set_cursor_hittest` by the per-pixel poller, to call it only on a change.
    last_hit: Option<bool>,
    /// The latest failure of the per-pixel poller, for the caller to report once.
    poll_error: Option<String>,
}

impl State {
    /// Applies the options to a created window and returns what happened per feature.
    pub fn new(window: &Window, options: Options, mask: &Mask<'_>) -> (Self, Vec<Feature>) {
        let mut features = Vec::new();
        features.push(window_level(window, options));
        let (os, mut extra) = os::State::new(window, options, mask);
        features.append(&mut extra);
        let mut state = Self { options, os, last_hit: None, poll_error: None };
        if options.click_through == ClickThrough::Window {
            features.push(state.set_window_click_through(window, true));
        }
        (state, features)
    }

    fn set_window_click_through(&mut self, window: &Window, through: bool) -> Feature {
        match self.os.set_click_through(window, self.options, through) {
            Ok(how) => Feature::applied("click_through_window", how),
            Err(e) => Feature::failed("click_through_window", e),
        }
    }

    /// Called about 60 times a second. On Windows and macOS, per-pixel click-through needs the global cursor position,
    /// because a click-through window receives no mouse events to tell where the cursor is.
    pub fn tick(&mut self, window: &Window, mask: &Mask<'_>) {
        if self.options.click_through != ClickThrough::Pixel || !self.os.polls_cursor() {
            return;
        }
        let Some(cursor) = self.os.cursor_position(window) else { return };
        let Ok(origin) = window.inner_position() else { return };
        let (x, y) = local(cursor, origin);
        let hit = mask.hit(x, y);
        if self.last_hit != Some(hit) {
            self.last_hit = Some(hit);
            if let Err(e) = self.os.set_click_through(window, self.options, !hit) {
                self.poll_error = Some(e);
            }
        }
    }

    /// Takes the latest per-pixel toggle failure, if any, so it is reported once rather than every frame.
    pub fn take_poll_error(&mut self) -> Option<String> {
        self.poll_error.take()
    }

    /// Called when the mask changes (the window size, or the opacity the mascot is shown at): an input shape is in window
    /// pixels and covers only what is visible, so it is rebuilt.
    pub fn refresh(&mut self, window: &Window, mask: &Mask<'_>) -> Option<Feature> {
        // The poller's cached answer was for the old mask.
        self.last_hit = None;
        if self.options.click_through == ClickThrough::Pixel {
            return self.os.refresh(window, mask);
        }
        None
    }

    /// Reads the OS state back (properties, styles, shapes) so a probe run shows what was really set.
    pub fn describe(&self, window: &Window) -> Vec<String> {
        self.os.describe(window)
    }
}

/// Cursor position relative to the window's client area, in window pixels.
fn local(cursor: (f64, f64), origin: PhysicalPosition<i32>) -> (f32, f32) {
    ((cursor.0 - f64::from(origin.x)) as f32, (cursor.1 - f64::from(origin.y)) as f32)
}

fn window_level(window: &Window, options: Options) -> Feature {
    if options.level == Level::Normal {
        return Feature::unsupported("window_level", "not requested (normal level)");
    }
    // winit sets the level at creation; Wayland ignores it (no such protocol outside layer-shell).
    if os::is_wayland(window) {
        return Feature::unsupported("window_level", "Wayland has no stacking request for ordinary windows");
    }
    window.set_window_level(options.level.into());
    Feature::applied(
        "window_level",
        format!("WindowLevel for {:?} (check by overlapping another window)", options.level),
    )
}

/// CPU time this process has used so far (user plus system, all threads), or `None` where the platform call fails.
pub fn process_cpu_seconds() -> Option<f64> {
    os::process_cpu_seconds()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_subtracts_the_window_origin() {
        assert_eq!(local((110.0, 220.5), PhysicalPosition::new(100, 200)), (10.0, 20.5));
        assert_eq!(local((90.0, 190.0), PhysicalPosition::new(100, 200)), (-10.0, -10.0));
    }

    #[test]
    fn feature_lines_are_labelled() {
        assert!(Feature::applied("a", "x").to_string().starts_with("[ok]"));
        assert!(Feature::unsupported("a", "y").to_string().starts_with("[unsupported]"));
        assert!(Feature::failed("a", "z").to_string().starts_with("[FAILED]"));
    }
}
