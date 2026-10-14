//! Key caps: `Ctrl K`, `Enter`, `Esc` drawn as small keys (the prototypes' key-cap hints, `design-system.md` intro).

use egui::{Align2, Color32, Context, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, vec2};

use crate::ds::icons;
use crate::ds::paint;
use crate::ds::theme::Ds;
use crate::ds::tokens::space;

const HEIGHT: f32 = 18.0;
const MIN_WIDTH: f32 = 18.0;
const GAP: f32 = 3.0;
const RADIUS: u8 = 3;

/// The colours of a set of key caps, chosen by the surface they sit on.
#[derive(Clone, Copy, Debug)]
pub struct CapStyle {
    /// Cap face.
    pub fill: Color32,
    /// Cap outline.
    pub stroke: Color32,
    /// Label colour.
    pub text: Color32,
}

/// What a cap shows.
enum Face<'a> {
    Icon(&'static str),
    Text(&'a str),
}

/// A shortcut as caps. `text` is space-separated keys: `"Ctrl K"`, `"Enter"`, `"Shift Tab"`.
#[derive(Clone, Copy, Debug)]
pub struct KeyCaps<'a> {
    /// The keys, separated by spaces.
    pub text: &'a str,
}

impl<'a> KeyCaps<'a> {
    /// Caps for a shortcut such as `"Ctrl Enter"`.
    pub const fn new(text: &'a str) -> Self {
        Self { text }
    }

    fn font() -> FontId {
        FontId::monospace(11.0)
    }

    /// Words with a better picture than their name.
    fn face(key: &str) -> Face<'_> {
        match key {
            "Enter" => Face::Icon(icons::ENTER),
            "Backspace" => Face::Icon(icons::BACKSPACE),
            "Up" => Face::Text("↑"),
            "Down" => Face::Text("↓"),
            "Left" => Face::Text("←"),
            "Right" => Face::Text("→"),
            other => Face::Text(other),
        }
    }

    fn cap_width(ctx: &Context, key: &str) -> f32 {
        match Self::face(key) {
            Face::Icon(_) => MIN_WIDTH,
            Face::Text(label) => (paint::text_width(ctx, label, &Self::font()) + 2.0 * space::S4 + 2.0).max(MIN_WIDTH),
        }
    }

    /// Total width of all caps with their gaps.
    pub fn width(&self, ctx: &Context) -> f32 {
        let mut width = 0.0;
        let mut count = 0.0_f32;
        for key in self.text.split(' ').filter(|k| !k.is_empty()) {
            width += Self::cap_width(ctx, key);
            count += 1.0;
        }
        width + GAP * (count - 1.0).max(0.0)
    }

    /// Draws the caps with their left edge at `left_centre` (vertically centred on it); returns the rectangle covered.
    pub fn paint(&self, painter: &Painter, ds: &Ds, left_centre: Pos2, style: CapStyle) -> Rect {
        let start = left_centre.x;
        let mut x = start;
        for key in self.text.split(' ').filter(|k| !k.is_empty()) {
            let width = Self::cap_width(painter.ctx(), key);
            let cap = Rect::from_min_size(Pos2::new(x, left_centre.y - HEIGHT / 2.0), vec2(width, HEIGHT));
            painter.rect(cap, RADIUS, style.fill, Stroke::new(1.0, style.stroke), StrokeKind::Inside);
            match Self::face(key) {
                Face::Icon(glyph) => {
                    paint::icon(painter, ds, cap.center(), glyph, 12.0, style.text, false);
                }
                Face::Text(label) => {
                    paint::text(painter, cap.center(), Align2::CENTER_CENTER, label, &Self::font(), style.text);
                }
            }
            x += width + GAP;
        }
        Rect::from_min_max(
            Pos2::new(start, left_centre.y - HEIGHT / 2.0),
            Pos2::new((x - GAP).max(start), left_centre.y + HEIGHT / 2.0),
        )
    }
}
