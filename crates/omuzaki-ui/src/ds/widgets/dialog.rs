//! `Dialog` (`design-system.md` 7.0): centred, 8 pt radius, a title, a body and a **verb-named primary** on the right.
//!
//! Dialogs are for irreversible actions and for data the app cannot infer (`shell.md` section 15), never for "are you
//! sure?" about something undoable. So:
//!
//! - **Focus is trapped.** egui's modal layer keeps the pointer and Tab inside until the dialog is gone, and focus
//!   returns to what had it when the dialog opened. (The area is egui's own modal recipe, drawn here because
//!   `egui::Modal` keeps its layer for a frame after the dialog ends and would refuse the returning focus.)
//! - **Esc cancels**, unless the dialog says it is [`busy`](Dialog::busy): an irreversible action in progress cannot be
//!   walked away from.
//! - **The primary names its action** ("Delete 3 parts"), never "OK". A [`dangerous`](Dialog::dangerous) one is
//!   outlined in `danger`, opens on **Cancel**, and takes the two-step confirm of the approval card
//!   ([`confirm`](crate::ds::confirm)): the first press turns the button into "Confirm: delete 3 parts" and only a
//!   second, deliberate press after a short dwell goes through. A double-click or a held Enter cannot do both steps.
//!
//! The caller owns whether the dialog is open: it calls [`show`](Dialog::show) every frame it should be on screen and
//! stops when an action comes back. The body is any UI; a `Dropdown` inside it closes on Esc before the dialog does.

use egui::{
    Align2, Area, Color32, Context, Id, Key, Margin, Modifiers, Order, RichText, Sense, Ui, UiBuilder, UiKind, Vec2,
    accesskit, vec2,
};

use super::button::{Button, ButtonKind};
use crate::ds::a11y;
use crate::ds::confirm::Arm;
use crate::ds::input;
use crate::ds::paint;
use crate::ds::strings::Msg;
use crate::ds::theme::ds;
use crate::ds::tokens::space;

/// What the person chose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogAction {
    /// The primary action. For a dangerous dialog, only after both steps.
    Primary,
    /// Cancel, Esc.
    Cancel,
}

/// Which control has the focus when the dialog opens.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FirstFocus {
    /// The primary button; a dangerous dialog opens on Cancel instead.
    #[default]
    Default,
    /// Cancel.
    Cancel,
    /// Nothing: the body has an input that takes the focus (naming a version), and asks for it itself.
    Body,
}

/// A dialog.
#[must_use = "a dialog does nothing until it is shown"]
pub struct Dialog<'a> {
    id: Id,
    title: &'a str,
    primary: &'a str,
    cancel: Option<&'a str>,
    confirm: Option<&'a str>,
    primary_disabled: Option<&'a str>,
    busy: bool,
    first_focus: FirstFocus,
    width: f32,
    opener: Option<Id>,
}

/// Width of the dialog's content: wide enough for a sentence, narrow enough to read.
const WIDTH: f32 = 420.0;
/// Space inside the frame.
const MARGIN: i8 = 20;
/// Opacity of the scrim over the work: black at about 43% reads in every theme, including high contrast.
const SCRIM_ALPHA: u8 = 110;

/// What a dialog remembers between frames.
#[derive(Clone, Copy, Default)]
struct State {
    /// The last frame it was shown; a gap means it was closed and opened again.
    shown: u64,
    /// The widget that had focus when it opened, which gets it back.
    restore: Option<Id>,
    /// The first-focus request was made.
    focused: bool,
    /// A dangerous primary was pressed once and waits for its deliberate second press.
    arm: Option<Arm>,
}

