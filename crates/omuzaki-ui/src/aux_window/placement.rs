//! [`PlacementBook`]: where a secondary window was, per monitor arrangement.
//!
//! A window that was on a projector last week must not open off-screen today. So a position is remembered with the
//! arrangement it was on (the monitor's physical size and scale) and restored only when the app sees that monitor
//! again; otherwise the window opens where the OS puts it.

use serde::{Deserialize, Serialize};

use super::size::PhysicalSize;

/// Arrangements a book remembers; the oldest is forgotten first.
pub const MAX_ARRANGEMENTS: usize = 8;

/// A monitor as the app can identify it: its size in physical pixels and its scale (in thousandths, so it compares
/// exactly).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Arrangement {
    /// The monitor's size in physical pixels.
    pub monitor: PhysicalSize,
    /// The monitor's scale × 1000 (`1250` is 125 %).
    pub scale_milli: u32,
}

impl Arrangement {
    /// The monitor `monitor` pixels big at `scale` (`1.25` for 125 %).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a scale is a small positive number
    pub fn new(monitor: PhysicalSize, scale: f32) -> Self {
        Self { monitor, scale_milli: (scale.max(0.0) * 1000.0).round() as u32 }
    }
}

/// One remembered position: the window's top-left corner in physical desktop pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    on: Arrangement,
    x: i32,
    y: i32,
}

/// Remembered positions, newest last. Serialisable, so the shell's settings can keep it between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementBook {
    entries: Vec<Entry>,
}

impl PlacementBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers that the window's top-left corner was at (`x`, `y`) physical pixels while on `on`, replacing what was
    /// remembered for that arrangement.
    pub fn remember(&mut self, on: Arrangement, x: i32, y: i32) {
        self.entries.retain(|entry| entry.on != on);
        if self.entries.len() >= MAX_ARRANGEMENTS {
            self.entries.remove(0);
        }
        self.entries.push(Entry { on, x, y });
    }

    /// The position to restore: the newest one remembered for a monitor in `present` (the monitors the app sees now),
    /// or `None`, in which case the OS places the window.
    pub fn restore(&self, present: &[Arrangement]) -> Option<(i32, i32)> {
        self.entries.iter().rev().find(|entry| present.contains(&entry.on)).map(|entry| (entry.x, entry.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(w: u32, h: u32, scale: f32) -> Arrangement {
        Arrangement::new(PhysicalSize::new(w, h), scale)
    }

    #[test]
    fn a_position_comes_back_only_while_its_monitor_is_there() {
        let (desk, projector) = (monitor(2560, 1440, 1.25), monitor(1920, 1080, 1.0));
        let mut book = PlacementBook::new();
        book.remember(projector, 2600, 40);
        assert_eq!(book.restore(&[desk]), None, "the projector is gone: the OS places the window");
        assert_eq!(book.restore(&[desk, projector]), Some((2600, 40)));
        book.remember(desk, 100, 80);
        assert_eq!(book.restore(&[desk, projector]), Some((100, 80)), "the newest wins");
        book.remember(projector, 2700, 50);
        assert_eq!(book.restore(&[projector]), Some((2700, 50)), "one entry per arrangement, replaced");
    }

    #[test]
    fn the_scale_is_part_of_the_arrangement_and_the_book_is_bounded() {
        let mut book = PlacementBook::new();
        book.remember(monitor(1920, 1080, 1.0), 1, 1);
        assert_eq!(book.restore(&[monitor(1920, 1080, 1.5)]), None);
        for i in 0..20 {
            book.remember(monitor(1000 + i, 1000, 1.0), 0, 0);
        }
        assert_eq!(book.entries.len(), MAX_ARRANGEMENTS);
        assert_eq!(book.restore(&[monitor(1019, 1000, 1.0)]), Some((0, 0)));
        assert_eq!(book.restore(&[monitor(1000, 1000, 1.0)]), None, "the oldest was forgotten");
    }

    #[test]
    fn the_book_round_trips_through_toml() {
        let mut book = PlacementBook::new();
        book.remember(monitor(1920, 1080, 1.25), -1920, 0);
        let text = toml::to_string(&book).expect("serialises");
        assert_eq!(toml::from_str::<PlacementBook>(&text).expect("parses"), book);
    }
}
