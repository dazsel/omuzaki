//! Painting helpers shared by the components: cached text, icons, the focus ring, hover fades and small shapes.
//!
//! Components draw with the painter rather than by composing child widgets: a row costs one interaction rect and a
//! handful of shapes, and nothing here allocates once the text cache is warm (`design-system.md` section 9).

use egui::emath::easing;
use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Frame, Id, Painter, Pos2, Rect, Response, Shadow, Stroke,
    StrokeKind, vec2,
};

use super::focus;
use super::text;
use super::theme::Ds;
use super::tokens::{DISABLED_ALPHA, Theme, dur, radius};

/// A colour between `a` and `b`, blended in gamma space like egui's own hover fades.
pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// Draws one line of text anchored at `pos`, returning the rectangle it covers. No allocation once cached.
pub fn text(painter: &Painter, pos: Pos2, anchor: Align2, label: &str, font: &FontId, colour: Color32) -> Rect {
    let galley = text::galley(painter.ctx(), label, font, f32::INFINITY);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, colour);
    rect
}

/// Draws one line of text inside `rect`, ending in an ellipsis if it does not fit, aligned by `align`.
pub fn text_in(painter: &Painter, rect: Rect, align: Align2, label: &str, font: &FontId, colour: Color32) -> Rect {
    let galley = text::galley(painter.ctx(), label, font, rect.width());
    let drawn = align.align_size_within_rect(galley.size(), rect);
    painter.galley(drawn.min, galley, colour);
    drawn
}

/// The width `label` takes in `font`.
pub fn text_width(ctx: &Context, label: &str, font: &FontId) -> f32 {
    text::galley(ctx, label, font, f32::INFINITY).size().x
}

/// Draws an icon glyph centred on `centre`. `filled` picks the Fill weight, which marks "on" states.
pub fn icon(painter: &Painter, ds: &Ds, centre: Pos2, glyph: &str, size: f32, colour: Color32, filled: bool) -> Rect {
    text(painter, centre, Align2::CENTER_CENTER, glyph, &ds.text.icon(size, filled), colour)
}

/// Where the focus ring sits relative to the widget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RingPlacement {
    /// 1 pt outside the widget edge (most controls).
    Outside,
    /// Inside the row, for dense lists where an outside ring would be clipped or overlap its neighbours.
    Inside,
}

/// Draws the keyboard focus ring if `response` has focus and focus arrived by keyboard (see [`focus`]).
pub fn focus_ring(
    painter: &Painter,
    theme: &Theme,
    response: &Response,
    rect: Rect,
    radius: u8,
    placement: RingPlacement,
) {
    if !(response.has_focus() && focus::visible(&response.ctx)) {
        return;
    }
    draw_ring(painter, theme, rect, radius, placement);
}

/// Draws the ring unconditionally, for widgets that decide focus themselves (a roving group).
pub fn draw_ring(painter: &Painter, theme: &Theme, rect: Rect, radius: u8, placement: RingPlacement) {
    let ring = theme.focus;
    // `Middle` strokes straddle the rect edge, so the rect is grown by the gap plus half the width.
    let offset = match placement {
        RingPlacement::Outside => ring.gap + ring.width / 2.0,
        RingPlacement::Inside => -(ring.gap + ring.width / 2.0),
    };
    let rounding = CornerRadius::same((f32::from(radius) + offset.max(0.0)).round().clamp(0.0, 255.0) as u8);
    if let Some(edge) = ring.edge {
        let outer = rect.expand(offset + ring.width / 2.0 + 0.5);
        painter.rect_stroke(outer, rounding, Stroke::new(1.0, edge), StrokeKind::Middle);
    }
    painter.rect_stroke(rect.expand(offset), rounding, Stroke::new(ring.width, ring.colour), StrokeKind::Middle);
}

