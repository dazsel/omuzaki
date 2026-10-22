//! The Windows keyboard and mouse backend (design §13 IN-4, research §1.1 and §7): Raw Input with `RIDEV_INPUTSINK` on
//! the crate's own message-only window and capture thread, read in batches with `GetRawInputBuffer`. No hooks: nothing
//! sits in another program's input path, and nothing loads into one. Each Raw Input device handle is its own source,
//! with `SourceUp` and `SourceGone` from `WM_INPUT_DEVICE_CHANGE`.
//!
//! It registers only what capture reads (the keyboard collection while the keyboard is asked for, the mouse collection
//! while the mouse is) and removes both while capture is paused or off, so Windows sends nothing then. Records queued
//! before a run began are dropped. Raw Input registration is per process: while capture reads, the hidden window holds
//! the keyboard and mouse registrations, so winit's device events stop (nothing in the studio reads them; its window
//! events are untouched). The pen is IN-10's.
//!
//! This file holds what does not need Windows (the record translation and the device bookkeeping), so its tests run
//! everywhere; `raw` is the Win32 side.
#![cfg_attr(not(windows), allow(dead_code, reason = "the Win32 side calls these; their tests run everywhere"))]

#[cfg(windows)]
pub(crate) mod raw;

use std::collections::BTreeMap;

use omuzaki_input_api::event::DeviceClass;

use crate::normalise::{RawEvent, windows_codes};

/// `RI_KEY_BREAK`, `RI_KEY_E0` and `RI_KEY_E1`: a keyboard record's flags.
const KEY_BREAK: u16 = 1;
const KEY_E0: u16 = 2;
const KEY_E1: u16 = 4;
/// `VK_PAUSE`, which the first record of Pause's `E1` sequence carries.
const VK_PAUSE: u16 = 0x13;
/// The virtual key of a record that is part of an escape sequence rather than a key.
const VK_NONE: u16 = 0xFF;
/// `MOUSE_MOVE_ABSOLUTE`: the record's motion is a position (a remote session, a tablet, a virtual machine), not a
/// relative move.
const MOVE_ABSOLUTE: u16 = 1;
/// `RI_MOUSE_WHEEL`, and the notch size its data counts in.
const WHEEL: u16 = 0x400;
const WHEEL_DELTA: f32 = 120.0;
/// Input no device sent (another program's synthetic input, an on-screen keyboard, a remote session) arrives with a
/// null device handle for both classes; each class reads it as a source of its own, at these handles.
const SYNTHETIC_KEYBOARD: u64 = 1 << 63;
const SYNTHETIC_MOUSE: u64 = 1 << 63 | 1;

/// The handle the backend files a record from Raw Input handle `handle` of `class` under.
pub(crate) fn device_of(handle: u64, class: DeviceClass) -> u64 {
    match (handle, class) {
        (0, DeviceClass::Mouse) => SYNTHETIC_MOUSE,
        (0, _) => SYNTHETIC_KEYBOARD,
        _ => handle,
    }
}

/// The key event of a keyboard record: its scan code `make`, `flags` and virtual key `vkey`. `None` for a record that
/// is no key the zone map knows (the halves of an escape sequence, the shifts Windows fakes around the navigation keys).
pub(crate) fn key(device: u64, make: u16, flags: u16, vkey: u16) -> Option<RawEvent> {
    let down = flags & KEY_BREAK == 0;
    let code = if vkey == VK_PAUSE {
        windows_codes::PAUSE
    } else if vkey == VK_NONE || flags & KEY_E1 != 0 {
        return None;
    } else if flags & KEY_E0 != 0 {
        windows_codes::EXTENDED | make
    } else {
        make
    };
    windows_codes::key(device, code, down)
}

