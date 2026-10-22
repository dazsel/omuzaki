//! IN-6's acceptance (design §13): the X11 backend on an Xvfb server driven by `XTest` (skipped with a message where the
//! runner has no Xvfb), and the evdev backend on a `uinput` device where the runner allows one.
#![cfg(target_os = "linux")]
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use omuzaki_input_api::event::{DeviceClass, DeviceMask, MotionEvent, SourceTag};
use omuzaki_input_api::zones::KeyZone;
use support::{Rig, keys};
use x11rb::connection::Connection;
use x11rb::protocol::xinput::{
    ChangeMode, ConnectionExt as _, DeviceType, HierarchyChange, HierarchyChangeData, HierarchyChangeDataAddMaster,
    HierarchyChangeDataRemoveMaster, XIEventMask,
};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// The longest a test waits for the capture thread: a ceiling for a failing test, never an ordering.
const CEILING: Duration = Duration::from_secs(10);
/// The desk test backend (keyboard, mouse and pen) runs the X11 loop.
const DESK_BACKEND: usize = 0;

/// An Xvfb server for one test, stopped when dropped.
struct Xvfb {
    child: Child,
    display: String,
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// A fresh server on a display it picks itself (`-displayfd`), or `None` with a message where there is no Xvfb.
fn xvfb() -> Option<Xvfb> {
    let child = Command::new("Xvfb")
        .args(["-displayfd", "1", "-nolisten", "tcp", "-noreset", "-screen", "0", "640x480x24"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("skipped: no Xvfb on this runner");
        return None;
    };
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let display = format!(":{}", line.trim());
    Some(Xvfb { child, display })
}

/// Waits until `f` holds, up to the ceiling.
fn until(mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < CEILING {
        if f() {
            return true;
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
    false
}

/// The raw key events, as the backend selects them.
fn key_events() -> u32 {
    u32::from(XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE)
}

/// The raw buttons and motion.
fn pointer_events() -> u32 {
    u32::from(XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_BUTTON_RELEASE | XIEventMask::RAW_MOTION)
}

/// The test's own client, which types and moves through `XTest`.
struct Typist {
    conn: RustConnection,
    root: xproto::Window,
}

impl Typist {
    fn new(display: &str) -> Self {
        let (conn, screen) = RustConnection::connect(Some(display)).unwrap();
        let root = conn.setup().roots[screen].root;
        Self { conn, root }
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) {
        self.conn.xtest_fake_input(kind, detail, 0, self.root, x, y, 0).unwrap();
    }

    /// Linux key code `code` down then up (X keycodes are the kernel's plus 8).
    fn tap(&self, code: u8) {
        self.fake(xproto::KEY_PRESS_EVENT, code + 8, 0, 0);
        self.fake(xproto::KEY_RELEASE_EVENT, code + 8, 0, 0);
        self.sync();
    }

    /// Waits until the server has handled everything sent.
    fn sync(&self) {
        self.conn.get_input_focus().unwrap().reply().unwrap();
    }
}

/// Everything the motion feed gives until `done` holds for it, up to the ceiling; the clock moves on, so pointer
/// periods end.
fn collect(rig: &mut Rig, mut done: impl FnMut(&[MotionEvent]) -> bool) -> Vec<MotionEvent> {
    let mut all = Vec::new();
    let found = until(|| {
        rig.advance(0.01);
        all.extend(rig.motion().0);
        done(&all)
    });
    assert!(found, "the feed never showed it: {all:?}");
    all
}

/// The X11 loop on a fresh server with capture on for `classes`, once it has selected what they need.
fn x11(classes: DeviceMask, want: u32) -> Option<(Rig, Xvfb, std::sync::Arc<AtomicU32>)> {
    let server = xvfb()?;
    let rig = Rig::new();
    let (probe, _thread) = rig.hub.run_x11(DESK_BACKEND, &server.display).unwrap();
    rig.indicator.beat();
    rig.control.start(classes).unwrap();
    assert!(until(|| probe.load(Ordering::Acquire) == want), "selected {:#x}", probe.load(Ordering::Acquire));
    Some((rig, server, probe))
}

fn sources(events: &[MotionEvent], class: DeviceClass) -> Vec<SourceTag> {
    events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::SourceUp { src, class: c, .. } if c == class => Some(src),
            _ => None,
        })
        .collect()
}

#[test]
fn xtest_keys_and_motion_reach_the_feed_as_zones_deltas_and_buttons() {
    let Some((mut rig, server, _probe)) = x11(DeviceMask::ALL, key_events() | pointer_events()) else { return };
    let typist = Typist::new(&server.display);
    // KEY_A, KEY_W and KEY_F9.
    typist.tap(30);
    typist.tap(17);
    typist.tap(67);
    let events = collect(&mut rig, |e| keys(e).len() == 6);
    let zones: Vec<(KeyZone, bool)> = keys(&events).iter().map(|(_, zone, down)| (*zone, *down)).collect();
    assert_eq!(
        zones,
        [
            (KeyZone::LEFT_OUTER_LOWER, true),
            (KeyZone::LEFT_OUTER_LOWER, false),
            (KeyZone::LEFT_OUTER_UPPER, true),
            (KeyZone::LEFT_OUTER_UPPER, false),
            (KeyZone::FUNCTION, true),
            (KeyZone::FUNCTION, false),
        ]
    );
    assert!(!sources(&events, DeviceClass::Keyboard).is_empty(), "the XTest keyboard came up: {events:?}");

    // Relative motion right and down, then the primary button.
    typist.fake(xproto::MOTION_NOTIFY_EVENT, 1, 40, 10);
    typist.sync();
    let events = collect(&mut rig, |e| e.iter().any(|e| matches!(e, MotionEvent::Pointer { .. })));
    let (dx, dy) = events
        .iter()
        .find_map(|e| match *e {
            MotionEvent::Pointer { dx, dy, .. } => Some((dx, dy)),
            _ => None,
        })
        .unwrap();
    assert!(dx > 0.0 && dy < 0.0, "right and down (up is positive on the pad): {dx} {dy}");
    typist.fake(xproto::BUTTON_PRESS_EVENT, 1, 0, 0);
    typist.fake(xproto::BUTTON_RELEASE_EVENT, 1, 0, 0);
    typist.sync();
    let events = collect(&mut rig, |e| e.iter().filter(|e| matches!(e, MotionEvent::Buttons { .. })).count() == 2);
    let buttons: Vec<(u8, u8)> = events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::Buttons { pressed, released, .. } => Some((pressed, released)),
            _ => None,
        })
        .collect();
    assert_eq!(buttons, [(1, 0), (0, 1)]);
}

