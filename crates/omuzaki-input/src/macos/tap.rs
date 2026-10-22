//! The Core Graphics side of the macOS backend: the permission check, the listen-only tap and its run loop.
// The tap's callback is a C function handed a pointer to the backend's own context; each call has its `SAFETY` note.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Duration;

use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
    CGPreflightListenEventAccess, CGRequestListenEventAccess,
};
use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestType};
use omuzaki_input_api::capture::{BackendState, CaptureCaps, PermissionHow, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};

use super::{Fields, KEYBOARD, MOUSE, mask, translate};
use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::{RawEvent, RawSink};

const EVENT_TAP: InputBackend = InputBackend {
    name: "macos.event_tap",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse),
    caps,
    run: Some(run),
    chosen: Chosen::Always,
};
register_input_backend!(EVENT_TAP);

/// How long one turn of the run loop lasts while events arrive; the idle call after it pushes finished pointer periods.
const POLL: Duration = Duration::from_millis(4);
/// How long it lasts once two turns in a row brought nothing, so an idle capture thread wakes twenty times a second.
const IDLE_POLL: Duration = Duration::from_millis(50);
/// How long the loop parks between looks while capture does not read, or while the permission is missing.
const PARK: Duration = Duration::from_millis(250);
/// The most events one turn keeps; a turn of a few milliseconds sees far fewer.
const TURN_EVENTS: usize = 1024;

/// Whether the person has granted Input Monitoring. Core Graphics' answer can lag a grant, so `IOKit`'s is asked too.
fn permitted() -> bool {
    CGPreflightListenEventAccess() || IOHIDCheckAccess(IOHIDRequestType::ListenEvent) == IOHIDAccessType::Granted
}

fn caps() -> CaptureCaps {
    let support = if permitted() { Support::Available } else { Support::NeedsPermission };
    CaptureCaps { typing: support, pointer: support, bindings: support, ..CaptureCaps::default() }
}

pub(crate) fn run(mut sink: RawSink) {
    let mut tap: Option<Tap> = None;
    let mut run: Option<(u32, u32)> = None;
    let mut asked_once = false;
    let mut empty_turns = 0u8;
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        if !sink.reading() {
            tap = None;
            sink.idle();
            continue;
        }
        if run != Some(sink.epochs()) {
            tap = None;
            if !permitted() {
                // The prompt shows once per install whatever we do; ask the first time a person starts capture.
                if !asked_once {
                    asked_once = true;
                    let _shown = CGRequestListenEventAccess();
                }
                sink.set_state(BackendState::needs_permission(PermissionHow::InputMonitoring));
                std::thread::park_timeout(PARK);
                continue;
            }
            let asked = sink.asked();
            let Some(made) = Tap::new(mask(asked.contains(DeviceClass::Keyboard), asked.contains(DeviceClass::Mouse)))
            else {
                sink.set_state(BackendState::needs_permission(PermissionHow::InputMonitoring));
                std::thread::park_timeout(PARK);
                continue;
            };
            run = Some(sink.epochs());
            tap = Some(made);
            sink.set_state(BackendState::Running);
            // A tap cannot tell devices apart: one keyboard and one mouse, there from the start.
            sink.push(RawEvent::DeviceUp { device: KEYBOARD, class: DeviceClass::Keyboard });
            sink.push(RawEvent::DeviceUp { device: MOUSE, class: DeviceClass::Mouse });
        }
        if let Some(tap) = &tap {
            let turn = if empty_turns >= 2 { IDLE_POLL } else { POLL };
            let events = tap.turn(turn);
            empty_turns = if events.is_empty() { empty_turns.saturating_add(1) } else { 0 };
            for fields in &events {
                // A pause that came and went during the turn: what it caught belongs before the new run.
                if run != Some(sink.epochs()) {
                    break;
                }
                translate(fields, |raw| sink.push(raw));
            }
        }
        sink.idle();
    }
}

/// What the callback writes to: the events of the current turn, and whether macOS disabled the tap.
#[derive(Default)]
struct Context {
    events: RefCell<Vec<Fields>>,
    disabled: Cell<bool>,
}

/// A listen-only tap on this thread's run loop. Dropping it disables and removes it.
struct Tap {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
    /// Boxed so its address, which the tap holds, never moves; dropped after the tap is gone.
    context: Box<Context>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.port, false);
        // SAFETY: `kCFRunLoopDefaultMode` is a constant string Core Foundation defines.
        let mode = unsafe { kCFRunLoopDefaultMode };
        self.run_loop.remove_source(Some(&self.source), mode);
        self.port.invalidate();
    }
}

impl Tap {
    /// A session tap listening for the events in `mask`; `None` without the permission.
    fn new(mask: u64) -> Option<Self> {
        let context = Box::<Context>::default();
        let user = std::ptr::from_ref::<Context>(&context).cast_mut().cast::<c_void>();
        // SAFETY: `callback` matches `CGEventTapCallBack`, and `user` points at the boxed context, which the returned
        // tap owns and drops only after the tap is invalidated.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                mask,
                Some(callback),
                user,
            )
        }?;
        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)?;
        let run_loop = CFRunLoop::current()?;
        // SAFETY: as in `drop`.
        let mode = unsafe { kCFRunLoopDefaultMode };
        run_loop.add_source(Some(&source), mode);
        CGEvent::tap_enable(&port, true);
        Some(Self { port, source, run_loop, context })
    }

    /// Runs this thread's run loop for `turn`, re-enables the tap if macOS disabled it, and hands back what it saw.
    fn turn(&self, turn: Duration) -> Vec<Fields> {
        // SAFETY: as in `drop`.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _result = CFRunLoop::run_in_mode(mode, turn.as_secs_f64(), false);
        if self.context.disabled.replace(false) {
            CGEvent::tap_enable(&self.port, true);
        }
        self.context.events.try_borrow_mut().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }
}

/// The tap's callback: copies the fields the backend reads and hands the event back untouched (a listen-only tap
/// cannot change it).
unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user` is the context the tap was made with, alive while the tap is; the callback runs on the thread
    // that owns it, inside `turn`, which holds no borrow of it.
    let context = unsafe { &*user.cast::<Context>() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        context.disabled.set(true);
        return event.as_ptr();
    }
    // SAFETY: the event is valid for the length of the call.
    let read = unsafe { event.as_ref() };
    let field = |f: CGEventField| CGEvent::integer_value_field(Some(read), f);
    let fields = Fields {
        kind: kind.0,
        keycode: u16::try_from(field(CGEventField::KeyboardEventKeycode)).unwrap_or(u16::MAX),
        repeat: field(CGEventField::KeyboardEventAutorepeat) != 0,
        flags: CGEvent::flags(Some(read)).0,
        button: field(CGEventField::MouseEventButtonNumber),
        delta: [field(CGEventField::MouseEventDeltaX), field(CGEventField::MouseEventDeltaY)],
        scroll: CGEvent::double_value_field(Some(read), CGEventField::ScrollWheelEventFixedPtDeltaAxis1),
    };
    if let Ok(mut events) = context.events.try_borrow_mut()
        && events.len() < TURN_EVENTS
    {
        events.push(fields);
    }
    event.as_ptr()
}
