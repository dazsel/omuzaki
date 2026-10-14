//! Segmented control (`design-system.md` section 7.3): two to five mutually exclusive options in one row.
//!
//! One tab stop for the whole group; Left and Right move the selection (radio-group semantics, roving focus). The
//! selected segment is a `bg4` fill with a 2 pt accent underline and Medium weight, so selection never relies on
//! colour alone. Icon-only segments need a label, which is also their accessible name and their tooltip. The group
//! never grows past the room it is given: a label that does not fit ends in an ellipsis and its tooltip says it all.

use egui::{
    Align2, EventFilter, Id, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, Widget, WidgetInfo,
    WidgetType, accesskit, pos2, vec2,
};

use super::ids;
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{radius, size, space};

/// One option: the value it sets and the label it shows.
#[derive(Clone, Copy, Debug)]
#[must_use = "a segment does nothing until it is passed to a segmented control"]
pub struct Segment<'a, T> {
    /// What the control's value becomes when this segment is chosen.
    pub value: T,
    /// The label; for an icon-only segment it is the accessible name and the tooltip.
    pub label: &'a str,
    /// An optional leading icon glyph.
    pub icon: Option<&'a str>,
    /// Show only the icon (the label stays the accessible name).
    pub icon_only: bool,
}

impl<'a, T> Segment<'a, T> {
    /// A text segment.
    pub fn new(value: T, label: &'a str) -> Self {
        a11y::require_label(label);
        Self { value, label, icon: None, icon_only: false }
    }

    /// Adds an icon before the label.
    pub const fn icon(mut self, glyph: &'a str) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// Shows only the icon; the label remains the accessible name.
    pub const fn icon_only(mut self) -> Self {
        self.icon_only = true;
        self
    }
}

/// A row of 2 to 5 exclusive options bound to `value`.
#[must_use = "a segmented control does nothing until it is added to a Ui"]
pub struct Segmented<'a, T> {
    label: &'a str,
    value: &'a mut T,
    segments: &'a [Segment<'a, T>],
    stretch: bool,
    disabled: Option<&'a str>,
    salt: Option<Id>,
}

impl<'a, T: Copy + PartialEq> Segmented<'a, T> {
    /// A control named `label` (the group's accessible name) over `segments`.
    pub fn new(label: &'a str, value: &'a mut T, segments: &'a [Segment<'a, T>]) -> Self {
        a11y::require_label(label);
        debug_assert!((2..=5).contains(&segments.len()), "a segmented control holds 2 to 5 options; use a dropdown");
        Self { label, value, segments, stretch: false, disabled: None, salt: None }
    }

    /// Fill the available width instead of sizing to the labels.
    pub const fn stretch(mut self) -> Self {
        self.stretch = true;
        self
    }

    /// Disables the control, saying why (the accessible description).
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the control's state (which segment has the keyboard focus, and every segment's hover) by `salt` instead
    /// of by its position in the `Ui`, so reordering the rows of a form keeps the focus with its control. Use a value
    /// that names the setting, unique among the segmented controls of one `Ui`.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }
}

/// What a segmented control's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.segmented";

impl<T: Copy + PartialEq> Widget for Segmented<'_, T> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let ctx = ui.ctx().clone();
        let enabled = ui.is_enabled() && self.disabled.is_none();
        // Only the control's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
        let looks_enabled = self.disabled.is_none();
        let count = self.segments.len();

        let natural = natural_width(&ctx, &ds, self.segments);
        #[allow(clippy::cast_precision_loss, reason = "at most five segments")]
        let width =
            if self.stretch { ui.available_width() } else { (natural * count as f32).min(paint::fit_width(ui)) };
        let (rect, mut group) = ids::allocate(ui, vec2(width, m.control_row), Sense::hover(), KIND, self.salt);
        let gid = group.id;
        let rect = paint::snap(&ctx, rect);

        // Keyboard first, so the segment that becomes selected is also the one created focusable this frame.
        let mut selected = self.segments.iter().position(|s| s.value == *self.value);
        if enabled && let Some(to) = roving_target(&ctx, gid, count) {
            *self.value = self.segments[to].value;
            group.mark_changed();
            selected = Some(to);
            ctx.memory_mut(|mem| mem.request_focus(gid.with(to)));
        }

        let tab_stop = selected.unwrap_or(0);
        let seg_w = rect.width() / (count as f32);
        let builder = UiBuilder::new().id(gid.with("segments")).max_rect(rect).accessibility_parent(gid);
        let mut clicked_index = None;
        let mut ring_index = None;
        // A child `Ui` that does not move the parent's cursor: the group was already allocated above, and `scope_builder`
        // would advance the cursor again by the child's content, which for painted-only content is nothing, pulling
        // the next widget of a left-to-right row back over this one.
        {
            let ui = &mut ui.new_child(builder);
            let painter = ui.painter().clone();
            painter.rect_filled(rect, radius::CONTROL, paint::dim(looks_enabled, t.bg2));
            for (i, segment) in self.segments.iter().enumerate() {
                let seg =
                    Rect::from_min_size(pos2(rect.left() + seg_w * (i as f32), rect.top()), vec2(seg_w, rect.height()));
                let sense = if !enabled {
                    Sense::hover()
                } else if i == tab_stop {
                    Sense::click()
                } else {
                    Sense::CLICK
                };
                let response = ui.interact(seg, gid.with(i), sense);
                if response.has_focus() {
                    ring_index = Some(i);
                }
                if response.clicked() && enabled {
                    clicked_index = Some(i);
                    ctx.memory_mut(|mem| mem.request_focus(gid.with(i)));
                }
                let is_selected = selected == Some(i);
                let hover =
                    paint::fade(&ctx, &ds, gid.with(i).with("hover"), enabled && paint::hot(&response) && !is_selected);
                let cut = draw_segment(&painter, &ds, seg, segment, is_selected, hover, looks_enabled);
                a11y::describe(
                    &response,
                    || WidgetInfo::selected(WidgetType::RadioButton, enabled, is_selected, segment.label),
                    |_| {},
                );
                // An icon alone, or a label that was cut, is completed by the tooltip.
                if segment.icon_only || cut {
                    let _ = response.tip(Tip::new(segment.label));
                }
            }
            painter.rect_stroke(
                rect,
                radius::CONTROL,
                Stroke::new(t.control_stroke, paint::dim(looks_enabled, t.border_control)),
                StrokeKind::Inside,
            );
            if let Some(i) = ring_index.filter(|_| crate::ds::focus::visible(&ctx)) {
                let seg =
                    Rect::from_min_size(pos2(rect.left() + seg_w * (i as f32), rect.top()), vec2(seg_w, rect.height()));
                paint::draw_ring(&painter, t, seg, radius::CONTROL, RingPlacement::Outside);
            }
        }

        if let Some(i) = clicked_index
            && selected != Some(i)
        {
            *self.value = self.segments[i].value;
            group.mark_changed();
        }

        let selected_label = self.segments.iter().find(|s| s.value == *self.value).map(|s| s.label);
        a11y::describe(
            &group,
            || WidgetInfo::labeled(WidgetType::RadioGroup, enabled, self.label),
            |node| {
                node.set_role(accesskit::Role::RadioGroup);
                if let Some(label) = selected_label {
                    node.set_value(label);
                }
                if let Some(reason) = self.disabled {
                    node.set_description(reason);
                }
            },
        );
        group
    }
}

