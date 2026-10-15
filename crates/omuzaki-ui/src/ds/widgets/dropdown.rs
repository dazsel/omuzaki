//! `Dropdown` (`design-system.md` 7.0): a button with a chevron that opens a list under it.
//!
//! For one of many choices that does not fit a [`Segmented`](super::Segmented) control (more than five, or long
//! labels). The list is a popup, not a window, drawn by [`Listbox`]. It is a *select-only combobox*: the button keeps
//! the keyboard focus the whole time and the list only shows the active row, so closing it, by Esc or by choosing,
//! needs no focus to be returned.
//!
//! Keyboard: Enter, Space and Down or Up open it; while open, Up, Down, Page Up, Page Down, Home and End move the
//! active row, Enter or Space chooses it, Esc closes without choosing, and Tab chooses it and moves on. Typing
//! jumps to the next row that starts with what was typed (type-ahead, forgotten after a second); typing on a closed
//! dropdown opens it there. Screen readers get `Increment` and `Decrement` (step to the next or previous choice),
//! `SetValue` (the text of a choice), and `Expand` and `Collapse`.

use egui::{
    Area, Context, Event, EventFilter, Id, Key, Margin, Modifiers, Order, PointerButton, Response, Sense, Stroke,
    StrokeKind, Ui, Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::button::{ButtonKind, face};
use super::ids;
use super::listbox::{self, ListItem, Listbox, Row, link_combobox};
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::clock;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::input;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, space};
use crate::register_contrast;

/// What a dropdown's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.dropdown";

/// One option: the value it sets, and its row.
#[derive(Clone, Copy, Debug)]
#[must_use = "a choice does nothing until it is passed to a dropdown"]
pub struct Choice<'a, T> {
    /// What the dropdown's value becomes when this row is chosen.
    pub value: T,
    /// What the row says.
    pub item: ListItem<'a>,
}

impl<'a, T> Choice<'a, T> {
    /// A choice that sets `value` and says `label`.
    pub fn new(value: T, label: &'a str) -> Self {
        Self { value, item: ListItem::new(label) }
    }

    /// Adds quiet text at the right end of the row.
    pub const fn detail(mut self, detail: &'a str) -> Self {
        self.item = self.item.detail(detail);
        self
    }

    /// Adds an icon glyph before the label.
    pub const fn icon(mut self, glyph: &'a str) -> Self {
        self.item = self.item.icon(glyph);
        self
    }

    /// Makes the choice unavailable, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.item = self.item.disabled(reason);
        self
    }

    /// Draws the label in mono.
    pub const fn mono(mut self) -> Self {
        self.item = self.item.mono();
        self
    }
}

impl<T> Row for Choice<'_, T> {
    fn item(&self) -> ListItem<'_> {
        self.item
    }
}

/// A dropdown over `choices`, bound to `value`. Add it to a `Ui` and read `changed()` on the response.
#[must_use = "a dropdown does nothing until it is added to a Ui"]
pub struct Dropdown<'a, T> {
    label: &'a str,
    value: &'a mut T,
    choices: &'a [Choice<'a, T>],
    placeholder: Option<&'a str>,
    salt: Option<Id>,
    stretch: bool,
    disabled: Option<&'a str>,
}

impl<'a, T: Copy + PartialEq> Dropdown<'a, T> {
    /// A dropdown named `label` (its accessible name) choosing among `choices`.
    pub fn new(label: &'a str, value: &'a mut T, choices: &'a [Choice<'a, T>]) -> Self {
        a11y::require_label(label);
        Self { label, value, choices, placeholder: None, salt: None, stretch: false, disabled: None }
    }

    /// What to show while `value` matches no choice (a "Choose…" is shown otherwise).
    pub const fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = Some(text);
        self
    }

    /// Keys the open state by `salt` instead of by position, so reordering rows in a form keeps it with its dropdown.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Fill the available width instead of sizing to the widest choice.
    pub const fn stretch(mut self) -> Self {
        self.stretch = true;
        self
    }

    /// Disables the dropdown, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }
}