#[test]
fn raw_motion_is_selected_only_while_the_mouse_is_read_and_nothing_while_paused() {
    let Some((mut rig, server, probe)) = x11(DeviceMask::NONE.with(DeviceClass::Keyboard), key_events()) else {
        return;
    };
    let typist = Typist::new(&server.display);
    typist.fake(xproto::MOTION_NOTIFY_EVENT, 1, 40, 10);
    typist.tap(30);
    let events = collect(&mut rig, |e| keys(e).len() == 2);
    assert!(!events.iter().any(|e| matches!(e, MotionEvent::Pointer { .. })), "{events:?}");

    rig.control.pause();
    assert!(until(|| probe.load(Ordering::Acquire) == 0), "nothing is selected while paused");
    // Typed while paused: never read.
    typist.tap(17);
    rig.start();
    assert!(until(|| probe.load(Ordering::Acquire) == key_events() | pointer_events()));
    typist.tap(30);
    let events = collect(&mut rig, |e| keys(e).len() == 2);
    let zones: Vec<KeyZone> = keys(&events).iter().map(|(_, zone, _)| *zone).collect();
    assert_eq!(zones, [KeyZone::LEFT_OUTER_LOWER; 2], "the key typed while paused never arrives: {events:?}");

    rig.control.stop();
    assert!(until(|| probe.load(Ordering::Acquire) == 0), "nothing is selected while off");
}

/// A master device pair named `name`, made and removed through XI2's hierarchy requests: its `XTest` keyboard and
/// pointer come and go like plugged devices.
fn add_master(conn: &RustConnection, name: &str) {
    let name = name.as_bytes().to_vec();
    let len = u16::try_from((8 + name.len().next_multiple_of(4)) / 4).unwrap();
    let add = HierarchyChangeDataAddMaster { send_core: true, enable: true, name };
    let change = HierarchyChange { len, data: HierarchyChangeData::AddMaster(add) };
    conn.xinput_xi_change_hierarchy(&[change]).unwrap().check().unwrap();
}

fn remove_master(conn: &RustConnection, name: &str) {
    let devices = conn.xinput_xi_query_device(0u16).unwrap().reply().unwrap();
    let master = devices
        .infos
        .iter()
        .find(|d| d.type_ == DeviceType::MASTER_POINTER && d.name.starts_with(name.as_bytes()))
        .unwrap();
    let remove = HierarchyChangeDataRemoveMaster {
        deviceid: master.deviceid,
        return_mode: ChangeMode::FLOAT,
        return_pointer: 0,
        return_keyboard: 0,
    };
    let change = HierarchyChange { len: 3, data: HierarchyChangeData::RemoveMaster(remove) };
    conn.xinput_xi_change_hierarchy(&[change]).unwrap().check().unwrap();
}

