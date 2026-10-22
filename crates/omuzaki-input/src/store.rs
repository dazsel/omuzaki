//! The binding store (design §5): the bindings with their chords, in the machine-local `input-bindings.toml` in the
//! data folder, read only by the hub. It is not a setting (settings are listed to agents and their values appear in
//! events), and nothing outside the crate ever sees a chord: the ops get ids, names, devices and actions, and the
//! binding editor gets a `ChordLabel`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use omuzaki_input_api::binding::{BindingAction, BindingId, BindingInfo, Intent, MAX_BINDINGS};
use omuzaki_input_api::capture::InputError;
use omuzaki_input_api::event::DeviceClass;
use serde::{Deserialize, Serialize};

use crate::matcher::{Chord, Mods};
use crate::normalise::HidUsage;
use crate::shared::{TableEntry, lock};

/// The store's file in the data folder.
pub(crate) const FILE: &str = "input-bindings.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    next_id: u32,
    #[serde(default)]
    binding: Vec<Stored>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    id: u32,
    name: String,
    device: DeviceClass,
    /// The action as JSON text: an op's arguments are any JSON, which TOML cannot always hold.
    action: String,
    chord: Option<Chord>,
}

impl Stored {
    fn action(&self) -> Option<BindingAction> {
        serde_json::from_str(&self.action).ok()
    }
}

/// A preset: bindings with their chords, which stay inside the hub.
pub(crate) struct Preset {
    pub(crate) name: &'static str,
    pub(crate) bindings: &'static [(&'static str, DeviceClass, Intent, Chord)],
}

const fn key(usage: u16) -> Chord {
    Chord::Key { usage: HidUsage(usage), mods: Mods(0) }
}

/// The presets that ship (design §5): nothing is bound until the person applies one.
pub(crate) const PRESETS: &[Preset] = &[Preset {
    name: "locomotion",
    bindings: &[
        ("walk forward", DeviceClass::Keyboard, Intent::moving(0, 127), key(0x1A)),
        ("walk left", DeviceClass::Keyboard, Intent::moving(-127, 0), key(0x04)),
        ("walk back", DeviceClass::Keyboard, Intent::moving(0, -127), key(0x16)),
        ("walk right", DeviceClass::Keyboard, Intent::moving(127, 0), key(0x07)),
        ("jump", DeviceClass::Keyboard, Intent::Jump, key(0x2C)),
        ("run", DeviceClass::Keyboard, Intent::Run, key(0xE1)),
        ("sit", DeviceClass::Keyboard, Intent::Sit, key(0x06)),
    ],
}];

/// The bindings, kept in memory and written to the file on every change.
#[derive(Debug)]
pub(crate) struct Store {
    path: Option<PathBuf>,
    file: Mutex<File>,
}

fn store_error(reason: &str) -> InputError {
    InputError::Store(reason.to_owned())
}

