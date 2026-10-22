//! The Linux evdev keyboard and mouse backend (design §13 IN-6, research §1.4): reads `/dev/input/event*` directly.
//! It is the only way to read the keyboard and mouse on Wayland, and it reads every keyboard on the machine, so it runs
//! only when the person opted in (`HubConfig::with_device_access`), in place of the X11 backend. Without access to the
//! devices (membership of the `input` group) it reports `NeedsPermission { InputGroup }`.
//!
//! It never grabs a device, reads pads (the gamepad backend's) or tablets (the pen is IN-10's), and it holds no device
//! open while capture does not read: a run opens them, so nothing typed before it is ever read.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use evdev::{Device, EventType, KeyCode, RelativeAxisCode};
use omuzaki_input_api::capture::{BackendState, CaptureCaps, PermissionHow, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::{RawEvent, RawSink, linux_codes};

const EVDEV: InputBackend = InputBackend {
    name: "linux.evdev",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse),
    caps,
    run: Some(run),
    chosen: Chosen::WhenOptedIn,
};
register_input_backend!(EVDEV);

/// Where the kernel's input devices are.
const DEV_INPUT: &str = "/dev/input";
/// How long the loop waits on the devices before its idle call, which pushes finished pointer periods.
const POLL: Duration = Duration::from_millis(4);
/// The wait with no device open.
const EMPTY_POLL: Duration = Duration::from_millis(100);
/// How long the loop parks between looks while capture does not read the keyboard or the mouse.
const PARK: Duration = Duration::from_millis(250);
/// How often the folder is read for devices plugged in.
const RESCAN: Duration = Duration::from_secs(1);

/// The mouse buttons the backend reads, by their kernel codes (`BTN_LEFT` to `BTN_EXTRA`), and their bits.
const BUTTONS: [(u16, u8); 5] = [(0x110, 1), (0x111, 1 << 1), (0x112, 1 << 2), (0x113, 1 << 3), (0x114, 1 << 4)];

fn caps() -> CaptureCaps {
    // Registered only when the person opted in; whether the devices can be opened shows in the backend's state.
    CaptureCaps {
        typing: Support::Available,
        pointer: Support::Available,
        bindings: Support::Available,
        ..CaptureCaps::default()
    }
}

fn run(sink: RawSink) {
    run_in(sink, Path::new(DEV_INPUT), None);
}

/// The loop over the devices in `dir`, or only those named `only` (the tests' virtual devices).
pub(crate) fn run_in(mut sink: RawSink, dir: &Path, only: Option<&str>) {
    let mut reader = EvdevReader {
        dir: dir.to_owned(),
        only: only.map(str::to_owned),
        handles: BTreeMap::new(),
        open: BTreeMap::new(),
        run: None,
        next_scan: Instant::now(),
        state: None,
    };
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        if !sink.reading() {
            // Nothing stays open while capture does not read.
            reader.open.clear();
            continue;
        }
        if reader.run != Some(sink.epochs()) {
            reader.run = Some(sink.epochs());
            reader.open.clear();
            reader.scan(&mut sink, true);
        } else if Instant::now() >= reader.next_scan {
            reader.scan(&mut sink, false);
        }
        reader.read(&mut sink);
        sink.idle();
    }
}

/// An open device.
struct Open {
    device: Device,
    class: DeviceClass,
    path: PathBuf,
}

struct EvdevReader {
    dir: PathBuf,
    only: Option<String>,
    /// The handle of each device node, kept across runs so a device is the same source while it stays plugged in.
    handles: BTreeMap<PathBuf, u64>,
    open: BTreeMap<u64, Open>,
    run: Option<(u32, u32)>,
    next_scan: Instant,
    state: Option<BackendState>,
}

impl EvdevReader {
    /// Reads the folder: opens the devices plugged in since the last look, closes those gone, and reports whether any
    /// could be opened. At a run's start (`fresh`), also tells the sink which devices it knew are gone.
    #[allow(clippy::disallowed_methods, reason = "lists the kernel's device folder, never a path an op names; the fence is for ops")]
    fn scan(&mut self, sink: &mut RawSink, fresh: bool) {
        self.next_scan = Instant::now() + RESCAN;
        let mut nodes: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        nodes.sort();
        let gone: Vec<u64> = self.open.iter().filter(|(_, o)| !nodes.contains(&o.path)).map(|(h, _)| *h).collect();
        for handle in gone {
            self.open.remove(&handle);
            sink.push(RawEvent::DeviceGone { device: handle });
        }
        let (mut opened, mut denied) = (self.open.len(), 0);
        for path in nodes {
            if self.open.values().any(|o| o.path == path) {
                continue;
            }
            match Device::open(&path) {
                Ok(device) => {
                    let Some(class) = class_of(&device) else { continue };
                    if self.only.as_deref().is_some_and(|name| device.name() != Some(name)) {
                        continue;
                    }
                    if device.set_nonblocking(true).is_err() {
                        continue;
                    }
                    let next = self.handles.values().max().map_or(1, |h| h + 1);
                    let handle = *self.handles.entry(path.clone()).or_insert(next);
                    self.open.insert(handle, Open { device, class, path });
                    if !fresh {
                        sink.push(RawEvent::DeviceUp { device: handle, class });
                    }
                    opened += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied += 1,
                Err(_) => {}
            }
        }
        if fresh {
            for class in [DeviceClass::Keyboard, DeviceClass::Mouse] {
                for known in sink.devices(class) {
                    if !self.open.contains_key(&known) {
                        sink.push(RawEvent::DeviceGone { device: known });
                    }
                }
            }
            for (handle, open) in &self.open {
                sink.push(RawEvent::DeviceUp { device: *handle, class: open.class });
            }
        }
        let state = state_of(opened, denied);
        if self.state.as_ref() != Some(&state) {
            sink.set_state(state.clone());
            self.state = Some(state);
        }
    }

    /// Waits for any device, then hands on what each readable one has.
    fn read(&mut self, sink: &mut RawSink) {
        let wait = if self.open.is_empty() { EMPTY_POLL } else { POLL };
        let timeout = Timespec { tv_sec: 0, tv_nsec: i64::try_from(wait.as_nanos()).unwrap_or(4_000_000) };
        let handles: Vec<u64> = self.open.keys().copied().collect();
        let ready: Vec<bool> = {
            let mut fds: Vec<PollFd<'_>> =
                self.open.values().map(|o| PollFd::from_borrowed_fd(o.device.as_fd(), PollFlags::IN)).collect();
            if poll(&mut fds, Some(&timeout)).is_err() {
                return;
            }
            fds.iter().map(|fd| !fd.revents().is_empty()).collect()
        };
        for (handle, ready) in handles.into_iter().zip(ready) {
            if !ready || self.run != Some(sink.epochs()) {
                continue;
            }
            let Some(open) = self.open.get_mut(&handle) else { continue };
            let class = open.class;
            let gone = match open.device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        if let Some(raw) = raw_of(handle, class, event.event_type(), event.code(), event.value()) {
                            sink.push(raw);
                        }
                    }
                    false
                }
                Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
            };
            // Unplugged (ENODEV) or broken: it is gone.
            if gone {
                self.open.remove(&handle);
                sink.push(RawEvent::DeviceGone { device: handle });
            }
        }
    }
}

