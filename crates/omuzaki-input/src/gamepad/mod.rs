//! The gamepad backend (design §13 IN-5): the one reader of pads on every OS, so a pad is never read twice and its
//! [`PadId`](omuzaki_input_api::event::PadId) comes from the hub's one allocator. On Windows and Linux it reads
//! through `gilrs` (`XInput` on Windows, evdev through libudev on Linux), registered as `gamepad.gilrs`; on macOS the
//! same backend reads through gilrs's `IOKit` path until the `GameController` reader lands.
//!
//! The capture loop ([`run_with`]) is written over a [`PadSource`], so the mapping from pad events to the hub's raw
//! pad state is tested with a scripted source (`test-hooks`) and runs over gilrs in the App. A pad's buttons are
//! numbered by [`button_bit`]; a pad chord in the binding store holds those bits.

mod gilrs_source;
#[cfg(feature = "test-hooks")]
pub(crate) mod script;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use gilrs::{Axis, Button};
use omuzaki_input_api::event::{DeviceClass, PadZones};

use crate::normalise::{RawEvent, RawSink, quantise_stick};

/// How long the loop waits for a pad event before its idle call. Pads need no pointer coalescing, so this only bounds
/// how late a backlog waiting for ring space is pushed.
const POLL: Duration = Duration::from_millis(8);
/// How long the loop parks between looks while capture does not read pads.
const PARK: Duration = Duration::from_millis(250);
/// A d-pad reported as an axis counts as pressed past this.
const DPAD_AXIS: f32 = 0.5;

/// The buttons a pad chord can hold, in bit order: bit `i` of a pad's `buttons` is `BUTTONS[i]`.
const BUTTONS: [Button; 19] = [
    Button::South,
    Button::East,
    Button::North,
    Button::West,
    Button::C,
    Button::Z,
    Button::LeftTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::Mode,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

/// A button's bit in a pad's `buttons`, or 0 for a button no chord can hold.
pub(crate) fn button_bit(button: Button) -> u32 {
    BUTTONS.iter().position(|b| *b == button).map_or(0, |i| 1 << i)
}

/// The zone a button is in (design §3): gilrs's `LeftTrigger` is the shoulder button and `LeftTrigger2` the trigger.
fn zone_of(button: Button) -> PadZones {
    match button {
        Button::South | Button::East | Button::North | Button::West | Button::C | Button::Z => PadZones::FACE,
        Button::LeftTrigger => PadZones::LEFT_SHOULDER,
        Button::RightTrigger => PadZones::RIGHT_SHOULDER,
        Button::LeftTrigger2 => PadZones::LEFT_TRIGGER,
        Button::RightTrigger2 => PadZones::RIGHT_TRIGGER,
        Button::Select | Button::Start | Button::Mode => PadZones::CENTRE,
        Button::LeftThumb => PadZones::LEFT_STICK,
        Button::RightThumb => PadZones::RIGHT_STICK,
        Button::DPadUp | Button::DPadDown | Button::DPadLeft | Button::DPadRight => PadZones::DPAD,
        Button::Unknown => PadZones::NONE,
    }
}

/// The zones a set of buttons is in.
fn zones_of(buttons: u32) -> PadZones {
    BUTTONS
        .iter()
        .enumerate()
        .filter(|(i, _)| buttons & (1 << i) != 0)
        .fold(PadZones::NONE, |zones, (_, b)| zones.or(zone_of(*b)))
}

/// One pad event, the way gilrs gives it, without gilrs's own ids and codes (which nothing outside gilrs can make).
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum PadInput {
    /// The pad connected.
    Connected,
    /// It left.
    Disconnected,
    /// A button went down or up.
    Button(Button, bool),
    /// A button's analog value in `0..=1` (the triggers').
    Value(Button, f32),
    /// An axis in `-1..=1`, up and right positive.
    Axis(Axis, f32),
}

/// What a [`PadSource`] gave.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Polled {
    /// An event from pad `pad` (the source's own handle), read at `at`.
    Event { pad: u64, input: PadInput, at: SystemTime },
    /// Nothing within the wait.
    Quiet,
    /// The source has ended: the backend's thread ends.
    Ended,
}

/// Where the loop reads pads: gilrs in the App, a script in the tests.
pub(crate) trait PadSource {
    /// The pads connected now, by the source's handles.
    fn connected(&self) -> Vec<u64>;

    /// The next event, waiting up to `wait`.
    fn next(&mut self, wait: Duration) -> Polled;
}

/// One pad's state as the events built it.
#[derive(Copy, Clone, Debug, Default)]
struct Pad {
    buttons: u32,
    /// Buttons held when a run began: they start nothing until released (design §4).
    held_over: u32,
    sticks: [[f32; 2]; 2],
    triggers: [f32; 2],
    /// The d-pad as reported through axes, as button bits.
    dpad_axes: u32,
}

impl Pad {
    fn buttons(&self) -> u32 {
        (self.buttons | self.dpad_axes) & !self.held_over
    }

