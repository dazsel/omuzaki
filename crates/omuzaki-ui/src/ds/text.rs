//! Cached text layout and allocation-free number formatting (`design-system.md` section 9, rules 4 and 8).
//!
//! egui memoises laid-out text, but building the key for a lookup allocates (a `String` and a `Vec` per call). A
//! widget that draws a label every frame would therefore allocate every frame. [`galley`] keeps the laid-out text in
//! a per-context cache keyed by a hash of the inputs, and compares the stored string on a hit, so a steady-state
//! frame looks text up without allocating anything. Galleys are laid out with a placeholder colour and tinted when
//! painted, so a hover fade never invalidates an entry.
//!
//! A cached galley points into egui's font atlas, which egui recreates when it fills up or the fonts change. Entries
//! must not outlive that, so once per pass the cache lays out a probe string through egui's own memo: if the probe
//! comes back as a different `Arc` than last pass, egui started a new font set and the cache starts empty too.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use egui::text::{Galley, LayoutJob, TextWrapping};
use egui::{Color32, Context, FontId, Id};

/// Entries kept before a sweep drops the ones that were not used recently.
const CAPACITY: usize = 2048;
/// An entry unused for this many passes is stale.
const STALE_AFTER: u64 = 120;

struct Entry {
    text: Box<str>,
    galley: Arc<Galley>,
    last_used: u64,
}

/// Laid-out text for one egui context.
#[derive(Default)]
struct Cache {
    entries: HashMap<Id, Entry>,
    /// A galley egui memoised for the probe string, and the pass it was last confirmed in.
    probe: Option<Arc<Galley>>,
    checked_pass: Option<u64>,
}

impl Cache {
    fn sweep(&mut self, now: u64) {
        self.entries.retain(|_, e| now.saturating_sub(e.last_used) < STALE_AFTER);
        if self.entries.len() >= CAPACITY {
            self.entries.clear();
        }
    }
}

fn cache(ctx: &Context) -> Arc<Mutex<Cache>> {
    let id = Id::new("omuzaki_ui::ds::text_cache");
    if let Some(found) = ctx.data(|d| d.get_temp::<Arc<Mutex<Cache>>>(id)) {
        return found;
    }
    let fresh = Arc::new(Mutex::new(Cache::default()));
    ctx.data_mut(|d| d.insert_temp(id, Arc::clone(&fresh)));
    fresh
}

/// One line of `text` in `font`, ending in an ellipsis when it is wider than `max_width` (infinite for no limit).
///
/// Paint it with `painter.galley(pos, galley, colour)`: the galley has no colour of its own.
pub fn galley(ctx: &Context, text: &str, font: &FontId, max_width: f32) -> Arc<Galley> {
    let key = Id::new((text, font.size.to_bits(), &font.family, ctx.pixels_per_point().to_bits(), max_width.to_bits()));
    let now = ctx.cumulative_pass_nr();
    let shared = cache(ctx);
    check_fonts_unchanged(ctx, &shared, now);
    {
        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = guard.entries.get_mut(&key)
            && &*entry.text == text
        {
            entry.last_used = now;
            return Arc::clone(&entry.galley);
        }
    }
    let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job(text, font, max_width)));
    let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.entries.len() >= CAPACITY {
        guard.sweep(now);
    }
    guard.entries.insert(key, Entry { text: text.into(), galley: Arc::clone(&galley), last_used: now });
    galley
}

/// Once per pass, drops every entry if egui has recreated its fonts since the last pass (see the module docs).
fn check_fonts_unchanged(ctx: &Context, shared: &Mutex<Cache>, now: u64) {
    {
        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.checked_pass == Some(now) {
            return;
        }
        guard.checked_pass = Some(now);
    }
    let probe = ctx.fonts_mut(|fonts| fonts.layout_job(job("omuzaki", &FontId::proportional(12.0), f32::INFINITY)));
    let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if !guard.probe.as_ref().is_some_and(|old| Arc::ptr_eq(old, &probe)) {
        guard.entries.clear();
        guard.probe = Some(probe);
    }
}

fn job(text: &str, font: &FontId, max_width: f32) -> LayoutJob {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font.clone(), Color32::PLACEHOLDER);
    if max_width.is_finite() {
        job.wrap = TextWrapping { max_width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    }
    job
}

/// A stack buffer a number is formatted into, so formatting a value that is about to be looked up allocates nothing.
pub struct NumberBuf {
    bytes: [u8; 40],
    len: usize,
}

impl NumberBuf {
    /// Formats `value` with exactly `decimals` digits after the point (`-0.00` prints as `0.00`).
    pub fn new(value: f64, decimals: usize) -> Self {
        let mut buf = Self { bytes: [0; 40], len: 0 };
        // 40 bytes hold any f64 at the decimals we use; on overflow `write!` fails and the text is cut, never wrong.
        let _ = write!(buf, "{value:.decimals$}");
        if buf.bytes[0] == b'-' && buf.bytes[1..buf.len].iter().all(|b| matches!(b, b'0' | b'.')) {
            buf.bytes.copy_within(1..buf.len, 0);
            buf.len -= 1;
        }
        buf
    }

    /// The formatted number.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl std::fmt::Write for NumberBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        let slot = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        slot.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Decimals to show for a range, per the parameter slider spec: ≥ 100 wide → 0, ≥ 10 → 1, ≥ 1 → 2, else 3.
pub fn decimals_for_range(min: f64, max: f64) -> usize {
    let span = (max - min).abs();
    if span >= 100.0 {
        0
    } else if span >= 10.0 {
        1
    } else if span >= 1.0 {
        2
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_without_a_heap_and_never_print_negative_zero() {
        assert_eq!(NumberBuf::new(0.62, 2).as_str(), "0.62");
        assert_eq!(NumberBuf::new(-0.001, 2).as_str(), "0.00");
        assert_eq!(NumberBuf::new(-0.0, 1).as_str(), "0.0");
        assert_eq!(NumberBuf::new(-1.5, 1).as_str(), "-1.5");
        assert_eq!(NumberBuf::new(1234.0, 0).as_str(), "1234");
    }

    #[test]
    fn decimals_follow_the_range() {
        assert_eq!(decimals_for_range(0.0, 360.0), 0);
        assert_eq!(decimals_for_range(0.0, 20.0), 1);
        assert_eq!(decimals_for_range(0.0, 2.0), 2);
        assert_eq!(decimals_for_range(0.0, 1.0), 2);
        assert_eq!(decimals_for_range(0.0, 0.5), 3);
    }
}
