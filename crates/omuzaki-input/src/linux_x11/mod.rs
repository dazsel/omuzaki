//! The Linux X11 keyboard and mouse backend (design §13 IN-6, research §1.3): `XInput` 2 raw events on the root window
//! for all master devices, over x11rb's pure-Rust connection. Raw events reach every client whatever has focus, and
//! carry the physical device (`sourceid`), so each keyboard and mouse is its own source.
//!
//! It selects only what capture reads: the raw key events while the keyboard is asked for, the raw buttons and
//! `XI_RawMotion` while the mouse is (winit warns that unfocused device events cost CPU), and nothing while capture
//! is paused or off, so the X server sends no keys then. Events that arrived before a run began are dropped. On a
//! Wayland session it does not run: X11 sees only X clients there, and the keyboard and mouse need the evdev opt-in.
//! The pen is IN-10's.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use omuzaki_input_api::capture::{BackendState, CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{
    ConnectionExt as _, DeviceClassData, DeviceType, EventMask, Fp3232, HierarchyMask, KeyEventFlags, ValuatorMode,
    XIDeviceInfo, XIEventMask,
};
use x11rb::protocol::xproto::{ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::{RawEvent, RawSink, linux_codes};

const X11: InputBackend = InputBackend {
    name: "linux.x11",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse),
    caps,
    run: Some(run),
    chosen: Chosen::UnlessOptedIn,
};
register_input_backend!(X11);

/// How long the loop waits on the X connection before its idle call, which pushes finished pointer periods.
const POLL: Duration = Duration::from_millis(4);
/// How long the loop parks between looks while capture does not read the keyboard or the mouse.
const PARK: Duration = Duration::from_millis(250);
/// `XIAllDevices` and `XIAllMasterDevices`.
const ALL_DEVICES: u16 = 0;
const ALL_MASTER_DEVICES: u16 = 1;
/// The X keycode of Linux key code 0: X's evdev keycodes are the kernel's plus 8.
const KEYCODE_OFFSET: u32 = 8;

/// Whether this is a Wayland session, where X11 sees only X clients.
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"))
}

fn caps() -> CaptureCaps {
    let support = if wayland() {
        Support::NeedsOptIn
    } else if std::env::var_os("DISPLAY").is_some() {
        Support::Available
    } else {
        Support::Unavailable
    };
    CaptureCaps { typing: support, pointer: support, bindings: support, ..CaptureCaps::default() }
}

fn run(sink: RawSink) {
    if wayland() {
        sink.set_state(BackendState::failed("a Wayland session: the keyboard and mouse need the input-device opt-in"));
        return;
    }
    run_on(sink, None, None);
}

/// The loop on display `display` (the environment's with `None`); `probe`, when given, mirrors the selected event mask
/// for the tests.
pub(crate) fn run_on(mut sink: RawSink, display: Option<&str>, probe: Option<Arc<AtomicU32>>) {
    let Some(mut x) = X11Reader::connect(display, probe) else {
        sink.set_state(BackendState::failed("no X display with XInput 2.2 to read"));
        return;
    };
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        let ok = if !sink.reading() {
            x.quiet()
        } else if x.run != Some(sink.epochs()) {
            x.run = Some(sink.epochs());
            x.begin(&mut sink)
        } else {
            x.read(&mut sink)
        };
        if !ok {
            sink.set_state(BackendState::failed("the X connection closed"));
            return;
        }
        sink.idle();
    }
}

/// One X device the backend reads.
#[derive(Copy, Clone, Debug)]
struct XDevice {
    class: DeviceClass,
    /// The sub-count left over from relative motion, so slow motion is not lost to rounding.
    carry: [f64; 2],
}

struct X11Reader {
    conn: RustConnection,
    root: Window,
    devices: BTreeMap<u16, XDevice>,
    /// The raw event mask selected now.
    selected: u32,
    run: Option<(u32, u32)>,
    probe: Option<Arc<AtomicU32>>,
}

impl X11Reader {
    fn connect(display: Option<&str>, probe: Option<Arc<AtomicU32>>) -> Option<Self> {
        let (conn, screen) = RustConnection::connect(display).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let version = conn.xinput_xi_query_version(2, 2).ok()?.reply().ok()?;
        if (version.major_version, version.minor_version) < (2, 2) {
            return None;
        }
        Some(Self { conn, root, devices: BTreeMap::new(), selected: 0, run: None, probe })
    }

    /// Selects `mask` on the root for all master devices (and the hierarchy for all devices while anything is).
    fn select(&mut self, mask: u32) -> bool {
        let hierarchy = if mask == 0 { 0 } else { u32::from(XIEventMask::HIERARCHY) };
        let masks = [
            EventMask { deviceid: ALL_MASTER_DEVICES, mask: vec![XIEventMask::from(mask)] },
            EventMask { deviceid: ALL_DEVICES, mask: vec![XIEventMask::from(hierarchy)] },
        ];
        let sent = self.conn.xinput_xi_select_events(self.root, &masks).is_ok_and(|c| c.check().is_ok());
        self.selected = mask;
        if let Some(probe) = &self.probe {
            probe.store(mask, Ordering::Release);
        }
        sent
    }

