//! Telling a deliberate press from a held key or a second click.
//!
//! egui turns Enter and Space on a focused widget into a click on every key event, auto-repeat included, so a held
//! Enter clicks a button thirty times a second. Most controls can take that; a few cannot (opening and closing a
//! popover, confirming a dangerous request), and use this to ask whether a click was only a repeat.

use egui::{Event, Key, PointerButton, Response};

/// True when `response` was clicked this frame only by the auto-repeat of a held Enter or Space.
///
/// A pointer click, an assistive-technology click and a fresh key press all return false.
pub fn only_repeats(response: &Response) -> bool {
    if response.clicked_by(PointerButton::Primary) {
        return false;
    }
    response.ctx.input(|i| {
        let mut repeated = false;
        for event in &i.events {
            match event {
                Event::Key { key: Key::Enter | Key::Space, pressed: true, repeat, .. } => {
                    if !repeat {
                        return false;
                    }
                    repeated = true;
                }
                Event::AccessKitActionRequest(_) => return false,
                _ => {}
            }
        }
        repeated
    })
}

/// True when `response` was clicked this frame by a pointer, an assistive technology or the *press* of Enter or Space,
/// and not by the auto-repeat or the release of a key that went down earlier (the Enter that opened a dialog is
/// released while the dialog's primary button already has the focus).
pub fn is_press(response: &Response) -> bool {
    if !response.clicked() {
        return false;
    }
    if response.clicked_by(PointerButton::Primary) {
        return true;
    }
    response.ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                Event::AccessKitActionRequest(_)
                    | Event::Key { key: Key::Enter | Key::Space, pressed: true, repeat: false, .. }
            )
        })
    })
}
