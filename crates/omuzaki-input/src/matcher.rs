//! Chords and matching (design §5). A chord is a key with its modifiers, a pad's buttons or a MIDI control; it never
//! leaves the crate, and outside it a binding is only its id and action. Matching runs on the capture thread, after the zone.

use omuzaki_input_api::binding::ChordLabel;
use serde::{Deserialize, Serialize};

use crate::normalise::{HidUsage, MidiControl};
use crate::normalise::zone_map;
use crate::shared::Table;

/// Modifier bits: Ctrl, Shift, Alt, Super (left and right alike).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Mods(pub(crate) u8);

impl Mods {
    pub(crate) const CTRL: u8 = 1;
    pub(crate) const SHIFT: u8 = 2;
    pub(crate) const ALT: u8 = 4;
    pub(crate) const SUPER: u8 = 8;

    /// The modifier bit a key is, if it is a modifier.
    pub(crate) fn of(usage: HidUsage) -> u8 {
        match usage.0 {
            0xE0 | 0xE4 => Self::CTRL,
            0xE1 | 0xE5 => Self::SHIFT,
            0xE2 | 0xE6 => Self::ALT,
            0xE3 | 0xE7 => Self::SUPER,
            _ => 0,
        }
    }
}

/// A chord: a key with exactly these modifiers, or a set of pad buttons.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub(crate) enum Chord {
    /// A key, with the modifiers held when it went down (not counting itself).
    Key {
        /// The key.
        usage: HidUsage,
        /// The modifiers.
        mods: Mods,
    },
    /// Pad buttons, all held.
    Pad {
        /// The buttons, one bit each (the gamepad backend's numbering).
        buttons: u32,
    },
    /// A MIDI note, controller or program change, on any controller.
    Midi {
        /// The control.
        control: MidiControl,
    },
}

/// No key value in a debug print: a chord is a key identity (design §3 rule 3).
impl std::fmt::Debug for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<chord>")
    }
}

impl Chord {
    /// Its display text, for the binding editor only.
    pub(crate) fn label(self) -> ChordLabel {
        match self {
            Self::Key { usage, mods } => {
                let mut text = String::new();
                for (bit, name) in [(Mods::CTRL, "Ctrl"), (Mods::SHIFT, "Shift"), (Mods::ALT, "Alt"), (Mods::SUPER, "Super")]
                {
                    if mods.0 & bit != 0 {
                        text.push_str(name);
                        text.push('+');
                    }
                }
                text.push_str(zone_map::label(usage));
                ChordLabel::new(text)
            }
            Self::Pad { buttons } => {
                let names: Vec<String> = (0..32).filter(|b| buttons & (1 << b) != 0).map(|b| format!("Pad {b}")).collect();
                ChordLabel::new(names.join("+"))
            }
            Self::Midi { control } => {
                let channel = control.channel() + 1;
                let number = control.number();
                ChordLabel::new(match control.kind() {
                    MidiControl::NOTE => {
                        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
                        let name = NAMES.get(usize::from(number % 12)).copied().unwrap_or("C");
                        let octave = i32::from(number / 12) - 1;
                        format!("MIDI note {name}{octave}, channel {channel}")
                    }
                    MidiControl::CONTROLLER => format!("MIDI controller {number}, channel {channel}"),
                    _ => format!("MIDI program {}, channel {channel}", u16::from(number) + 1),
                })
            }
        }
    }
}

/// The slot of the binding a key press matches: the exact chord (key and modifiers) first, else the key with no
/// modifiers, so a held Shift (run) does not stop W (walk). `None` when no binding has the key.
pub(crate) fn match_key(table: &Table, usage: HidUsage, mods: Mods) -> Option<usize> {
    let find = |want: Mods| {
        table.entries.iter().position(|e| e.chord == Some(Chord::Key { usage, mods: want }))
    };
    find(mods).or_else(|| if mods.0 == 0 { None } else { find(Mods(0)) })
}