    /// Drops every event already sent: a round trip makes sure each one generated before it has arrived.
    fn discard(&mut self) -> bool {
        let Ok(cookie) = self.conn.get_input_focus() else { return false };
        if cookie.reply().is_err() {
            return false;
        }
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(_dropped)) => {}
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }

    /// Capture is not reading: select nothing, so the server sends no keys, and drop what was sent.
    fn quiet(&mut self) -> bool {
        if self.selected == 0 {
            return true;
        }
        self.select(0) && self.discard()
    }

    /// A run begins: drop what came before it, tell the sink which devices are there, and select what it reads.
    fn begin(&mut self, sink: &mut RawSink) -> bool {
        if !(self.select(0) && self.discard()) {
            return false;
        }
        let Some(devices) = self.query(ALL_DEVICES) else { return false };
        for class in [DeviceClass::Keyboard, DeviceClass::Mouse] {
            for gone in sink.devices(class) {
                if u16::try_from(gone).ok().is_none_or(|id| !devices.contains_key(&id)) {
                    sink.push(RawEvent::DeviceGone { device: gone });
                }
            }
        }
        for (id, device) in &devices {
            sink.push(RawEvent::DeviceUp { device: u64::from(*id), class: device.class });
        }
        self.devices = devices;
        let asked = sink.asked();
        let mut mask = 0;
        if asked.contains(DeviceClass::Keyboard) {
            mask |= u32::from(XIEventMask::RAW_KEY_PRESS) | u32::from(XIEventMask::RAW_KEY_RELEASE);
        }
        if asked.contains(DeviceClass::Mouse) {
            mask |= u32::from(XIEventMask::RAW_BUTTON_PRESS)
                | u32::from(XIEventMask::RAW_BUTTON_RELEASE)
                | u32::from(XIEventMask::RAW_MOTION);
        }
        self.select(mask)
    }

    /// The slave keyboards and relative pointers among `id` (or every device).
    fn query(&self, id: u16) -> Option<BTreeMap<u16, XDevice>> {
        let reply = self.conn.xinput_xi_query_device(id).ok()?.reply().ok()?;
        Some(reply.infos.iter().filter_map(|info| class_of(info).map(|class| (info.deviceid, XDevice { class, carry: [0.0; 2] }))).collect())
    }

    /// Waits for the connection, then hands every event on until the run changes.
    fn read(&mut self, sink: &mut RawSink) -> bool {
        let stream = self.conn.stream().as_fd();
        let mut fds = [PollFd::from_borrowed_fd(stream, PollFlags::IN)];
        let timeout = Timespec { tv_sec: 0, tv_nsec: i64::try_from(POLL.as_nanos()).unwrap_or(4_000_000) };
        if poll(&mut fds, Some(&timeout)).is_err() {
            return true;
        }
        loop {
            // A pause that came and went: what is still queued belongs before the new run, which drops it.
            if self.run != Some(sink.epochs()) {
                return true;
            }
            match self.conn.poll_for_event() {
                Ok(Some(event)) => self.handle(sink, event),
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }

    fn handle(&mut self, sink: &mut RawSink, event: Event) {
        match event {
            Event::XinputRawKeyPress(e) => self.key(sink, e.sourceid, e.detail, true, e.flags),
            Event::XinputRawKeyRelease(e) => self.key(sink, e.sourceid, e.detail, false, e.flags),
            Event::XinputRawButtonPress(e) => self.button(sink, e.sourceid, e.detail, true),
            Event::XinputRawButtonRelease(e) => self.button(sink, e.sourceid, e.detail, false),
            Event::XinputRawMotion(e) => {
                let Some(device) = self.devices.get_mut(&e.sourceid).filter(|d| d.class == DeviceClass::Mouse) else {
                    return;
                };
                let mut moved = [0i32; 2];
                let values = valuators(&e.valuator_mask, &e.axisvalues_raw);
                for ((value, carry), slot) in values.iter().zip(&mut device.carry).zip(&mut moved) {
                    if let Some(value) = value {
                        let total = *carry + value;
                        let whole = total.trunc();
                        *carry = total - whole;
                        // A raw delta in device counts: far inside i32.
                        #[allow(clippy::cast_possible_truncation)]
                        let counts = whole as i32;
                        *slot = counts;
                    }
                }
                if moved != [0, 0] {
                    sink.push(RawEvent::Motion { device: u64::from(e.sourceid), dx: moved[0], dy: moved[1], wheel: 0.0 });
                }
            }
            Event::XinputHierarchy(e) => {
                for info in &e.infos {
                    let flags = u32::from(info.flags);
                    let left = u32::from(HierarchyMask::SLAVE_REMOVED)
                        | u32::from(HierarchyMask::DEVICE_DISABLED)
                        | u32::from(HierarchyMask::SLAVE_DETACHED);
                    let came = u32::from(HierarchyMask::SLAVE_ADDED)
                        | u32::from(HierarchyMask::DEVICE_ENABLED)
                        | u32::from(HierarchyMask::SLAVE_ATTACHED);
                    if flags & left != 0 && self.devices.remove(&info.deviceid).is_some() {
                        sink.push(RawEvent::DeviceGone { device: u64::from(info.deviceid) });
                    } else if flags & came != 0
                        && let Some(found) = self.query(info.deviceid)
                    {
                        for (id, device) in found {
                            self.devices.insert(id, device);
                            sink.push(RawEvent::DeviceUp { device: u64::from(id), class: device.class });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn key(&self, sink: &mut RawSink, source: u16, keycode: u32, down: bool, flags: KeyEventFlags) {
        if self.devices.get(&source).is_none_or(|d| d.class != DeviceClass::Keyboard) {
            return;
        }
        let repeat = u32::from(flags) & u32::from(KeyEventFlags::KEY_REPEAT) != 0;
        let code = keycode.checked_sub(KEYCODE_OFFSET).and_then(|c| u16::try_from(c).ok());
        if let Some(raw) = code.and_then(|c| linux_codes::key(u64::from(source), c, down, repeat)) {
            sink.push(raw);
        }
    }

    fn button(&self, sink: &mut RawSink, source: u16, button: u32, down: bool) {
        if self.devices.get(&source).is_none_or(|d| d.class != DeviceClass::Mouse) {
            return;
        }
        let device = u64::from(source);
        // X's buttons: 1 left, 2 middle, 3 right, 4 and 5 the wheel up and down, 6 and 7 sideways, 8 back, 9 forward.
        let bit = match button {
            1 => 1,
            3 => 1 << 1,
            2 => 1 << 2,
            8 => 1 << 3,
            9 => 1 << 4,
            4 | 5 if down => {
                let wheel = if button == 4 { 1.0 } else { -1.0 };
                sink.push(RawEvent::Motion { device, dx: 0, dy: 0, wheel });
                return;
            }
            _ => return,
        };
        let (pressed, released) = if down { (bit, 0) } else { (0, bit) };
        sink.push(RawEvent::Buttons { device, class: DeviceClass::Mouse, pressed, released });
    }
}

/// The class the backend reads `info` as: an enabled slave keyboard, or an enabled slave pointer whose first two
/// valuators are relative (a mouse or a touchpad; a tablet's absolute axes are the pen's, IN-10).
fn class_of(info: &XIDeviceInfo) -> Option<DeviceClass> {
    if !info.enabled {
        return None;
    }
    if info.type_ == DeviceType::SLAVE_KEYBOARD {
        return Some(DeviceClass::Keyboard);
    }
    if info.type_ != DeviceType::SLAVE_POINTER {
        return None;
    }
    let relative = |number: u16| {
        info.classes.iter().any(|c| {
            matches!(&c.data, DeviceClassData::Valuator(v) if v.number == number && v.mode == ValuatorMode::RELATIVE)
        })
    };
    (relative(0) && relative(1)).then_some(DeviceClass::Mouse)
}

/// The values of valuators 0 and 1 in a raw event: `values` holds one per bit set in `mask`, in bit order.
fn valuators(mask: &[u32], values: &[Fp3232]) -> [Option<f64>; 2] {
    let bits = mask.first().copied().unwrap_or(0);
    let value = |i: usize| values.get(i).map(|v| f64::from(v.integral) + f64::from(v.frac) / 4_294_967_296.0);
    let x = (bits & 1 != 0).then(|| value(0)).flatten();
    let y = (bits & 2 != 0).then(|| value(usize::from(bits & 1 != 0))).flatten();
    [x, y]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valuators_are_read_in_mask_order_and_only_the_first_two() {
        let half = Fp3232 { integral: 0, frac: 1 << 31 };
        let three = Fp3232 { integral: 3, frac: 0 };
        let minus = Fp3232 { integral: -2, frac: 0 };
        assert_eq!(valuators(&[0b11], &[three, minus]), [Some(3.0), Some(-2.0)]);
        assert_eq!(valuators(&[0b10], &[minus]), [None, Some(-2.0)]);
        assert_eq!(valuators(&[0b101], &[half, three]), [Some(0.5), None]);
        assert_eq!(valuators(&[0b100], &[three]), [None, None]);
        assert_eq!(valuators(&[], &[]), [None, None]);
    }
}