    fn raw(&self, device: u64) -> RawEvent {
        let buttons = self.buttons();
        RawEvent::Pad { device, buttons, zones: zones_of(buttons), sticks: self.sticks, triggers: self.triggers }
    }
}

/// Every connected pad's state, keyed by the source's handle.
#[derive(Debug, Default)]
pub(crate) struct Pads {
    pads: BTreeMap<u64, Pad>,
}

impl Pads {
    fn new(connected: impl IntoIterator<Item = u64>) -> Self {
        Self { pads: connected.into_iter().map(|id| (id, Pad::default())).collect() }
    }

    /// Applies an event; returns what to hand the sink, if anything changed that the hub sees.
    pub(crate) fn apply(&mut self, device: u64, input: PadInput) -> Option<RawEvent> {
        match input {
            PadInput::Connected => {
                self.pads.entry(device).or_default();
                return Some(RawEvent::DeviceUp { device, class: DeviceClass::Gamepad });
            }
            PadInput::Disconnected => {
                return self.pads.remove(&device).map(|_| RawEvent::DeviceGone { device });
            }
            _ => {}
        }
        let pad = self.pads.entry(device).or_default();
        let before = (pad.buttons(), pad.sticks.map(|s| s.map(quantise_stick)), triggers_u8(pad.triggers));
        match input {
            PadInput::Button(button, down) => {
                let bit = button_bit(button);
                if down {
                    pad.buttons |= bit;
                } else {
                    pad.buttons &= !bit;
                    pad.held_over &= !bit;
                }
            }
            PadInput::Value(Button::LeftTrigger2, value) => pad.triggers[0] = value.clamp(0.0, 1.0),
            PadInput::Value(Button::RightTrigger2, value) => pad.triggers[1] = value.clamp(0.0, 1.0),
            PadInput::Axis(axis, value) => {
                let value = value.clamp(-1.0, 1.0);
                match axis {
                    Axis::LeftStickX => pad.sticks[0][0] = value,
                    Axis::LeftStickY => pad.sticks[0][1] = value,
                    Axis::RightStickX => pad.sticks[1][0] = value,
                    Axis::RightStickY => pad.sticks[1][1] = value,
                    Axis::DPadX => {
                        let (left, right) = (button_bit(Button::DPadLeft), button_bit(Button::DPadRight));
                        let now = if value < -DPAD_AXIS { left } else if value > DPAD_AXIS { right } else { 0 };
                        pad.held_over &= !((left | right) & !now);
                        pad.dpad_axes = pad.dpad_axes & !(left | right) | now;
                    }
                    Axis::DPadY => {
                        let (up, down) = (button_bit(Button::DPadUp), button_bit(Button::DPadDown));
                        let now = if value > DPAD_AXIS { up } else if value < -DPAD_AXIS { down } else { 0 };
                        pad.held_over &= !((up | down) & !now);
                        pad.dpad_axes = pad.dpad_axes & !(up | down) | now;
                    }
                    // A Z axis is a trigger gilrs has no mapping for; its range differs by pad, so it is not read.
                    Axis::LeftZ | Axis::RightZ | Axis::Unknown => {}
                }
            }
            // Other buttons' values repeat their presses.
            PadInput::Value(..) | PadInput::Connected | PadInput::Disconnected => {}
        }
        let after = (pad.buttons(), pad.sticks.map(|s| s.map(quantise_stick)), triggers_u8(pad.triggers));
        (after != before).then(|| pad.raw(device))
    }

    /// A run begins: every button held now starts nothing until it is released.
    fn hold_over(&mut self) {
        for pad in self.pads.values_mut() {
            pad.held_over |= pad.buttons | pad.dpad_axes;
        }
    }

    fn contains(&self, device: u64) -> bool {
        self.pads.contains_key(&device)
    }
}

fn triggers_u8(triggers: [f32; 2]) -> [u8; 2] {
    // The hub quantises triggers to bytes; an event that leaves the byte unchanged changes nothing it shows.
    triggers.map(|t| {
        let scaled = (t.clamp(0.0, 1.0) * 255.0).round();
        // In 0..=255 after the clamp.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = scaled as u8;
        byte
    })
}

/// The capture loop over `source`: parks while capture does not read pads, and when a run begins (capture started or
/// resumed), takes the events read before it as state only, holds over what is pressed, tells the sink which pads
/// are there, and then hands each event on.
pub(crate) fn run_with(mut sink: RawSink, mut source: impl PadSource) {
    let mut pads = Pads::new(source.connected());
    let mut run = None;
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        if !sink.reading() {
            continue;
        }
        if run != Some(sink.epochs()) {
            run = Some(sink.epochs());
            if !begin_run(&mut sink, &mut source, &mut pads, None) {
                return;
            }
        }
        match source.next(POLL) {
            // A pause came and went while the loop waited: the event was read before the new run began.
            Polled::Event { pad, input, at } if run != Some(sink.epochs()) => {
                run = Some(sink.epochs());
                if !begin_run(&mut sink, &mut source, &mut pads, Some((pad, input, at))) {
                    return;
                }
            }
            Polled::Event { pad, input, .. } => hand_on(&mut sink, &mut pads, pad, input),
            Polled::Quiet => {}
            Polled::Ended => return,
        }
        sink.idle();
    }
}

