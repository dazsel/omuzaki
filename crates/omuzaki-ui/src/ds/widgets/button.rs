//! Button (`design-system.md` section 7.0): primary, secondary, ghost and danger.
//!
//! Height is the control row. The label is a verb. A disabled button keeps its place and says why on hover and on
//! keyboard focus (P9), so "Approve" greyed out is never a mystery. A button never grows past the room it is given: a
//! label that does not fit (German, or the pseudo-locale) ends in an ellipsis and the tooltip carries the whole label.

use egui::{Align2, Color32, Context, Response, Sense, Stroke, StrokeKind, Ui, Widget, vec2};

use super::keycap::{CapStyle, KeyCaps};
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, Theme, mix, radius, space};
use crate::register_contrast;

/// How loudly a button asks to be pressed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonKind {
    /// The one main action of a surface: accent fill.
    Primary,
    /// Everything else: raised fill with a control border.
    #[default]
    Secondary,
    /// Text only, for toolbars and quiet actions.
    Ghost,
    /// Destructive: outlined in the danger colour (never filled until it is confirmed).
    Danger,
}

impl ButtonKind {
    /// Every kind, for the contrast table and the gallery.
    pub const ALL: [Self; 4] = [Self::Primary, Self::Secondary, Self::Ghost, Self::Danger];
}

/// A button. Build it, then `ui.add(..)` it and read the response's `clicked()`.
#[derive(Clone, Copy, Debug)]
#[must_use = "a button does nothing until it is added to a Ui"]
pub struct Button<'a> {
    label: &'a str,
    kind: ButtonKind,
    icon: Option<&'a str>,
    shortcut: Option<&'a str>,
    hint_only: bool,
    op: Option<&'a str>,
    disabled: Option<&'a str>,
    accessible_label: Option<&'a str>,
    min_width: f32,
}

impl<'a> Button<'a> {
    /// A secondary button. `label` is a verb ("Approve", "Reject…") and is also the accessible name.
    pub fn new(label: &'a str) -> Self {
        a11y::require_label(label);
        Self {
            label,
            kind: ButtonKind::Secondary,
            icon: None,
            shortcut: None,
            hint_only: false,
            op: None,
            disabled: None,
            accessible_label: None,
            min_width: 0.0,
        }
    }

    /// Sets the kind.
    pub const fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// The main action: accent fill.
    pub const fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    /// Text only.
    pub const fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    /// Destructive.
    pub const fn danger(self) -> Self {
        self.kind(ButtonKind::Danger)
    }

    /// A leading icon glyph from [`crate::ds::icons`].
    pub const fn icon(mut self, glyph: &'a str) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// The shortcut, as key caps ("Ctrl Enter"); also shown in the tooltip.
    pub const fn shortcut(mut self, keys: &'a str) -> Self {
        self.shortcut = Some(keys);
        self
    }

    /// The shortcut, shown in the tooltip only: for a button too crowded to carry key caps.
    pub const fn shortcut_hint(mut self, keys: &'a str) -> Self {
        self.shortcut = Some(keys);
        self.hint_only = true;
        self
    }

    /// The op this runs, shown in mono in the tooltip.
    pub const fn op(mut self, op: &'a str) -> Self {
        self.op = Some(op);
        self
    }

    /// Disables the button, saying why (shown on hover and keyboard focus).
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Disables the button with `reason` when `condition` holds.
    pub const fn disabled_if(self, condition: bool, reason: &'a str) -> Self {
        if condition { self.disabled(reason) } else { self }
    }

    /// A fuller accessible name than the visible label, such as "Approve rendering 24 contact sheets for Designer".
    pub const fn accessible_label(mut self, label: &'a str) -> Self {
        self.accessible_label = Some(label);
        self
    }

    /// At least this wide; the content is centred.
    pub const fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    /// The width the button will take, so a row can be laid out left to right (and so tab in reading order) while
    /// still being aligned to its right edge.
    pub fn width(&self, ctx: &Context) -> f32 {
        self.measure(ctx, &ds(ctx)).width
    }

    fn measure(&self, ctx: &Context, ds: &Ds) -> Measure<'a> {
        let label_w = paint::text_width(ctx, self.label, &ds.text.strong);
        let icon_w = if self.icon.is_some() { 16.0 + space::S6 } else { 0.0 };
        let caps = self.shortcut.filter(|_| !self.hint_only).map(KeyCaps::new);
        let caps_w = caps.map_or(0.0, |c| c.width(ctx) + space::S8);
        let content_w = icon_w + label_w + caps_w;
        Measure { label_w, icon_w, content_w, width: (content_w + 2.0 * PAD).max(self.min_width), caps }
    }
}