/// The backend's state from what a scan found: it needs the `input` group when it could open nothing and was refused.
fn state_of(opened: usize, denied: usize) -> BackendState {
    if opened == 0 && denied > 0 {
        BackendState::needs_permission(PermissionHow::InputGroup)
    } else {
        BackendState::Running
    }
}

/// What the backend reads `device` as: a keyboard (it has letter keys and a space bar) or a relative mouse with a
/// left button. Pads are the gamepad backend's and tablets the pen's, so neither is read here.
fn class_of(device: &Device) -> Option<DeviceClass> {
    let keys = device.supported_keys();
    let has = |code: u16| keys.is_some_and(|k| k.contains(KeyCode(code)));
    // BTN_SOUTH (pads), BTN_JOYSTICK, BTN_TOOL_PEN.
    if has(0x130) || has(0x120) || has(0x140) {
        return None;
    }
    // KEY_A and KEY_SPACE.
    if has(30) && has(57) {
        return Some(DeviceClass::Keyboard);
    }
    let axes = device.supported_relative_axes();
    let rel = |code: u16| axes.is_some_and(|a| a.contains(RelativeAxisCode(code)));
    (rel(0) && rel(1) && has(0x110)).then_some(DeviceClass::Mouse)
}

/// The raw event for one kernel event, if the backend reads it.
fn raw_of(device: u64, class: DeviceClass, kind: EventType, code: u16, value: i32) -> Option<RawEvent> {
    match (class, kind) {
        (DeviceClass::Keyboard, EventType::KEY) => linux_codes::key(device, code, value != 0, value == 2),
        (DeviceClass::Mouse, EventType::KEY) => {
            let bit = BUTTONS.iter().find(|(c, _)| *c == code)?.1;
            let (pressed, released) = match value {
                1 => (bit, 0),
                0 => (0, bit),
                _ => return None,
            };
            Some(RawEvent::Buttons { device, class, pressed, released })
        }
        // REL_X, REL_Y and REL_WHEEL (the high-resolution wheel repeats it).
        (DeviceClass::Mouse, EventType::RELATIVE) => match code {
            0 => Some(RawEvent::Motion { device, dx: value, dy: 0, wheel: 0.0 }),
            1 => Some(RawEvent::Motion { device, dx: 0, dy: value, wheel: 0.0 }),
            // A notch count: small.
            #[allow(clippy::cast_precision_loss)]
            8 => Some(RawEvent::Motion { device, dx: 0, dy: 0, wheel: value as f32 }),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_needs_the_input_group_only_when_it_was_refused_and_opened_nothing() {
        assert_eq!(state_of(0, 3), BackendState::needs_permission(PermissionHow::InputGroup));
        assert_eq!(state_of(1, 3), BackendState::Running);
        assert_eq!(state_of(0, 0), BackendState::Running);
    }

    #[test]
    fn kernel_events_map_to_keys_buttons_and_motion() {
        let key = |value| raw_of(1, DeviceClass::Keyboard, EventType::KEY, 30, value);
        assert!(matches!(key(1), Some(RawEvent::Key { down: true, repeat: false, .. })));
        assert!(matches!(key(2), Some(RawEvent::Key { down: true, repeat: true, .. })));
        assert!(matches!(key(0), Some(RawEvent::Key { down: false, .. })));
        assert!(raw_of(1, DeviceClass::Keyboard, EventType::KEY, 0x2F0, 1).is_none(), "a key the zone map lacks");
        let mouse = |kind, code, value| raw_of(2, DeviceClass::Mouse, kind, code, value);
        assert!(matches!(mouse(EventType::KEY, 0x111, 1), Some(RawEvent::Buttons { pressed: 2, released: 0, .. })));
        assert!(matches!(mouse(EventType::KEY, 0x110, 0), Some(RawEvent::Buttons { pressed: 0, released: 1, .. })));
        assert!(matches!(mouse(EventType::RELATIVE, 1, -3), Some(RawEvent::Motion { dx: 0, dy: -3, .. })));
        assert!(mouse(EventType::RELATIVE, 0x0B, 120).is_none(), "the high-resolution wheel is not counted twice");
        assert!(mouse(EventType::KEY, 30, 1).is_none(), "a mouse's keys are not read");
    }
}
