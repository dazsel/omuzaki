//! The global keyboard map (`shell.md` section 16, "Global" scope): which key does which shell action.
//!
//! [`resolve`] only reads the input and names the action, so the map is tested without a window and the shell is the
//! one place that carries actions out. Ctrl is Cmd on macOS. A key is consumed when it matches, so it never reaches a
//! widget as well. While a text field is being edited nothing is consumed (Undo in a field is the field's own); a
//! focused button or tab does not stop them, so the keys keep working after a click.

use egui::{Context, Key, Modifiers};

/// What a key press asks the shell to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Ctrl+1 to Ctrl+9: the nth workspace tab, from 0.
    Workspace(usize),
    /// Ctrl+0: the Theatre.
    Theatre,
    /// Ctrl+Z.
    Undo,
    /// Ctrl+Shift+Z or Ctrl+Y.
    Redo,
    /// Ctrl+Shift+L.
    ToggleLock,
    /// Ctrl+Space (and Esc to restore).
    ToggleMaximise,
    /// F6.
    NextPanel,
    /// Shift+F6.
    PreviousPanel,
    /// Ctrl+/: the shortcut sheet.
    ShortcutSheet,
    /// Ctrl+Alt+P: pause or resume the active agent.
    PauseAgent,
    /// Ctrl+, : the Settings window.
    Settings,
}

const NUMBERS: [Key; 9] =
    [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];

/// The action the keys pressed this frame ask for, consuming them. At most one per frame.
pub fn resolve(ctx: &Context) -> Option<Action> {
    if ctx.text_edit_focused() {
        return None;
    }
    ctx.input_mut(|i| {
        let cmd = Modifiers::COMMAND;
        // The more specific chords first: Ctrl+Shift+Z must not be read as Ctrl+Z.
        if i.consume_key(cmd | Modifiers::SHIFT, Key::Z) || i.consume_key(cmd, Key::Y) {
            Some(Action::Redo)
        } else if i.consume_key(cmd | Modifiers::ALT, Key::P) {
            Some(Action::PauseAgent)
        } else if i.consume_key(cmd, Key::Slash) {
            Some(Action::ShortcutSheet)
        } else if i.consume_key(cmd, Key::Comma) {
            Some(Action::Settings)
        } else if i.consume_key(cmd | Modifiers::SHIFT, Key::L) {
            Some(Action::ToggleLock)
        } else if i.consume_key(cmd, Key::Z) {
            Some(Action::Undo)
        } else if i.consume_key(cmd, Key::Space) {
            Some(Action::ToggleMaximise)
        } else if i.consume_key(cmd, Key::Num0) {
            Some(Action::Theatre)
        } else if i.consume_key(Modifiers::SHIFT, Key::F6) {
            Some(Action::PreviousPanel)
        } else if i.consume_key(Modifiers::NONE, Key::F6) {
            Some(Action::NextPanel)
        } else {
            NUMBERS.iter().position(|key| i.consume_key(cmd, *key)).map(Action::Workspace)
        }
    })
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;

    use super::*;
    use crate::ds::testing::press;

    /// A window that records every action the keys ask for, with a text field to give the keyboard to.
    fn harness() -> Harness<'static, Vec<Action>> {
        let mut harness = Harness::new_ui_state(
            |ui, seen: &mut Vec<Action>| {
                seen.extend(resolve(ui.ctx()));
                ui.add(egui::TextEdit::singleline(&mut String::new()).id(egui::Id::new("field")));
            },
            Vec::new(),
        );
        harness.run();
        harness
    }

    #[test]
    fn each_global_chord_names_its_action() {
        let (cmd, shift) = (Modifiers::COMMAND, Modifiers::SHIFT);
        for (modifiers, key, action) in [
            (cmd, Key::Num1, Action::Workspace(0)),
            (cmd, Key::Num9, Action::Workspace(8)),
            (cmd, Key::Num0, Action::Theatre),
            (cmd, Key::Z, Action::Undo),
            (cmd | shift, Key::Z, Action::Redo),
            (cmd, Key::Y, Action::Redo),
            (cmd | shift, Key::L, Action::ToggleLock),
            (cmd, Key::Space, Action::ToggleMaximise),
            (Modifiers::NONE, Key::F6, Action::NextPanel),
            (shift, Key::F6, Action::PreviousPanel),
            (cmd, Key::Slash, Action::ShortcutSheet),
            (cmd | Modifiers::ALT, Key::P, Action::PauseAgent),
            (cmd, Key::Comma, Action::Settings),
        ] {
            let mut harness = harness();
            press(&mut harness, modifiers, key);
            assert_eq!(harness.state(), &vec![action], "{modifiers:?} {key:?}");
        }
    }

    #[test]
    fn a_plain_key_or_a_chord_the_map_does_not_have_is_left_alone() {
        for (modifiers, key) in [(Modifiers::NONE, Key::Num1), (Modifiers::NONE, Key::Z), (Modifiers::COMMAND, Key::Q)]
        {
            let mut harness = harness();
            press(&mut harness, modifiers, key);
            assert!(harness.state().is_empty(), "{modifiers:?} {key:?} belongs to someone else");
        }
    }

    #[test]
    fn nothing_is_taken_while_a_text_field_has_the_keyboard() {
        let mut harness = harness();
        harness.ctx.memory_mut(|m| m.request_focus(egui::Id::new("field")));
        harness.run();
        press(&mut harness, Modifiers::COMMAND, Key::Z);
        assert_eq!(harness.state().len(), 0);
    }
}