/// The width a widget may take: what is left on the line, or the whole line in a wrapping layout (where a widget that
/// does not fit moves to the next line instead of shrinking).
///
/// Components that size themselves to their text cap that width with this, so a long translated label ends in an
/// ellipsis rather than pushing the control out of its panel.
pub fn fit_width(ui: &egui::Ui) -> f32 {
    if ui.layout().main_wrap { ui.max_rect().width() } else { ui.available_width() }
}

/// A 0 to 1 fade for hover and press, eased out over [`dur::HOVER`] (zero under reduce motion).
pub fn fade(ctx: &Context, ds: &Ds, id: Id, target: bool) -> f32 {
    ctx.animate_bool_with_time_and_easing(id.with("fade"), target, ds.motion(dur::HOVER), easing::cubic_out)
}

/// Hovered, or highlighted by `Response::highlight` (the gallery uses it to show a hovered specimen without a pointer).
pub fn hot(response: &Response) -> bool {
    response.hovered() || response.highlighted()
}

/// `colour` as drawn on a control that is disabled by its own flag (`.disabled(reason)`): a fill or a stroke at the
/// same alpha egui uses for its own disabled widgets.
///
/// A control inside a disabled `Ui` is not dimmed here: egui already fades everything painted in that `Ui`, and
/// fading twice squares the effect (0.55 x 0.55) and leaves the text unreadable.
pub fn dim(enabled: bool, colour: Color32) -> Color32 {
    if enabled { colour } else { colour.gamma_multiply(DISABLED_ALPHA) }
}

/// The colour of text on a control that is disabled by its own flag: the `text.disabled` token, not a faded copy of
/// the normal colour. Fading `text.2` to 55% leaves 2.7:1 on the light panel; the token is held to 3:1 everywhere.
pub fn ink(theme: &Theme, enabled: bool, colour: Color32) -> Color32 {
    if enabled { colour } else { theme.text_disabled }
}

/// Pixel-snaps a rectangle's edges so 1 pt strokes stay crisp at 100% scale.
pub fn snap(ctx: &Context, rect: Rect) -> Rect {
    let ppp = ctx.pixels_per_point();
    let round = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(Pos2::new(round(rect.min.x), round(rect.min.y)), Pos2::new(round(rect.max.x), round(rect.max.y)))
}

/// A square of side `size` centred on `centre`.
pub fn square(centre: Pos2, size: f32) -> Rect {
    Rect::from_center_size(centre, vec2(size, size))
}

/// A check mark centred in `rect`, drawn as two strokes with round ends so it scales with the control, needs no font
/// and allocates nothing (a polyline would need a `Vec`).
pub fn check_mark(painter: &Painter, rect: Rect, colour: Color32, width: f32) {
    let c = rect.center();
    let s = rect.width().min(rect.height());
    let (left, bottom, right) = (
        Pos2::new(c.x - 0.28 * s, c.y + 0.02 * s),
        Pos2::new(c.x - 0.08 * s, c.y + 0.24 * s),
        Pos2::new(c.x + 0.30 * s, c.y - 0.22 * s),
    );
    let stroke = Stroke::new(width, colour);
    painter.line_segment([left, bottom], stroke);
    painter.line_segment([bottom, right], stroke);
    for end in [left, bottom, right] {
        painter.circle_filled(end, width / 2.0, colour);
    }
}

/// The frame everything that floats over the work shares (`design-system.md` section 4): `bg2`, the 8 pt popup radius,
/// the border rule (the hairline `line`, or the 2 pt control border in high contrast) and the popup shadow.
///
/// Popups, menus, dialogs, the palette and toasts all use it, so a change to elevation is made in one place. The
/// frame has no inner margin: each caller picks its own.
pub fn overlay_frame(ds: &Ds) -> Frame {
    let t = &ds.theme;
    let border = if t.control_stroke > 1.0 { t.border_control } else { t.line };
    Frame::new().fill(t.bg2).stroke(Stroke::new(t.control_stroke, border)).corner_radius(radius::POPUP).shadow(Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(t.shadow_alpha),
    })
}
