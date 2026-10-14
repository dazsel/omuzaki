//! Toast (`design-system.md` section 7.8): a short message that says what happened and who did it.
//!
//! 320 pt wide on `bg2`, 8 pt radius: status icon, message, up to two actions and a close button. They stack at the
//! bottom right above the status bar, at most three, newest at the bottom, duplicates fold into "×3". Success and info
//! vanish after 4 s, warnings after 8 s, errors stay until dismissed; the timer pauses while the pointer is over a
//! toast or a toast has keyboard focus. A toast **never takes focus**, and nothing decision-bearing lives in one
//! (that is an approval card). Everything is also written to the notification log, the accessible path.
//!
//! Time comes from the clock in the egui context ([`clock`](crate::ds::clock)): a toast has a deadline, and a frame
//! compares the clock with it. Adding up frame times would make a toast outlive its four seconds by minutes in a
//! reactive app, which repaints once a second or less.

use egui::{Align2, Area, Context, Id, Margin, Order, Rect, Response, Sense, Ui, accesskit, vec2};

use crate::ds::a11y;
use crate::ds::clock;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, dur, radius, size, space};
use crate::ds::widgets::{Button, ButtonKind};

/// Toasts shown at once; older ones are dropped from the screen (never from the log).
const MAX_VISIBLE: usize = 3;
/// Most actions a toast carries.
const MAX_ACTIONS: usize = 2;
/// Seconds between frames while something fades: about 60 Hz.
const FADE_FRAME: f64 = 0.016;

/// A message to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    /// How it is coloured and how long it stays.
    pub status: Status,
    /// What happened, saying who did it: "Designer changed the face profile".
    pub message: String,
    /// Up to two action labels: "Undo", "Show", "Details".
    pub actions: Vec<String>,
}

impl Toast {
    /// A toast with no actions.
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), actions: Vec::new() }
    }

    /// Adds an action button (at most two are kept).
    #[must_use]
    pub fn action(mut self, label: impl Into<String>) -> Self {
        if self.actions.len() < MAX_ACTIONS {
            self.actions.push(label.into());
        }
        self
    }

    /// How long it stays on screen; `None` means until dismissed.
    pub const fn lifetime(&self) -> Option<f32> {
        match self.status {
            Status::Info | Status::Ok => Some(4.0),
            Status::Warn => Some(8.0),
            Status::Danger => None,
        }
    }
}

/// One line of the notification log.
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    /// The toast as it was pushed.
    pub toast: Toast,
    /// When, on the context's clock ([`clock::now`], seconds).
    pub time: f64,
}

/// What the user did to a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastEvent {
    /// An action button was pressed: the toast's message and the button's label.
    Action {
        /// The toast the button belongs to.
        message: String,
        /// The button's label.
        label: String,
    },
    /// The close button was pressed.
    Dismissed,
}

struct Live {
    toast: Toast,
    count: u32,
    /// When the toast starts to leave, or `None` for one that stays until dismissed. Moves later while paused.
    deadline: Option<f64>,
    born: f64,
    /// When the toast started to leave (so it can fade for 100 ms before it is removed).
    leaving: Option<f64>,
}

/// The toast stack and the notification log. Keep one in the app and call [`Toasts::show`] every frame.
#[derive(Default)]
pub struct Toasts {
    live: Vec<Live>,
    log: Vec<LogEntry>,
    /// The clock reading of the previous frame, so a frame knows how long the app slept.
    last_frame: Option<f64>,
    /// The pointer was over the stack, or a toast had focus, in the previous frame.
    was_paused: bool,
}

impl Toasts {
    /// Pushes a toast: it shows, and it is logged. A duplicate of a visible toast folds into it ("×3").
    pub fn push(&mut self, ctx: &Context, toast: Toast) {
        let now = clock::now(ctx);
        self.log.push(LogEntry { toast: toast.clone(), time: now });
        let deadline = toast.lifetime().map(|seconds| now + f64::from(seconds));
        if let Some(existing) = self.live.iter_mut().find(|l| l.leaving.is_none() && l.toast == toast) {
            existing.count += 1;
            existing.deadline = deadline;
            return;
        }
        self.live.push(Live { deadline, toast, count: 1, born: now, leaving: None });
        let visible = self.live.iter().filter(|l| l.leaving.is_none()).count();
        for l in self.live.iter_mut().filter(|l| l.leaving.is_none()).take(visible.saturating_sub(MAX_VISIBLE)) {
            l.leaving = Some(now);
        }
    }

    /// Everything that was ever shown, oldest first: the accessible path to what a toast said.
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// How many toasts are on screen.
    pub fn visible(&self) -> usize {
        self.live.iter().filter(|l| l.leaving.is_none()).count()
    }

