//! `ColourField` (`design-system.md` section 7.4): a swatch, its hex in mono and a chevron; a popover to change it.
//!
//! The popover (264 pt) holds a palette-slot row (click a slot to **link**, so edits propagate), a saturation-value
//! area, a hue bar, an alpha bar when the property has alpha, hex and RGB and OKLCH fields, an eyedropper request,
//! eight recent colours and a contrast chip against the neighbouring slot. A change previews live; closing the
//! popover or pressing Enter commits one transaction; Esc cancels and restores the colour from before it opened.

use egui::epaint::Vertex;
use egui::{
    Align2, Area, Color32, Context, Id, Key, Margin, Mesh, Order, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use omuzaki_core::colour::Colour;

use super::chip::StatusChip;
use super::ids;
use super::ime;
use crate::ds::a11y;
use crate::ds::contrast::contrast_ratio;
use crate::ds::icons;
use crate::ds::input;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::text::NumberBuf;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, radius, size, space};

const POPOVER_WIDTH: f32 = 264.0;
const AREA_SIZE: egui::Vec2 = vec2(POPOVER_WIDTH - 24.0, 132.0);
const BAR_HEIGHT: f32 = 14.0;

/// A named colour of the character's palette (`LK-03`): linking a field to one makes edits propagate.
#[derive(Clone, Copy, Debug)]
pub struct PaletteSlot<'a> {
    /// The slot name ("hair.root").
    pub name: &'a str,
    /// Its current colour.
    pub colour: Colour,
}

/// What happened to a colour field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColourEvent {
    /// The popover changed the colour: preview it live.
    Preview(Colour),
    /// The popover closed or Enter was pressed: commit one transaction.
    Commit {
        /// The colour when the popover opened.
        from: Colour,
        /// The colour now.
        to: Colour,
    },
    /// Esc: the colour was restored to this.
    Cancel(Colour),
    /// A palette slot was clicked: link the property to the slot at this index.
    Link(usize),
    /// The eyedropper was pressed: the host samples the viewport or a reference and writes the colour back.
    PickRequested,
}

/// The result of showing a [`ColourField`].
#[derive(Debug)]
pub struct ColourOutput {
    /// The closed field's response.
    pub response: Response,
    /// What happened this frame.
    pub event: Option<ColourEvent>,
}

/// A colour property bound to `colour`.
#[must_use = "a colour field does nothing until it is shown"]
pub struct ColourField<'a> {
    label: &'a str,
    colour: &'a mut Colour,
    alpha: bool,
    linked: Option<&'a str>,
    palette: &'a [PaletteSlot<'a>],
    recent: &'a [Colour],
    neighbour: Option<PaletteSlot<'a>>,
    disabled: Option<&'a str>,
    salt: Option<Id>,
}

impl<'a> ColourField<'a> {
    /// A field named `label` ("Hair root colour"); the name is also the accessible name.
    pub fn new(label: &'a str, colour: &'a mut Colour) -> Self {
        a11y::require_label(label);
        Self {
            label,
            colour,
            alpha: false,
            linked: None,
            palette: &[],
            recent: &[],
            neighbour: None,
            disabled: None,
            salt: None,
        }
    }

    /// The property has an alpha channel: shows the alpha bar.
    pub const fn alpha(mut self, alpha: bool) -> Self {
        self.alpha = alpha;
        self
    }

    /// The property is linked to this palette slot: the field shows the slot name and a chain icon.
    pub const fn linked(mut self, slot: &'a str) -> Self {
        self.linked = Some(slot);
        self
    }

    /// The palette the popover offers for linking.
    pub const fn palette(mut self, palette: &'a [PaletteSlot<'a>]) -> Self {
        self.palette = palette;
        self
    }

    /// The last colours used (the popover shows up to eight).
    pub const fn recent(mut self, recent: &'a [Colour]) -> Self {
        self.recent = recent;
        self
    }

    /// The neighbouring slot to measure value contrast against ("skin.base").
    pub const fn neighbour(mut self, slot: PaletteSlot<'a>) -> Self {
        self.neighbour = Some(slot);
        self
    }

    /// Disables the field, saying why (the accessible description).
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the field's state (keyboard focus, whether the popover is open and what it remembers, the hex text being
    /// typed) by `salt` instead of by its position in the `Ui`, so reordering the rows of a list keeps an open
    /// popover with its field. Use a value that names the property, unique among the colour fields of one `Ui`.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the field and, when open, its popover.
    pub fn show(self, ui: &mut Ui) -> ColourOutput {
        Show::new(self, ui).run(ui)
    }
}

impl Widget for ColourField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

// ----------------------------------------------------------------------------
// Colour maths, free of egui widgets so it can be tested exactly.

/// Conversions between sRGB, HSV, OKLCH and hex text. They work on [`Colour`] (unmultiplied sRGB bytes), not egui's
/// premultiplied `Color32`, so a translucent colour keeps its exact channels.
#[allow(clippy::many_single_char_names, reason = "r, g, b, h, s, v, l, a are the standard names in colour maths")]
mod maths {
    use omuzaki_core::colour::Colour;

