//! `TextField` (`design-system.md` section 7.0): `bg2` fill, a control border, a placeholder in `text3`, a clear button
//! when there is text, and Esc that clears first and then leaves the field. A search field adds a leading icon.
//!
//! IME composition (SE-02) goes through [`super::ime`]: the preedit is drawn at the caret but is never in the text, and
//! Enter and Esc belong to the IME while it composes, so neither submits, clears nor leaves.

use egui::{Align, Frame, Id, Key, Response, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Widget, pos2, vec2};

use super::ids;
use super::ime;
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, radius, size, space};

/// A single-line text input.
#[must_use = "a text field does nothing until it is added to a Ui"]
pub struct TextField<'a> {
    label: &'a str,
    text: &'a mut String,
    hint: &'a str,
    search: bool,
    error: Option<&'a str>,
    width: Option<f32>,
    clearable: bool,
    disabled: Option<&'a str>,
    salt: Option<Id>,
}

impl<'a> TextField<'a> {
    /// A field named `label` (the accessible name) editing `text`.
    pub fn new(label: &'a str, text: &'a mut String) -> Self {
        a11y::require_label(label);
        Self {
            label,
            text,
            hint: "",
            search: false,
            error: None,
            width: None,
            clearable: true,
            disabled: None,
            salt: None,
        }
    }

    /// The placeholder shown while empty.
    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// A search field: adds a leading magnifying glass.
    pub const fn search(mut self) -> Self {
        self.search = true;
        self
    }

    /// An error message: the border turns `danger` and the message shows under the field.
    pub const fn error(mut self, message: &'a str) -> Self {
        self.error = Some(message);
        self
    }

    /// Whether the clear button shows when there is text (on by default); off for short fixed-format fields such as hex.
    pub const fn clearable(mut self, clearable: bool) -> Self {
        self.clearable = clearable;
        self
    }

    /// A fixed width instead of the available width.
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Disables the field, saying why (the accessible description).
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the field's state (keyboard focus, the caret and selection, undo) by `salt` instead of by its position
    /// in the `Ui`, so reordering the rows of a form keeps the caret with its field. Use a value that names what the
    /// field edits, unique among the text fields of one `Ui`.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }
}

/// What a text field's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.text_field";