/// Longest the typed prefix grows: more than any label is worth typing.
const AHEAD_CAP: usize = 24;
/// Seconds of silence after which type-ahead starts over.
const AHEAD_RESET: f64 = 1.0;
/// Narrowest dropdown, and the narrowest popup.
const MIN_WIDTH: f32 = 96.0;
/// Widest popup: a longer label ends in an ellipsis with its tooltip.
const MAX_POPUP_WIDTH: f32 = 420.0;
/// Inner margin of the popup around its rows.
const POPUP_MARGIN: i8 = 4;
/// Choices measured to size the button; a longer list fills the width it is given instead.
const MEASURED_CHOICES: usize = 32;

/// What has been typed to jump to a row, kept on the stack so typing never allocates.
#[derive(Clone, Copy, Default, PartialEq)]
struct TypeAhead {
    buf: [u8; AHEAD_CAP],
    len: usize,
    at: f64,
}

impl TypeAhead {
    fn alive(&self, now: f64) -> bool {
        self.len > 0 && now - self.at <= AHEAD_RESET
    }

    /// Adds typed characters, starting over if the last was a while ago.
    fn push(&mut self, now: f64, typed: &str) {
        if !self.alive(now) {
            self.len = 0;
        }
        self.at = now;
        for c in typed.chars().filter(|c| !c.is_control()) {
            let mut utf8 = [0u8; 4];
            let bytes = c.encode_utf8(&mut utf8).as_bytes();
            let Some(slot) = self.buf.get_mut(self.len..self.len + bytes.len()) else { break };
            slot.copy_from_slice(bytes);
            self.len += bytes.len();
        }
    }

    fn prefix(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

/// Whether `label` starts with `prefix`, ignoring case.
fn starts_with_fold(label: &str, prefix: &str) -> bool {
    let mut label = label.chars().flat_map(char::to_lowercase);
    prefix.chars().flat_map(char::to_lowercase).all(|p| label.next() == Some(p))
}

/// The row that typing `prefix` leads to. One typed letter cycles through the rows with that initial (pressing "d"
/// twice goes to the second "d"); more letters narrow down from the active row, which stays if it still matches.
fn type_ahead_target<R: Row>(rows: &[R], active: Option<usize>, prefix: &str) -> Option<usize> {
    if rows.is_empty() || prefix.is_empty() {
        return None;
    }
    let cycling = prefix.chars().nth(1).is_none();
    let start = match (active, cycling) {
        (Some(a), true) => a + 1,
        (Some(a), false) => a,
        (None, _) => 0,
    };
    (0..rows.len()).map(|k| (start + k) % rows.len()).find(|&i| {
        let item = rows[i].item();
        item.disabled.is_none() && starts_with_fold(item.label, prefix)
    })
}

/// The dropdown's state between frames. `Copy`, so reading it never allocates.
#[derive(Clone, Copy, Default, PartialEq)]
struct State {
    open: bool,
    active: Option<usize>,
    ahead: TypeAhead,
    /// The active row was moved by a key: scroll it into view.
    scroll: bool,
}

/// Closes the list and forgets where it was: the next time it opens at the chosen row.
fn close(state: &mut State) {
    *state = State::default();
}

/// What the keyboard and assistive technology asked of the dropdown this frame.
#[derive(Default)]
struct Asked {
    open: bool,
    close: bool,
    choose: Option<usize>,
}

impl<T: Copy + PartialEq> Widget for Dropdown<'_, T> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let ctx = ui.ctx().clone();
        let enabled = ui.is_enabled() && self.disabled.is_none();
        // Only the dropdown's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
        let looks_enabled = self.disabled.is_none();

        let selected = self.choices.iter().position(|c| c.value == *self.value);
        let choose_text = ds.text(Msg::DropdownChoose);
        let shown: &str = match (selected, self.placeholder) {
            (Some(i), _) => self.choices[i].item.label,
            (None, Some(text)) => text,
            (None, None) => &choose_text,
        };
        let width = self.width(&ctx, &ds, shown, ui);
        let (rect, _) = ui.allocate_exact_size(vec2(width, m.control_row), Sense::hover());
        let id = self.salt.map_or_else(|| ui.next_auto_id().with("dropdown"), |salt| ids::salted(ui, KIND, salt));
        let sense = if enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let mut response = ui.interact(rect, id, sense);
        let rect = paint::snap(&ctx, rect);

        let before: State = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        let mut state = before;
        // The list of choices can shrink while it is open: a remembered row that is gone is forgotten.
        state.active = state.active.filter(|&a| a < self.choices.len());
        let mut choice = None;
        if enabled {
            let asked = self.input(&ctx, &response, &mut state, selected, &mut choice);
            if asked.open {
                state.open = true;
                state.active = state.active.or(selected).or_else(|| listbox::first(self.choices));
                state.scroll = true;
            }
            if asked.close {
                close(&mut state);
            }
            choice = choice.or(asked.choose);
        }

        if ui.is_rect_visible(rect) {
            let hover = paint::fade(&ctx, &ds, id.with("hover"), enabled && (paint::hot(&response) || state.open));
            let pressed = enabled && (response.is_pointer_button_down_on() || state.open);
            let cut = Self::paint_button(ui, &ds, rect, shown, selected.is_none(), (hover, pressed), looks_enabled);
            paint::focus_ring(ui.painter(), t, &response, rect, radius::CONTROL, RingPlacement::Outside);
            if cut || self.disabled.is_some() {
                let tip = Tip::new(shown);
                response = response.tip(self.disabled.map_or(tip, |reason| tip.detail(reason)));
            }
        }

        if state.open {
            self.popup(&ctx, &ds, &response, &mut state, selected, &mut choice);
        }
        if let Some(i) = choice.filter(|&i| i < self.choices.len())
            && enabled
        {
            if self.choices[i].value != *self.value {
                *self.value = self.choices[i].value;
                response.mark_changed();
            }
            close(&mut state);
            // Choosing with the pointer moved the focus to the row; the person was at the button.
            response.request_focus();
        }
        if enabled && response.has_focus() {
            // The arrows are the dropdown's while it has focus, and so is Esc while its list is open: egui would
            // otherwise take the focus away at the start of the next frame, before the list could close.
            let filter = EventFilter { vertical_arrows: true, escape: state.open, ..EventFilter::default() };
            ctx.memory_mut(|mem| mem.set_focus_lock_filter(id, filter));
        }
        if state != before {
            ctx.data_mut(|d| d.insert_temp(id, state));
            // A key that moved the active row is acted on next frame, when the popup exists.
            ctx.request_repaint();
        }
        self.accessibility(&response, id, &state, selected);
        response
    }
}

