//! Whether to draw the keyboard focus ring.
//!
//! egui paints no ring of its own (spike item 1), so every widget draws the 2 pt ring from `Response::has_focus()`.
//! Like a browser's `:focus-visible`, the ring shows when focus arrived by keyboard and stays hidden after a mouse
//! click, so pointer users do not see rings everywhere while keyboard users always do (WCAG 2.4.7).

use egui::{Context, Event, Id};

#[derive(Clone, Copy)]
struct State {
    pass: u64,
    keyboard: bool,
}

/// True when the last thing the user did with an input device was press a key.
///
/// The first call in each pass scans the pass's input events; later calls read the stored answer.
pub fn visible(ctx: &Context) -> bool {
    let id = Id::new("omuzaki_ui::ds::focus_visible");
    let pass = ctx.cumulative_pass_nr();
    let mut state = ctx.data(|d| d.get_temp::<State>(id)).unwrap_or(State { pass: u64::MAX, keyboard: false });
    if state.pass != pass {
        ctx.input(|i| {
            for event in &i.events {
                match event {
                    // Assistive technology moves focus without a pointer, so it gets the ring like a keyboard does.
                    Event::Key { pressed: true, .. } | Event::AccessKitActionRequest(_) => state.keyboard = true,
                    Event::PointerButton { pressed: true, .. } | Event::Touch { .. } => state.keyboard = false,
                    _ => {}
                }
            }
        });
        state.pass = pass;
        ctx.data_mut(|d| d.insert_temp(id, state));
    }
    state.keyboard
}
