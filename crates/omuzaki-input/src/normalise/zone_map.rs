//! The map from a key (its USB HID keyboard-page usage, the physical position) to its zone and its label. Key
//! identities live here and in the rest of `normalise` only (design §3, D2): nothing outside this module names a usage.
//!
//! Zones are by physical position, so AZERTY, QWERTZ and JIS hands land where the fingers are and the table needs no
//! layout setting. The label is the key's US-layout name, used only for a `ChordLabel` in the binding editor.

use omuzaki_input_api::zones::KeyZone;

use super::HidUsage;

/// Every key the boards have, sorted by usage: the usage, its zone and its label.
const KEYS: [(u16, KeyZone, &str); 111] = [
    (0x04, KeyZone::LEFT_OUTER_LOWER, "A"),
    (0x05, KeyZone::LEFT_INNER_LOWER, "B"),
    (0x06, KeyZone::LEFT_OUTER_LOWER, "C"),
    (0x07, KeyZone::LEFT_OUTER_LOWER, "D"),
    (0x08, KeyZone::LEFT_OUTER_UPPER, "E"),
    (0x09, KeyZone::LEFT_INNER_LOWER, "F"),
    (0x0A, KeyZone::LEFT_INNER_LOWER, "G"),
    (0x0B, KeyZone::RIGHT_INNER_LOWER, "H"),
    (0x0C, KeyZone::RIGHT_OUTER_UPPER, "I"),
    (0x0D, KeyZone::RIGHT_INNER_LOWER, "J"),
    (0x0E, KeyZone::RIGHT_OUTER_LOWER, "K"),
    (0x0F, KeyZone::RIGHT_OUTER_LOWER, "L"),
    (0x10, KeyZone::RIGHT_INNER_LOWER, "M"),
    (0x11, KeyZone::RIGHT_INNER_LOWER, "N"),
    (0x12, KeyZone::RIGHT_OUTER_UPPER, "O"),
    (0x13, KeyZone::RIGHT_OUTER_UPPER, "P"),
    (0x14, KeyZone::LEFT_OUTER_UPPER, "Q"),
    (0x15, KeyZone::LEFT_INNER_UPPER, "R"),
    (0x16, KeyZone::LEFT_OUTER_LOWER, "S"),
    (0x17, KeyZone::LEFT_INNER_UPPER, "T"),
    (0x18, KeyZone::RIGHT_INNER_UPPER, "U"),
    (0x19, KeyZone::LEFT_INNER_LOWER, "V"),
    (0x1A, KeyZone::LEFT_OUTER_UPPER, "W"),
    (0x1B, KeyZone::LEFT_OUTER_LOWER, "X"),
    (0x1C, KeyZone::RIGHT_INNER_UPPER, "Y"),
    (0x1D, KeyZone::LEFT_OUTER_LOWER, "Z"),
    (0x1E, KeyZone::LEFT_OUTER_UPPER, "1"),
    (0x1F, KeyZone::LEFT_OUTER_UPPER, "2"),
    (0x20, KeyZone::LEFT_OUTER_UPPER, "3"),
    (0x21, KeyZone::LEFT_INNER_UPPER, "4"),
    (0x22, KeyZone::LEFT_INNER_UPPER, "5"),
    (0x23, KeyZone::RIGHT_INNER_UPPER, "6"),
    (0x24, KeyZone::RIGHT_INNER_UPPER, "7"),
    (0x25, KeyZone::RIGHT_OUTER_UPPER, "8"),
    (0x26, KeyZone::RIGHT_OUTER_UPPER, "9"),
    (0x27, KeyZone::RIGHT_OUTER_UPPER, "0"),
    (0x28, KeyZone::RIGHT_OUTER_LOWER, "Enter"),
    (0x29, KeyZone::LEFT_OUTER_UPPER, "Esc"),
    (0x2A, KeyZone::RIGHT_OUTER_UPPER, "Backspace"),
    (0x2B, KeyZone::LEFT_OUTER_UPPER, "Tab"),
    (0x2C, KeyZone::SPACE, "Space"),
    (0x2D, KeyZone::RIGHT_OUTER_UPPER, "-"),
    (0x2E, KeyZone::RIGHT_OUTER_UPPER, "="),
    (0x2F, KeyZone::RIGHT_OUTER_UPPER, "["),
    (0x30, KeyZone::RIGHT_OUTER_UPPER, "]"),
    (0x31, KeyZone::RIGHT_OUTER_UPPER, "\\"),
    (0x32, KeyZone::RIGHT_OUTER_LOWER, "#"),
    (0x33, KeyZone::RIGHT_OUTER_LOWER, ";"),
    (0x34, KeyZone::RIGHT_OUTER_LOWER, "'"),
    (0x35, KeyZone::LEFT_OUTER_UPPER, "`"),
    (0x36, KeyZone::RIGHT_OUTER_LOWER, ","),
    (0x37, KeyZone::RIGHT_OUTER_LOWER, "."),
    (0x38, KeyZone::RIGHT_OUTER_LOWER, "/"),
    (0x39, KeyZone::LEFT_OUTER_LOWER, "Caps Lock"),
    (0x3A, KeyZone::FUNCTION, "F1"),
    (0x3B, KeyZone::FUNCTION, "F2"),
    (0x3C, KeyZone::FUNCTION, "F3"),
    (0x3D, KeyZone::FUNCTION, "F4"),
    (0x3E, KeyZone::FUNCTION, "F5"),
    (0x3F, KeyZone::FUNCTION, "F6"),
    (0x40, KeyZone::FUNCTION, "F7"),
    (0x41, KeyZone::FUNCTION, "F8"),
    (0x42, KeyZone::FUNCTION, "F9"),
    (0x43, KeyZone::FUNCTION, "F10"),
    (0x44, KeyZone::FUNCTION, "F11"),
    (0x45, KeyZone::FUNCTION, "F12"),
    (0x46, KeyZone::NAVIGATION, "Print Screen"),
    (0x47, KeyZone::NAVIGATION, "Scroll Lock"),
    (0x48, KeyZone::NAVIGATION, "Pause"),
    (0x49, KeyZone::NAVIGATION, "Insert"),
    (0x4A, KeyZone::NAVIGATION, "Home"),
    (0x4B, KeyZone::NAVIGATION, "Page Up"),
    (0x4C, KeyZone::NAVIGATION, "Delete"),
    (0x4D, KeyZone::NAVIGATION, "End"),
    (0x4E, KeyZone::NAVIGATION, "Page Down"),
    (0x4F, KeyZone::ARROWS, "Right"),
    (0x50, KeyZone::ARROWS, "Left"),
    (0x51, KeyZone::ARROWS, "Down"),
    (0x52, KeyZone::ARROWS, "Up"),
    (0x53, KeyZone::NUMPAD, "Num Lock"),
    (0x54, KeyZone::NUMPAD, "Num /"),
    (0x55, KeyZone::NUMPAD, "Num *"),
    (0x56, KeyZone::NUMPAD, "Num -"),
    (0x57, KeyZone::NUMPAD, "Num +"),
    (0x58, KeyZone::NUMPAD, "Num Enter"),
    (0x59, KeyZone::NUMPAD, "Num 1"),
    (0x5A, KeyZone::NUMPAD, "Num 2"),
    (0x5B, KeyZone::NUMPAD, "Num 3"),
    (0x5C, KeyZone::NUMPAD, "Num 4"),
    (0x5D, KeyZone::NUMPAD, "Num 5"),
    (0x5E, KeyZone::NUMPAD, "Num 6"),
    (0x5F, KeyZone::NUMPAD, "Num 7"),
    (0x60, KeyZone::NUMPAD, "Num 8"),
    (0x61, KeyZone::NUMPAD, "Num 9"),
    (0x62, KeyZone::NUMPAD, "Num 0"),
    (0x63, KeyZone::NUMPAD, "Num ."),
    (0x64, KeyZone::LEFT_OUTER_LOWER, "\\ (ISO)"),
    (0x65, KeyZone::RIGHT_MODIFIERS, "Menu"),
    (0x87, KeyZone::RIGHT_OUTER_LOWER, "Ro"),
    (0x88, KeyZone::RIGHT_MODIFIERS, "Kana"),
    (0x89, KeyZone::RIGHT_OUTER_UPPER, "Yen"),
    (0x8A, KeyZone::SPACE, "Henkan"),
    (0x8B, KeyZone::SPACE, "Muhenkan"),
    (0xE0, KeyZone::LEFT_MODIFIERS, "Left Ctrl"),
    (0xE1, KeyZone::LEFT_OUTER_LOWER, "Left Shift"),
    (0xE2, KeyZone::LEFT_MODIFIERS, "Left Alt"),
    (0xE3, KeyZone::LEFT_MODIFIERS, "Left Super"),
    (0xE4, KeyZone::RIGHT_MODIFIERS, "Right Ctrl"),
    (0xE5, KeyZone::RIGHT_OUTER_LOWER, "Right Shift"),
    (0xE6, KeyZone::RIGHT_MODIFIERS, "Right Alt"),
    (0xE7, KeyZone::RIGHT_MODIFIERS, "Right Super"),
];