impl<T: Copy + PartialEq> Dropdown<'_, T> {
    /// The width of the button: the widest choice and the chevron, or all the room given.
    fn width(&self, ctx: &Context, ds: &Ds, shown: &str, ui: &Ui) -> f32 {
        let room = paint::fit_width(ui);
        if self.stretch || self.choices.len() > MEASURED_CHOICES {
            return room;
        }
        let widest = self
            .choices
            .iter()
            .map(|c| c.item.label)
            .chain(std::iter::once(shown))
            .map(|label| paint::text_width(ctx, label, &ds.text.body))
            .fold(0.0, f32::max);
        (widest + 2.0 * space::S12 + CHEVRON_ROOM).clamp(MIN_WIDTH, room.max(MIN_WIDTH))
    }

    /// Reads the keyboard and assistive technology. Moves `state`; reports what else was asked.
    fn input(
        &self,
        ctx: &Context,
        response: &Response,
        state: &mut State,
        selected: Option<usize>,
        choice: &mut Option<usize>,
    ) -> Asked {
        let mut asked = Asked::default();
        let id = response.id;
        let was_open = state.open;
        let focused = response.has_focus();
        let now = clock::now(ctx);

        // Screen readers address the node directly, focused or not.
        let (steps, set, expand) = ctx.input(|i| {
            let count = |action| i.num_accesskit_action_requests(id, action).cast_signed();
            let set = i.accesskit_action_requests(id, accesskit::Action::SetValue).last().and_then(|r| match &r.data {
                Some(accesskit::ActionData::Value(text)) => {
                    self.choices.iter().position(|c| c.item.disabled.is_none() && c.item.label == &**text)
                }
                _ => None,
            });
            let expand = count(accesskit::Action::Expand) - count(accesskit::Action::Collapse);
            (count(accesskit::Action::Increment) - count(accesskit::Action::Decrement), set, expand)
        });
        if let Some(i) = set {
            *choice = Some(i);
        } else if steps != 0 {
            *choice = listbox::step(self.choices, selected, steps);
        }
        match expand.signum() {
            1 => asked.open = true,
            -1 => asked.close = true,
            _ => {}
        }

        // A click opens it, or closes it again. A held Enter or Space repeats the click and must not toggle it back.
        if response.clicked() && !input::only_repeats(response) {
            let at_click = ctx.input(|i| i.num_accesskit_action_requests(id, accesskit::Action::Click) > 0);
            let by_key = !response.clicked_by(PointerButton::Primary) && !at_click;
            match (was_open, by_key && focused) {
                (false, _) => asked.open = true,
                (true, false) => asked.close = true,
                // Enter or Space on an open list chooses the active row (below).
                (true, true) => {}
            }
        }
        // Tab moves the focus on before the dropdown is drawn, so it is already gone by the time it is asked: what is
        // active is chosen, as when Enter is pressed.
        let tabbed =
            ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: Key::Tab, pressed: true, .. })));
        if was_open && !focused && response.lost_focus() && tabbed {
            *choice = choice.or(state.active);
            asked.close = true;
        }
        if !focused {
            return asked;
        }
        let typed = ctx.input(|i| {
            let plain = !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.command;
            let mut text = String::new();
            for event in &i.events {
                if let Event::Text(t) = event
                    && plain
                {
                    text.push_str(t);
                }
            }
            text
        });
        if !typed.is_empty() {
            let space_in_word = typed.trim().is_empty() && !state.ahead.alive(now);
            if !space_in_word {
                state.ahead.push(now, &typed);
                let target = type_ahead_target(self.choices, state.active.or(selected), state.ahead.prefix());
                if target.is_some() {
                    asked.open = true;
                    state.active = target;
                    state.scroll = true;
                }
            }
        }

        if !was_open {
            let key = |k| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
            if key(Key::ArrowDown) || key(Key::ArrowUp) {
                asked.open = true;
            }
            return asked;
        }

        // Open: the list answers the keys, the button keeps the focus.
        if let Some(movement) = listbox::pressed_move(ctx, true) {
            state.active = listbox::apply(self.choices, state.active.or(selected), movement);
            state.scroll = true;
        }
        let (enter, space, escape) = ctx.input_mut(|i| {
            let pressed = |i: &egui::InputState, k| {
                i.events.iter().any(|e| matches!(e, Event::Key { key, pressed: true, repeat: false, .. } if *key == k))
            };
            (pressed(i, Key::Enter), pressed(i, Key::Space), i.consume_key(Modifiers::NONE, Key::Escape))
        });
        let typing = state.ahead.alive(now) && !typed.is_empty();
        if escape {
            asked.close = true;
        } else if enter || (space && !typing) {
            *choice = choice.or(state.active);
            if choice.is_none() {
                asked.close = true;
            }
        }
        asked
    }

    /// Draws the button; true when the label had to be cut.
    fn paint_button(
        ui: &Ui,
        ds: &Ds,
        rect: egui::Rect,
        shown: &str,
        placeholder: bool,
        (hover, pressed): (f32, bool),
        enabled: bool,
    ) -> bool {
        let t = &ds.theme;
        let face = face(ButtonKind::Secondary, t, enabled);
        let painter = ui.painter();
        let fill = if pressed { face.fill[2] } else { paint::lerp(face.fill[0], face.fill[1], hover) };
        painter.rect_filled(rect, radius::CONTROL, fill);
        if let Some([rest, hovered]) = face.stroke {
            painter.rect_stroke(
                rect,
                radius::CONTROL,
                Stroke::new(t.control_stroke, paint::lerp(rest, hovered, hover)),
                StrokeKind::Inside,
            );
        }
        let ink = if placeholder { quiet_ink(t, enabled, pressed) } else { face.text };
        let area = egui::Rect::from_min_max(
            pos2(rect.left() + space::S12, rect.top()),
            pos2(rect.right() - CHEVRON_ROOM - space::S4, rect.bottom()),
        );
        let font = if placeholder { &ds.text.body } else { &ds.text.strong };
        paint::text_in(painter, area, egui::Align2::LEFT_CENTER, shown, font, ink);
        let caret = if pressed && enabled { icons::CARET_UP } else { icons::CARET_DOWN };
        paint::icon(
            painter,
            ds,
            pos2(rect.right() - space::S12 - 6.0, rect.center().y),
            caret,
            12.0,
            quiet_ink(t, enabled, pressed),
            false,
        );
        paint::text_width(ui.ctx(), shown, font) > area.width()
    }

    /// The popup under (or, with no room below, over) the button.
    fn popup(
        &self,
        ctx: &Context,
        ds: &Ds,
        anchor: &Response,
        state: &mut State,
        selected: Option<usize>,
        choice: &mut Option<usize>,
    ) {
        let list_id = anchor.id.with("list");
        let row_height = ds.metrics.tree_row;
        #[allow(clippy::cast_precision_loss, reason = "a row count")]
        let list_height = row_height * self.choices.len().min(listbox::VISIBLE_ROWS) as f32;
        let frame_height = list_height + 2.0 * f32::from(POPUP_MARGIN);
        let screen = ctx.content_rect();
        let below = screen.bottom() - anchor.rect.bottom();
        let above = anchor.rect.top() - screen.top();
        let flip = below < frame_height + space::S8 && above > below;
        let (pos, pivot) = if flip {
            (pos2(anchor.rect.left(), anchor.rect.top() - space::S4), egui::Align2::LEFT_BOTTOM)
        } else {
            (pos2(anchor.rect.left(), anchor.rect.bottom() + space::S4), egui::Align2::LEFT_TOP)
        };
        let widest = self
            .choices
            .iter()
            .map(|c| {
                paint::text_width(ctx, c.item.label, &ds.text.strong)
                    + c.item.detail.map_or(0.0, |d| paint::text_width(ctx, d, &ds.text.small) + space::S8)
            })
            .fold(0.0, f32::max);
        let width = (widest + 4.0 * space::S12).clamp(anchor.rect.width(), MAX_POPUP_WIDTH.max(anchor.rect.width()));
        let mut hovered = None;
        let mut clicked = None;
        let area = Area::new(anchor.id.with("popup"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .constrain(true)
            .show(ctx, |ui| {
                paint::overlay_frame(ds).inner_margin(Margin::same(POPUP_MARGIN)).show(ui, |ui| {
                    ui.set_width(width - 2.0 * f32::from(POPUP_MARGIN));
                    let out = Listbox::new(list_id, self.label, self.choices)
                        .selected(selected)
                        .active(state.active)
                        .follow_active(state.scroll)
                        .show(ui);
                    hovered = out.hovered;
                    clicked = out.clicked;
                });
            });
        state.scroll = false;
        let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        if let Some(i) = hovered.filter(|_| moved) {
            state.active = Some(i);
        }
        *choice = choice.or(clicked);
        // A press anywhere else closes it without choosing.
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !anchor.rect.contains(p))
        });
        if outside {
            close(state);
        }
    }

    fn accessibility(&self, response: &Response, id: Id, state: &State, selected: Option<usize>) {
        let enabled = response.enabled() && self.disabled.is_none();
        let value = selected.map(|i| self.choices[i].item.label);
        a11y::describe(
            response,
            || WidgetInfo::labeled(WidgetType::ComboBox, enabled, self.label),
            |node| {
                node.set_value(value.unwrap_or_default());
                for action in [
                    accesskit::Action::Increment,
                    accesskit::Action::Decrement,
                    accesskit::Action::SetValue,
                    accesskit::Action::Expand,
                    accesskit::Action::Collapse,
                ] {
                    node.add_action(action);
                }
                if let Some(reason) = self.disabled {
                    node.set_description(reason);
                }
            },
        );
        link_combobox(response, accesskit::Role::ComboBox, id.with("list"), state.open, state.active);
    }
}

