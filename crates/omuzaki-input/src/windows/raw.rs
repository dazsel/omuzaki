//! The Win32 side of the Windows backend: the message-only window, the Raw Input registrations and the reads.
// Raw Input is a C API: every call below is an FFI call into user32, each with its `SAFETY` note.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use omuzaki_input_api::capture::{BackendState, CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use windows_sys::Win32::Foundation::{HANDLE, HWND};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputBuffer, GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTDEVICELIST, RAWINPUTHEADER, RAWKEYBOARD, RAWMOUSE, RID_DEVICE_INFO, RID_INPUT, RIDEV_DEVNOTIFY,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICEINFO, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GIDC_ARRIVAL, GIDC_REMOVAL, HWND_MESSAGE, MSG,
    MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT, WM_INPUT, WM_INPUT_DEVICE_CHANGE,
};

use super::{Devices, device_of, key, mouse};
use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::RawSink;

const RAW_INPUT: InputBackend = InputBackend {
    name: "windows.raw_input",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse),
    caps,
    run: Some(run),
    chosen: Chosen::Always,
};
register_input_backend!(RAW_INPUT);

/// How long the loop waits on the message queue before its idle call, which pushes finished pointer periods, while
/// input is arriving.
const POLL: Duration = Duration::from_millis(4);
/// How long it waits once two looks in a row found nothing (no pointer period is open then), so an idle capture thread
/// wakes twenty times a second. A pause that comes meanwhile is seen at the next look, and what was queued before it is
/// dropped then.
const IDLE_POLL: Duration = Duration::from_millis(50);
/// How long the loop parks between looks while capture does not read the keyboard or the mouse.
const PARK: Duration = Duration::from_millis(250);
/// The read buffer, in 8-byte words: records in it are 8-aligned on 64-bit Windows. 16 KiB holds a few hundred
/// keyboard and mouse records, more than a 1000 Hz mouse sends between two looks.
const BUFFER_WORDS: usize = 2048;
/// The generic desktop usage page and its keyboard and mouse usages.
const DESKTOP: u16 = 0x01;
const KEYBOARD_USAGE: u16 = 0x06;
const MOUSE_USAGE: u16 = 0x02;
/// What the Raw Input calls return on failure.
const FAILED: u32 = u32::MAX;

fn caps() -> CaptureCaps {
    CaptureCaps {
        typing: Support::Available,
        pointer: Support::Available,
        bindings: Support::Available,
        ..CaptureCaps::default()
    }
}

fn run(sink: RawSink) {
    run_on(sink, Os::REAL, None);
}

/// Where the backend learns which devices exist; the tests add devices of their own.
#[derive(Copy, Clone)]
pub(crate) struct Os {
    /// The keyboards and mice attached now, by Raw Input handle.
    pub(crate) attached: fn() -> Vec<(u64, DeviceClass)>,
    /// The class of device `handle`, when it is a keyboard or a mouse.
    pub(crate) class_of: fn(u64) -> Option<DeviceClass>,
}

impl Os {
    pub(crate) const REAL: Self = Self { attached, class_of };
}

/// What the tests see of a running backend: its window, and the classes it has registered (bit 0 the keyboard, bit 1
/// the mouse).
#[derive(Debug, Default)]
pub struct Probe {
    /// The message-only window's handle.
    pub window: AtomicUsize,
    /// The registered classes.
    pub registered: AtomicU8,
}

/// The loop: one message-only window on this capture thread, registered for the classes capture reads.
pub(crate) fn run_on(mut sink: RawSink, os: Os, probe: Option<Arc<Probe>>) {
    let Some(mut reader) = Reader::open(os, probe) else {
        sink.set_state(BackendState::failed("the Raw Input window could not be made"));
        return;
    };
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        let ok = if !sink.reading() {
            reader.quiet()
        } else if reader.run != Some(sink.epochs()) {
            reader.run = Some(sink.epochs());
            reader.begin(&mut sink)
        } else {
            reader.read(&mut sink);
            true
        };
        if !ok {
            sink.set_state(BackendState::failed("Windows refused the Raw Input registration"));
            return;
        }
        sink.idle();
    }
}