impl Widget for TextField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let ctx = ui.ctx().clone();
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, outer) = ids::allocate(ui, vec2(width, m.control_row), Sense::hover(), KIND, self.salt);
        let rect = paint::snap(&ctx, rect);
        let id = outer.id.with("edit");
        let focused = ctx.memory(|mem| mem.has_focus(id));
        let ime = ime::begin(&ctx, id, self.text);

        let lead = if self.search { 26.0 } else { space::S8 };
        let clear_w = if self.text.is_empty() || !self.clearable { space::S8 } else { size::MIN_TARGET + space::S4 };
        let hover = paint::fade(&ctx, &ds, outer.id, enabled && (paint::hot(&outer) || focused));
        self.paint_frame(ui, &ds, rect, focused, hover);

        let inner = egui::Rect::from_min_max(
            pos2(rect.left() + lead, rect.top() + 1.0),
            pos2(rect.right() - clear_w, rect.bottom() - 1.0),
        );
        // A child `Ui` that does not move the parent's cursor: the field was allocated above, and `scope_builder` would
        // set the cursor to the bottom of the inner rectangle, a point above it, so the next widget of a vertical layout
        // would overlap the field. Left to right and centred, so the text sits mid-height whatever the parent lays out.
        let mut response = {
            let ui =
                &mut ui.new_child(UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(Align::Center)));
            let ink = paint::ink(t, self.disabled.is_none(), t.text);
            let mut composing_layout = ime.layouter(ds.text.body.clone(), ink, Align::LEFT);
            let mut edit = TextEdit::singleline(self.text)
                .id(id)
                .frame(Frame::NONE)
                .font(ds.text.body.clone())
                .text_color(paint::ink(t, self.disabled.is_none(), t.text))
                .hint_text(
                    egui::RichText::new(self.hint)
                        .color(paint::ink(t, self.disabled.is_none(), t.text3))
                        .font(ds.text.body.clone()),
                )
                .vertical_align(Align::Center)
                .margin(egui::Margin::ZERO)
                .desired_width(inner.width())
                .interactive(enabled)
                // Esc is ours: it clears first and only then leaves, so the TextEdit must not surrender focus itself. The
                // arrows stay too, as they do in egui's own filter: they move the caret, never the keyboard to the clear
                // button.
                .event_filter(egui::EventFilter {
                    escape: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..egui::EventFilter::default()
                });
            if ime.composing() {
                // The preedit is drawn by the layouter, over the place the placeholder would be.
                edit = edit.layouter(&mut composing_layout).hint_text("");
            }
            edit.show(ui).response.response
        };
        ime.finish(&ctx, id, self.text);

        // Tabbing into a field selects its text, so typing replaces it (a click keeps the caret where it landed).
        if response.gained_focus()
            && crate::ds::focus::visible(&ctx)
            && let Some(mut edit) = TextEdit::load_state(&ctx, id)
        {
            let end = self.text.chars().count();
            edit.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(end),
            )));
            TextEdit::store_state(&ctx, id, edit);
        }

        // Esc clears, then leaves.
        if response.has_focus() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.text.is_empty() {
                response.surrender_focus();
            } else {
                self.text.clear();
                response.mark_changed();
            }
        }

        if self.clearable && !self.text.is_empty() && enabled && self.clear_button(ui, &ds, rect, outer.id) {
            self.text.clear();
            response.mark_changed();
            response.request_focus();
        }

        self.show_error(ui, &ds);
        self.describe(&ctx, &ds, response.id);
        response
    }
}