    /// Draws the stack at the bottom right of the window, `inset` away from the corner (above the status bar), and
    /// returns what the user did. Call every frame; it asks for frames only while a timer or a fade is running.
    pub fn show(&mut self, ctx: &Context, inset: egui::Vec2) -> Vec<ToastEvent> {
        let ds = ds(ctx);
        let now = clock::now(ctx);
        let mut events = Vec::new();
        if self.live.is_empty() {
            self.last_frame = None;
            self.was_paused = false;
            return events;
        }
        // Time the stack spent paused is given back to every deadline, however long the app slept in between.
        let slept = self.last_frame.map_or(0.0, |before| (now - before).max(0.0));
        if self.was_paused {
            self.live.iter_mut().filter_map(|l| l.deadline.as_mut()).for_each(|deadline| *deadline += slept);
        }
        self.last_frame = Some(now);
        let mut hovered_any = false;
        let mut dismiss: Option<usize> = None;
        Area::new(Id::new("omuzaki_ui::toasts"))
            .order(Order::Tooltip)
            .anchor(Align2::RIGHT_BOTTOM, -inset)
            .interactable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = space::S8;
                ui.set_width(size::TOAST_WIDTH);
                for (index, live) in self.live.iter().enumerate() {
                    let age = (now - live.born) as f32;
                    let fade_in = ds.motion(dur::TOAST_IN);
                    let fade_out = ds.motion(dur::TOAST_OUT);
                    let appear = if fade_in > 0.0 { (age / fade_in).clamp(0.0, 1.0) } else { 1.0 };
                    let vanish = live.leaving.map_or(1.0, |t| {
                        if fade_out > 0.0 { 1.0 - (((now - t) as f32) / fade_out).clamp(0.0, 1.0) } else { 0.0 }
                    });
                    let opacity = appear * vanish;
                    if opacity <= 0.0 {
                        continue;
                    }
                    ui.scope(|ui| {
                        ui.set_opacity(opacity);
                        ui.add_space((1.0 - appear) * 4.0);
                        let out = ToastCard::new(&live.toast).count(live.count).show(ui);
                        hovered_any |= out.hovered;
                        if let Some(i) = out.action {
                            events.push(ToastEvent::Action {
                                message: live.toast.message.clone(),
                                label: live.toast.actions[i].clone(),
                            });
                            dismiss = Some(index);
                        }
                        if out.closed {
                            events.push(ToastEvent::Dismissed);
                            dismiss = Some(index);
                        }
                    });
                }
            });

        // Timers run unless the pointer is over the stack or a toast has keyboard focus.
        self.was_paused = hovered_any;
        if let Some(i) = dismiss
            && let Some(l) = self.live.get_mut(i)
        {
            l.leaving.get_or_insert(now);
        }
        let fade_in = f64::from(ds.motion(dur::TOAST_IN));
        let mut next_wake: Option<f64> = None;
        let mut wake = |seconds: f64| next_wake = Some(next_wake.map_or(seconds, |w| w.min(seconds)));
        for l in &mut self.live {
            if l.leaving.is_some() || now - l.born < fade_in {
                // A fade is running: frames at the display rate until it ends.
                wake(FADE_FRAME);
                continue;
            }
            if let Some(deadline) = l.deadline.filter(|_| !hovered_any) {
                if now >= deadline {
                    l.leaving = Some(now);
                    wake(FADE_FRAME);
                } else {
                    wake(deadline - now);
                }
            }
        }
        let fade_out = f64::from(ds.motion(dur::TOAST_OUT));
        self.live.retain(|l| l.leaving.is_none_or(|t| now - t < fade_out + 0.001 && fade_out > 0.0));
        if let Some(wait) = next_wake {
            ctx.request_repaint_after_secs(wait.max(FADE_FRAME) as f32);
        }
        events
    }
}

/// What a drawn toast card reports.
#[derive(Debug)]
pub struct ToastCardOutput {
    /// The card's response.
    pub response: Response,
    /// The pointer is over it, or it has keyboard focus: its timer should pause.
    pub hovered: bool,
    /// The index of the action pressed, if any.
    pub action: Option<usize>,
    /// The close button was pressed.
    pub closed: bool,
}

/// One toast, drawn in place. [`Toasts`] stacks these; the gallery shows them directly.
#[must_use = "a toast card does nothing until it is shown"]
pub struct ToastCard<'a> {
    toast: &'a Toast,
    count: u32,
}

impl<'a> ToastCard<'a> {
    /// A card for `toast`.
    pub const fn new(toast: &'a Toast) -> Self {
        Self { toast, count: 1 }
    }

    /// Folded duplicates: shows "×3".
    pub const fn count(mut self, count: u32) -> Self {
        self.count = count;
        self
    }