/// The width one segment needs for its widest label, icon and padding: every segment of a group is this wide.
fn natural_width<T>(ctx: &egui::Context, ds: &Ds, segments: &[Segment<'_, T>]) -> f32 {
    segments
        .iter()
        .map(|s| {
            let text = if s.icon_only { 0.0 } else { paint::text_width(ctx, s.label, &ds.text.strong) };
            let icon = if s.icon.is_some() { 16.0 + if s.icon_only { 0.0 } else { space::S6 } } else { 0.0 };
            text + icon + 2.0 * space::S12
        })
        .fold(size::SEGMENT_MIN, f32::max)
}

/// The segment an arrow, Home or End key asks for, when one of the group's segments has focus (roving focus).
pub(super) fn roving_target(ctx: &egui::Context, gid: egui::Id, count: usize) -> Option<usize> {
    let from = (0..count).find(|&i| ctx.memory(|mem| mem.has_focus(gid.with(i))))?;
    ctx.memory_mut(|mem| {
        mem.set_focus_lock_filter(gid.with(from), EventFilter { horizontal_arrows: true, ..EventFilter::default() });
    });
    let target = ctx.input(|i| {
        if i.key_pressed(Key::ArrowRight) {
            Some((from + 1).min(count - 1))
        } else if i.key_pressed(Key::ArrowLeft) {
            Some(from.saturating_sub(1))
        } else if i.key_pressed(Key::Home) {
            Some(0)
        } else if i.key_pressed(Key::End) {
            Some(count - 1)
        } else {
            None
        }
    });
    target.filter(|&to| to != from)
}

/// One segment's fill, underline, icon and label. Returns whether the label had to be cut to fit.
fn draw_segment<T>(
    painter: &egui::Painter,
    ds: &Ds,
    seg: Rect,
    segment: &Segment<'_, T>,
    selected: bool,
    hover: f32,
    enabled: bool,
) -> bool {
    let t = &ds.theme;
    let ctx = painter.ctx();
    let inner = seg.shrink(2.0);
    if selected {
        // A disabled control keeps the underline but not the fill: `text.disabled` on a dimmed `bg4` is 2.7:1.
        if enabled {
            painter.rect_filled(inner, radius::CONTROL - 1, t.bg4);
        }
        let bar = Rect::from_center_size(
            pos2(inner.center().x, inner.bottom() - 1.0),
            vec2((inner.width() - 12.0).max(8.0), 2.0),
        );
        painter.rect_filled(bar, 1, paint::dim(enabled, t.accent));
    } else if hover > 0.0 {
        painter.rect_filled(inner, radius::CONTROL - 1, t.bg3.gamma_multiply(hover));
    }
    let (font, ink) = if selected { (&ds.text.strong, t.text) } else { (&ds.text.body, t.text2) };
    let ink = paint::ink(t, enabled, ink);
    let icon_w = if segment.icon.is_some() { 16.0 + if segment.icon_only { 0.0 } else { space::S6 } } else { 0.0 };
    // The label gets what the segment has left after its padding and icon; wider than that, it ends in an ellipsis.
    let room = (inner.width() - 2.0 * space::S8 - icon_w).max(0.0);
    let wanted = if segment.icon_only { 0.0 } else { paint::text_width(ctx, segment.label, font) };
    let text_w = wanted.min(room);
    let mut x = seg.center().x - f32::midpoint(text_w, icon_w);
    let cy = seg.center().y - if selected { 1.0 } else { 0.0 };
    if let Some(glyph) = segment.icon {
        paint::icon(painter, ds, pos2(x + 8.0, cy), glyph, 16.0, ink, selected);
        x += icon_w;
    }
    if !segment.icon_only {
        let area = Rect::from_min_size(pos2(x, cy - seg.height() / 2.0), vec2(text_w, seg.height()));
        paint::text_in(painter, area, Align2::LEFT_CENTER, segment.label, font, ink);
    }
    wanted > room
}