/// Horizontal padding inside a button.
const PAD: f32 = 12.0;
/// The least room a label keeps when the button is squeezed: an ellipsis and a letter.
const MIN_LABEL: f32 = 16.0;

/// The parts of a button's width.
struct Measure<'a> {
    label_w: f32,
    icon_w: f32,
    content_w: f32,
    width: f32,
    caps: Option<KeyCaps<'a>>,
}

/// The resting, hovered and pressed colours of a button face. Shared with the other controls that look like a button
/// (the dropdown), so they stay in step with it and with its rows in the contrast table.
pub(super) struct Face {
    pub(super) fill: [Color32; 3],
    pub(super) stroke: Option<[Color32; 2]>,
    pub(super) text: Color32,
    pub(super) cap: CapStyle,
}

pub(super) fn face(kind: ButtonKind, t: &Theme, enabled: bool) -> Face {
    let transparent = Color32::TRANSPARENT;
    if !enabled {
        let cap = CapStyle { fill: transparent, stroke: t.line, text: t.text_disabled };
        // The primary fill is `bg2` too: `text.disabled` on `bg4` is only 2.3:1 in dark, so a dead primary button
        // looks like a dead secondary one, which is what disabled means.
        return match kind {
            ButtonKind::Primary | ButtonKind::Secondary => {
                Face { fill: [t.bg2; 3], stroke: Some([t.line; 2]), text: t.text_disabled, cap }
            }
            ButtonKind::Ghost => Face { fill: [transparent; 3], stroke: None, text: t.text_disabled, cap },
            ButtonKind::Danger => {
                Face { fill: [transparent; 3], stroke: Some([t.line; 2]), text: t.text_disabled, cap }
            }
        };
    }
    match kind {
        ButtonKind::Primary => Face {
            fill: [t.accent, t.accent_hover, t.accent_press],
            stroke: None,
            text: t.on_accent,
            // The cap is darker than the face (`accent.press`), never lighter: a lighter cap under `on.accent` text
            // was 3.8:1 in light.
            cap: CapStyle { fill: t.accent_press, stroke: mix(t.on_accent, t.accent, 45), text: t.on_accent },
        },
        ButtonKind::Secondary => Face {
            fill: [t.bg3, t.bg4, mix(t.border_control, t.bg4, 18)],
            stroke: Some([t.border_control, t.text3]),
            text: t.text,
            cap: CapStyle { fill: t.bg2, stroke: t.line, text: t.text2 },
        },
        ButtonKind::Ghost => Face {
            fill: [transparent, t.bg3, t.bg4],
            stroke: None,
            text: t.text2,
            cap: CapStyle { fill: t.bg2, stroke: t.line, text: t.text2 },
        },
        ButtonKind::Danger => Face {
            fill: [transparent, t.status_tint(Status::Danger, t.bg1), t.status_press(Status::Danger, t.bg1)],
            stroke: Some([t.danger, t.danger]),
            text: t.danger,
            cap: CapStyle { fill: t.bg2, stroke: t.line, text: t.text2 },
        },
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let measure = self.measure(ui.ctx(), &ds);
        // At least room for the padding, the icon and a letter with an ellipsis; at most what the layout has left.
        let width = measure.width.min(paint::fit_width(ui)).max(2.0 * PAD + measure.icon_w + MIN_LABEL);
        let sense = if enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let (rect, response) = ui.allocate_exact_size(vec2(width, ds.metrics.control_row), sense);
        self.finish(ui, &ds, &measure, rect, response)
    }
}

impl<'a> Button<'a> {
    /// Shows the button in `rect`, wherever the layout's cursor is, as the widget `id`.
    ///
    /// For a composite that places its own parts (a job row's Cancel, a step card's actions): it takes no layout, so a
    /// row of them costs no child `Ui`. The caller makes `id` unique and stable; the button fills `rect`.
    pub fn show_at(self, ui: &mut Ui, id: egui::Id, rect: egui::Rect) -> Response {
        let ds = ds(ui.ctx());
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let measure = self.measure(ui.ctx(), &ds);
        let sense = if enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let response = ui.interact(rect, id, sense);
        self.finish(ui, &ds, &measure, rect, response)
    }