struct Reader {
    window: HWND,
    os: Os,
    devices: Devices,
    /// The classes registered now.
    registered: DeviceMask,
    run: Option<(u32, u32)>,
    /// Looks in a row that found nothing.
    empty_looks: u8,
    buffer: Box<[u64; BUFFER_WORDS]>,
    probe: Option<Arc<Probe>>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _removed = self.register(DeviceMask::NONE);
        // SAFETY: the window was made on this thread by `open` and nothing else destroys it.
        let _destroyed = unsafe { DestroyWindow(self.window) };
    }
}

impl Reader {
    fn open(os: Os, probe: Option<Arc<Probe>>) -> Option<Self> {
        let class: Vec<u16> = "STATIC".encode_utf16().chain([0]).collect();
        // SAFETY: a null name asks for this program's own module handle.
        let module = unsafe { GetModuleHandleW(null()) };
        // SAFETY: `class` is a NUL-terminated UTF-16 system class name that outlives the call; a message-only window
        // (parent `HWND_MESSAGE`) is never shown and gets no broadcasts, so a parked loop blocks no other program.
        let window = unsafe {
            CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), module, null())
        };
        if window.is_null() {
            return None;
        }
        if let Some(probe) = &probe {
            probe.window.store(window.addr(), Ordering::Release);
        }
        Some(Self {
            window,
            os,
            devices: Devices::default(),
            registered: DeviceMask::NONE,
            run: None,
            empty_looks: 0,
            buffer: Box::new([0; BUFFER_WORDS]),
            probe,
        })
    }

    /// Registers the keyboard and mouse collections in `mask` to this window, and removes the others.
    fn register(&mut self, mask: DeviceMask) -> bool {
        let mut changes = Vec::with_capacity(2);
        for (class, usage) in [(DeviceClass::Keyboard, KEYBOARD_USAGE), (DeviceClass::Mouse, MOUSE_USAGE)] {
            let want = mask.contains(class);
            if want == self.registered.contains(class) {
                continue;
            }
            let (flags, target) = if want { (RIDEV_INPUTSINK | RIDEV_DEVNOTIFY, self.window) } else { (RIDEV_REMOVE, null_mut()) };
            changes.push(RAWINPUTDEVICE { usUsagePage: DESKTOP, usUsage: usage, dwFlags: flags, hwndTarget: target });
        }
        if changes.is_empty() {
            return true;
        }
        let count = u32::try_from(changes.len()).unwrap_or(0);
        // SAFETY: `changes` holds `count` initialised entries of the size passed.
        let ok = unsafe { RegisterRawInputDevices(changes.as_ptr(), count, size_32::<RAWINPUTDEVICE>()) } != 0;
        if ok {
            self.registered = mask;
            if let Some(probe) = &self.probe {
                let bits = u8::from(mask.contains(DeviceClass::Keyboard)) | u8::from(mask.contains(DeviceClass::Mouse)) << 1;
                probe.registered.store(bits, Ordering::Release);
            }
        }
        ok
    }

    /// Capture is not reading: register nothing, so Windows sends no keys, and drop what was sent.
    fn quiet(&mut self) -> bool {
        if self.registered == DeviceMask::NONE {
            return true;
        }
        let ok = self.register(DeviceMask::NONE);
        let _dropped = self.pump(None);
        ok
    }

    /// A run begins: drop what came before it, tell the sink which devices are there, and register what it reads.
    fn begin(&mut self, sink: &mut RawSink) -> bool {
        if !self.register(DeviceMask::NONE) {
            return false;
        }
        let _dropped = self.pump(None);
        let mut changes = Vec::new();
        self.devices.reconcile((self.os.attached)(), |class| sink.devices(class), |raw| changes.push(raw));
        for raw in changes {
            sink.push(raw);
        }
        let asked = sink.asked();
        let reads = [DeviceClass::Keyboard, DeviceClass::Mouse]
            .into_iter()
            .filter(|c| asked.contains(*c))
            .fold(DeviceMask::NONE, DeviceMask::with);
        self.register(reads)
    }

    /// Waits for the queue, then hands every record on until the run changes.
    fn read(&mut self, sink: &mut RawSink) {
        let wait = if self.empty_looks >= 2 { IDLE_POLL } else { POLL };
        let timeout = u32::try_from(wait.as_millis()).unwrap_or(4);
        // SAFETY: no handles to wait on, only this thread's queue.
        let _woke = unsafe { MsgWaitForMultipleObjects(0, null(), 0, timeout, QS_ALLINPUT) };
        let found = self.pump(Some(sink));
        self.empty_looks = if found { 0 } else { self.empty_looks.saturating_add(1) };
    }

    /// Drains the queue: the buffered records first, then the messages (a record the buffer missed, device changes,
    /// anything else for the window). With no sink, everything is dropped. Whether there was anything.
    fn pump(&mut self, mut sink: Option<&mut RawSink>) -> bool {
        let mut found = false;
        loop {
            let mut size = size_32::<[u64; BUFFER_WORDS]>();
            let header = size_32::<RAWINPUTHEADER>();
            // SAFETY: the buffer is a writable, 8-aligned allocation of `size` bytes; the call writes at most that many
            // bytes of records into it and returns how many.
            let count = unsafe { GetRawInputBuffer(self.buffer.as_mut_ptr().cast::<RAWINPUT>(), &raw mut size, header) };
            if count == 0 || count == FAILED {
                break;
            }
            found = true;
            let mut at = 0;
            for _ in 0..count {
                if sink.as_deref().is_some_and(|s| self.run != Some(s.epochs())) {
                    sink = None;
                }
                let Some(next) = self.record(at, sink.as_deref_mut()) else { break };
                at = next;
            }
        }
        let mut msg = MSG::default();
        // SAFETY: `msg` is a writable MSG; a null window takes every message of this thread.
        while unsafe { PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            found = true;
            if sink.as_deref().is_some_and(|s| self.run != Some(s.epochs())) {
                sink = None;
            }
            match msg.message {
                WM_INPUT => {
                    let mut size = size_32::<[u64; BUFFER_WORDS]>();
                    let header = size_32::<RAWINPUTHEADER>();
                    // SAFETY: the message's `lParam` is its raw input handle; the buffer is writable for `size` bytes.
                    let got = unsafe {
                        GetRawInputData(handle(msg.lParam), RID_INPUT, self.buffer.as_mut_ptr().cast::<c_void>(), &raw mut size, header)
                    };
                    if got != 0 && got != FAILED {
                        let _end = self.record(0, sink.as_deref_mut());
                    }
                    // SAFETY: the system frees the record's data in the default procedure, as the message asks.
                    let _done = unsafe { DefWindowProcW(msg.hwnd, msg.message, msg.wParam, msg.lParam) };
                }
                WM_INPUT_DEVICE_CHANGE => {
                    if let Some(sink) = sink.as_deref_mut() {
                        let device = handle(msg.lParam).addr() as u64;
                        let change = match u32::try_from(msg.wParam).unwrap_or(0) {
                            GIDC_ARRIVAL => (self.os.class_of)(device).and_then(|c| self.devices.arrived(device, c)),
                            GIDC_REMOVAL => self.devices.removed(device),
                            _ => None,
                        };
                        if let Some(raw) = change {
                            sink.push(raw);
                        }
                    }
                }
                _ => {
                    // SAFETY: `msg` came from this thread's queue.
                    let _done = unsafe { DispatchMessageW(&raw const msg) };
                }
            }
        }
        found
    }

    /// Hands on the record at byte `at` of the buffer (dropping it with no sink); the byte the next one starts at, or
    /// `None` past the end.
    fn record(&mut self, at: usize, sink: Option<&mut RawSink>) -> Option<usize> {
        let header: RAWINPUTHEADER = self.read_at(at)?;
        let size = usize::try_from(header.dwSize).ok().filter(|s| *s >= size_of::<RAWINPUTHEADER>())?;
        let next = (at + size).next_multiple_of(8);
        let Some(sink) = sink else { return Some(next) };
        let data = at + offset_of!(RAWINPUT, data);
        let handle = header.hDevice.addr() as u64;
        match header.dwType {
            RIM_TYPEKEYBOARD => {
                let record: RAWKEYBOARD = self.read_at(data)?;
                let device = device_of(handle, DeviceClass::Keyboard);
                if let Some(up) = self.devices.arrived(handle, DeviceClass::Keyboard) {
                    sink.push(up);
                }
                if let Some(raw) = key(device, record.MakeCode, record.Flags, record.VKey) {
                    sink.push(raw);
                }
            }
            RIM_TYPEMOUSE => {
                let record: RAWMOUSE = self.read_at(data)?;
                let device = device_of(handle, DeviceClass::Mouse);
                if let Some(up) = self.devices.arrived(handle, DeviceClass::Mouse) {
                    sink.push(up);
                }
                // SAFETY: both views of the union are plain integers.
                let buttons = unsafe { record.Anonymous.Anonymous };
                let last = [record.lLastX, record.lLastY];
                mouse(device, record.usFlags, buttons.usButtonFlags, buttons.usButtonData, last, |raw| sink.push(raw));
            }
            _ => {}
        }
        Some(next)
    }

    /// The plain-data `T` at byte `at` of the buffer, or `None` past its end.
    fn read_at<T: Copy>(&self, at: usize) -> Option<T> {
        let end = at.checked_add(size_of::<T>())?;
        if end > size_of::<[u64; BUFFER_WORDS]>() {
            return None;
        }
        // SAFETY: `at..end` lies inside the buffer, which is initialised; `T` is a C record of integers and handles, for
        // which every bit pattern is valid, read unaligned.
        Some(unsafe { self.buffer.as_ptr().cast::<u8>().add(at).cast::<T>().read_unaligned() })
    }
}