impl<'a> Dialog<'a> {
    /// A dialog keyed by `id_salt`, titled `title`, whose primary action is `primary` (a verb phrase).
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, title: &'a str, primary: &'a str) -> Self {
        a11y::require_label(title);
        a11y::require_label(primary);
        Self {
            id: Id::new(id_salt),
            title,
            primary,
            cancel: None,
            confirm: None,
            primary_disabled: None,
            busy: false,
            first_focus: FirstFocus::Default,
            width: WIDTH,
            opener: None,
        }
    }

    /// The label of the cancel button; "Cancel" by default.
    pub const fn cancel_label(mut self, label: &'a str) -> Self {
        self.cancel = Some(label);
        self
    }

    /// Makes the primary action dangerous: outlined in `danger`, opening on Cancel, and needing two steps. `confirm`
    /// is the armed button's label ("Confirm: delete 3 parts").
    pub const fn dangerous(mut self, confirm: &'a str) -> Self {
        self.confirm = Some(confirm);
        self
    }

    /// Disables the primary action, saying why ("Enter a name first").
    pub const fn primary_disabled(mut self, reason: &'a str) -> Self {
        self.primary_disabled = Some(reason);
        self
    }

    /// Disables the primary action with `reason` when `condition` holds.
    pub const fn primary_disabled_if(self, condition: bool, reason: &'a str) -> Self {
        if condition { self.primary_disabled(reason) } else { self }
    }

    /// An irreversible action is in progress: Esc and Cancel are ignored.
    pub const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// The control that opened the dialog (`response.id`), which gets the focus back when it closes. A click does not
    /// focus a button in egui, so without this a dialog opened with the mouse has nobody to hand the focus back to;
    /// one opened from the keyboard remembers the focused control by itself.
    pub const fn opened_from(mut self, opener: Id) -> Self {
        self.opener = Some(opener);
        self
    }

    /// Which control has the focus when the dialog opens.
    pub const fn first_focus(mut self, first: FirstFocus) -> Self {
        self.first_focus = first;
        self
    }

    /// Shows the dialog over everything, with `body` between the title and the buttons. Returns the action chosen
    /// this frame, after which the caller stops showing it.
    pub fn show(self, ctx: &Context, body: impl FnOnce(&mut Ui)) -> Option<DialogAction> {
        let ds = ds(ctx);
        let mut state = self.opened(ctx);
        let node = self.id.with("node");
        let role = if self.confirm.is_some() { accesskit::Role::AlertDialog } else { accesskit::Role::Dialog };
        // The dialog's node first, so its controls attach to it; completed with its bounds below.
        ctx.accesskit_node_builder(node, |n| n.set_role(role));
        let mut first = None;
        let mut action = None;
        let area = Area::new(self.id.with("modal"))
            .kind(UiKind::Modal)
            .sense(Sense::hover())
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(Order::Foreground)
            .interactable(true);
        let shown = area.show(ctx, |ui| {
            // A scrim that swallows clicks, so nothing behind the dialog can be reached with the pointer.
            let screen = ui.ctx().content_rect();
            let mut scrim = ui.new_child(UiBuilder::new().sense(Sense::CLICK | Sense::DRAG).max_rect(screen));
            scrim.set_min_size(screen.size());
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(SCRIM_ALPHA));
            ui.scope_builder(UiBuilder::new().sense(Sense::CLICK | Sense::DRAG), |ui| {
                paint::overlay_frame(&ds).inner_margin(Margin::same(MARGIN)).show(ui, |ui| {
                    ui.scope_builder(UiBuilder::new().accessibility_parent(node), |ui| {
                        (action, first) = self.contents(ui, &mut state, body);
                    });
                    let whole = ui.interact(ui.min_rect(), node, Sense::hover());
                    let title = self.title;
                    a11y::describe(
                        &whole,
                        || egui::WidgetInfo::labeled(egui::WidgetType::Other, true, title),
                        |n| {
                            n.set_role(role);
                            n.set_modal();
                        },
                    );
                });
            });
        });
        let layer = shown.response.layer_id;
        let topmost = ctx.memory(|m| m.top_modal_layer().is_none_or(|top| top == layer));
        // Esc after the body, so a dropdown or popover inside it has had its turn with the key.
        if action.is_none() && !self.busy && topmost && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            action = Some(DialogAction::Cancel);
        }
        // egui keeps a modal layer, and the focus it traps, for one frame after the frame that set it. A dialog that is
        // finished must not set it again, or the focus it hands back would be refused for a frame.
        if action.is_none() {
            ctx.memory_mut(|m| m.set_modal_layer(layer));
        }
        self.settle(ctx, state, action, first);
        action
    }

    /// The same dialog drawn in place, as a card in the page: for the gallery and for documentation. It takes no
    /// focus and no Esc, and covers nothing.
    pub fn show_inline(self, ui: &mut Ui, body: impl FnOnce(&mut Ui)) -> Option<DialogAction> {
        let ds = ds(ui.ctx());
        let mut state = self.opened(ui.ctx());
        let (action, _) = ui
            .vertical(|ui| {
                paint::overlay_frame(&ds)
                    .inner_margin(Margin::same(MARGIN))
                    .show(ui, |ui| self.contents(ui, &mut state, body))
                    .inner
            })
            .inner;
        self.settle(ui.ctx(), state, action, None);
        action
    }

    /// The dialog's state, fresh when it was not shown on the previous frame: it remembers who had the focus.
    fn opened(&self, ctx: &Context) -> State {
        let mut state: State = ctx.data(|d| d.get_temp(self.id)).unwrap_or_default();
        let frame = ctx.cumulative_frame_nr();
        if state.shown == 0 || state.shown + 1 < frame {
            state = State { restore: ctx.memory(egui::Memory::focused).or(self.opener), ..State::default() };
        }
        state.shown = frame;
        state
    }

    /// Title, body and buttons. Returns the action and the ids of (Cancel, primary).
    fn contents(
        &self,
        ui: &mut Ui,
        state: &mut State,
        body: impl FnOnce(&mut Ui),
    ) -> (Option<DialogAction>, Option<(Id, Id)>) {
        let ds = ds(ui.ctx());
        ui.set_width(self.width.min(ui.available_width()));
        ui.spacing_mut().item_spacing = vec2(space::S8, space::S16);
        ui.label(RichText::new(self.title).font(ds.text.title.clone()).color(ds.theme.text));
        body(ui);
        let mut ids = None;
        let action = self.buttons(ui, state, &mut ids);
        (action, ids)
    }

    /// Asks for the first focus once, and keeps or clears the state: a finished dialog hands the focus back.
    fn settle(&self, ctx: &Context, mut state: State, action: Option<DialogAction>, first: Option<(Id, Id)>) {
        if !state.focused {
            let dangerous = self.confirm.is_some();
            let wanted = match (self.first_focus, dangerous) {
                (FirstFocus::Body, _) => None,
                (FirstFocus::Cancel, _) | (FirstFocus::Default, true) => first.map(|(cancel, _)| cancel),
                (FirstFocus::Default, false) => first.map(|(_, primary)| primary),
            };
            if let Some(id) = wanted {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            state.focused = first.is_some();
        }
        if action.is_some() {
            Self::finish(ctx, self.id, &state);
        } else {
            ctx.data_mut(|d| d.insert_temp(self.id, state));
        }
    }

    /// Closes a dialog from outside (the project was closed under it): hands the focus back and forgets its state. A
    /// dialog that returned an action has already done this.
    pub fn dismiss(ctx: &Context, id_salt: impl std::hash::Hash + std::fmt::Debug) {
        let id = Id::new(id_salt);
        if let Some(state) = ctx.data(|d| d.get_temp::<State>(id)) {
            Self::finish(ctx, id, &state);
        }
    }

    fn finish(ctx: &Context, id: Id, state: &State) {
        if let Some(back) = state.restore {
            ctx.memory_mut(|m| m.request_focus(back));
        } else {
            warn_no_opener(id);
        }
        ctx.data_mut(|d| d.remove::<State>(id));
    }

    /// The Cancel and primary buttons, right-aligned but built left to right so Tab follows the eye. Reports their ids
    /// (cancel, primary) for the first focus.
    fn buttons(&self, ui: &mut Ui, state: &mut State, ids: &mut Option<(Id, Id)>) -> Option<DialogAction> {
        let ds = ds(ui.ctx());
        let ctx = ui.ctx().clone();
        let mut action = None;
        // The frame the dialog opens in still holds the Enter that opened it, and the first focus may be on a button.
        let opening = !state.focused;
        let cancel_text = ds.text(Msg::DialogCancel);
        let cancel_label = self.cancel.unwrap_or(&cancel_text);
        let armed = state.arm.is_some();
        let dangerous = self.confirm.is_some();
        let label = match (self.confirm, armed) {
            (Some(confirm), true) => confirm,
            _ => self.primary,
        };
        let kind = if dangerous { ButtonKind::Danger } else { ButtonKind::Primary };
        // Both labels of a dangerous primary take the room of the longer, so the button does not move under the pointer.
        let wide = self.confirm.map_or(0.0, |confirm| {
            [self.primary, confirm].map(|l| Button::new(l).kind(kind).width(&ctx)).into_iter().fold(0.0, f32::max)
        });
        let mut primary = Button::new(label).kind(kind).min_width(wide);
        primary = primary.disabled_if(self.primary_disabled.is_some(), self.primary_disabled.unwrap_or_default());
        let cancel = Button::new(cancel_label).ghost();
        // The spacer and each button are separated by the item spacing: two gaps in all.
        let used = cancel.width(&ctx) + primary.width(&ctx) + 2.0 * ui.spacing().item_spacing.x;

        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - used).max(0.0));
            let cancel = ui.add(cancel);
            let primary = ui.add(primary);
            *ids = Some((cancel.id, primary.id));
            if opening {
                return;
            }
            if cancel.clicked() && !self.busy {
                action = Some(DialogAction::Cancel);
            }
            match (dangerous, state.arm.as_mut()) {
                (false, _) => {
                    // A held Enter repeats the click; only the press itself acts.
                    if input::is_press(&primary) {
                        action = Some(DialogAction::Primary);
                    }
                }
                (true, None) => {
                    if input::is_press(&primary) {
                        state.arm = Some(Arm::start(&ctx));
                    }
                }
                (true, Some(arm)) => {
                    if arm.confirmed(&primary) {
                        action = Some(DialogAction::Primary);
                    }
                }
            }
        });
        action
    }
}

/// Says once per dialog that it closed with nowhere to return the focus: it was opened with the mouse and its caller did
/// not pass [`Dialog::opened_from`]. It is a `tracing` event, so the app chooses where it goes; it never panics.
fn warn_no_opener(id: Id) {
    use std::sync::{Mutex, PoisonError};
    static WARNED: Mutex<Vec<Id>> = Mutex::new(Vec::new());
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if !warned.contains(&id) {
        warned.push(id);
        tracing::warn!(?id, fix = "pass_Dialog::opened_from", "dialog.no_opener");
    }
}
