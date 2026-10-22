//! The macOS keyboard and mouse backend (design §13 IN-7, research §1.2 and §7): a listen-only `CGEventTap` on the
//! capture thread's own run loop. A listen-only tap observes and never changes or holds an event, and needs the Input
//! Monitoring permission (not Accessibility); until it is granted the backend reports
//! `NeedsPermission { how: InputMonitoring }` and asks macOS to show its prompt once, when a person first starts
//! capture. A tap macOS disables (a slow callback, or user input) is enabled again at once.
//!
//! The tap exists only while capture reads, and listens only for the classes asked for; while capture is paused or off
//! there is no tap, so nothing is read. While a password field holds secure input, macOS sends a tap no keys at all.
//! A tap does not say which keyboard or mouse an event came from, so on macOS the keyboard is one source and the mouse
//! another. Pads are the gamepad backend's; the pen is IN-10's.
//!
//! This file holds what does not need macOS (the event translation), so its tests run everywhere; `tap` is the
//! Core Graphics side.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "the tap calls these; their tests run everywhere"))]

#[cfg(target_os = "macos")]
pub(crate) mod tap;

use omuzaki_input_api::event::DeviceClass;

use crate::normalise::{RawEvent, macos_codes};

/// The handles the backend files the keyboard and the mouse under.
pub(crate) const KEYBOARD: u64 = 1;
pub(crate) const MOUSE: u64 = 2;

/// The `CGEventType`s the backend reads.
pub(crate) mod kind {
    pub(crate) const LEFT_DOWN: u32 = 1;
    pub(crate) const LEFT_UP: u32 = 2;
    pub(crate) const RIGHT_DOWN: u32 = 3;
    pub(crate) const RIGHT_UP: u32 = 4;
    pub(crate) const MOVED: u32 = 5;
    pub(crate) const LEFT_DRAGGED: u32 = 6;
    pub(crate) const RIGHT_DRAGGED: u32 = 7;
    pub(crate) const KEY_DOWN: u32 = 10;
    pub(crate) const KEY_UP: u32 = 11;
    pub(crate) const FLAGS_CHANGED: u32 = 12;
    pub(crate) const SCROLL: u32 = 22;
    pub(crate) const OTHER_DOWN: u32 = 25;
    pub(crate) const OTHER_UP: u32 = 26;
    pub(crate) const OTHER_DRAGGED: u32 = 27;
    /// The keyboard's event types.
    pub(crate) const KEYBOARD: [u32; 3] = [KEY_DOWN, KEY_UP, FLAGS_CHANGED];
    /// The mouse's.
    pub(crate) const MOUSE: [u32; 11] = [
        LEFT_DOWN,
        LEFT_UP,
        RIGHT_DOWN,
        RIGHT_UP,
        MOVED,
        LEFT_DRAGGED,
        RIGHT_DRAGGED,
        SCROLL,
        OTHER_DOWN,
        OTHER_UP,
        OTHER_DRAGGED,
    ];
}

/// `kVK_CapsLock`.
const CAPS_LOCK: u16 = 0x39;

/// What the tap's callback copies out of an event: its type and the fields the backend reads. Plain values, so the
/// callback stays tiny and the translation runs after the run loop returns.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Fields {
    pub(crate) kind: u32,
    /// The virtual key code, and whether macOS marks the key down as an auto-repeat.
    pub(crate) keycode: u16,
    pub(crate) repeat: bool,
    /// The event's flags (for a modifier's down or up).
    pub(crate) flags: u64,
    /// The mouse button number (0 left, 1 right, 2 middle, 3 back, 4 forward).
    pub(crate) button: i64,
    /// Relative motion in device counts, right and down positive.
    pub(crate) delta: [i64; 2],
    /// Scroll in lines, away from the person positive.
    pub(crate) scroll: f64,
}

/// The classes the tap listens for, as a `CGEventMask`.
pub(crate) fn mask(keyboard: bool, mouse: bool) -> u64 {
    let bits = |kinds: &[u32]| kinds.iter().fold(0u64, |m, k| m | 1 << k);
    (if keyboard { bits(&kind::KEYBOARD) } else { 0 }) | (if mouse { bits(&kind::MOUSE) } else { 0 })
}

/// The device-dependent flag of a modifier key: its own bit in the event's flags while it is held.
fn modifier_bit(keycode: u16) -> Option<u64> {
    match keycode {
        0x3B => Some(0x1),    // left Control
        0x38 => Some(0x2),    // left Shift
        0x3C => Some(0x4),    // right Shift
        0x37 => Some(0x8),    // left Command
        0x36 => Some(0x10),   // right Command
        0x3A => Some(0x20),   // left Option
        0x3D => Some(0x40),   // right Option
        0x3E => Some(0x2000), // right Control
        _ => None,
    }
}

