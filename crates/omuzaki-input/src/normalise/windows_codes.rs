//! Windows Raw Input scan codes (set 1, with the `E0` prefix as `0xE000`) to USB HID usages, for every key the zone map
//! knows: Microsoft's own HID to PS/2 translation table, inverted. The Windows backend hands a code here and gets a usage
//! back, so no backend names a key. Not gated to Windows, so its coverage test runs everywhere.

use super::{HidUsage, RawEvent};

/// The `E0` prefix of an extended key, as the high byte of a code here.
pub(crate) const EXTENDED: u16 = 0xE000;

/// The code the Pause key reads as. Raw Input sends it as an `E1` sequence whose first record carries `VK_PAUSE`; this
/// table files it under `Ctrl+Break`'s `E0 46`, the same key.
pub(crate) const PAUSE: u16 = EXTENDED | 0x46;

/// `(scan code, HID usage)`, sorted by code. The ISO key beside Enter (HID 0x32) sends the backslash key's code (0x2B),
/// so it reads as 0x31, as on Linux. `E0 2A` and `E0 36`, the shifts Windows fakes around the navigation keys, are not
/// keys and are not listed.
const CODES: [(u16, u16); 112] = [
    (0x01, 0x29),
    (0x02, 0x1E),
    (0x03, 0x1F),
    (0x04, 0x20),
    (0x05, 0x21),
    (0x06, 0x22),
    (0x07, 0x23),
    (0x08, 0x24),
    (0x09, 0x25),
    (0x0A, 0x26),
    (0x0B, 0x27),
    (0x0C, 0x2D),
    (0x0D, 0x2E),
    (0x0E, 0x2A),
    (0x0F, 0x2B),
    (0x10, 0x14),
    (0x11, 0x1A),
    (0x12, 0x08),
    (0x13, 0x15),
    (0x14, 0x17),
    (0x15, 0x1C),
    (0x16, 0x18),
    (0x17, 0x0C),
    (0x18, 0x12),
    (0x19, 0x13),
    (0x1A, 0x2F),
    (0x1B, 0x30),
    (0x1C, 0x28),
    (0x1D, 0xE0),
    (0x1E, 0x04),
    (0x1F, 0x16),
    (0x20, 0x07),
    (0x21, 0x09),
    (0x22, 0x0A),
    (0x23, 0x0B),
    (0x24, 0x0D),
    (0x25, 0x0E),
    (0x26, 0x0F),
    (0x27, 0x33),
    (0x28, 0x34),
    (0x29, 0x35),
    (0x2A, 0xE1),
    (0x2B, 0x31),
    (0x2C, 0x1D),
    (0x2D, 0x1B),
    (0x2E, 0x06),
    (0x2F, 0x19),
    (0x30, 0x05),
    (0x31, 0x11),
    (0x32, 0x10),
    (0x33, 0x36),
    (0x34, 0x37),
    (0x35, 0x38),
    (0x36, 0xE5),
    (0x37, 0x55),
    (0x38, 0xE2),
    (0x39, 0x2C),
    (0x3A, 0x39),
    (0x3B, 0x3A),
    (0x3C, 0x3B),
    (0x3D, 0x3C),
    (0x3E, 0x3D),
    (0x3F, 0x3E),
    (0x40, 0x3F),
    (0x41, 0x40),
    (0x42, 0x41),
    (0x43, 0x42),
    (0x44, 0x43),
    (0x45, 0x53),
    (0x46, 0x47),
    (0x47, 0x5F),
    (0x48, 0x60),
    (0x49, 0x61),
    (0x4A, 0x56),
    (0x4B, 0x5C),
    (0x4C, 0x5D),
    (0x4D, 0x5E),
    (0x4E, 0x57),
    (0x4F, 0x59),
    (0x50, 0x5A),
    (0x51, 0x5B),
    (0x52, 0x62),
    (0x53, 0x63),
    (0x54, 0x46),
    (0x56, 0x64),
    (0x57, 0x44),
    (0x58, 0x45),
    (0x70, 0x88),
    (0x73, 0x87),
    (0x79, 0x8A),
    (0x7B, 0x8B),
    (0x7D, 0x89),
    (0xE01C, 0x58),
    (0xE01D, 0xE4),
    (0xE035, 0x54),
    (0xE037, 0x46),
    (0xE038, 0xE6),
    (0xE045, 0x53),
    (0xE046, 0x48),
    (0xE047, 0x4A),
    (0xE048, 0x52),
    (0xE049, 0x4B),
    (0xE04B, 0x50),
    (0xE04D, 0x4F),
    (0xE04F, 0x4D),
    (0xE050, 0x51),
    (0xE051, 0x4E),
    (0xE052, 0x49),
    (0xE053, 0x4C),
    (0xE05B, 0xE3),
    (0xE05C, 0xE7),
    (0xE05D, 0x65),
];

/// The usage of scan code `code`, or `None` for a key the zone map does not know.
pub(crate) fn usage(code: u16) -> Option<HidUsage> {
    CODES.binary_search_by_key(&code, |(c, _)| *c).ok().and_then(|i| CODES.get(i)).map(|(_, u)| HidUsage(*u))
}

/// A key event from scan code `code` on `device`, or `None` for a key the zone map does not know (it is not read at
/// all). Raw Input does not mark auto-repeats; the sink reads a down on a key already down as one.
pub(crate) fn key(device: u64, code: u16, down: bool) -> Option<RawEvent> {
    usage(code).map(|usage| RawEvent::Key { device, usage, down, repeat: false })
}

#[cfg(test)]
mod tests {
    use super::super::zone_map;
    use super::*;

    #[test]
    fn every_key_the_zone_map_knows_has_a_scan_code_and_no_code_is_listed_twice() {
        assert!(CODES.windows(2).all(|w| w[0].0 < w[1].0), "sorted, each code once");
        assert!(CODES.iter().all(|(_, u)| zone_map::listed(HidUsage(*u))), "every usage is a known key");
        let missing: Vec<u16> = (0..=0xFFu16)
            .filter(|u| zone_map::listed(HidUsage(*u)) && *u != 0x32)
            .filter(|u| !CODES.iter().any(|(_, usage)| usage == u))
            .collect();
        assert!(missing.is_empty(), "{} keys have no code", missing.len());
    }

    #[test]
    fn known_codes_land_on_their_keys() {
        // A, W, F9, the left Ctrl, the right Ctrl (extended), the up arrow and Pause.
        assert_eq!(usage(0x1E), Some(HidUsage(0x04)));
        assert_eq!(usage(0x11), Some(HidUsage(0x1A)));
        assert_eq!(usage(0x43), Some(HidUsage(0x42)));
        assert_eq!(usage(0x1D), Some(HidUsage(0xE0)));
        assert_eq!(usage(EXTENDED | 0x1D), Some(HidUsage(0xE4)));
        assert_eq!(usage(EXTENDED | 0x48), Some(HidUsage(0x52)));
        assert_eq!(usage(PAUSE), Some(HidUsage(0x48)));
        // The faked shift is no key.
        assert_eq!(usage(EXTENDED | 0x2A), None);
        assert_eq!(usage(0), None);
    }
}