/// A Win32 handle carried in a message parameter.
fn handle(value: isize) -> HANDLE {
    std::ptr::without_provenance_mut(value.cast_unsigned())
}

/// The size of `T` as the `u32` the Raw Input calls take.
fn size_32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(0)
}

/// The keyboards and mice attached now.
pub(crate) fn attached() -> Vec<(u64, DeviceClass)> {
    let entry = size_32::<RAWINPUTDEVICELIST>();
    // Devices can arrive between the count and the list: try again with the new count a few times.
    for _ in 0..4 {
        let mut count = 0u32;
        // SAFETY: a null list asks for the count only.
        if unsafe { GetRawInputDeviceList(null_mut(), &raw mut count, entry) } == FAILED {
            return Vec::new();
        }
        let mut list = vec![RAWINPUTDEVICELIST { hDevice: null_mut(), dwType: 0 }; usize::try_from(count).unwrap_or(0)];
        // SAFETY: `list` holds `count` writable entries of `entry` bytes.
        let got = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &raw mut count, entry) };
        if got != FAILED {
            list.truncate(usize::try_from(got).unwrap_or(0));
            return list.iter().filter_map(|d| class_of_type(d.dwType).map(|c| (d.hDevice.addr() as u64, c))).collect();
        }
    }
    Vec::new()
}

/// The class of device `handle`, asked of Windows.
pub(crate) fn class_of(handle: u64) -> Option<DeviceClass> {
    let mut info = RID_DEVICE_INFO { cbSize: size_32::<RID_DEVICE_INFO>(), ..RID_DEVICE_INFO::default() };
    let mut size = info.cbSize;
    let device: HANDLE = std::ptr::without_provenance_mut(usize::try_from(handle).ok()?);
    // SAFETY: `info` is a RID_DEVICE_INFO of `size` bytes with its `cbSize` set, as `RIDI_DEVICEINFO` asks.
    let got = unsafe { GetRawInputDeviceInfoW(device, RIDI_DEVICEINFO, (&raw mut info).cast::<c_void>(), &raw mut size) };
    if got == 0 || got == FAILED {
        return None;
    }
    class_of_type(info.dwType)
}

fn class_of_type(kind: u32) -> Option<DeviceClass> {
    match kind {
        RIM_TYPEKEYBOARD => Some(DeviceClass::Keyboard),
        RIM_TYPEMOUSE => Some(DeviceClass::Mouse),
        _ => None,
    }
}
