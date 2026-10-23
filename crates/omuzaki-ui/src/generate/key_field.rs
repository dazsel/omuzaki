//! The masked key field of a generator (#1009, `connectors.md` section 8, #784): the one place a key is typed, and it
//! goes straight to the keychain. No op, MCP tool or setting carries it.
//!
//! The field takes a paste, Save writes it to the keychain under the generator's label (`connector/<id>`) and empties
//! the field, and Remove deletes it. The field also empties when focus goes anywhere but Save, so a key never waits
//! in memory behind a closed page. The keychain value zeroes the one buffer the key was in. The field is drawn
//! and exposed to assistive technology as dots, with copy, cut and undo off ([`TextField::password`]). Whether a key
//! is stored is read through `connect.key_status`, which never returns the key.

use egui::{Key, RichText, Ui};
use omuzaki_secrets_api::prelude::{SecretError, SecretLabel, SecretValue};

use super::text;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Button, FormRow, StatusChip, TextField};
use crate::panels::text::say;
use crate::settings::connectors::Keychain;

/// The shortest text taken as a key; the host refuses shorter ones too.
pub const MIN_KEY: usize = 8;

/// What the last Save or Remove did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyNote {
    /// The key is too short to be one.
    Short,
    /// The keychain refused, with what to do.
    Failed(String),
}

/// The field's state, kept while the page is open. It never holds a key longer than the field has focus.
#[derive(Debug, Default)]
pub struct KeyField {
    typed: String,
    note: Option<KeyNote>,
}

impl KeyField {
    /// What the last Save or Remove said, if anything went wrong.
    pub fn note(&self) -> Option<&KeyNote> {
        self.note.as_ref()
    }

    /// Whether the field holds any text.
    pub fn is_empty(&self) -> bool {
        self.typed.is_empty()
    }

    /// Draws the field for the key under `label`; `present` is `connect.key_status`'s answer. True when the keychain
    /// changed, so the caller reads the status again.
    pub fn show(&mut self, ui: &mut Ui, keychain: Option<&Keychain>, label: &str, present: Option<bool>) -> bool {
        let ds = ds(ui.ctx());
        let chip = match present {
            Some(true) => Some((Status::Ok, say(&ds, text::KEY_SAVED, &[]))),
            Some(false) => Some((Status::Warn, say(&ds, text::KEY_MISSING, &[]))),
            None => None,
        };
        let mut changed = false;
        ui.horizontal(|ui| {
            if let Some((tone, word)) = &chip {
                ui.add(StatusChip::new(*tone, word));
            }
            if present == Some(true)
                && let Some(keychain) = keychain
            {
                let remove = say(&ds, text::KEY_REMOVE, &[]);
                if ui.add(Button::new(&remove).ghost()).clicked() {
                    changed |= self.remove(keychain, label);
                }
            }
        });
        let Some(keychain) = keychain else {
            ui.label(RichText::new(say(&ds, text::NO_KEYCHAIN, &[])).color(ds.theme.text2));
            return changed;
        };
        let name = say(&ds, text::KEY, &[]);
        FormRow::new(&name).show(ui, |ui, name| {
            // The field takes the row and Save sits under it, so both fit the narrow detail column.
            let hint = say(&ds, text::KEY_HINT, &[]);
            let field = TextField::new(name, &mut self.typed).password().hint(&hint).clearable(false);
            let field = field.width(ui.available_width().clamp(120.0, 320.0));
            let field = ui.add(field.id_salt(("generate.key", label)));
            ui.add_space(space::S4);
            let save = say(&ds, text::KEY_SAVE, &[]);
            let empty = say(&ds, text::KEY_EMPTY, &[]);
            let save = ui.add(Button::new(&save).primary().disabled_if(self.typed.is_empty(), &empty));
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if save.clicked() || entered {
                changed |= self.save(keychain, label);
            } else if !(field.has_focus() || field.lost_focus() || save.has_focus() || save.hovered()) {
                // Focus went anywhere but Save (a frame after it left the field, so Tab to Save keeps the key).
                self.typed.clear();
            }
        });
        match &self.note {
            Some(KeyNote::Short) => {
                ui.label(RichText::new(say(&ds, text::KEY_SHORT, &[])).color(ds.theme.danger));
            }
            Some(KeyNote::Failed(reason)) => {
                let line = say(&ds, text::KEY_FAILED, &[("reason", reason)]);
                ui.label(RichText::new(line).color(ds.theme.danger));
            }
            None => {}
        }
        ui.add_space(space::S4);
        let small = ds.text.small.clone();
        ui.label(RichText::new(say(&ds, text::KEY_WHERE, &[])).font(small).color(ds.theme.text2));
        changed
    }

    /// Writes the typed key to the keychain and empties the field, whatever happens.
    fn save(&mut self, keychain: &Keychain, label: &str) -> bool {
        // Moved, not copied: the keychain's value zeroes the one buffer the field held.
        let key = std::mem::take(&mut self.typed);
        if key.trim().chars().count() < MIN_KEY {
            drop(SecretValue::new(key));
            self.note = Some(KeyNote::Short);
            return false;
        }
        let result = SecretLabel::new(label).and_then(|label| keychain.0.set(&label, SecretValue::new(key)));
        self.note = result.err().map(|e| KeyNote::Failed(reason(&e)));
        self.note.is_none()
    }

    /// Deletes the stored key.
    fn remove(&mut self, keychain: &Keychain, label: &str) -> bool {
        let result = SecretLabel::new(label).and_then(|label| keychain.0.delete(&label));
        self.note = result.err().map(|e| KeyNote::Failed(reason(&e)));
        self.note.is_none()
    }
}

/// What the person can do about `error`: the keychain's own hint when there is none to talk to.
fn reason(error: &SecretError) -> String {
    match error {
        SecretError::Unavailable { hint, .. } => hint.clone(),
        other => other.to_string(),
    }
}