    /// Hue 0..360, saturation and value 0..1.
    pub type Hsv = [f32; 3];

    pub fn to_hsv(c: Colour) -> Hsv {
        let (r, g, b) = (f32::from(c.r) / 255.0, f32::from(c.g) / 255.0, f32::from(c.b) / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let h = if d < f32::EPSILON {
            0.0
        } else if (max - r).abs() < f32::EPSILON {
            60.0 * (((g - b) / d) % 6.0)
        } else if (max - g).abs() < f32::EPSILON {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        [if h < 0.0 { h + 360.0 } else { h }, if max <= 0.0 { 0.0 } else { d / max }, max]
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "channels are clamped to 0..=255 first")]
    fn byte(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub fn from_hsv([h, s, v]: Hsv, alpha: u8) -> Colour {
        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Colour::rgba(byte(r + m), byte(g + m), byte(b + m), alpha)
    }

    fn lin(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    /// sRGB to OKLCH: lightness 0..1, chroma ~0..0.4, hue 0..360.
    pub fn to_oklch(c: Colour) -> [f64; 3] {
        let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        let big_l = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
        let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
        let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
        let chroma = a.hypot(bb);
        let hue = bb.atan2(a).to_degrees();
        [big_l, chroma, if hue < 0.0 { hue + 360.0 } else { hue }]
    }

    /// OKLCH to sRGB, clamped into gamut.
    pub fn from_oklch([big_l, chroma, hue]: [f64; 3], alpha: u8) -> Colour {
        let (a, b) = (chroma * hue.to_radians().cos(), chroma * hue.to_radians().sin());
        let l = (big_l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
        let m = (big_l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
        let s = (big_l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
        let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
        let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
        let bl = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s;
        let enc = |v: f64| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a 0..1 value scaled to a byte")]
        let byte = |v: f64| (enc(v) * 255.0).round() as u8;
        Colour::rgba(byte(r), byte(g), byte(bl), alpha)
    }

    /// `#RRGGBB`, or `#RRGGBBAA` when `alpha` is set.
    pub fn to_hex(c: Colour, alpha: bool) -> String {
        if alpha {
            format!("#{:02X}{:02X}{:02X}{:02X}", c.r, c.g, c.b, c.a)
        } else {
            format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
        }
    }

    /// Reads `#RGB`, `#RRGGBB`, `#RRGGBBAA` (the `#` is optional; surrounding spaces and pasted text are fine).
    pub fn parse_hex(text: &str) -> Option<Colour> {
        let digits = text.trim().trim_start_matches('#');
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let pair = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
        let nibble = |i: usize| u8::from_str_radix(digits.get(i..=i)?, 16).ok().map(|n| n * 17);
        match digits.len() {
            3 => Some(Colour::rgb(nibble(0)?, nibble(1)?, nibble(2)?)),
            6 => Some(Colour::rgb(pair(0)?, pair(2)?, pair(4)?)),
            8 => Some(Colour::rgba(pair(0)?, pair(2)?, pair(4)?, pair(6)?)),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hsv_round_trips_every_primary_and_a_mid_tone() {
            for c in [
                Colour::rgb(255, 0, 0),
                Colour::rgb(0, 255, 0),
                Colour::rgb(0, 0, 255),
                Colour::rgb(138, 111, 209),
                Colour::rgb(90, 90, 90),
            ] {
                assert_eq!(from_hsv(to_hsv(c), 255), c, "{c:?}");
            }
        }

        #[test]
        fn oklch_round_trips_within_a_unit() {
            for c in [Colour::rgb(230, 195, 106), Colour::rgb(185, 162, 255), Colour::rgb(20, 20, 26)] {
                let back = from_oklch(to_oklch(c), 255);
                let off = |a: u8, b: u8| a.abs_diff(b);
                assert!(
                    off(c.r, back.r) <= 1 && off(c.g, back.g) <= 1 && off(c.b, back.b) <= 1,
                    "{c:?} came back as {back:?}"
                );
            }
            let [l, ..] = to_oklch(Colour::WHITE);
            assert!((l - 1.0).abs() < 1e-3, "white is lightness 1");
        }

        #[test]
        fn hex_accepts_short_long_alpha_and_pasted_text() {
            assert_eq!(parse_hex("#8A6FD1"), Some(Colour::rgb(0x8A, 0x6F, 0xD1)));
            assert_eq!(parse_hex("  8a6fd1 "), Some(Colour::rgb(0x8A, 0x6F, 0xD1)));
            assert_eq!(parse_hex("#fa0"), Some(Colour::rgb(0xFF, 0xAA, 0x00)));
            assert_eq!(parse_hex("#8A6FD180"), Some(Colour::rgba(0x8A, 0x6F, 0xD1, 0x80)));
            assert_eq!(parse_hex("#8A6FD"), None);
            assert_eq!(parse_hex("#GGGGGG"), None);
            assert_eq!(parse_hex(""), None);
        }

        #[test]
        fn hex_formats_with_and_without_alpha_and_keeps_translucent_channels_exact() {
            assert_eq!(to_hex(Colour::rgb(1, 2, 3), false), "#010203");
            assert_eq!(
                to_hex(Colour::rgba(1, 2, 3, 4), true),
                "#01020304",
                "a nearly transparent colour keeps its channels"
            );
        }
    }
}

// ----------------------------------------------------------------------------

/// What a colour field's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.colour_field";

/// What the popover remembers while it is open.
#[derive(Clone, Copy)]
struct Pop {
    open: bool,
    original: Colour,
    hsv: [f32; 3],
    alpha: u8,
}

impl Pop {
    const CLOSED: Self = Self { open: false, original: Colour::BLACK, hsv: [0.0; 3], alpha: 255 };
}

/// egui paints premultiplied colours; ours are unmultiplied sRGB bytes.
fn c32(c: Colour) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

struct Show<'a> {
    f: ColourField<'a>,
    ds: std::sync::Arc<Ds>,
    ctx: Context,
    id: Id,
    event: Option<ColourEvent>,
}

impl<'a> Show<'a> {
    fn new(f: ColourField<'a>, ui: &Ui) -> Self {
        Self { f, ds: ds(ui.ctx()), ctx: ui.ctx().clone(), id: Id::NULL, event: None }
    }

    fn run(mut self, ui: &mut Ui) -> ColourOutput {
        let ds = std::sync::Arc::clone(&self.ds);
        let (t, m) = (&ds.theme, &ds.metrics);
        let enabled = ui.is_enabled() && self.f.disabled.is_none();
        // Only the field's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
        let looks_enabled = self.f.disabled.is_none();
        let hex = maths::to_hex(*self.f.colour, self.f.alpha);
        let shown: &str = self.f.linked.unwrap_or(hex.as_str());
        let text_font = if self.f.linked.is_some() { ds.text.body.clone() } else { ds.text.mono.clone() };
        let text_w = paint::text_width(&self.ctx, shown, &text_font);
        let chain_w = if self.f.linked.is_some() { 16.0 } else { 0.0 };
        let width = space::S8 + size::SWATCH.x + space::S8 + chain_w + text_w + space::S8 + 12.0 + space::S8;
        let (rect, response) = ids::allocate(
            ui,
            vec2(width.max(136.0), m.control_row),
            if enabled { Sense::click() } else { Sense::hover() },
            KIND,
            self.f.salt,
        );
        self.id = response.id;
        let mut pop: Pop = self.ctx.data(|d| d.get_temp(self.id)).unwrap_or(Pop::CLOSED);

        if ui.is_rect_visible(rect) {
            let rect = paint::snap(&self.ctx, rect);
            let hover = paint::fade(&self.ctx, &ds, self.id, enabled && (paint::hot(&response) || pop.open));
            let painter = ui.painter();
            painter.rect_filled(rect, radius::CONTROL, paint::dim(looks_enabled, paint::lerp(t.bg2, t.bg3, hover)));
            painter.rect_stroke(
                rect,
                radius::CONTROL,
                Stroke::new(
                    t.control_stroke,
                    paint::dim(looks_enabled, paint::lerp(t.border_control, t.text3, hover * 0.6)),
                ),
                StrokeKind::Inside,
            );
            let swatch = Rect::from_min_size(
                pos2(rect.left() + space::S8, rect.center().y - size::SWATCH.y / 2.0),
                size::SWATCH,
            );
            draw_swatch(painter, &ds, swatch, c32(*self.f.colour), 3);
            let mut x = swatch.right() + space::S8;
            if self.f.linked.is_some() {
                paint::icon(painter, &ds, pos2(x + 6.0, rect.center().y), icons::LINK, 14.0, t.text2, false);
                x += chain_w;
            }
            let label_rect = Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 26.0, rect.bottom()));
            paint::text_in(
                painter,
                label_rect,
                Align2::LEFT_CENTER,
                shown,
                &text_font,
                paint::ink(t, looks_enabled, t.text),
            );
            paint::icon(
                painter,
                &ds,
                pos2(rect.right() - 14.0, rect.center().y),
                if pop.open { icons::CARET_UP } else { icons::CARET_DOWN },
                12.0,
                t.text2,
                false,
            );
            paint::focus_ring(painter, t, &response, rect, radius::CONTROL, RingPlacement::Outside);
        }

        // Enter and Space open it and close it again. A held key repeats, and a repeat must not toggle it back.
        let was_open = pop.open;
        if response.clicked() && enabled && !input::only_repeats(&response) {
            if pop.open {
                self.close(&mut pop, true);
                response.request_focus();
            } else {
                pop = Pop {
                    open: true,
                    original: *self.f.colour,
                    hsv: maths::to_hsv(*self.f.colour),
                    alpha: self.f.colour.a,
                };
            }
        }
        if pop.open {
            self.popover(&response, &mut pop, was_open);
        }
        self.ctx.data_mut(|d| d.insert_temp(self.id, pop));

        let (label, hex_text, linked, reason) = (self.f.label, hex, self.f.linked, self.f.disabled);
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::ColorButton, enabled, label),
            |node| {
                node.set_role(accesskit::Role::ColorWell);
                node.set_value(match linked {
                    Some(slot) => ds.text_with(Msg::ColourLinkedValue, &[("hex", &hex_text), ("slot", slot)]),
                    None => hex_text.clone(),
                });
                if let Some(reason) = reason {
                    node.set_description(reason);
                }
            },
        );
        ColourOutput { response, event: self.event }
    }

    /// Ends the popover: commits (closing or Enter) or cancels (Esc).
    fn close(&mut self, pop: &mut Pop, commit: bool) {
        pop.open = false;
        // Rejected channel text does not outlive the popover.
        self.ctx.data_mut(|d| {
            for i in 0..3 {
                d.remove::<String>(invalid_key(self.id.with(("rgb", i))));
                d.remove::<String>(invalid_key(self.id.with(("lch", i))));
            }
        });
        if commit {
            if *self.f.colour != pop.original {
                self.event = Some(ColourEvent::Commit { from: pop.original, to: *self.f.colour });
            }
        } else {
            *self.f.colour = pop.original;
            self.event = Some(ColourEvent::Cancel(pop.original));
        }
    }

    fn set(&mut self, pop: &mut Pop, colour: Colour) {
        pop.hsv = maths::to_hsv(colour);
        pop.alpha = colour.a;
        *self.f.colour = colour;
        self.event = Some(ColourEvent::Preview(colour));
    }

    fn set_hsv(&mut self, pop: &mut Pop, hsv: [f32; 3]) {
        pop.hsv = hsv;
        *self.f.colour = maths::from_hsv(hsv, pop.alpha);
        self.event = Some(ColourEvent::Preview(*self.f.colour));
    }

    /// Draws the popover. Its keys (Enter commits, Esc cancels) act only when it was already open at the start of the
    /// frame: the Enter that opened it is still in this frame's input and must not close it again.
    fn popover(&mut self, anchor: &Response, pop: &mut Pop, was_open: bool) {
        let ds = std::sync::Arc::clone(&self.ds);
        let pos = pos2(anchor.rect.left(), anchor.rect.bottom() + 4.0);
        let ctx = self.ctx.clone();
        let area = Area::new(self.id.with("popover")).order(Order::Foreground).fixed_pos(pos).constrain(true).show(
            &ctx,
            |ui| {
                paint::overlay_frame(&ds).inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(POPOVER_WIDTH - 24.0);
                    ui.spacing_mut().item_spacing = vec2(space::S8, space::S8);
                    self.popover_body(ui, pop);
                });
            },
        );
        if !was_open {
            return;
        }
        // Enter commits (a held key's repeats do not count); Esc cancels; a click outside commits. Keyboard closes
        // hand focus back to the field, where the person was.
        let (enter, escape, pressed_outside) = self.ctx.input(|i| {
            let outside = i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p) && !anchor.rect.contains(p));
            let fresh_enter = i
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, repeat: false, .. }));
            (fresh_enter, i.key_pressed(Key::Escape), outside)
        });

        if escape {
            self.close(pop, false);
            anchor.request_focus();
        } else if enter {
            self.close(pop, true);
            anchor.request_focus();
        } else if pressed_outside {
            self.close(pop, true);
        }
    }

    fn popover_body(&mut self, ui: &mut Ui, pop: &mut Pop) {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;

        if !self.f.palette.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(space::S6, space::S6);
                for (i, slot) in self.f.palette.iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                    draw_swatch(ui.painter(), &ds, rect, c32(slot.colour), 10);
                    let linked = self.f.linked == Some(slot.name);
                    if linked {
                        ui.painter().circle_stroke(rect.center(), 12.0, Stroke::new(2.0, t.accent));
                    }
                    paint::focus_ring(ui.painter(), t, &response, rect, 10, RingPlacement::Outside);
                    a11y::describe(
                        &response,
                        || {
                            WidgetInfo::labeled(
                                WidgetType::Button,
                                true,
                                ds.text_with(Msg::ColourLinkTo, &[("slot", slot.name)]),
                            )
                        },
                        |_| {},
                    );
                    if response.clicked() {
                        self.event = Some(ColourEvent::Link(i));
                    }
                    response.on_hover_text(slot.name);
                }
            });
        }

        self.saturation_value(ui, pop);
        self.hue_bar(ui, pop);
        if self.f.alpha {
            self.alpha_bar(ui, pop);
        }
        self.fields(ui, pop);
        self.recent_row(ui, pop);
        if let Some(n) = self.f.neighbour {
            let ratio = contrast_ratio(c32(*self.f.colour).to_opaque(), c32(n.colour).to_opaque());
            let text = ds.text_with(Msg::ColourContrast, &[("ratio", &format!("{ratio:.1}")), ("slot", n.name)]);
            let status = if ratio >= 3.0 { Status::Ok } else { Status::Warn };
            ui.add(StatusChip::new(status, &text));
        }
    }

    fn saturation_value(&mut self, ui: &mut Ui, pop: &mut Pop) {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;
        let (rect, response) = ui.allocate_exact_size(AREA_SIZE, Sense::click_and_drag());
        let hue = pop.hsv[0];
        let mut mesh = Mesh::default();
        let (white, pure, clear, black) =
            (Color32::WHITE, c32(maths::from_hsv([hue, 1.0, 1.0], 255)), Color32::TRANSPARENT, Color32::BLACK);
        // Two layers: white to the pure hue left to right, then transparent to black top to bottom.
        for colours in [[white, pure, pure, white], [clear, clear, black, black]] {
            let base = u32::try_from(mesh.vertices.len()).unwrap_or(0);
            for (corner, colour) in
                [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()].into_iter().zip(colours)
            {
                mesh.vertices.push(Vertex { pos: corner, uv: egui::epaint::WHITE_UV, color: colour });
            }
            mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let painter = ui.painter();
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(rect, 0, Stroke::new(1.0, t.border_control), StrokeKind::Outside);

        if let Some(p) = response
            .interact_pointer_pos()
            .filter(|_| response.is_pointer_button_down_on() || response.dragged() || response.clicked())
        {
            let s = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let v = 1.0 - ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            self.set_hsv(pop, [hue, s, v]);
            response.request_focus();
        }
        if response.has_focus() {
            self.ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..egui::EventFilter::default()
                    },
                );
            });
            let step = self.ctx.input(|i| if i.modifiers.shift { 0.10 } else { 0.01 });
            let (dx, dy) = self.ctx.input(|i| {
                (
                    f32::from(i.key_pressed(Key::ArrowRight)) - f32::from(i.key_pressed(Key::ArrowLeft)),
                    f32::from(i.key_pressed(Key::ArrowUp)) - f32::from(i.key_pressed(Key::ArrowDown)),
                )
            });
            if dx != 0.0 || dy != 0.0 {
                self.set_hsv(
                    pop,
                    [hue, (pop.hsv[1] + dx * step).clamp(0.0, 1.0), (pop.hsv[2] + dy * step).clamp(0.0, 1.0)],
                );
            }
        }
        // Two-tone handle: a white ring with a black edge, readable on any colour.
        let handle = pos2(rect.left() + pop.hsv[1] * rect.width(), rect.top() + (1.0 - pop.hsv[2]) * rect.height());
        painter.circle_stroke(handle, 6.5, Stroke::new(1.0, Color32::from_black_alpha(180)));
        painter.circle_stroke(handle, 5.0, Stroke::new(2.0, Color32::WHITE));
        painter.circle_stroke(handle, 3.5, Stroke::new(1.0, Color32::from_black_alpha(180)));
        paint::focus_ring(painter, t, &response, rect, 0, RingPlacement::Outside);
        let (s, v) = (pop.hsv[1], pop.hsv[2]);
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, ds.text(Msg::ColourSatVal)),
            |node| {
                node.set_role(accesskit::Role::Slider);
                node.set_value(ds.text_with(
                    Msg::ColourSatValValue,
                    &[("saturation", &format!("{:.0}", s * 100.0)), ("value", &format!("{:.0}", v * 100.0))],
                ));
            },
        );
    }

    fn hue_bar(&mut self, ui: &mut Ui, pop: &mut Pop) {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;
        let (rect, response) = ui.allocate_exact_size(vec2(AREA_SIZE.x, BAR_HEIGHT), Sense::click_and_drag());
        let mut mesh = Mesh::default();
        for i in 0..=6u8 {
            let x = rect.left() + rect.width() * f32::from(i) / 6.0;
            let colour = c32(maths::from_hsv([f32::from(i) * 60.0, 1.0, 1.0], 255));
            mesh.vertices.push(Vertex { pos: pos2(x, rect.top()), uv: egui::epaint::WHITE_UV, color: colour });
            mesh.vertices.push(Vertex { pos: pos2(x, rect.bottom()), uv: egui::epaint::WHITE_UV, color: colour });
            if i > 0 {
                let b = u32::from(i) * 2;
                mesh.indices.extend_from_slice(&[b - 2, b - 1, b, b - 1, b + 1, b]);
            }
        }
        let painter = ui.painter();
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(rect, 0, Stroke::new(1.0, t.border_control), StrokeKind::Outside);
        if let Some(p) = response
            .interact_pointer_pos()
            .filter(|_| response.is_pointer_button_down_on() || response.dragged() || response.clicked())
        {
            self.set_hsv(pop, [(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * 360.0, pop.hsv[1], pop.hsv[2]]);
            response.request_focus();
        }
        if response.has_focus() {
            self.ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { horizontal_arrows: true, ..egui::EventFilter::default() },
                );
            });
            let step = self.ctx.input(|i| if i.modifiers.shift { 10.0 } else { 1.0 });
            let dx = self
                .ctx
                .input(|i| f32::from(i.key_pressed(Key::ArrowRight)) - f32::from(i.key_pressed(Key::ArrowLeft)));
            if dx != 0.0 {
                self.set_hsv(pop, [(pop.hsv[0] + dx * step).rem_euclid(360.0), pop.hsv[1], pop.hsv[2]]);
            }
        }
        draw_bar_handle(painter, rect, pop.hsv[0] / 360.0, c32(maths::from_hsv([pop.hsv[0], 1.0, 1.0], 255)));
        paint::focus_ring(painter, t, &response, rect, 0, RingPlacement::Outside);
        let hue = pop.hsv[0];
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, ds.text(Msg::ColourHue)),
            |node| {
                node.set_role(accesskit::Role::Slider);
                node.set_value(ds.text_with(Msg::ColourHueValue, &[("degrees", &format!("{hue:.0}"))]));
            },
        );
    }

    fn alpha_bar(&mut self, ui: &mut Ui, pop: &mut Pop) {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;
        let (rect, response) = ui.allocate_exact_size(vec2(AREA_SIZE.x, BAR_HEIGHT), Sense::click_and_drag());
        let painter = ui.painter();
        checkerboard(painter, &ds, rect, 0);
        let opaque = c32(maths::from_hsv(pop.hsv, 255));
        let mut mesh = Mesh::default();
        for (x, colour) in [(rect.left(), opaque.gamma_multiply(0.0)), (rect.right(), opaque)] {
            mesh.vertices.push(Vertex { pos: pos2(x, rect.top()), uv: egui::epaint::WHITE_UV, color: colour });
            mesh.vertices.push(Vertex { pos: pos2(x, rect.bottom()), uv: egui::epaint::WHITE_UV, color: colour });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(rect, 0, Stroke::new(1.0, t.border_control), StrokeKind::Outside);
        if let Some(p) = response
            .interact_pointer_pos()
            .filter(|_| response.is_pointer_button_down_on() || response.dragged() || response.clicked())
        {
            let a = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            pop.alpha = (a * 255.0).round() as u8;
            *self.f.colour = maths::from_hsv(pop.hsv, pop.alpha);
            self.event = Some(ColourEvent::Preview(*self.f.colour));
            response.request_focus();
        }
        if response.has_focus() {
            self.ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { horizontal_arrows: true, ..egui::EventFilter::default() },
                );
            });
            let step = self.ctx.input(|i| if i.modifiers.shift { 10 } else { 1 });
            let dx = self
                .ctx
                .input(|i| i32::from(i.key_pressed(Key::ArrowRight)) - i32::from(i.key_pressed(Key::ArrowLeft)));
            if dx != 0 {
                pop.alpha = u8::try_from((i32::from(pop.alpha) + dx * step).clamp(0, 255)).unwrap_or(255);
                *self.f.colour = maths::from_hsv(pop.hsv, pop.alpha);
                self.event = Some(ColourEvent::Preview(*self.f.colour));
            }
        }
        draw_bar_handle(painter, rect, f32::from(pop.alpha) / 255.0, c32(*self.f.colour));
        paint::focus_ring(painter, t, &response, rect, 0, RingPlacement::Outside);
        let alpha = pop.alpha;
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, ds.text(Msg::ColourAlpha)),
            |node| {
                node.set_role(accesskit::Role::Slider);
                node.set_value(
                    ds.text_with(Msg::ColourAlphaValue, &[("percent", &format!("{:.0}", f32::from(alpha) / 2.55))]),
                );
            },
        );
    }

    /// Hex, RGB and OKLCH fields, and the eyedropper: the keyboard-friendly path (7.4).
    fn fields(&mut self, ui: &mut Ui, pop: &mut Pop) {
        let ds = std::sync::Arc::clone(&self.ds);
        // Hex.
        // While the field has focus its text is the person's, not a re-formatting of the colour.
        let editing = self.ctx.data(|d| d.get_temp::<bool>(self.id.with("hex_editing"))).unwrap_or(false);
        let mut hex = if editing {
            self.ctx.data(|d| d.get_temp::<String>(self.id.with("hex"))).unwrap_or_default()
        } else {
            maths::to_hex(*self.f.colour, self.f.alpha)
        };
        let mut hex_focused = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            let (hex_name, hex_hint) = (ds.text(Msg::ColourHex), ds.text(Msg::HexHint));
            let field =
                ui.add(super::TextField::new(&hex_name, &mut hex).width(104.0).hint(&hex_hint).clearable(false));
            if field.changed()
                && let Some(c) = maths::parse_hex(&hex)
            {
                self.set(pop, if self.f.alpha { c } else { Colour::rgb(c.r, c.g, c.b) });
            }
            hex_focused = field.has_focus();
            let (pick_label, pick_name) = (ds.text(Msg::ColourPick), ds.text(Msg::ColourPickName));
            let pick =
                ui.add(super::Button::new(&pick_label).icon(icons::EYEDROPPER).ghost().accessible_label(&pick_name));
            if pick.clicked() {
                self.event = Some(ColourEvent::PickRequested);
            }
        });
        self.ctx.data_mut(|d| {
            d.insert_temp(self.id.with("hex_editing"), hex_focused);
            d.insert_temp(self.id.with("hex"), hex);
        });

        // RGB and OKLCH read-outs with their own numeric inputs.
        let rgb = [self.f.colour.r, self.f.colour.g, self.f.colour.b];
        let mut edited = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            for (i, (name, value)) in ["R", "G", "B"].into_iter().zip(rgb).enumerate() {
                let mut v = f64::from(value);
                if numeric(ui, &ds, self.id.with(("rgb", i)), name, &mut v, 0.0, 255.0, 0) {
                    edited = Some((i, v));
                }
            }
        });
        if let Some((i, v)) = edited {
            let mut c = [self.f.colour.r, self.f.colour.g, self.f.colour.b];
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255")]
            {
                c[i] = v.round().clamp(0.0, 255.0) as u8;
            }
            self.set(pop, Colour::rgba(c[0], c[1], c[2], pop.alpha));
        }
        let lch = maths::to_oklch(*self.f.colour);
        let mut edited = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            for (i, (name, value, max, decimals)) in
                [("L", lch[0], 1.0, 3usize), ("C", lch[1], 0.4, 3), ("H", lch[2], 360.0, 0)].into_iter().enumerate()
            {
                let mut v = value;
                if numeric(ui, &ds, self.id.with(("lch", i)), name, &mut v, 0.0, max, decimals) {
                    edited = Some((i, v));
                }
            }
        });
        if let Some((i, v)) = edited {
            let mut l = lch;
            l[i] = v;
            self.set(pop, maths::from_oklch(l, pop.alpha));
        }
    }

    fn recent_row(&mut self, ui: &mut Ui, pop: &mut Pop) {
        if self.f.recent.is_empty() {
            return;
        }
        let ds = std::sync::Arc::clone(&self.ds);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            for (i, colour) in self.f.recent.iter().take(8).enumerate() {
                let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                draw_swatch(ui.painter(), &ds, rect, c32(*colour), 4);
                paint::focus_ring(ui.painter(), &ds.theme, &response, rect, 4, RingPlacement::Outside);
                a11y::describe(
                    &response,
                    || {
                        WidgetInfo::labeled(
                            WidgetType::Button,
                            true,
                            ds.text_with(Msg::ColourRecent, &[("number", &(i + 1).to_string())]),
                        )
                    },
                    |node| {
                        node.set_value(maths::to_hex(*colour, true));
                    },
                );
                if response.clicked() {
                    self.set(pop, *colour);
                }
            }
        });
    }
}

