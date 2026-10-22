//! Per-pixel click-through for a character on the desktop (#759; research `docs/research/2026-10-mascot-window.md`
//! §5.2): with [`ClickThrough::Transparent`](omuzaki_stream_api::window::ClickThrough), a click on a pixel the character
//! doesn't cover reaches the window underneath, and a press on the character still drags it.
//!
//! winit has no per-pixel hit test, so the window polls: each frame it reads the desktop cursor through a
//! [`DesktopProbe`], looks up the character's alpha under it in an [`AlphaMask`] of the last presented frame, and turns
//! the whole window's hit testing off over a clear pixel and back on over the character
//! (`ViewportCommand::MousePassthrough`). The mask is a quarter-resolution copy of the frame's alpha, refreshed at most
//! four times a second and only while the cursor is over the window, never per frame.
//!
//! The probe also gives the way back from full click-through, where the window takes no input at all: the
//! [`WAY_BACK`](crate::live::wiring::WAY_BACK) chord held anywhere on the desktop, read by polling the keys' state (no
//! hook, nothing recorded), only while full click-through is on.
//!
//! Probes: Windows (`GetCursorPos`, `GetAsyncKeyState`) and X11 (`QueryPointer`, `QueryKeymap`). Elsewhere (macOS,
//! Wayland) [`os_probe`] is `None` and the window keeps whole-window hit testing: a click anywhere on it takes it, as
//! before, and the studio's toggle and shortcut are the way back.

#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod x11;

use std::time::{Duration, Instant};

use egui::{Pos2, Rect};
use omuzaki_render_api::Image;

/// Below this alpha (0 to 1) a pixel is clear and a click there passes through.
pub const ALPHA_THRESHOLD: f32 = 0.1;
/// The mask is refreshed at most this often while the cursor is over the window.
pub const MASK_PERIOD: Duration = Duration::from_millis(250);
/// One mask cell covers this many frame pixels each way.
const CELL: u32 = 4;

/// Reads the desktop's pointer and the way-back chord, wherever the focus is.
pub trait DesktopProbe: Send + std::fmt::Debug {
    /// The cursor in desktop physical pixels; `None` when it can't be read.
    fn cursor(&self) -> Option<Pos2>;
    /// Whether Ctrl, Alt and M are all held now.
    fn way_back_held(&self) -> bool;
}

