//! Linux key codes (the kernel's `KEY_*`, which X11's keycodes are plus 8) to USB HID usages: the inverse of the
//! kernel's own HID table (`drivers/hid/hid-input.c`) for every key the zone map knows. The X11 and evdev backends
//! hand a code here and get a usage back, so no backend names a key.

use super::{HidUsage, RawEvent};

/// `(Linux key code, HID usage)`, sorted by code. The ISO key beside Enter (HID 0x32) has no code of its own: the
/// kernel reports it as the backslash key, so it reads as 0x31.
const CODES: [(u16, u16); 111] = [
    (1, 0x29),
    (2, 0x1E),
    (3, 0x1F),
    (4, 0x20),
    (5, 0x21),
    (6, 0x22),
    (7, 0x23),
    (8, 0x24),
    (9, 0x25),
    (10, 0x26),
    (11, 0x27),
    (12, 0x2D),
    (13, 0x2E),
    (14, 0x2A),
    (15, 0x2B),
    (16, 0x14),
    (17, 0x1A),
    (18, 0x08),
    (19, 0x15),
    (20, 0x17),
    (21, 0x1C),
    (22, 0x18),
    (23, 0x0C),
    (24, 0x12),
    (25, 0x13),
    (26, 0x2F),
    (27, 0x30),
    (28, 0x28),
    (29, 0xE0),
    (30, 0x04),
    (31, 0x16),
    (32, 0x07),
    (33, 0x09),
    (34, 0x0A),
    (35, 0x0B),
    (36, 0x0D),
    (37, 0x0E),
    (38, 0x0F),
    (39, 0x33),
    (40, 0x34),
    (41, 0x35),
    (42, 0xE1),
    (43, 0x31),
    (44, 0x1D),
    (45, 0x1B),
    (46, 0x06),
    (47, 0x19),
    (48, 0x05),
    (49, 0x11),
    (50, 0x10),
    (51, 0x36),
    (52, 0x37),
    (53, 0x38),
    (54, 0xE5),
    (55, 0x55),
    (56, 0xE2),
    (57, 0x2C),
    (58, 0x39),
    (59, 0x3A),
    (60, 0x3B),
    (61, 0x3C),
    (62, 0x3D),
    (63, 0x3E),
    (64, 0x3F),
    (65, 0x40),
    (66, 0x41),
    (67, 0x42),
    (68, 0x43),
    (69, 0x53),
    (70, 0x47),
    (71, 0x5F),
    (72, 0x60),
    (73, 0x61),
    (74, 0x56),
    (75, 0x5C),
    (76, 0x5D),
    (77, 0x5E),
    (78, 0x57),
    (79, 0x59),
    (80, 0x5A),
    (81, 0x5B),
    (82, 0x62),
    (83, 0x63),
    (85, 0x35),
    (86, 0x64),
    (87, 0x44),
    (88, 0x45),
    (89, 0x87),
    (92, 0x8A),
    (93, 0x88),
    (94, 0x8B),
    (96, 0x58),
    (97, 0xE4),
    (98, 0x54),
    (99, 0x46),
    (100, 0xE6),
    (102, 0x4A),
    (103, 0x52),
    (104, 0x4B),
    (105, 0x50),
    (106, 0x4F),
    (107, 0x4D),
    (108, 0x51),
    (109, 0x4E),
    (110, 0x49),
    (111, 0x4C),
    (119, 0x48),
    (124, 0x89),
    (125, 0xE3),
    (126, 0xE7),
    (127, 0x65),
];

/// The usage of Linux key code `code`, or `None` for a key the zone map does not know.
pub(crate) fn usage(code: u16) -> Option<HidUsage> {
    CODES.binary_search_by_key(&code, |(c, _)| *c).ok().and_then(|i| CODES.get(i)).map(|(_, u)| HidUsage(*u))
}

/// A key event from Linux key code `code` on `device`, or `None` for a key the zone map does not know (it is not read
/// at all).
pub(crate) fn key(device: u64, code: u16, down: bool, repeat: bool) -> Option<RawEvent> {
    usage(code).map(|usage| RawEvent::Key { device, usage, down, repeat })
}

#[cfg(test)]
mod tests {
    use super::super::zone_map;
    use super::*;

    #[test]
    fn every_key_the_zone_map_knows_has_a_linux_code_and_no_code_is_listed_twice() {
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
        // KEY_A, KEY_W, KEY_F9 and KEY_LEFTCTRL.
        assert_eq!(usage(30), Some(HidUsage(0x04)));
        assert_eq!(usage(17), Some(HidUsage(0x1A)));
        assert_eq!(usage(67), Some(HidUsage(0x42)));
        assert_eq!(usage(29), Some(HidUsage(0xE0)));
        assert_eq!(usage(0), None);
        assert_eq!(usage(240), None);
    }
}
