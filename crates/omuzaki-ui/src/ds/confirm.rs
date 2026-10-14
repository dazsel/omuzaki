//! The second step of a dangerous request (`design-system.md` section 7.9): going live, publishing, spending money,
//! deleting.
//!
//! The first press turns the button into "Confirm: go live as @name". The second step must be a *deliberate,
//! separate action*, and a gesture that happens to produce two clicks must not count as one:
//!
//! - The press has to **start after the confirm control appeared and a short dwell has passed** ([`ARM_DELAY`]), so
//!   the second click of a double-click, which lands a few frames after the first, is ignored.
//! - A **held key** does not count. egui clicks a focused button on every auto-repeat of Enter or Space, so a held
//!   Enter would arm the button and confirm it a moment later; only the press itself (the key going down for the
//!   first time) can confirm.
//!
//! The dwell is not a timeout: nothing ever happens when it ends, and an armed request waits for as long as the person
//! likes ("no timers, ever"). It only says how soon a press may count. Time comes from the context's clock
//! ([`clock`](super::clock)), so tests move it by hand.

use egui::{Context, Event, Key, PointerButton, Response, accesskit};

use super::clock;

/// Seconds after the confirm control appears before a press can confirm.
pub const ARM_DELAY: f64 = 0.4;

/// A request whose first step has been taken, waiting for its second.
#[derive(Clone, Copy, Debug)]
pub struct Arm {
    armed_at: f64,
    /// The press now in progress began after the dwell, so its release may confirm.
    fresh_press: bool,
}

impl Arm {
    /// The first step was just taken.
    pub fn start(ctx: &Context) -> Self {
        Self { armed_at: clock::now(ctx), fresh_press: false }
    }

    /// Whether `response`, the confirm control, was just pressed deliberately: the press began after the dwell, and it
    /// was a pointer press, a first key-down of Enter or Space, or an assistive-technology click, never a repeat.
    pub fn confirmed(&mut self, response: &Response) -> bool {
        let ctx = &response.ctx;
        let ready = clock::now(ctx) - self.armed_at >= ARM_DELAY;
        let control = response.id.accesskit_id();
        // Asked before `ctx.input`: the focus lives behind another lock of the same context.
        let focused = response.has_focus();
        let mut released = false;
        ctx.input(|i| {
            for event in &i.events {
                let begins_press = match event {
                    Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, .. } => {
                        response.rect.contains(*pos)
                    }
                    Event::Key { key: Key::Enter | Key::Space, pressed: true, repeat: false, .. } => focused,
                    Event::AccessKitActionRequest(request) => {
                        request.action == accesskit::Action::Click && request.target_node == control
                    }
                    _ => false,
                };
                if begins_press {
                    self.fresh_press = ready;
                }
                released |= matches!(
                    event,
                    Event::PointerButton { pressed: false, .. }
                        | Event::Key { key: Key::Enter | Key::Space, pressed: false, .. }
                );
            }
        });
        let confirmed = response.clicked() && self.fresh_press && ready;
        // A press that ended without confirming (released elsewhere, or a key let go) does not carry over to the next
        // click, and neither does one that was just used.
        if released || response.clicked() {
            self.fresh_press = false;
        }
        confirmed
    }
}