/// A small labelled numeric input: the label, then a mono field you click and type into.
#[allow(clippy::too_many_arguments, reason = "one internal helper shared by the RGB and OKLCH rows")]
fn numeric(ui: &mut Ui, ds: &Ds, id: Id, label: &str, value: &mut f64, min: f64, max: f64, decimals: usize) -> bool {
    let t = &ds.theme;
    let ctx = ui.ctx().clone();
    let mut changed = false;
    ui.label(egui::RichText::new(label).font(ds.text.micro.clone()).color(t.text3));
    // Text that was not a number stays in the box (and its message stays) until it is fixed, whether the person
    // pressed Enter or left the box: a rejected entry must not quietly turn back into the old value.
    let invalid = ctx.data(|d| d.get_temp::<String>(invalid_key(id)));
    let formatted = || NumberBuf::new(*value, decimals).as_str().to_owned();
    let mut buffer = if ctx.memory(|m| m.has_focus(id)) {
        ctx.data(|d| d.get_temp::<String>(id.with("buf"))).unwrap_or_default()
    } else if let Some(left_behind) = ctx.data_mut(|d| d.remove_temp::<String>(id.with("buf"))) {
        // The frame the box was left in: its text is judged like an Enter would, not replaced by the old number.
        left_behind
    } else {
        invalid.clone().unwrap_or_else(formatted)
    };
    // Enter belongs to the IME while it composes: the call takes it out of the frame's events. The preedit is drawn
    // beside the text and never in it, so a blur mid-composition judges the committed text.
    let ime = ime::begin(&ctx, id, &buffer);
    // Read before the edit is shown: egui's `TextEdit` gives up the focus on Enter, so afterwards it looks unfocused.
    let enter = ctx.memory(|m| m.has_focus(id)) && ctx.input(|i| i.key_pressed(Key::Enter));
    let mut composing_layout = ime.layouter(ds.text.mono.clone(), t.text, egui::Align::RIGHT);
    let mut edit = egui::TextEdit::singleline(&mut buffer)
        .id(id)
        .font(ds.text.mono.clone())
        .horizontal_align(egui::Align::RIGHT)
        .desired_width(46.0)
        .margin(Margin::symmetric(4, 3));
    if ime.composing() {
        edit = edit.layouter(&mut composing_layout);
    }
    let response = ui.add(edit);
    ime.finish(&ctx, id, &buffer);
    if response.gained_focus() {
        buffer = invalid.unwrap_or_else(formatted);
    }
    if response.has_focus() {
        ctx.data_mut(|d| d.insert_temp(id.with("buf"), buffer.clone()));
    }
    if enter || response.lost_focus() {
        if let Ok(v) = crate::ds::expr::parse(&buffer, "", min, max) {
            *value = v.clamp(min, max);
            changed = true;
            ctx.data_mut(|d| d.remove::<String>(invalid_key(id)));
        } else {
            ctx.data_mut(|d| d.insert_temp(invalid_key(id), buffer.clone()));
            if enter {
                // The popover's Enter must not close over text that is not a number: keep the box open.
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { key: Key::Enter, .. })));
                response.request_focus();
            }
        }
    }
    if ctx.data(|d| d.get_temp::<String>(invalid_key(id))).is_some() {
        error_bubble(&ctx, ds, response.rect, id, (min, max, decimals));
    }
    a11y::describe(
        &response,
        || WidgetInfo::labeled(WidgetType::TextEdit, true, ds.text_with(Msg::ColourChannel, &[("channel", label)])),
        |_| {},
    );
    changed
}