    /// Paints the button into `rect` and describes it. The room the label gets is what `rect` leaves after the
    /// padding, icon and key caps; less than the label needs means it is cut, and the tooltip says it whole.
    fn finish(self, ui: &mut Ui, ds: &Ds, measure: &Measure<'a>, rect: egui::Rect, response: Response) -> Response {
        let (t, ctx) = (&ds.theme, ui.ctx().clone());
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let (icon_w, caps) = (measure.icon_w, measure.caps);
        let caps_w = measure.content_w - measure.label_w - icon_w;
        let label_room = (rect.width() - 2.0 * PAD - icon_w - caps_w).max(MIN_LABEL);
        let label_w = measure.label_w.min(label_room);
        let truncated = measure.label_w > label_room;
        let content_w = icon_w + label_w + caps_w;

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            // Only the button's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
            let face = face(self.kind, t, self.disabled.is_none());
            let hover = paint::fade(&ctx, ds, response.id.with("hover"), enabled && paint::hot(&response));
            let pressed = enabled && response.is_pointer_button_down_on();
            let fill = if pressed { face.fill[2] } else { paint::lerp(face.fill[0], face.fill[1], hover) };
            let rect = paint::snap(&ctx, rect);
            painter.rect_filled(rect, radius::CONTROL, fill);
            if let Some([rest, hovered]) = face.stroke {
                let colour = paint::lerp(rest, hovered, hover);
                painter.rect_stroke(rect, radius::CONTROL, Stroke::new(t.control_stroke, colour), StrokeKind::Inside);
            }

            let mut x = rect.center().x - content_w / 2.0;
            let cy = rect.center().y;
            if let Some(glyph) = self.icon {
                paint::icon(painter, ds, egui::pos2(x + 8.0, cy), glyph, 16.0, face.text, false);
                x += icon_w;
            }
            let label_area = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(label_room, rect.height()));
            paint::text_in(painter, label_area, Align2::LEFT_CENTER, self.label, &ds.text.strong, face.text);
            if let Some(caps) = caps {
                caps.paint(painter, ds, egui::pos2(x + label_w + space::S8, cy), face.cap);
            }
            paint::focus_ring(painter, t, &response, rect, radius::CONTROL, RingPlacement::Outside);
        }

        a11y::describe(
            &response,
            || {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    enabled,
                    self.accessible_label.unwrap_or(self.label),
                )
            },
            |node| {
                if let Some(reason) = self.disabled {
                    node.set_description(reason);
                }
            },
        );

        let mut tip = Tip::new(self.label);
        tip.shortcut = self.shortcut;
        tip.op = self.op;
        tip.detail = self.disabled;
        if truncated || self.disabled.is_some() || self.op.is_some() || self.shortcut.is_some() {
            response.tip(tip)
        } else {
            response
        }
    }
}

register_contrast!("button", pairs);

/// Every label and key cap of every kind in every state, on the fill it is drawn on.
fn pairs(table: &mut Table<'_>) {
    let t = *table.theme();
    let surfaces = table.panel_surfaces();
    for kind in ButtonKind::ALL {
        for enabled in [true, false] {
            let face = face(kind, &t, enabled);
            let rule = if enabled { Rule::Text } else { Rule::Disabled };
            let look = if enabled { "" } else { "disabled " };
            for (state, fill) in ["rest", "hover", "pressed"].into_iter().zip(face.fill) {
                let name = format!("{kind:?} {look}{state}");
                // A transparent fill shows the panel or card behind it.
                let behind: Vec<(&str, Color32)> =
                    if fill.a() == 0 { surfaces.to_vec() } else { vec![("its fill", fill)] };
                table.add_on(&format!("{name} label"), face.text, &behind, rule);
                let cap_behind: Vec<(&str, Color32)> =
                    if face.cap.fill.a() == 0 { behind.clone() } else { vec![("its cap", face.cap.fill)] };
                table.add_on(&format!("{name} key cap"), face.cap.text, &cap_behind, rule);
            }
        }
    }
}
