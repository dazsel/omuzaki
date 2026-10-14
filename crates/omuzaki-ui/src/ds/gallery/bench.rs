//! Measures the UI cost of a gallery frame: layout, painting and tessellation, with no GPU (`design-system.md` 9).
//!
//! The budget is 2 ms p95 and 4 ms p99 for the busiest workspace at 1920 x 1080 on the mid laptop. The gallery draws
//! every component at once, so it is a stress case; the numbers go in the PR.

use std::time::Instant;

use egui::{Context, Event, PointerButton, Pos2, RawInput, Rect, ViewportId, vec2};

use super::Gallery;
use crate::ds::theme::Settings;

/// Percentiles of a series of frame times in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    /// Number of frames.
    pub frames: usize,
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Slowest frame.
    pub max: f64,
}

impl Summary {
    /// Sorts `times` and picks the percentiles. Empty input gives zeros.
    pub fn of(times: &mut [f64]) -> Self {
        times.sort_by(f64::total_cmp);
        let at = |q: f64| -> f64 {
            if times.is_empty() {
                return 0.0;
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index into the sorted slice"
            )]
            let index = ((times.len() as f64 - 1.0) * q).round() as usize;
            times[index.min(times.len() - 1)]
        };
        Self { frames: times.len(), p50: at(0.5), p95: at(0.95), p99: at(0.99), max: at(1.0) }
    }
}

/// Runs `frames` frames of the gallery at 1920 x 1080 and returns the per-frame UI time (run + tessellate).
///
/// The pointer moves over the content each frame so hover fades and tooltips stay in play.
pub fn measure(settings: Settings, frames: u32) -> Summary {
    let ctx = Context::default();
    let mut gallery = Gallery::new(settings);
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
    let mut times = Vec::new();
    // Warm up: fonts activate on the second frame, glyphs rasterise and caches fill.
    for frame in 0..(30 + frames) {
        let t = f64::from(frame) / 60.0;
        let x = 200.0 + 1400.0 * ((t * 0.7).sin() * 0.5 + 0.5) as f32;
        let y = 120.0 + 800.0 * ((t * 0.43).cos() * 0.5 + 0.5) as f32;
        let input = RawInput {
            viewport_id: ViewportId::ROOT,
            screen_rect: Some(screen),
            time: Some(t),
            events: vec![
                Event::PointerMoved(Pos2::new(x, y)),
                Event::PointerButton {
                    pos: Pos2::new(x, y),
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..RawInput::default()
        };
        let start = Instant::now();
        let mut output = ctx.run_ui(input, |ui| gallery.ui(ui));
        let shapes = std::mem::take(&mut output.shapes);
        let primitives = ctx.tessellate(shapes, output.pixels_per_point);
        // No GPU here, so the texture uploads are dropped on purpose (egui panics on an unhandled delta otherwise).
        output.textures_delta.clear();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(&primitives);
        if frame >= 30 {
            times.push(elapsed);
        }
    }
    Summary::of(&mut times)
}

/// Prints the measurement for the `gallery --bench` command.
///
/// # Errors
/// Never fails today; the signature matches the other headless commands.
pub fn run(settings: Settings, frames: u32) -> Result<(), Box<dyn std::error::Error>> {
    let s = measure(settings, frames);
    println!(
        "gallery UI frame time over {} frames (run + tessellate, 1920x1080, {} scale, {:?}): p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        s.frames,
        settings.scale.percent(),
        settings.theme,
        s.p50,
        s.p95,
        s.p99,
        s.max
    );
    Ok(())
}