/// This OS's probe, if it has one this build can use.
pub fn os_probe() -> Option<Box<dyn DesktopProbe>> {
    #[cfg(windows)]
    {
        Some(Box::new(win::WinProbe))
    }
    #[cfg(target_os = "linux")]
    {
        x11::X11Probe::connect().map(|probe| Box::new(probe) as Box<dyn DesktopProbe>)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// The frame's alpha at a quarter of its resolution: each cell holds the most opaque pixel it covers, so the
/// character's thin edges (a strand of hair, a finger) still take a press.
#[derive(Clone, Debug, PartialEq)]
pub struct AlphaMask {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

impl AlphaMask {
    /// The mask of `image` (straight or premultiplied: alpha is the same); `None` for an empty or short image.
    pub fn from_image(image: &Image) -> Option<Self> {
        let (w, h) = (image.size.x, image.size.y);
        let pixels = usize::try_from(u64::from(w) * u64::from(h) * 4).ok()?;
        if w == 0 || h == 0 || image.rgba8.len() < pixels {
            return None;
        }
        let (width, height) = (w.div_ceil(CELL), h.div_ceil(CELL));
        let mut alpha = vec![0_u8; usize::try_from(width * height).ok()?];
        let row = usize::try_from(w).ok()?;
        for (i, px) in image.rgba8.as_chunks::<4>().0.iter().take(pixels / 4).enumerate() {
            let (x, y) = (i % row, i / row);
            let cell = (y / CELL as usize) * width as usize + x / CELL as usize;
            if let Some(slot) = alpha.get_mut(cell) {
                *slot = (*slot).max(px[3]);
            }
        }
        Some(Self { width, height, alpha })
    }

    /// The alpha (0 to 1) at `(u, v)`, each 0 to 1 across the frame; 0 outside it.
    pub fn alpha_at(&self, u: f32, v: f32) -> f32 {
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // in range
        let (x, y) = ((u * self.width as f32) as usize, (v * self.height as f32) as usize);
        self.alpha.get(y * self.width as usize + x).map_or(0.0, |a| f32::from(*a) / 255.0)
    }
}

/// The window's hit-testing state: what it last told the OS, the mask, and the way-back chord's last reading.
#[derive(Debug, Default)]
pub struct PixelHits {
    mask: Option<AlphaMask>,
    mask_at: Option<Instant>,
    /// Whether the window passes clicks through now, as last sent; `None` before anything was sent.
    passing: Option<bool>,
    /// Whether the cursor was over the window at the last update.
    over: bool,
    /// Whether the chord was held at the last poll.
    held: bool,
}

impl PixelHits {
    /// One frame of per-pixel click-through: `cursor` and the `window` in desktop physical pixels, and where the
    /// program `frame` is drawn in it. Returns whether the window must pass clicks through from now, when that changes.
    ///
    /// Until the first mask arrives the window takes clicks everywhere, as it did before.
    pub fn update(&mut self, cursor: Option<Pos2>, window: Rect, frame: Option<Rect>) -> Option<bool> {
        self.over = cursor.is_some_and(|at| window.contains(at));
        let at = cursor.filter(|_| self.over)?;
        let mask = self.mask.as_ref()?;
        let alpha = frame.map_or(0.0, |frame| {
            let uv = (at - frame.min) / frame.size();
            mask.alpha_at(uv.x, uv.y)
        });
        self.send(alpha < ALPHA_THRESHOLD)
    }

    /// Leaves per-pixel mode: back to whole-window hit testing (`full` passes everything). Returns the state to send
    /// when the window was left in another one.
    pub fn settle(&mut self, full: bool) -> Option<bool> {
        self.over = false;
        let passing = self.passing.take()?;
        (passing != full).then_some(full)
    }

    /// Whether a fresh mask is wanted at `now`: the cursor is over the window and the last one is older than
    /// [`MASK_PERIOD`].
    pub fn wants_mask(&self, now: Instant) -> bool {
        self.over && self.mask_at.is_none_or(|at| now.duration_since(at) >= MASK_PERIOD)
    }

    /// Keeps `mask`, read at `now`.
    pub fn set_mask(&mut self, mask: AlphaMask, now: Instant) {
        self.mask = Some(mask);
        self.mask_at = Some(now);
    }

    /// The chord's state now; `true` once, when it goes down.
    pub fn way_back(&mut self, held: bool) -> bool {
        let pressed = held && !self.held;
        self.held = held;
        pressed
    }

    fn send(&mut self, passing: bool) -> Option<bool> {
        (self.passing != Some(passing)).then(|| {
            self.passing = Some(passing);
            passing
        })
    }
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2};
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::AlphaEncoding;

    use super::*;

    /// A 64x32 frame, clear but for an opaque square in its left half (x 8..24, y 8..24) and one opaque pixel at (40, 4).
    fn frame() -> Image {
        let mut rgba8 = vec![0_u8; 64 * 32 * 4];
        for y in 0..32_usize {
            for x in 0..64_usize {
                if (8..24).contains(&x) && (8..24).contains(&y) || (x, y) == (40, 4) {
                    rgba8[(y * 64 + x) * 4 + 3] = 255;
                }
            }
        }
        Image { size: UVec2::new(64, 32), alpha: AlphaEncoding::Premultiplied, rgba8 }
    }

    #[test]
    fn the_mask_keeps_the_most_opaque_pixel_of_each_cell() {
        let mask = AlphaMask::from_image(&frame()).expect("a mask");
        assert_eq!((mask.width, mask.height), (16, 8));
        assert!((mask.alpha_at(16.0 / 64.0, 16.0 / 32.0) - 1.0).abs() < 1e-6, "inside the square");
        assert_eq!(mask.alpha_at(50.0 / 64.0, 20.0 / 32.0), 0.0, "clear");
        assert!(mask.alpha_at(41.0 / 64.0, 5.0 / 32.0) > 0.9, "a single opaque pixel still takes a press");
        assert_eq!(mask.alpha_at(1.2, 0.5), 0.0, "outside the frame");
    }

    #[test]
    fn clicks_pass_over_clear_pixels_and_stop_over_the_character() {
        let mut hits = PixelHits::default();
        // The window at (1000, 500) on the desktop, 128x64 physical pixels, the frame filling it at twice its size.
        let window = Rect::from_min_size(pos2(1000.0, 500.0), vec2(128.0, 64.0));
        let over_character = pos2(1000.0 + 32.0, 500.0 + 32.0);
        let over_clear = pos2(1000.0 + 100.0, 500.0 + 40.0);
        assert_eq!(hits.update(Some(over_clear), window, Some(window)), None, "no mask yet: takes clicks as before");
        assert!(hits.wants_mask(Instant::now()));
        hits.set_mask(AlphaMask::from_image(&frame()).expect("a mask"), Instant::now());
        assert!(!hits.wants_mask(Instant::now()), "at most four times a second");
        assert_eq!(hits.update(Some(over_clear), window, Some(window)), Some(true));
        assert_eq!(hits.update(Some(over_clear), window, Some(window)), None, "sent once");
        assert_eq!(hits.update(Some(over_character), window, Some(window)), Some(false));
        // Off the window: nothing changes, and no mask is read.
        assert_eq!(hits.update(Some(pos2(10.0, 10.0)), window, Some(window)), None);
        assert!(!hits.wants_mask(Instant::now() + MASK_PERIOD));
        assert_eq!(hits.update(None, window, Some(window)), None);
    }

    #[test]
    fn leaving_per_pixel_mode_restores_whole_window_hit_testing() {
        let mut hits = PixelHits::default();
        let window = Rect::from_min_size(Pos2::ZERO, vec2(64.0, 32.0));
        hits.set_mask(AlphaMask::from_image(&frame()).expect("a mask"), Instant::now());
        assert_eq!(hits.update(Some(pos2(50.0, 20.0)), window, Some(window)), Some(true));
        assert_eq!(hits.settle(false), Some(false), "back to taking clicks");
        assert_eq!(hits.settle(false), None);
        assert_eq!(hits.update(Some(pos2(50.0, 20.0)), window, Some(window)), Some(true));
        assert_eq!(hits.settle(true), None, "full click-through already passes everything");
    }

    #[test]
    fn the_way_back_fires_once_per_press() {
        let mut hits = PixelHits::default();
        assert!(!hits.way_back(false));
        assert!(hits.way_back(true));
        assert!(!hits.way_back(true), "held: once");
        assert!(!hits.way_back(false));
        assert!(hits.way_back(true));
    }
}
