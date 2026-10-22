//! Pads through gilrs, registered as `gamepad.gilrs`. Built without gilrs's default filters: the hub applies its own
//! dead zone and quantisation, so the edges stay exact (§3), and the d-pad on axes is read by [`Pads`](super::Pads).

use std::time::Duration;

use gilrs::{EventType, Gilrs, GilrsBuilder};
use omuzaki_input_api::capture::{BackendState, CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};

use super::{PadInput, PadSource, Polled, run_with};
use crate::hub::{InputBackend, register_input_backend};
use crate::normalise::RawSink;

const GILRS: InputBackend = InputBackend {
    name: "gamepad.gilrs",
    classes: DeviceMask::NONE.with(DeviceClass::Gamepad),
    caps,
    run: Some(run),
};
register_input_backend!(GILRS);

fn caps() -> CaptureCaps {
    CaptureCaps { gamepad: Support::Available, ..CaptureCaps::default() }
}

fn run(sink: RawSink) {
    let built = GilrsBuilder::new().with_default_filters(false).add_env_mappings(false).build();
    match built {
        Ok(gilrs) => run_with(sink, GilrsSource { gilrs }),
        // The reason is fixed text: gilrs's own error can name a device.
        Err(_) => sink.set_state(BackendState::failed("the gamepad service could not start")),
    }
}

struct GilrsSource {
    gilrs: Gilrs,
}

impl PadSource for GilrsSource {
    fn connected(&self) -> Vec<u64> {
        self.gilrs.gamepads().map(|(id, _)| handle(id)).collect()
    }

    fn next(&mut self, wait: Duration) -> Polled {
        let event = if wait.is_zero() { self.gilrs.next_event() } else { self.gilrs.next_event_blocking(Some(wait)) };
        let Some(event) = event else { return Polled::Quiet };
        let input = match event.event {
            EventType::Connected => PadInput::Connected,
            EventType::Disconnected => PadInput::Disconnected,
            EventType::ButtonPressed(button, _) => PadInput::Button(button, true),
            EventType::ButtonReleased(button, _) => PadInput::Button(button, false),
            EventType::ButtonChanged(button, value, _) => PadInput::Value(button, value),
            EventType::AxisChanged(axis, value, _) => PadInput::Axis(axis, value),
            // Repeats come from a filter this backend does not use; the rest carry no input.
            EventType::ButtonRepeated(..) | EventType::Dropped | EventType::ForceFeedbackEffectCompleted => {
                return Polled::Quiet;
            }
            _ => return Polled::Quiet,
        };
        Polled::Event { pad: handle(event.id), input, at: event.time }
    }
}

fn handle(id: gilrs::GamepadId) -> u64 {
    u64::try_from(usize::from(id)).unwrap_or(u64::MAX)
}