/// The events of a mouse record: its `flags`, button flags, button data and motion. Relative motion and the wheel come
/// first, then the buttons, one bit each in the order left, right, middle, back, forward (Raw Input's own order, two
/// flags per button: down, then up).
pub(crate) fn mouse(device: u64, flags: u16, buttons: u16, data: u16, last: [i32; 2], mut out: impl FnMut(RawEvent)) {
    let wheel = if buttons & WHEEL == 0 { 0.0 } else { f32::from(data.cast_signed()) / WHEEL_DELTA };
    let [dx, dy] = if flags & MOVE_ABSOLUTE == 0 { last } else { [0, 0] };
    if dx != 0 || dy != 0 || wheel != 0.0 {
        out(RawEvent::Motion { device, dx, dy, wheel });
    }
    let (mut pressed, mut released) = (0u8, 0u8);
    for bit in 0..5 {
        if buttons & 1 << (2 * bit) != 0 {
            pressed |= 1 << bit;
        }
        if buttons & 1 << (2 * bit + 1) != 0 {
            released |= 1 << bit;
        }
    }
    if pressed | released != 0 {
        out(RawEvent::Buttons { device, class: DeviceClass::Mouse, pressed, released });
    }
}

/// The devices the backend reads, by the handle it files them under.
#[derive(Debug, Default)]
pub(crate) struct Devices(BTreeMap<u64, DeviceClass>);

impl Devices {
    /// A run begins with `attached` there (the synthetic sources always are): the devices the sink knows that are not
    /// (`known`, per class) are gone, and every attached one is up.
    pub(crate) fn reconcile(
        &mut self,
        attached: impl IntoIterator<Item = (u64, DeviceClass)>,
        known: impl Fn(DeviceClass) -> Vec<u64>,
        mut out: impl FnMut(RawEvent),
    ) {
        let synthetic = [(SYNTHETIC_KEYBOARD, DeviceClass::Keyboard), (SYNTHETIC_MOUSE, DeviceClass::Mouse)];
        self.0 = attached.into_iter().filter(|(handle, _)| *handle != 0).chain(synthetic).collect();
        for class in [DeviceClass::Keyboard, DeviceClass::Mouse] {
            for gone in known(class) {
                if !self.0.contains_key(&gone) {
                    out(RawEvent::DeviceGone { device: gone });
                }
            }
        }
        for (&device, &class) in &self.0 {
            out(RawEvent::DeviceUp { device, class });
        }
    }

    /// Device `handle` of `class` is there (it arrived, or a record named it first): its `DeviceUp`, once.
    pub(crate) fn arrived(&mut self, handle: u64, class: DeviceClass) -> Option<RawEvent> {
        let device = device_of(handle, class);
        self.0.insert(device, class).is_none().then_some(RawEvent::DeviceUp { device, class })
    }