/// Hands one event on to the sink.
fn hand_on(sink: &mut RawSink, pads: &mut Pads, pad: u64, input: PadInput) {
    if let Some(raw) = pads.apply(pad, input) {
        sink.push(raw);
        if matches!(raw, RawEvent::DeviceUp { .. })
            && let Some(state) = pads.pads.get(&pad).map(|p| p.raw(pad))
        {
            // Levels for the new pad, so it is listed before it first moves.
            sink.push(state);
        }
    }
}

/// Starts a run; `false` when the source ended. Events read before it (`read`, and what waits in the source) are
/// taken as state only.
fn begin_run(
    sink: &mut RawSink,
    source: &mut impl PadSource,
    pads: &mut Pads,
    read: Option<(u64, PadInput, SystemTime)>,
) -> bool {
    let began = SystemTime::now();
    let mut first = None;
    let mut next = read.map(|(pad, input, at)| Polled::Event { pad, input, at });
    loop {
        match next.take().unwrap_or_else(|| source.next(Duration::ZERO)) {
            Polled::Event { pad, input, at } if at < began => {
                let _state_only = pads.apply(pad, input);
            }
            Polled::Event { pad, input, .. } => {
                first = Some((pad, input));
                break;
            }
            Polled::Quiet => break,
            Polled::Ended => return false,
        }
    }
    pads.hold_over();
    for gone in sink.devices(DeviceClass::Gamepad) {
        if !pads.contains(gone) {
            sink.push(RawEvent::DeviceGone { device: gone });
        }
    }
    let states: Vec<RawEvent> = pads.pads.iter().map(|(id, pad)| pad.raw(*id)).collect();
    for state in states {
        if let RawEvent::Pad { device, .. } = state {
            sink.push(RawEvent::DeviceUp { device, class: DeviceClass::Gamepad });
        }
        sink.push(state);
    }
    if let Some((pad, input)) = first {
        hand_on(sink, pads, pad, input);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_has_its_own_bit_and_a_zone() {
        let bits: Vec<u32> = BUTTONS.iter().map(|b| button_bit(*b)).collect();
        assert!(bits.iter().all(|b| b.is_power_of_two()));
        assert_eq!(bits.iter().fold(0, |all, b| all | b).count_ones(), 19);
        assert!(BUTTONS.iter().all(|b| !zone_of(*b).is_empty()));
        assert_eq!(button_bit(Button::Unknown), 0);
    }

    #[test]
    fn a_held_over_button_starts_nothing_until_it_is_released() {
        let mut pads = Pads::new([7]);
        let _ = pads.apply(7, PadInput::Button(Button::South, true));
        pads.hold_over();
        let Some(RawEvent::Pad { buttons, .. }) = pads.apply(7, PadInput::Axis(Axis::LeftStickX, 1.0)) else {
            panic!("a stick move changes the levels");
        };
        assert_eq!(buttons, 0);
        let _ = pads.apply(7, PadInput::Button(Button::South, false));
        let Some(RawEvent::Pad { buttons, .. }) = pads.apply(7, PadInput::Button(Button::South, true)) else {
            panic!("a fresh press changes the buttons");
        };
        assert_eq!(buttons, button_bit(Button::South));
    }

    #[test]
    fn a_dpad_on_axes_presses_the_dpad_buttons() {
        let mut pads = Pads::new([1]);
        let Some(RawEvent::Pad { buttons, zones, .. }) = pads.apply(1, PadInput::Axis(Axis::DPadY, 1.0)) else {
            panic!("the d-pad pressed");
        };
        assert_eq!(buttons, button_bit(Button::DPadUp));
        assert_eq!(zones, PadZones::DPAD);
        let Some(RawEvent::Pad { buttons, .. }) = pads.apply(1, PadInput::Axis(Axis::DPadY, 0.0)) else {
            panic!("the d-pad released");
        };
        assert_eq!(buttons, 0);
    }

    #[test]
    fn a_change_the_hub_cannot_see_is_not_pushed() {
        let mut pads = Pads::new([1]);
        assert!(pads.apply(1, PadInput::Axis(Axis::LeftStickX, 0.05)).is_none(), "inside the dead zone");
        assert!(pads.apply(1, PadInput::Value(Button::LeftTrigger2, 0.0001)).is_none(), "below one step");
        assert!(pads.apply(1, PadInput::Value(Button::South, 1.0)).is_none(), "a digital button's value");
        assert!(pads.apply(1, PadInput::Axis(Axis::LeftStickX, 0.5)).is_some());
    }
}