/// The zone of a key: any key not on the boards' list is "other".
pub(crate) fn zone(usage: HidUsage) -> KeyZone {
    KEYS.binary_search_by_key(&usage.0, |(u, _, _)| *u).ok().and_then(|i| KEYS.get(i)).map_or(KeyZone::OTHER, |k| k.1)
}

/// The label of a key, for a chord's display text.
pub(crate) fn label(usage: HidUsage) -> &'static str {
    KEYS.binary_search_by_key(&usage.0, |(u, _, _)| *u).ok().and_then(|i| KEYS.get(i)).map_or("Key", |k| k.2)
}

/// The keys of `zone`, for the `test-hooks` zone-equivalence test.
#[cfg(feature = "test-hooks")]
pub(crate) fn in_zone(zone: KeyZone) -> Vec<u16> {
    KEYS.iter().filter(|k| k.1 == zone).map(|k| k.0).collect()
}

/// Whether the table lists `usage` (it maps to its own zone, not the "other" fallback).
#[cfg(test)]
pub(crate) fn listed(usage: HidUsage) -> bool {
    KEYS.binary_search_by_key(&usage.0, |(u, _, _)| *u).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ANSI board's 104 keys.
    fn ansi() -> Vec<u16> {
        let mut keys: Vec<u16> = (0x04..=0x31).collect(); // letters, digits, Enter to backslash
        keys.extend(0x33..=0x65); // ; to Menu (with ISO's 0x64 removed below)
        keys.retain(|u| *u != 0x64);
        keys.extend(0xE0..=0xE7);
        keys
    }

    /// The ISO board's 105: ANSI's backslash moves to 0x32, plus the key beside left Shift.
    fn iso() -> Vec<u16> {
        let mut keys: Vec<u16> = ansi().into_iter().filter(|u| *u != 0x31).collect();
        keys.extend([0x32, 0x64]);
        keys
    }

    /// The JIS board's 109: ANSI's backslash moves to 0x32, plus Ro, Kana, Yen, Henkan and Muhenkan.
    fn jis() -> Vec<u16> {
        let mut keys: Vec<u16> = ansi().into_iter().filter(|u| *u != 0x31).collect();
        keys.extend([0x32, 0x87, 0x88, 0x89, 0x8A, 0x8B]);
        keys
    }

    #[test]
    fn every_key_of_the_ansi_iso_and_jis_boards_has_its_own_zone() {
        for (board, keys, count) in [("ANSI", ansi(), 104), ("ISO", iso(), 105), ("JIS", jis(), 109)] {
            let mut unique = keys.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!((unique.len(), keys.len()), (count, count), "{board} has {count} distinct keys");
            for usage in keys {
                assert!(listed(HidUsage(usage)), "{board}: usage {usage:#04x} is listed");
            }
        }
    }

    #[test]
    fn the_zones_follow_the_design_s_grid() {
        let z = |u: u16| zone(HidUsage(u));
        assert_eq!(z(0x14), KeyZone::LEFT_OUTER_UPPER, "Q");
        assert_eq!(z(0x09), KeyZone::LEFT_INNER_LOWER, "F");
        assert_eq!(z(0x0D), KeyZone::RIGHT_INNER_LOWER, "J");
        assert_eq!(z(0x2A), KeyZone::RIGHT_OUTER_UPPER, "Backspace");
        assert_eq!(z(0xE5), KeyZone::RIGHT_OUTER_LOWER, "right Shift");
        assert_eq!(z(0x2C), KeyZone::SPACE);
        assert_eq!(z(0xE0), KeyZone::LEFT_MODIFIERS);
        assert_eq!(z(0x42), KeyZone::FUNCTION, "F9");
        assert_eq!(z(0x52), KeyZone::ARROWS);
        assert_eq!(z(0x62), KeyZone::NUMPAD);
        assert_eq!(z(0x80), KeyZone::OTHER, "volume up is not on the boards");
        assert!(KEYS.windows(2).all(|w| w[0].0 < w[1].0), "sorted, so the search works");
        let used: std::collections::BTreeSet<u8> = KEYS.iter().map(|k| k.1.index()).collect();
        assert_eq!(used.len(), 15, "every zone but \"other\" has keys");
    }
}