impl TextField<'_> {
    /// The error message under the field, led by the danger icon (P9), as on chips, banners and toasts.
    fn show_error(&self, ui: &mut Ui, ds: &Ds) {
        let Some(message) = self.error else { return };
        let t = &ds.theme;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S4;
            let icon = icons::status(Status::Danger);
            ui.label(egui::RichText::new(icon).font(ds.text.icon(12.0, true)).color(t.danger));
            ui.label(egui::RichText::new(message).font(ds.text.small.clone()).color(t.danger));
        });
    }

    /// The field's name, placeholder and description for assistive technology. An error is read as "Error: the
    /// message", the word a screen reader says where the colour is the cue, and marks the field invalid.
    fn describe(&self, ctx: &egui::Context, ds: &Ds, id: Id) {
        let (label, hint, invalid) = (self.label, self.hint, self.error.is_some());
        let status = ds.status_word(Status::Danger);
        let description = match (self.error, self.disabled) {
            (Some(message), Some(reason)) => Some(
                ds.text_with(Msg::FieldErrorDisabled, &[("status", &status), ("message", message), ("reason", reason)]),
            ),
            (Some(message), None) => Some(ds.text_with(Msg::FieldError, &[("status", &status), ("message", message)])),
            (None, reason) => reason.map(str::to_owned),
        };
        ctx.accesskit_node_builder(id, |node| {
            node.set_label(label);
            if !hint.is_empty() {
                node.set_placeholder(hint);
            }
            if let Some(description) = description {
                node.set_description(description);
            }
            if invalid {
                node.set_invalid(egui::accesskit::Invalid::True);
            }
        });
    }

    /// The fill, border (2 pt accent when focused, `danger` on error) and the search icon.
    fn paint_frame(&self, ui: &Ui, ds: &Ds, rect: egui::Rect, focused: bool, hover: f32) {
        let t = &ds.theme;
        // Only the field's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
        let enabled = self.disabled.is_none();
        let border = if self.error.is_some() {
            t.danger
        } else if focused {
            t.accent
        } else {
            paint::lerp(t.border_control, t.text3, hover * 0.6)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, radius::CONTROL, paint::dim(enabled, t.bg2));
        painter.rect_stroke(
            rect,
            radius::CONTROL,
            Stroke::new(if focused { 2.0 } else { t.control_stroke }, paint::dim(enabled, border)),
            StrokeKind::Inside,
        );
        if focused && self.error.is_some() {
            // The border is already `danger` here, so thickening it would be the only sign of focus: add the ring.
            paint::draw_ring(painter, t, rect, radius::CONTROL, RingPlacement::Outside);
        }
        if self.search {
            let centre = pos2(rect.left() + 15.0, rect.center().y);
            paint::icon(painter, ds, centre, icons::MAGNIFYING_GLASS, 14.0, paint::dim(enabled, t.text3), false);
        }
    }

    /// The clear button at the right end; true when it was pressed.
    fn clear_button(&self, ui: &mut Ui, ds: &Ds, rect: egui::Rect, id: egui::Id) -> bool {
        let t = &ds.theme;
        let button = egui::Rect::from_center_size(
            pos2(rect.right() - space::S4 - size::MIN_TARGET / 2.0, rect.center().y),
            vec2(size::MIN_TARGET, size::MIN_TARGET),
        );
        let clear = ui.interact(button, id.with("clear"), Sense::click());
        let hover = paint::fade(ui.ctx(), ds, id.with("clear"), paint::hot(&clear));
        if hover > 0.0 {
            ui.painter().rect_filled(button, radius::CONTROL, t.bg4.gamma_multiply(hover));
        }
        paint::icon(ui.painter(), ds, button.center(), icons::X, 12.0, paint::lerp(t.text3, t.text, hover), false);
        paint::focus_ring(ui.painter(), t, &clear, button, radius::CONTROL, RingPlacement::Outside);
        a11y::describe(
            &clear,
            || {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    ds.text_with(Msg::FieldClear, &[("label", self.label)]),
                )
            },
            |_| {},
        );
        clear.clicked()
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::{Invalid, Role};
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    use super::TextField;
    use crate::ds::Settings;
    use crate::ds::testing::{harness_state, press};

    #[test]
    fn an_error_is_read_with_its_status_word_and_marks_the_field_invalid() {
        let h = harness_state(Settings::default(), egui::vec2(320.0, 90.0), String::from("a b"), |ui, text| {
            ui.add(TextField::new("Name", text).error("Use letters and numbers"));
        });
        let field = h.get_by_role_and_label(Role::TextInput, "Name");
        let node = field.accesskit_node();
        assert_eq!(node.description().as_deref(), Some("Error: Use letters and numbers"));
        assert_eq!(node.data().invalid(), Some(Invalid::True));
    }

    #[test]
    fn the_arrow_keys_move_the_caret_and_never_the_keyboard_out_of_the_field() {
        let mut h = harness_state(Settings::default(), egui::vec2(320.0, 60.0), String::from("ab"), |ui, text| {
            ui.add(TextField::new("Name", text));
        });
        h.get_by_role_and_label(Role::TextInput, "Name").focus();
        h.run_steps(2);
        for key in
            [egui::Key::End, egui::Key::ArrowRight, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowUp]
        {
            press(&mut h, egui::Modifiers::NONE, key);
            let field = h.get_by_role_and_label(Role::TextInput, "Name");
            assert!(field.is_focused(), "{key:?} took the keyboard out of the field");
        }
        h.event(egui::Event::Text("x".to_owned()));
        h.run_steps(2);
        assert_eq!(h.state(), "xab", "Up went to the start, and typing lands there");
    }

    #[test]
    fn a_field_without_an_error_is_not_invalid() {
        let h = harness_state(Settings::default(), egui::vec2(320.0, 60.0), String::new(), |ui, text| {
            ui.add(TextField::new("Name", text));
        });
        let field = h.get_by_role_and_label(Role::TextInput, "Name");
        assert_eq!(field.accesskit_node().data().invalid(), None);
    }
}