    /// Device `handle` left: its `DeviceGone`, once. The synthetic sources never leave.
    pub(crate) fn removed(&mut self, handle: u64) -> Option<RawEvent> {
        (handle != 0 && self.0.remove(&handle).is_some()).then_some(RawEvent::DeviceGone { device: handle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw event as plain values, since `RawEvent` prints as a placeholder.
    fn plain(raw: RawEvent) -> String {
        match raw {
            RawEvent::DeviceUp { device, class } => format!("up {device:#x} {class:?}"),
            RawEvent::DeviceGone { device } => format!("gone {device:#x}"),
            RawEvent::Key { device, usage, down, repeat } => format!("key {device:#x} {:#x} {down} {repeat}", usage.0),
            RawEvent::Buttons { device, pressed, released, .. } => format!("buttons {device:#x} {pressed} {released}"),
            RawEvent::Motion { device, dx, dy, wheel } => format!("motion {device:#x} {dx} {dy} {wheel}"),
            RawEvent::Pen { .. } | RawEvent::Pad { .. } | RawEvent::Shortcut { .. } => "other".to_owned(),
        }
    }

    #[test]
    fn keyboard_records_become_keys_and_escape_sequences_do_not() {
        let key = |make, flags, vkey| key(7, make, flags, vkey).map(plain);
        // A down, then up.
        assert_eq!(key(0x1E, 0, 0x41).as_deref(), Some("key 0x7 0x4 true false"));
        assert_eq!(key(0x1E, KEY_BREAK, 0x41).as_deref(), Some("key 0x7 0x4 false false"));
        // The right Ctrl is the left one's code with E0; the up arrow likewise over the keypad's 8.
        assert_eq!(key(0x1D, KEY_E0, 0x11).as_deref(), Some("key 0x7 0xe4 true false"));
        assert_eq!(key(0x48, KEY_E0, 0x26).as_deref(), Some("key 0x7 0x52 true false"));
        assert_eq!(key(0x48, 0, 0x68).as_deref(), Some("key 0x7 0x60 true false"));
        // Pause: E1 1D with VK_PAUSE is the key, the E1 45 that follows is not; Ctrl+Break is the same key.
        assert_eq!(key(0x1D, KEY_E1, VK_PAUSE).as_deref(), Some("key 0x7 0x48 true false"));
        assert_eq!(key(0x45, KEY_E1, VK_NONE), None);
        assert_eq!(key(0x46, KEY_E0, 0x03).as_deref(), Some("key 0x7 0x48 true false"));
        // The shift faked around the navigation keys, and an overrun.
        assert_eq!(key(0x2A, KEY_E0, VK_NONE), None);
        assert_eq!(key(0xFF, 0, VK_NONE), None);
    }

    #[test]
    fn mouse_records_give_relative_motion_the_wheel_and_buttons_in_order() {
        let mouse = |flags, buttons, data, last| {
            let mut out = Vec::new();
            mouse(9, flags, buttons, data, last, |raw| out.push(plain(raw)));
            out
        };
        assert_eq!(mouse(0, 0, 0, [40, -3]), ["motion 0x9 40 -3 0"]);
        // An absolute record moves nothing (its position is no delta).
        assert_eq!(mouse(MOVE_ABSOLUTE, 0, 0, [30_000, 30_000]), Vec::<String>::new());
        // One notch away from the person, then two towards.
        assert_eq!(mouse(0, WHEEL, 120, [0, 0]), ["motion 0x9 0 0 1"]);
        assert_eq!(mouse(0, WHEEL, (-240i16).cast_unsigned(), [0, 0]), ["motion 0x9 0 0 -2"]);
        // Left down; right up with middle down; back and forward down; then all up with a move.
        assert_eq!(mouse(0, 0x1, 0, [0, 0]), ["buttons 0x9 1 0"]);
        assert_eq!(mouse(0, 0x8 | 0x10, 0, [0, 0]), ["buttons 0x9 4 2"]);
        assert_eq!(mouse(0, 0x40 | 0x100, 0, [0, 0]), ["buttons 0x9 24 0"]);
        assert_eq!(mouse(0, 0x2 | 0x20 | 0x80 | 0x200, 0, [1, 0]), ["motion 0x9 1 0 0", "buttons 0x9 0 29"]);
    }

    #[test]
    fn two_keyboards_are_two_devices_and_only_the_one_unplugged_leaves() {
        let mut devices = Devices::default();
        let mut out = Vec::new();
        // The sink knew a keyboard that left while capture read nothing.
        let known = |class| if class == DeviceClass::Keyboard { vec![0x30, 0x10] } else { Vec::new() };
        devices.reconcile(
            [(0x10, DeviceClass::Keyboard), (0x20, DeviceClass::Keyboard), (0x40, DeviceClass::Mouse)],
            known,
            |raw| out.push(plain(raw)),
        );
        assert_eq!(
            out,
            [
                "gone 0x30",
                "up 0x10 Keyboard",
                "up 0x20 Keyboard",
                "up 0x40 Mouse",
                "up 0x8000000000000000 Keyboard",
                "up 0x8000000000000001 Mouse",
            ]
        );
        // Unplugging one keyboard sends its leaving only, once; the synthetic source never leaves.
        assert_eq!(devices.removed(0x10).map(plain).as_deref(), Some("gone 0x10"));
        assert_eq!(devices.removed(0x10).map(plain), None);
        assert_eq!(devices.removed(0).map(plain), None);
        // An arrival is announced once, however often Windows reports it.
        assert_eq!(devices.arrived(0x10, DeviceClass::Keyboard).map(plain).as_deref(), Some("up 0x10 Keyboard"));
        assert_eq!(devices.arrived(0x10, DeviceClass::Keyboard).map(plain), None);
        assert_eq!(devices.arrived(0, DeviceClass::Mouse).map(plain), None);
        assert_eq!(device_of(0, DeviceClass::Keyboard), SYNTHETIC_KEYBOARD);
    }
}