#[test]
fn a_device_plugged_in_comes_up_as_its_own_source_and_unplugging_it_ends_only_it() {
    let Some((mut rig, server, _probe)) = x11(DeviceMask::ALL, key_events() | pointer_events()) else { return };
    let typist = Typist::new(&server.display);
    let first = collect(&mut rig, |e| !sources(e, DeviceClass::Keyboard).is_empty());
    let before = sources(&first, DeviceClass::Keyboard);
    add_master(&typist.conn, "second");
    let events = collect(&mut rig, |e| !sources(e, DeviceClass::Keyboard).is_empty());
    let added = sources(&events, DeviceClass::Keyboard);
    assert!(added.iter().all(|s| !before.contains(s)), "a new source: {before:?} {added:?}");
    remove_master(&typist.conn, "second");
    let events = collect(&mut rig, |e| e.iter().any(|e| matches!(e, MotionEvent::SourceGone { .. })));
    let gone: Vec<SourceTag> = events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::SourceGone { src, .. } => Some(src),
            _ => None,
        })
        .collect();
    assert!(gone.iter().all(|s| !before.contains(s)), "only the plugged device left: {gone:?}");
    // The first keyboard still types.
    typist.tap(30);
    let events = collect(&mut rig, |e| keys(e).len() == 2);
    assert!(keys(&events).iter().all(|(src, _, _)| before.contains(src)));
}

/// A virtual keyboard and mouse through `uinput`, or `None` with a message where the runner does not allow one.
fn virtual_desk(name: &str) -> Option<(evdev::uinput::VirtualDevice, evdev::uinput::VirtualDevice)> {
    use evdev::{AttributeSet, KeyCode, RelativeAxisCode};
    let make = || -> std::io::Result<_> {
        let keys: AttributeSet<KeyCode> = [KeyCode::KEY_A, KeyCode::KEY_W, KeyCode::KEY_SPACE].into_iter().collect();
        let keyboard = evdev::uinput::VirtualDevice::builder()?.name(name).with_keys(&keys)?.build()?;
        let buttons: AttributeSet<KeyCode> = [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT].into_iter().collect();
        let axes: AttributeSet<RelativeAxisCode> =
            [RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y].into_iter().collect();
        let mouse = evdev::uinput::VirtualDevice::builder()?
            .name(name)
            .with_keys(&buttons)?
            .with_relative_axes(&axes)?
            .build()?;
        Ok((keyboard, mouse))
    };
    match make() {
        Ok(devices) => Some(devices),
        Err(e) => {
            eprintln!("skipped: no uinput device on this runner ({:?})", e.kind());
            None
        }
    }
}

#[test]
fn evdev_reads_a_virtual_keyboard_and_mouse_and_sees_them_leave() {
    use evdev::{EventType, InputEvent, KeyCode, RelativeAxisCode};
    const NAME: &str = "omuzaki evdev test";
    let Some((mut keyboard, mut mouse)) = virtual_desk(NAME) else { return };
    let mut rig = Rig::new();
    let _thread = rig.hub.run_evdev(DESK_BACKEND, NAME).unwrap();
    rig.start();
    // The devices' nodes appear asynchronously; the backend opens them when it sees them.
    let ups = collect(&mut rig, |e| {
        !sources(e, DeviceClass::Keyboard).is_empty() && !sources(e, DeviceClass::Mouse).is_empty()
    });
    let pressed = |code: KeyCode, value| InputEvent::new(EventType::KEY.0, code.0, value);
    keyboard.emit(&[pressed(KeyCode::KEY_A, 1)]).unwrap();
    keyboard.emit(&[pressed(KeyCode::KEY_A, 0)]).unwrap();
    keyboard.emit(&[pressed(KeyCode::KEY_W, 1)]).unwrap();
    keyboard.emit(&[pressed(KeyCode::KEY_W, 0)]).unwrap();
    let events = collect(&mut rig, |e| keys(e).len() == 4);
    let zones: Vec<KeyZone> = keys(&events).iter().map(|(_, zone, _)| *zone).collect();
    assert_eq!(
        zones,
        [KeyZone::LEFT_OUTER_LOWER, KeyZone::LEFT_OUTER_LOWER, KeyZone::LEFT_OUTER_UPPER, KeyZone::LEFT_OUTER_UPPER]
    );

    mouse.emit(&[InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, 40)]).unwrap();
    mouse.emit(&[pressed(KeyCode::BTN_LEFT, 1), pressed(KeyCode::BTN_LEFT, 0)]).unwrap();
    let events = collect(&mut rig, |e| {
        e.iter().any(|e| matches!(e, MotionEvent::Pointer { .. }))
            && e.iter().filter(|e| matches!(e, MotionEvent::Buttons { .. })).count() == 2
    });
    assert!(events.iter().any(|e| matches!(*e, MotionEvent::Pointer { dx, .. } if dx > 0.0)), "{events:?}");

    let keyboard_src = sources(&ups, DeviceClass::Keyboard)[0];
    drop(keyboard);
    let events = collect(&mut rig, |e| e.iter().any(|e| matches!(e, MotionEvent::SourceGone { .. })));
    assert!(events.iter().all(|e| !matches!(*e, MotionEvent::SourceGone { src, .. } if src != keyboard_src)));
}