/// Where a channel box keeps text that was rejected.
fn invalid_key(id: Id) -> Id {
    id.with("invalid")
}

/// "Enter a number from 0 to 255" under a channel box whose text is not a number.
fn error_bubble(ctx: &egui::Context, ds: &Ds, field: Rect, id: Id, (min, max, decimals): (f64, f64, usize)) {
    let t = &ds.theme;
    let message = ds.text_with(
        Msg::ParamRangeError,
        &[("min", NumberBuf::new(min, decimals).as_str()), ("max", NumberBuf::new(max, decimals).as_str())],
    );
    egui::Area::new(id.with("error_bubble"))
        .order(Order::Foreground)
        .fixed_pos(pos2(field.right(), field.bottom() + space::S4))
        .pivot(Align2::RIGHT_TOP)
        .show(ctx, |ui| {
            paint::overlay_frame(ds).stroke(Stroke::new(1.0, t.danger)).inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(icons::WARNING).font(ds.text.icon(14.0, true)).color(t.danger));
                    ui.label(egui::RichText::new(message).font(ds.text.small.clone()).color(t.text));
                });
            });
        });
}

/// The swatch: a checkerboard behind the colour (so alpha shows), with a control border.
fn draw_swatch(painter: &egui::Painter, ds: &Ds, rect: Rect, colour: Color32, radius: u8) {
    if colour.a() < 255 {
        checkerboard(painter, ds, rect, radius);
        painter.rect_filled(rect, radius, colour);
    } else {
        painter.rect_filled(rect, radius, colour);
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_black_alpha(90)), StrokeKind::Inside);
    painter.rect_stroke(
        rect.shrink(1.0),
        radius.saturating_sub(1),
        Stroke::new(1.0, Color32::from_white_alpha(28)),
        StrokeKind::Inside,
    );
}

