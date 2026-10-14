//! Width classes and the collapse order (`shell.md` section 1).
//!
//! Everything here is arithmetic on the window size in points, so it holds at any OS scale and is tested without egui.

use egui::Vec2;

/// The four width classes of the shell.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WidthClass {
    /// 1920 pt and wider.
    Xl,
    /// 1600 to 1919.
    L,
    /// 1366 to 1599.
    M,
    /// 1280 to 1365 (and the 1280 minimum).
    S,
}

impl WidthClass {
    /// The class of a window `width` points wide.
    pub fn of(width: f32) -> Self {
        if width >= 1920.0 {
            Self::Xl
        } else if width >= 1600.0 {
            Self::L
        } else if width >= 1366.0 {
            Self::M
        } else {
            Self::S
        }
    }
}

/// Smallest window the shell supports: smaller is enforced by the window, and clamped here.
pub const MIN_WINDOW: Vec2 = Vec2::new(1280.0, 720.0);
/// The viewport never goes below this (`shell.md` section 1).
pub const MIN_CENTER: Vec2 = Vec2::new(480.0, 320.0);
/// Top bar height.
pub const TOP_BAR: f32 = 40.0;
/// Status bar height.
pub const STATUS_BAR: f32 = 24.0;
/// A dock collapsed to its icon rail.
pub const RAIL: f32 = 48.0;
/// Shortest bottom dock when open, and tallest.
pub const BOTTOM_MIN: f32 = 160.0;
/// Tallest bottom dock when open.
pub const BOTTOM_MAX: f32 = 320.0;
/// Below this height Agent and History merge into the inspector's tab group.
pub const TALL_ENOUGH: f32 = 800.0;

/// How a side dock is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DockMode {
    /// Open at this width.
    Open(f32),
    /// Collapsed to a 48 pt icon rail; it opens as an overlay drawer.
    Rail,
}

impl DockMode {
    /// The width the dock takes in the layout.
    pub const fn width(self) -> f32 {
        match self {
            Self::Open(w) => w,
            Self::Rail => RAIL,
        }
    }
}

/// What the window size decides about the shell's chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "independent facts about the chrome, each read on its own")]
pub struct Plan {
    /// The width class.
    pub class: WidthClass,
    /// The left dock.
    pub left: DockMode,
    /// The right dock.
    pub right: DockMode,
    /// The bottom dock's height: 0 when collapsed.
    pub bottom: f32,
    /// The Library is a tab beside the Outliner (class M and narrower).
    pub library_as_tab: bool,
    /// Agent and History are tabs of the inspector's group (a short window).
    pub agent_history_merged: bool,
    /// Inactive workspace tabs show only an icon.
    pub icon_tabs: bool,
    /// Go Live is an icon with a tooltip.
    pub go_live_icon: bool,
}

impl Plan {
    /// The plan for a window of `size`. `bottom_wanted` is the bottom dock's height when the workspace has one and it is
    /// open (0 otherwise). Docks collapse in the order bottom, Library, Agent and History into a tab, left dock to a
    /// rail, right dock to a rail, until the viewport has its minimum; the inspector is the last panel to go.
    pub fn for_window(size: Vec2, bottom_wanted: f32) -> Self {
        let size = size.max(MIN_WINDOW);
        let class = WidthClass::of(size.x);
        let (left_w, right_w) = match class {
            WidthClass::Xl | WidthClass::L => (300.0, 360.0),
            WidthClass::M => (240.0, 300.0),
            WidthClass::S => (240.0, 288.0),
        };
        // Class S shows the left dock as a rail from the start.
        let mut left = if class == WidthClass::S { DockMode::Rail } else { DockMode::Open(left_w) };
        let mut right = DockMode::Open(right_w);
        let mut bottom = bottom_wanted.clamp(0.0, BOTTOM_MAX);
        if bottom > 0.0 {
            bottom = bottom.max(BOTTOM_MIN);
        }
        let room_h = size.y - TOP_BAR - STATUS_BAR;
        if room_h - bottom < MIN_CENTER.y {
            bottom = 0.0;
        }
        if size.x - left.width() - right.width() < MIN_CENTER.x {
            left = DockMode::Rail;
        }
        if size.x - left.width() - right.width() < MIN_CENTER.x {
            right = DockMode::Rail;
        }
        Self {
            class,
            left,
            right,
            bottom,
            library_as_tab: matches!(class, WidthClass::M | WidthClass::S),
            agent_history_merged: size.y < TALL_ENOUGH,
            icon_tabs: matches!(class, WidthClass::M | WidthClass::S),
            go_live_icon: class == WidthClass::S,
        }
    }