    /// Draws the card at 320 pt wide.
    pub fn show(self, ui: &mut Ui) -> ToastCardOutput {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let status = self.toast.status;
        let mut action = None;
        let mut closed = false;
        let mut focus_within = false;
        let inner =
            paint::overlay_frame(&ds).inner_margin(Margin { left: 12, right: 8, top: 10, bottom: 10 }).show(ui, |ui| {
                let width = size::TOAST_WIDTH - 20.0 - 2.0;
                ui.set_width(width);
                // Rows are as tall as their text here, not the 28 pt control row.
                ui.spacing_mut().interact_size.y = 0.0;
                // Tab follows the eye (top to bottom, left to right), so the close button, which sits at the top
                // right, is created before the actions below the message.
                let close_rect = Rect::from_min_size(
                    egui::pos2(ui.cursor().left() + width - size::MIN_TARGET, ui.cursor().top()),
                    vec2(size::MIN_TARGET, size::MIN_TARGET),
                );
                let close = close_button(ui, &ds, close_rect);
                focus_within |= close.has_focus();
                closed = close.clicked();
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    let (icon_rect, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                    paint::icon(
                        ui.painter(),
                        &ds,
                        icon_rect.center(),
                        icons::status(status),
                        16.0,
                        t.status(status),
                        true,
                    );
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 28.0);
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &self.toast.message,
                            0.0,
                            egui::text::TextFormat::simple(ds.text.body.clone(), t.text),
                        );
                        if self.count > 1 {
                            job.append(
                                &format!("  ×{}", self.count),
                                0.0,
                                egui::text::TextFormat::simple(ds.text.micro.clone(), t.text3),
                            );
                        }
                        ui.label(job);
                        if !self.toast.actions.is_empty() {
                            ui.add_space(space::S4);
                            let row = vec2(ui.available_width(), ds.metrics.control_row);
                            ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = space::S4;
                                // Right-aligned, but laid out left to right so the first action is the first Tab stop.
                                let buttons: Vec<Button<'_>> =
                                    self.toast.actions.iter().map(|l| Button::new(l).kind(ButtonKind::Ghost)).collect();
                                let used: f32 = buttons.iter().map(|b| b.width(ui.ctx())).sum::<f32>()
                                    + space::S4 * (buttons.len().saturating_sub(1)) as f32;
                                ui.add_space((ui.available_width() - used).max(0.0));
                                for (i, button) in buttons.into_iter().enumerate() {
                                    let response = ui.add(button);
                                    focus_within |= response.has_focus();
                                    if response.clicked() {
                                        action = Some(i);
                                    }
                                }
                            });
                        }
                    });
                });
            });
        let response = inner.response;
        let hovered = response.contains_pointer() || focus_within;
        // A label's text is its value, so a folded toast's count joins the message instead of replacing it. Built inside
        // the closure: it only runs when a screen reader is listening.
        let (count, message) = (self.count, self.toast.message.as_str());
        let ui_response = ui.interact(response.rect, response.id.with("toast"), Sense::hover());
        a11y::describe(
            &ui_response,
            || {
                let spoken = if count > 1 {
                    ds.text_with(Msg::ToastRepeated, &[("message", message), ("count", &count.to_string())])
                } else {
                    message.to_owned()
                };
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, spoken)
            },
            |node| {
                node.set_role(accesskit::Role::Status);
                node.set_live(accesskit::Live::Polite);
                node.set_description(ds.status_word(status).into_owned());
            },
        );
        ToastCardOutput { response, hovered, action, closed }
    }
}

/// The close button: a 24 pt target (WCAG 2.5.8) with a 12 pt cross, placed at `rect`.
fn close_button(ui: &mut Ui, ds: &Ds, rect: Rect) -> Response {
    let t = &ds.theme;
    // Placed at an explicit rectangle, with the next automatic id: siblings of one `Ui` must not share an id.
    let id = ui.next_auto_id();
    ui.skip_ahead_auto_ids(1);
    let response = ui.interact(rect, id, Sense::click());
    let hover = paint::fade(ui.ctx(), ds, response.id, paint::hot(&response));
    if hover > 0.0 {
        ui.painter().rect_filled(rect, radius::CONTROL, t.bg4.gamma_multiply(hover));
    }
    paint::icon(ui.painter(), ds, rect.center(), icons::X, 12.0, paint::lerp(t.text3, t.text, hover), false);
    paint::focus_ring(ui.painter(), t, &response, rect, radius::CONTROL, RingPlacement::Outside);
    a11y::describe(
        &response,
        || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, ds.text(Msg::ToastDismiss)),
        |_| {},
    );
    response
}