/// Room at the right end for the chevron and its padding.
const CHEVRON_ROOM: f32 = 24.0;

/// The chevron and the placeholder: `text.2`, except on the open button's lighter fill, where it is `text` (in high
/// contrast `text.2` there is 6.5:1, short of AAA).
fn quiet_ink(t: &Theme, enabled: bool, open: bool) -> egui::Color32 {
    paint::ink(t, enabled, if open { t.text } else { t.text2 })
}

register_contrast!("dropdown", pairs);

/// The chevron and the placeholder on the button's three fills (its label is a Secondary button's, already tested).
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    let secondary = face(ButtonKind::Secondary, &t, true);
    for (state, fill) in ["rest", "hover", "open"].into_iter().zip(secondary.fill) {
        let ink = quiet_ink(&t, true, state == "open");
        table.add(format!("{state} chevron"), ink, fill, Rule::NonText);
        table.add(format!("{state} placeholder"), ink, fill, Rule::Text);
    }
    let dead = face(ButtonKind::Secondary, &t, false);
    table.add("disabled label", dead.text, dead.fill[0], Rule::Disabled);
}

#[cfg(test)]
mod tests {
    use super::listbox::Move;
    use super::*;

    fn rows<'a>(labels: &[&'a str]) -> Vec<ListItem<'a>> {
        labels.iter().map(|l| ListItem::new(l)).collect()
    }

    #[test]
    fn a_zero_step_stays_where_it_is_instead_of_searching_forever() {
        let items = [ListItem::new("a"), ListItem::new("b").disabled("no")];
        assert_eq!(listbox::step(&items, Some(1), 0), Some(1));
        assert_eq!(listbox::step(&items, None, 0), None);
    }

    #[test]
    fn typing_one_letter_cycles_through_the_rows_with_that_initial() {
        let rows = rows(&["Dark", "Light", "Dusk", "High contrast"]);
        assert_eq!(type_ahead_target(&rows, None, "d"), Some(0));
        assert_eq!(type_ahead_target(&rows, Some(0), "d"), Some(2), "a second d goes to the next d");
        assert_eq!(type_ahead_target(&rows, Some(2), "d"), Some(0), "and wraps");
        assert_eq!(type_ahead_target(&rows, Some(0), "du"), Some(2), "more letters narrow it down");
        assert_eq!(type_ahead_target(&rows, Some(2), "du"), Some(2), "and stay put while the row still matches");
        assert_eq!(type_ahead_target(&rows, None, "zz"), None);
    }

    #[test]
    fn type_ahead_ignores_case_and_skips_rows_that_cannot_be_chosen() {
        let items = [ListItem::new("Alpha").disabled("Not yet"), ListItem::new("alpine")];
        assert_eq!(type_ahead_target(&items, None, "AL"), Some(1));
    }

    #[test]
    fn the_typed_prefix_forgets_after_a_second_and_never_grows_past_its_buffer() {
        let mut ahead = TypeAhead::default();
        ahead.push(10.0, "ha");
        ahead.push(10.5, "i");
        assert_eq!(ahead.prefix(), "hai");
        ahead.push(12.0, "x");
        assert_eq!(ahead.prefix(), "x", "a pause starts over");
        ahead.push(12.1, &"é".repeat(40));
        assert!(ahead.prefix().len() <= AHEAD_CAP && ahead.prefix().starts_with('x'));
    }

    #[test]
    fn stepping_skips_disabled_rows_and_stops_at_the_ends() {
        let items = [
            ListItem::new("a"),
            ListItem::new("b").disabled("no"),
            ListItem::new("c"),
            ListItem::new("d").disabled("no"),
        ];
        assert_eq!(listbox::step(&items, Some(0), 1), Some(2), "skips the disabled b");
        assert_eq!(listbox::step(&items, Some(2), 1), Some(2), "d is disabled and last: stay");
        assert_eq!(listbox::step(&items, Some(2), -1), Some(0));
        assert_eq!(listbox::step(&items, None, 1), Some(0));
        assert_eq!(listbox::step(&items, None, -1), Some(2));
        assert_eq!(listbox::apply(&items, Some(0), Move::Page(1)), Some(2), "a page lands on a row that can be chosen");
        assert_eq!(listbox::apply(&items, Some(2), Move::First), Some(0));
        assert_eq!(listbox::apply(&items, Some(0), Move::Last), Some(2));
    }
}