impl Store {
    /// The store in `dir` (created on first save), or in memory only without a folder.
    #[allow(clippy::disallowed_methods, reason = "the hub's own file in its data folder, never a path an op names; the fence is for ops")]
    pub(crate) fn open(dir: Option<&Path>) -> Result<Self, InputError> {
        let path = dir.map(|d| d.join(FILE));
        let file = match &path {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(path).map_err(|_| store_error("the binding file could not be read"))?;
                toml::from_str(&text).map_err(|_| store_error("the binding file is not valid"))?
            }
            _ => File::default(),
        };
        Ok(Self { path, file: Mutex::new(file) })
    }

    #[allow(clippy::disallowed_methods, reason = "the hub's own file in its data folder, never a path an op names; the fence is for ops")]
    fn save(&self, file: &File) -> Result<(), InputError> {
        let Some(path) = &self.path else { return Ok(()) };
        let text = toml::to_string(file).map_err(|_| store_error("the bindings could not be written"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|_| store_error("the data folder could not be made"))?;
        }
        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, text).map_err(|_| store_error("the binding file could not be written"))?;
        std::fs::rename(&temp, path).map_err(|_| store_error("the binding file could not be replaced"))
    }

    fn change<T>(&self, f: impl FnOnce(&mut File) -> Result<T, InputError>) -> Result<T, InputError> {
        let mut file = lock(&self.file);
        let mut next = file.clone();
        let value = f(&mut next)?;
        self.save(&next)?;
        *file = next;
        Ok(value)
    }

    /// The table's entries, in store order.
    pub(crate) fn entries(&self) -> Vec<TableEntry> {
        lock(&self.file)
            .binding
            .iter()
            .filter_map(|b| b.action().map(|action| TableEntry { id: BindingId(b.id), chord: b.chord, action }))
            .collect()
    }

    /// What `input.binding.list` shows: never a chord.
    pub(crate) fn infos(&self) -> Vec<BindingInfo> {
        let file = lock(&self.file);
        file.binding
            .iter()
            .filter_map(|b| {
                let conflict =
                    b.chord.is_some() && file.binding.iter().any(|other| other.id != b.id && other.chord == b.chord);
                Some(BindingInfo { id: BindingId(b.id), name: b.name.clone(), device: b.device, action: b.action()?, conflict })
            })
            .collect()
    }

    pub(crate) fn add(
        &self,
        name: &str,
        device: DeviceClass,
        action: &BindingAction,
        chord: Option<Chord>,
    ) -> Result<BindingId, InputError> {
        let action = serde_json::to_string(action).map_err(|_| store_error("the action could not be written"))?;
        self.change(|file| {
            if file.binding.len() >= MAX_BINDINGS {
                return Err(InputError::TooManyBindings);
            }
            file.next_id = file.next_id.max(file.binding.iter().map(|b| b.id).max().unwrap_or(0)) + 1;
            let id = file.next_id;
            file.binding.push(Stored { id, name: name.to_owned(), device, action, chord });
            Ok(BindingId(id))
        })
    }

    fn with<T>(&self, id: BindingId, f: impl FnOnce(&mut File, usize) -> T) -> Result<T, InputError> {
        self.change(|file| {
            let at = file.binding.iter().position(|b| b.id == id.0).ok_or(InputError::UnknownBinding(id))?;
            Ok(f(file, at))
        })
    }

    pub(crate) fn set_action(&self, id: BindingId, action: &BindingAction) -> Result<(), InputError> {
        let action = serde_json::to_string(action).map_err(|_| store_error("the action could not be written"))?;
        self.with(id, |file, at| {
            if let Some(b) = file.binding.get_mut(at) {
                b.action = action;
            }
        })
    }

    pub(crate) fn remove(&self, id: BindingId) -> Result<(), InputError> {
        self.with(id, |file, at| {
            file.binding.remove(at);
        })
    }

    pub(crate) fn set_chord(&self, id: BindingId, chord: Option<Chord>) -> Result<(), InputError> {
        self.with(id, |file, at| {
            if let Some(b) = file.binding.get_mut(at) {
                b.chord = chord;
            }
        })
    }

    pub(crate) fn chord(&self, id: BindingId) -> Option<Chord> {
        lock(&self.file).binding.iter().find(|b| b.id == id.0).and_then(|b| b.chord)
    }

    pub(crate) fn contains(&self, id: BindingId) -> bool {
        lock(&self.file).binding.iter().any(|b| b.id == id.0)
    }

    /// The bindings sharing `id`'s chord.
    pub(crate) fn conflicts(&self, id: BindingId) -> Vec<BindingId> {
        let Some(chord) = self.chord(id) else { return Vec::new() };
        lock(&self.file)
            .binding
            .iter()
            .filter(|b| b.id != id.0 && b.chord == Some(chord))
            .map(|b| BindingId(b.id))
            .collect()
    }

    /// Applies a preset: a binding of the same name takes the preset's chord and action, others are added.
    pub(crate) fn apply(&self, preset: &Preset) -> Result<Vec<BindingId>, InputError> {
        let mut ids = Vec::new();
        for (name, device, intent, chord) in preset.bindings {
            let action = BindingAction::Intent(*intent);
            let existing = lock(&self.file).binding.iter().find(|b| b.name == *name).map(|b| BindingId(b.id));
            let id = match existing {
                Some(id) => {
                    self.set_action(id, &action)?;
                    self.set_chord(id, Some(*chord))?;
                    id
                }
                None => self.add(name, *device, &action, Some(*chord))?,
            };
            ids.push(id);
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods, reason = "the test reads the hub's file back; the fence is for ops")]
    fn the_store_round_trips_through_its_file_and_reports_conflicts() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::open(Some(dir.path())).unwrap_or_else(|_| unreachable!());
        let op = BindingAction::op("expression.set", serde_json::json!({ "name": "bleh", "fade": null }));
        let a = store.add("bleh", DeviceClass::Keyboard, &op, Some(key(0x42))).unwrap_or_else(|_| unreachable!());
        let b = store.add("wave", DeviceClass::Keyboard, &BindingAction::Trigger(3), Some(key(0x42)));
        let b = b.unwrap_or_else(|_| unreachable!());
        assert_eq!(store.conflicts(a), [b]);
        let again = Store::open(Some(dir.path())).unwrap_or_else(|_| unreachable!());
        assert_eq!(again.infos(), store.infos());
        assert!(again.infos().iter().all(|i| i.conflict));
        assert_eq!(again.entries().first().map(|e| e.action.clone()), Some(op));
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap_or_default();
        assert!(!text.contains("F9"), "the file holds the chord's usage, never a label");
        again.remove(a).unwrap_or_else(|_| unreachable!());
        assert_eq!(again.remove(a), Err(InputError::UnknownBinding(a)));
    }
}