/// The slot of the binding a MIDI control matches.
pub(crate) fn match_midi(table: &Table, control: MidiControl) -> Option<usize> {
    table.entries.iter().position(|e| e.chord == Some(Chord::Midi { control }))
}

/// The slot of the binding a set of pad buttons matches exactly.
pub(crate) fn match_pad(table: &Table, buttons: u32) -> Option<usize> {
    table.entries.iter().position(|e| e.chord == Some(Chord::Pad { buttons }))
}

#[cfg(test)]
mod tests {
    use omuzaki_input_api::binding::{BindingAction, BindingId};

    use super::*;
    use crate::shared::TableEntry;

    fn table(chords: &[Chord]) -> Table {
        let entries = chords
            .iter()
            .enumerate()
            .map(|(i, c)| TableEntry {
                id: BindingId(u32::try_from(i).unwrap_or(0) + 1),
                name: format!("binding {i}"),
                chord: Some(*c),
                action: BindingAction::Trigger(0),
            })
            .collect();
        Table { generation: 1, entries }
    }

    #[test]
    fn chords_match_with_their_modifiers_and_a_bare_key_matches_under_any() {
        let f9 = HidUsage(0x42);
        let w = HidUsage(0x1A);
        let t = table(&[
            Chord::Key { usage: f9, mods: Mods(Mods::CTRL) },
            Chord::Key { usage: f9, mods: Mods(Mods::CTRL | Mods::SHIFT) },
            Chord::Key { usage: w, mods: Mods(0) },
        ]);
        assert_eq!(match_key(&t, f9, Mods(Mods::CTRL)), Some(0));
        assert_eq!(match_key(&t, f9, Mods(Mods::CTRL | Mods::SHIFT)), Some(1));
        assert_eq!(match_key(&t, f9, Mods(0)), None, "F9 alone is not Ctrl+F9");
        assert_eq!(match_key(&t, w, Mods(Mods::SHIFT)), Some(2), "walk while running");
        assert_eq!(match_key(&t, HidUsage(0x1B), Mods(0)), None);
        assert_eq!(match_pad(&table(&[Chord::Pad { buttons: 0b11 }]), 0b11), Some(0));
        let note = MidiControl::new(MidiControl::NOTE, 0, 60);
        let t = table(&[Chord::Midi { control: MidiControl::new(MidiControl::CONTROLLER, 0, 60) }, Chord::Midi { control: note }]);
        assert_eq!(match_midi(&t, note), Some(1), "a note is not the controller with its number");
        assert_eq!(match_midi(&t, MidiControl::new(MidiControl::NOTE, 1, 60)), None, "nor the note on another channel");
    }

    #[test]
    fn a_label_names_the_chord_and_a_debug_print_does_not() {
        let chord = Chord::Key { usage: HidUsage(0x42), mods: Mods(Mods::CTRL | Mods::SHIFT) };
        let mut text = String::new();
        chord.label().render(&mut |t| text.push_str(t));
        assert_eq!(text, "Ctrl+Shift+F9");
        let label = |control| {
            let mut text = String::new();
            Chord::Midi { control }.label().render(&mut |t| text.push_str(t));
            text
        };
        assert_eq!(label(MidiControl::new(MidiControl::NOTE, 0, 60)), "MIDI note C4, channel 1");
        assert_eq!(label(MidiControl::new(MidiControl::NOTE, 9, 37)), "MIDI note C#2, channel 10");
        assert_eq!(label(MidiControl::new(MidiControl::CONTROLLER, 15, 20)), "MIDI controller 20, channel 16");
        assert_eq!(label(MidiControl::new(MidiControl::PROGRAM, 0, 0)), "MIDI program 1, channel 1");
        assert_eq!(format!("{chord:?}"), "<chord>");
        assert_eq!(Mods::of(HidUsage(0xE5)), Mods::SHIFT);
    }
}