    /// The viewport's size for a window of `size`.
    pub fn center(&self, size: Vec2) -> Vec2 {
        let size = size.max(MIN_WINDOW);
        Vec2::new(size.x - self.left.width() - self.right.width(), size.y - TOP_BAR - STATUS_BAR - self.bottom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_classes_start_at_the_widths_the_spec_gives() {
        for (width, class) in [
            (2560.0, WidthClass::Xl),
            (1920.0, WidthClass::Xl),
            (1919.0, WidthClass::L),
            (1600.0, WidthClass::L),
            (1599.0, WidthClass::M),
            (1366.0, WidthClass::M),
            (1365.0, WidthClass::S),
            (1280.0, WidthClass::S),
        ] {
            assert_eq!(WidthClass::of(width), class, "{width}");
        }
    }

    #[test]
    fn the_two_reference_screens_have_the_documented_docks() {
        let xl = Plan::for_window(Vec2::new(1920.0, 1080.0), 0.0);
        assert_eq!((xl.left, xl.right), (DockMode::Open(300.0), DockMode::Open(360.0)));
        assert!(!xl.library_as_tab && !xl.icon_tabs && !xl.go_live_icon && !xl.agent_history_merged);
        let m = Plan::for_window(Vec2::new(1366.0, 768.0), 0.0);
        assert_eq!((m.left, m.right), (DockMode::Open(240.0), DockMode::Open(300.0)));
        assert!(m.library_as_tab && m.icon_tabs && !m.go_live_icon);
        assert!(m.agent_history_merged, "768 is shorter than 800");
    }

    #[test]
    fn class_s_has_a_left_rail_a_288_inspector_and_an_icon_go_live() {
        let s = Plan::for_window(Vec2::new(1280.0, 720.0), 0.0);
        assert_eq!((s.left, s.right), (DockMode::Rail, DockMode::Open(288.0)));
        assert!(s.go_live_icon);
    }

    #[test]
    fn the_bottom_dock_is_clamped_to_its_range_and_never_starves_the_viewport() {
        assert!((Plan::for_window(Vec2::new(1920.0, 1080.0), 240.0).bottom - 240.0).abs() < f32::EPSILON);
        assert!((Plan::for_window(Vec2::new(1920.0, 1080.0), 100.0).bottom - BOTTOM_MIN).abs() < f32::EPSILON);
        // The tallest bottom dock still leaves the viewport its 320 pt in the shortest window (720 - 64 - 320 = 336).
        let short = Plan::for_window(Vec2::new(1920.0, 720.0), 400.0);
        assert!((short.bottom - BOTTOM_MAX).abs() < f32::EPSILON);
        assert!(short.center(Vec2::new(1920.0, 720.0)).y >= MIN_CENTER.y);
    }

    #[test]
    fn the_viewport_never_goes_below_its_minimum_at_any_size_the_shell_supports() {
        for width in (1280..=3840).step_by(40) {
            for height in (720..=1600).step_by(40) {
                for bottom in [0.0, 160.0, 320.0] {
                    let size = Vec2::new(width as f32, height as f32);
                    let plan = Plan::for_window(size, bottom);
                    let center = plan.center(size);
                    assert!(center.x >= MIN_CENTER.x && center.y >= MIN_CENTER.y, "{size:?} {bottom}: {center:?}");
                    assert_ne!(plan.right, DockMode::Rail, "{size:?}: the inspector is the last to collapse");
                }
            }
        }
    }

    #[test]
    fn a_window_smaller_than_the_minimum_is_planned_as_the_minimum() {
        assert_eq!(Plan::for_window(Vec2::new(800.0, 400.0), 0.0), Plan::for_window(MIN_WINDOW, 0.0));
    }
}