/// A 5 pt grey checkerboard, clipped to the rectangle.
fn checkerboard(painter: &egui::Painter, ds: &Ds, rect: Rect, radius: u8) {
    let t = &ds.theme;
    let (a, b) = (mix_grey(t.bg4, 0.4), mix_grey(t.text3, 0.55));
    painter.rect_filled(rect, radius, a);
    let clipped = painter.with_clip_rect(rect);
    let cell = 5.0;
    let (cols, rows) = ((rect.width() / cell).ceil() as i32, (rect.height() / cell).ceil() as i32);
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 == 1 {
                let r = Rect::from_min_size(
                    pos2(rect.left() + col as f32 * cell, rect.top() + row as f32 * cell),
                    vec2(cell, cell),
                );
                clipped.rect_filled(r, 0, b);
            }
        }
    }
}

fn mix_grey(c: Color32, t: f32) -> Color32 {
    let g = f32::from((u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b())) / 3);
    Color32::from_gray((g * t + 90.0 * (1.0 - t)) as u8)
}

/// A handle on a bar: a white pill with a dark edge showing the current value.
fn draw_bar_handle(painter: &egui::Painter, bar: Rect, t: f32, fill: Color32) {
    let x = bar.left() + t.clamp(0.0, 1.0) * bar.width();
    let handle = Rect::from_center_size(pos2(x, bar.center().y), vec2(8.0, bar.height() + 6.0));
    painter.rect_filled(handle, 3, fill.to_opaque());
    painter.rect_stroke(handle, 3, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
    painter.rect_stroke(handle.expand(1.0), 4, Stroke::new(1.0, Color32::from_black_alpha(170)), StrokeKind::Outside);
}
