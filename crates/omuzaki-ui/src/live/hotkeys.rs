//! [`Hotkeys`]: the keyboard as a trigger device. A bound key pressed anywhere in the app shows its expression, and
//! released (for a hold binding) lets it fade, through `trigger.press`, the same op an agent calls.
//!
//! The Live panel only *binds* keys (its trigger button captures the next chord and runs `trigger.bind`); this is what
//! makes a binding do something. It runs once per frame after the shell has drawn, so a key the panel took to bind
//! (consumed from the frame's input) never also fires, and a key typed into a text field stays the field's. It lives
//! with the app and not with the panel, because a performer keeps the workspace on any panel, or none of the Live
//! panel's own, while hotkeys must keep working.

use egui::{Context, Event, Key, Modifiers};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_stage::perform::triggers::{TriggerInfo, same_key};
use serde::Deserialize as _;

use super::model::{OP_TRIGGER_LIST, OP_TRIGGER_PRESS, object, run};

/// The name a key press is bound under: its modifiers, then the key, as `Ctrl+Shift+F12`.
pub fn chord(modifiers: Modifiers, key: Key) -> String {
    let mut name = String::new();
    for (held, word) in
        [(modifiers.ctrl, "Ctrl"), (modifiers.alt, "Alt"), (modifiers.shift, "Shift"), (modifiers.mac_cmd, "Cmd")]
    {
        if held {
            name.push_str(word);
            name.push('+');
        }
    }
    name.push_str(key.name());
    name
}

/// What one frame's input says to a hotkey.
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyEvent {
    /// A key went down (not a repeat), as this chord.
    Down(Key, String),
    /// A key came up.
    Up(Key),
    /// The window lost focus: the keys that were down will never be reported up.
    FocusLost,
}

fn key_event(event: &Event) -> Option<KeyEvent> {
    match event {
        Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
            Some(KeyEvent::Down(*key, chord(*modifiers, *key)))
        }
        Event::Key { key, pressed: false, .. } => Some(KeyEvent::Up(*key)),
        Event::WindowFocused(false) => Some(KeyEvent::FocusLost),
        _ => None,
    }
}

/// The keyboard's trigger presses: which keys it holds down, so each release reaches the binding its press did.
#[derive(Debug, Default)]
pub struct Hotkeys {
    /// Keys held down that fired a binding: the key, and the binding's own spelling of it.
    held: Vec<(Key, String)>,
}

impl Hotkeys {
    /// No key held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends this frame's key presses and releases to the bindings, as `actor`. Call it after the shell has drawn: the
    /// panel's capture and any focused text field have taken their keys from the input by then.
    pub fn dispatch(&mut self, ctx: &Context, runner: &dyn Runner, actor: &Actor) {
        // Collected only when a key event arrived, so a quiet frame allocates nothing.
        let events: Vec<KeyEvent> = ctx.input(|input| input.events.iter().filter_map(key_event).collect());
        if events.is_empty() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        for event in events {
            match event {
                KeyEvent::Down(key, chord) if !typing => self.down(runner, actor, key, &chord),
                KeyEvent::Down(..) => {}
                KeyEvent::Up(key) => self.up(runner, actor, key),
                KeyEvent::FocusLost => self.release_all(runner, actor),
            }
        }
    }

    fn down(&mut self, runner: &dyn Runner, actor: &Actor, key: Key, chord: &str) {
        let Some(bound) = bound_key(runner, actor, chord) else { return };
        if press(runner, actor, &bound, true) {
            self.held.push((key, bound));
        }
    }

    fn up(&mut self, runner: &dyn Runner, actor: &Actor, key: Key) {
        while let Some(at) = self.held.iter().position(|(held, _)| *held == key) {
            let (_, bound) = self.held.swap_remove(at);
            press(runner, actor, &bound, false);
        }
    }

    fn release_all(&mut self, runner: &dyn Runner, actor: &Actor) {
        for (_, bound) in std::mem::take(&mut self.held) {
            press(runner, actor, &bound, false);
        }
    }
}

/// The spelling the table binds `chord` under, if anything is bound to it. A key nothing is bound to is not sent to
/// `trigger.press`, which would refuse it as unknown.
fn bound_key(runner: &dyn Runner, actor: &Actor, chord: &str) -> Option<String> {
    let mut output = run(runner, actor, OP_TRIGGER_LIST, object(&[]))
        .inspect_err(|error| tracing::warn!(%error, "the hotkeys could not read the trigger table"))
        .ok()?;
    let triggers = output.get_mut("triggers").map(serde_json::Value::take).unwrap_or_default();
    let triggers = Vec::<TriggerInfo>::deserialize(triggers)
        .inspect_err(|error| tracing::warn!(%error, "the trigger table answered something else"))
        .ok()?;
    triggers.into_iter().map(|info| info.binding.key).find(|bound| same_key(bound, chord))
}

/// `trigger.press` on `key`; whether the performer took it.
fn press(runner: &dyn Runner, actor: &Actor, key: &str, down: bool) -> bool {
    let args = object(&[("key", key.into()), ("down", down.into())]);
    match run(runner, actor, OP_TRIGGER_PRESS, args) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, key, down, "a hotkey press was refused");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, pressed: bool, repeat: bool, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: Some(key), pressed, repeat, modifiers }
    }

    #[test]
    fn a_chord_names_its_modifiers_before_the_key() {
        assert_eq!(chord(Modifiers::NONE, Key::F1), "F1");
        let held = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        assert_eq!(chord(held, Key::F12), "Ctrl+Shift+F12");
    }

    #[test]
    fn a_press_a_release_and_a_lost_focus_are_events_and_a_repeat_is_not() {
        let none = Modifiers::NONE;
        assert_eq!(key_event(&key(Key::F1, true, false, none)), Some(KeyEvent::Down(Key::F1, "F1".into())));
        assert_eq!(key_event(&key(Key::F1, true, true, none)), None, "a held key repeating is not a new press");
        assert_eq!(key_event(&key(Key::F1, false, false, Modifiers::CTRL)), Some(KeyEvent::Up(Key::F1)));
        assert_eq!(key_event(&Event::WindowFocused(false)), Some(KeyEvent::FocusLost));
        assert_eq!(key_event(&Event::WindowFocused(true)), None);
        assert_eq!(key_event(&Event::Text("a".into())), None);
    }
}
