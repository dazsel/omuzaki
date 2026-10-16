//! [`OutputPlan`]: what the output window should be, and the egui viewport that carries it out.
//!
//! [`OutputHost`](super::OutputHost) builds one from the `WindowPlan` and `WindowSettings` the `output.window.*` ops
//! hand it (`omuzaki_stream_api::window`): the exact size, and the settings in the form egui takes them.

use egui::{Color32, ViewportBuilder, WindowLevel};
use omuzaki_stream_api::window as contract;

use crate::aux_window::{PhysicalSize, Transparency, points_for};

/// The presenting rate: what OBS's window capture and the call apps read at.
pub const DEFAULT_FPS: u32 = 30;
/// The contract's default key colour, broadcast green `#00b140`, as egui holds it.
pub const DEFAULT_KEY: Color32 =
    Color32::from_rgb(contract::DEFAULT_KEY.r, contract::DEFAULT_KEY.g, contract::DEFAULT_KEY.b);

/// What is behind the character in the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OutputBackdrop {
    /// A solid colour, keyed out by the capturing app.
    Key(Color32),
    /// Nothing: the desktop shows through, where a transparent surface is verified; elsewhere the key is used.
    Transparent {
        /// The key the window falls back to.
        fallback: Color32,
    },
}

/// The output window as the settings ask for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPlan {
    /// The client area, exactly, in physical pixels.
    pub size: PhysicalSize,
    /// What is behind the character.
    pub backdrop: OutputBackdrop,
    /// The OS window title, which capture apps list it by.
    pub title: String,
    /// Hide the pointer while it is over the window.
    pub hide_cursor: bool,
    /// Keep the window above others. Off unless the person turns it on.
    pub always_on_top: bool,
    /// How often the window presents, frames per second, even when it is covered.
    pub fps: u32,
}

impl Default for OutputPlan {
    fn default() -> Self {
        Self {
            size: PhysicalSize::FULL_HD,
            backdrop: OutputBackdrop::Key(DEFAULT_KEY),
            title: contract::DEFAULT_TITLE.to_owned(),
            hide_cursor: true,
            always_on_top: false,
            fps: DEFAULT_FPS,
        }
    }
}

/// The backdrop the window really shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// What fills the window behind the frame: the key, or fully transparent.
    pub fill: Color32,
    /// Whether the surface is transparent.
    pub transparent: bool,
    /// Whether the plan asked for transparency and the window fell back to the key (the UI says `fallback`).
    pub fallback: bool,
}

impl OutputPlan {
    /// The backdrop the window shows, given whether a transparent surface is verified here.
    pub fn shown(&self, transparency: Transparency) -> Shown {
        match (self.backdrop, transparency) {
            (OutputBackdrop::Key(key), _) => Shown { fill: key, transparent: false, fallback: false },
            (OutputBackdrop::Transparent { .. }, Transparency::Verified) => {
                Shown { fill: Color32::TRANSPARENT, transparent: true, fallback: false }
            }
            (OutputBackdrop::Transparent { fallback }, Transparency::Fallback) => {
                Shown { fill: fallback, transparent: false, fallback: true }
            }
        }
    }

    /// The viewport for this plan at `pixels_per_point` (the scale the window is expected to open at; it re-plans if
    /// it lands elsewhere), at `position` in points if one is restored. Borderless, not resizable, exactly `size`.
    pub fn viewport(&self, pixels_per_point: f32, shown: Shown, position: Option<egui::Pos2>) -> ViewportBuilder {
        let mut builder = ViewportBuilder::default()
            .with_title(self.title.clone())
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(shown.transparent)
            .with_taskbar(true)
            .with_window_level(if self.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
        if let Some(points) = points_for(self.size, pixels_per_point) {
            builder = builder.with_inner_size(points).with_min_inner_size(points).with_max_inner_size(points);
        }
        if let Some(position) = position {
            builder = builder.with_position(position);
        }
        builder
    }

    /// The time between presented frames.
    pub fn frame_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(1.0 / f64::from(self.fps.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_plan_is_a_clean_full_hd_window_titled_for_capture() {
        let plan = OutputPlan::default();
        assert_eq!(plan.size, PhysicalSize::FULL_HD);
        assert_eq!(plan.title, "Omuzaki Output");
        assert!(plan.hide_cursor && !plan.always_on_top);
        let shown = plan.shown(Transparency::Fallback);
        let viewport = plan.viewport(1.25, shown, None);
        assert_eq!(viewport.title.as_deref(), Some("Omuzaki Output"));
        assert_eq!((viewport.decorations, viewport.resizable), (Some(false), Some(false)));
        assert_eq!(viewport.inner_size, Some(egui::vec2(1536.0, 864.0)));
        assert_eq!(viewport.min_inner_size, viewport.inner_size);
        assert_eq!(viewport.max_inner_size, viewport.inner_size);
        assert_eq!(viewport.window_level, Some(WindowLevel::Normal));
        assert_eq!(viewport.transparent, Some(false));
    }

    #[test]
    fn transparency_is_used_only_where_verified_and_says_fallback_elsewhere() {
        let plan =
            OutputPlan { backdrop: OutputBackdrop::Transparent { fallback: DEFAULT_KEY }, ..OutputPlan::default() };
        let verified = plan.shown(Transparency::Verified);
        assert_eq!(verified, Shown { fill: Color32::TRANSPARENT, transparent: true, fallback: false });
        let fallback = plan.shown(Transparency::Fallback);
        assert_eq!(fallback, Shown { fill: DEFAULT_KEY, transparent: false, fallback: true });
        assert_eq!(plan.viewport(1.0, verified, None).transparent, Some(true));
        assert_eq!(plan.viewport(1.0, fallback, None).transparent, Some(false));
    }

    #[test]
    fn always_on_top_and_the_cadence_follow_the_plan() {
        let plan = OutputPlan { always_on_top: true, fps: 60, ..OutputPlan::default() };
        let viewport = plan.viewport(1.0, plan.shown(Transparency::Fallback), Some(egui::pos2(10.0, 20.0)));
        assert_eq!(viewport.window_level, Some(WindowLevel::AlwaysOnTop));
        assert_eq!(viewport.position, Some(egui::pos2(10.0, 20.0)));
        assert_eq!(plan.frame_interval(), std::time::Duration::from_secs_f64(1.0 / 60.0));
        assert_eq!(OutputPlan { fps: 0, ..plan }.frame_interval(), std::time::Duration::from_secs(1));
    }
}