/// The raw events of one tapped event.
pub(crate) fn translate(f: &Fields, mut out: impl FnMut(RawEvent)) {
    let clamp = |v: i64| i32::try_from(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0);
    let button = |pressed: bool, bit: u8| {
        let (pressed, released) = if pressed { (bit, 0) } else { (0, bit) };
        RawEvent::Buttons { device: MOUSE, class: DeviceClass::Mouse, pressed, released }
    };
    match f.kind {
        kind::KEY_DOWN | kind::KEY_UP => {
            if let Some(raw) = macos_codes::key(KEYBOARD, f.keycode, f.kind == kind::KEY_DOWN, f.repeat) {
                out(raw);
            }
        }
        kind::FLAGS_CHANGED => {
            if f.keycode == CAPS_LOCK {
                // macOS reports Caps Lock's toggled state, not the key: each change is one press.
                for down in [true, false] {
                    if let Some(raw) = macos_codes::key(KEYBOARD, CAPS_LOCK, down, false) {
                        out(raw);
                    }
                }
            } else if let Some(bit) = modifier_bit(f.keycode)
                && let Some(raw) = macos_codes::key(KEYBOARD, f.keycode, f.flags & bit != 0, false)
            {
                out(raw);
            }
        }
        kind::MOVED | kind::LEFT_DRAGGED | kind::RIGHT_DRAGGED | kind::OTHER_DRAGGED => {
            let [dx, dy] = f.delta.map(clamp);
            if dx != 0 || dy != 0 {
                out(RawEvent::Motion { device: MOUSE, dx, dy, wheel: 0.0 });
            }
        }
        kind::SCROLL => {
            // Lines as notches; a trackpad's fractions add up in the sink.
            #[allow(clippy::cast_possible_truncation, reason = "a scroll of a few lines")]
            let wheel = f.scroll as f32;
            if wheel != 0.0 {
                out(RawEvent::Motion { device: MOUSE, dx: 0, dy: 0, wheel });
            }
        }
        kind::LEFT_DOWN | kind::LEFT_UP => out(button(f.kind == kind::LEFT_DOWN, 1)),
        kind::RIGHT_DOWN | kind::RIGHT_UP => out(button(f.kind == kind::RIGHT_DOWN, 1 << 1)),
        kind::OTHER_DOWN | kind::OTHER_UP => {
            if let Some(bit) = u8::try_from(f.button).ok().filter(|b| (2..5).contains(b)) {
                out(button(f.kind == kind::OTHER_DOWN, 1 << bit));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `kCGEventFlagMaskAlphaShift`: Caps Lock's state, which toggles on each press.
    const CAPS_LOCK_STATE: u64 = 0x1_0000;

    /// A raw event as plain values, since `RawEvent` prints as a placeholder.
    fn plain(raw: RawEvent) -> String {
        match raw {
            RawEvent::Key { device, usage, down, repeat } => format!("key {device} {:#x} {down} {repeat}", usage.0),
            RawEvent::Buttons { device, pressed, released, .. } => format!("buttons {device} {pressed} {released}"),
            RawEvent::Motion { device, dx, dy, wheel } => format!("motion {device} {dx} {dy} {wheel}"),
            _ => "other".to_owned(),
        }
    }

    fn events(f: Fields) -> Vec<String> {
        let mut out = Vec::new();
        translate(&f, |raw| out.push(plain(raw)));
        out
    }

    #[test]
    fn keys_and_modifiers_become_key_events() {
        let key = |kind, keycode, repeat| events(Fields { kind, keycode, repeat, ..Fields::default() });
        assert_eq!(key(kind::KEY_DOWN, 0x00, false), ["key 1 0x4 true false"]);
        assert_eq!(key(kind::KEY_DOWN, 0x00, true), ["key 1 0x4 true true"]);
        assert_eq!(key(kind::KEY_UP, 0x0D, false), ["key 1 0x1a false false"]);
        // A modifier is down while its own flag is set; the right one has its own bit.
        let flags = |keycode, flags| events(Fields { kind: kind::FLAGS_CHANGED, keycode, flags, ..Fields::default() });
        assert_eq!(flags(0x3C, 0x2_0004), ["key 1 0xe5 true false"]);
        assert_eq!(flags(0x3C, 0x2_0002), ["key 1 0xe5 false false"], "the left Shift still held");
        assert_eq!(flags(0x3E, 0x4_2000), ["key 1 0xe4 true false"]);
        // Caps Lock: each change is one press, whichever way it toggled.
        assert_eq!(flags(CAPS_LOCK, CAPS_LOCK_STATE), ["key 1 0x39 true false", "key 1 0x39 false false"]);
        assert_eq!(flags(CAPS_LOCK, 0), ["key 1 0x39 true false", "key 1 0x39 false false"]);
        // Fn changes flags but is no key the zone map knows.
        assert_eq!(flags(0x3F, 0x80_0000), Vec::<String>::new());
    }

    #[test]
    fn the_mouse_gives_deltas_the_wheel_and_five_buttons() {
        let at = |kind, button, delta, scroll| events(Fields { kind, button, delta, scroll, ..Fields::default() });
        assert_eq!(at(kind::MOVED, 0, [40, 10], 0.0), ["motion 2 40 10 0"]);
        assert_eq!(at(kind::LEFT_DRAGGED, 0, [-3, 0], 0.0), ["motion 2 -3 0 0"]);
        assert_eq!(at(kind::MOVED, 0, [0, 0], 0.0), Vec::<String>::new());
        assert_eq!(at(kind::SCROLL, 0, [0, 0], 1.5), ["motion 2 0 0 1.5"]);
        assert_eq!(at(kind::LEFT_DOWN, 0, [0, 0], 0.0), ["buttons 2 1 0"]);
        assert_eq!(at(kind::RIGHT_UP, 1, [0, 0], 0.0), ["buttons 2 0 2"]);
        assert_eq!(at(kind::OTHER_DOWN, 2, [0, 0], 0.0), ["buttons 2 4 0"]);
        assert_eq!(at(kind::OTHER_UP, 4, [0, 0], 0.0), ["buttons 2 0 16"]);
        // A sixth button has no bit.
        assert_eq!(at(kind::OTHER_DOWN, 5, [0, 0], 0.0), Vec::<String>::new());
    }

    #[test]
    fn the_tap_listens_only_for_the_classes_asked() {
        assert_eq!(mask(false, false), 0);
        assert_eq!(mask(true, false), 1 << 10 | 1 << 11 | 1 << 12);
        let mouse = mask(false, true);
        assert_eq!(mouse & mask(true, false), 0);
        assert_eq!(mouse.count_ones(), 11);
        assert_eq!(mask(true, true), mask(true, false) | mouse);
    }
}
