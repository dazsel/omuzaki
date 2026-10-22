//! IN-4's acceptance (design §13): the Windows Raw Input backend, driven by `SendInput` into the test's own window. The
//! crate never sends input; only this test binary does. Elsewhere this file is empty, with a message (the record
//! translation and the device bookkeeping are unit tests in `src/windows` that run everywhere).
//!
//! The budgets (delivery at most 1 ms p95, capture-thread CPU at most 0.05 % of 8 cores idle over 60 s) are checked on
//! hardware: `cargo test -p omuzaki-input --test windows -- --ignored`. The tests that run on every Windows runner
//! check delivery against a ceiling that holds on a shared runner too, and print the number.

#[cfg(not(windows))]
#[test]
fn windows_only() {
    eprintln!("skipped: the Raw Input backend's tests run on Windows");
}

#[cfg(windows)]
mod support;

#[cfg(windows)]
mod on_windows {
    // The test's window and its input are Win32 calls; helpers outside a #[test] unwrap too.
    #![allow(unsafe_code, clippy::unwrap_used)]

    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use omuzaki_input::{FAKE_KEYBOARDS, RawInputProbe};
    use omuzaki_input_api::event::{DeviceClass, DeviceMask, MotionEvent, SourceTag};
    use omuzaki_input_api::zones::KeyZone;
    use windows_sys::Win32::Foundation::{FILETIME, HWND};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetThreadTimes;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GIDC_ARRIVAL, GIDC_REMOVAL, MSG, PM_REMOVE, PeekMessageW,
        PostMessageW, SetCursorPos, SetForegroundWindow, WM_INPUT_DEVICE_CHANGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_POPUP, WS_VISIBLE,
    };

    use super::support::{Rig, keys};

    /// The longest a test waits for the capture thread: a ceiling for a failing test, never an ordering.
    const CEILING: Duration = Duration::from_secs(10);
    /// The desk test backend (keyboard, mouse and pen) runs the Raw Input loop.
    const DESK_BACKEND: usize = 0;
    /// Scan codes: A, W and F9.
    const A: u16 = 0x1E;
    const W: u16 = 0x11;
    const F9: u16 = 0x43;

    /// Waits until `f` holds, up to the ceiling, pumping the test window's messages meanwhile.
    fn until(mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < CEILING {
            pump();
            if f() {
                return true;
            }
            std::thread::park_timeout(Duration::from_millis(1));
        }
        false
    }

    fn pump() {
        let mut msg = MSG::default();
        // SAFETY: `msg` is writable; the messages are this thread's own.
        while unsafe { PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: as above.
            let _done = unsafe { DispatchMessageW(&raw const msg) };
        }
    }

    /// The test's own window, topmost at (100, 100), 200 pixels square; input clicked and typed lands in it.
    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: made on this thread by `new`.
            let _destroyed = unsafe { DestroyWindow(self.0) };
        }
    }

    impl TestWindow {
        fn new() -> Self {
            let class: Vec<u16> = "STATIC".encode_utf16().chain([0]).collect();
            let title: Vec<u16> = "omuzaki input test".encode_utf16().chain([0]).collect();
            // SAFETY: the names are NUL-terminated and outlive the call.
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP | WS_VISIBLE,
                    100,
                    100,
                    200,
                    200,
                    null_mut(),
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                )
            };
            assert!(!window.is_null(), "the test window");
            // SAFETY: a window of this thread.
            let _front = unsafe { SetForegroundWindow(window) };
            pump();
            Self(window)
        }

        /// Puts the cursor over the window and clicks it, so what is typed next lands in it.
        fn click(&self) -> bool {
            // SAFETY: a window of this thread.
            let _front = unsafe { SetForegroundWindow(self.0) };
            // SAFETY: plain coordinates.
            let _moved = unsafe { SetCursorPos(200, 200) };
            send(&[pointer(0, 0, MOUSEEVENTF_LEFTDOWN), pointer(0, 0, MOUSEEVENTF_LEFTUP)])
        }
    }

    /// Sends `inputs`; `false` where the runner has no interactive desktop to send them to.
    fn send(inputs: &[INPUT]) -> bool {
        let count = u32::try_from(inputs.len()).unwrap();
        // SAFETY: `inputs` holds `count` initialised entries of the size passed.
        let sent = unsafe { SendInput(count, inputs.as_ptr(), i32::try_from(size_of::<INPUT>()).unwrap()) };
        sent == count
    }

    fn key(scan: u16, up: bool) -> INPUT {
        let flags = KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 };
        let ki = KEYBDINPUT { wVk: 0, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } }
    }

    fn pointer(dx: i32, dy: i32, flags: u32) -> INPUT {
        let mi = MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 };
        INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi } }
    }

    fn tap(scan: u16) -> bool {
        send(&[key(scan, false), key(scan, true)])
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

    /// The Raw Input loop with capture on for `classes`, once it has registered them (bit 0 the keyboard, bit 1 the
    /// mouse).
    fn raw_input(classes: DeviceMask, want: u8) -> (Rig, Arc<RawInputProbe>, std::thread::JoinHandle<()>) {
        let rig = Rig::new();
        let (probe, thread) = rig.hub.run_raw_input(DESK_BACKEND).unwrap();
        rig.indicator.beat();
        rig.control.start(classes).unwrap();
        assert!(
            until(|| probe.registered.load(Ordering::Acquire) == want),
            "registered {}",
            probe.registered.load(Ordering::Acquire)
        );
        (rig, probe, thread)
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

    fn gone(events: &[MotionEvent]) -> Vec<SourceTag> {
        events
            .iter()
            .filter_map(|e| match *e {
                MotionEvent::SourceGone { src, .. } => Some(src),
                _ => None,
            })
            .collect()
    }

    /// Posts a device change for `handle` to the backend's window, as Windows does when a device comes or goes.
    fn device_change(probe: &RawInputProbe, arrived: bool, handle: u64) {
        let window: HWND = std::ptr::without_provenance_mut(probe.window.load(Ordering::Acquire));
        let change = if arrived { GIDC_ARRIVAL } else { GIDC_REMOVAL };
        // SAFETY: a posted message carries plain values; the window belongs to the capture thread, which is running.
        let posted = unsafe {
            PostMessageW(
                window,
                WM_INPUT_DEVICE_CHANGE,
                usize::try_from(change).unwrap(),
                isize::try_from(handle).unwrap(),
            )
        };
        assert_ne!(posted, 0, "posted");
    }

    #[test]
    fn send_input_clicks_keys_and_motion_reach_the_feed_as_buttons_zones_and_deltas() {
        let (mut rig, _probe, _thread) = raw_input(DeviceMask::ALL, 0b11);
        let window = TestWindow::new();
        if !window.click() {
            eprintln!("skipped: this runner has no interactive desktop to send input to");
            return;
        }
        let events = collect(&mut rig, |e| e.iter().filter(|e| matches!(e, MotionEvent::Buttons { .. })).count() == 2);
        let buttons: Vec<(u8, u8)> = events
            .iter()
            .filter_map(|e| match *e {
                MotionEvent::Buttons { pressed, released, .. } => Some((pressed, released)),
                _ => None,
            })
            .collect();
        assert_eq!(buttons, [(1, 0), (0, 1)]);

        assert!(tap(A) && tap(W) && tap(F9));
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

        // Relative motion right and down.
        assert!(send(&[pointer(40, 10, MOUSEEVENTF_MOVE)]));
        let events = collect(&mut rig, |e| e.iter().any(|e| matches!(e, MotionEvent::Pointer { .. })));
        let (dx, dy) = events
            .iter()
            .find_map(|e| match *e {
                MotionEvent::Pointer { dx, dy, .. } => Some((dx, dy)),
                _ => None,
            })
            .unwrap();
        assert!(dx > 0.0 && dy < 0.0, "right and down (up is positive on the pad): {dx} {dy}");
    }

    #[test]
    fn only_the_classes_asked_are_registered_and_nothing_while_paused() {
        let (mut rig, probe, _thread) = raw_input(DeviceMask::NONE.with(DeviceClass::Keyboard), 0b01);
        let window = TestWindow::new();
        if !window.click() {
            eprintln!("skipped: this runner has no interactive desktop to send input to");
            return;
        }
        assert!(send(&[pointer(40, 10, MOUSEEVENTF_MOVE)]) && tap(A));
        let events = collect(&mut rig, |e| keys(e).len() == 2);
        assert!(
            !events.iter().any(|e| matches!(e, MotionEvent::Pointer { .. } | MotionEvent::Buttons { .. })),
            "{events:?}"
        );

        rig.control.pause();
        assert!(until(|| probe.registered.load(Ordering::Acquire) == 0), "nothing is registered while paused");
        // Typed while paused: never read.
        assert!(tap(W));
        rig.start();
        assert!(until(|| probe.registered.load(Ordering::Acquire) == 0b11));
        assert!(tap(A));
        let events = collect(&mut rig, |e| keys(e).len() == 2);
        let zones: Vec<KeyZone> = keys(&events).iter().map(|(_, zone, _)| *zone).collect();
        assert_eq!(zones, [KeyZone::LEFT_OUTER_LOWER; 2], "the key typed while paused never arrives: {events:?}");

        rig.control.stop();
        assert!(until(|| probe.registered.load(Ordering::Acquire) == 0), "nothing is registered while off");
    }

    #[test]
    fn two_keyboards_are_two_sources_and_unplugging_one_ends_only_it() {
        let (mut rig, probe, _thread) = raw_input(DeviceMask::NONE.with(DeviceClass::Keyboard), 0b01);
        // The two test keyboards and the synthetic one at least.
        let before =
            sources(&collect(&mut rig, |e| sources(e, DeviceClass::Keyboard).len() >= 3), DeviceClass::Keyboard);
        device_change(&probe, false, FAKE_KEYBOARDS[0]);
        // Windows may report a removal twice; then the keyboard comes back. Its new `SourceUp` orders the test after
        // both removals.
        device_change(&probe, false, FAKE_KEYBOARDS[0]);
        device_change(&probe, true, FAKE_KEYBOARDS[0]);
        let events = collect(&mut rig, |e| !sources(e, DeviceClass::Keyboard).is_empty());
        let left = gone(&events);
        assert_eq!(left.len(), 1, "one keyboard left, once: {events:?}");
        assert!(before.contains(&left[0]));
        assert_eq!(sources(&events, DeviceClass::Keyboard).len(), 1, "and came back as one source: {events:?}");
    }

    /// The delivery time of `taps` key presses: from the `SendInput` call to the key on the feed, which bounds the time
    /// to the ring from above.
    fn delivery(rig: &mut Rig, taps: usize) -> Option<Duration> {
        let mut times = Vec::with_capacity(taps);
        for _ in 0..taps {
            let start = Instant::now();
            if !send(&[key(F9, false)]) {
                return None;
            }
            let mut seen = false;
            while !seen && start.elapsed() < CEILING {
                seen = keys(&rig.motion().0).iter().any(|(_, _, down)| *down);
            }
            times.push(start.elapsed());
            send(&[key(F9, true)]);
            assert!(until(|| keys(&rig.motion().0).iter().any(|(_, _, down)| !*down)));
        }
        times.sort();
        times.get(taps * 95 / 100).copied()
    }

    #[test]
    fn delivery_from_send_input_to_the_feed_stays_under_a_runner_ceiling() {
        let (mut rig, _probe, _thread) = raw_input(DeviceMask::NONE.with(DeviceClass::Keyboard), 0b01);
        let window = TestWindow::new();
        if !window.click() {
            eprintln!("skipped: this runner has no interactive desktop to send input to");
            return;
        }
        let p95 = delivery(&mut rig, 100).unwrap();
        eprintln!("delivery p95: {} us", p95.as_micros());
        // A shared runner's ceiling; the budget is the hardware test's.
        assert!(p95 < Duration::from_millis(20), "{p95:?}");
    }

    /// The thread's CPU time so far (kernel plus user).
    fn cpu(thread: &std::thread::JoinHandle<()>) -> Duration {
        let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        let [created, exited, kernel, user] = &mut times;
        // SAFETY: a live thread's handle and four writable FILETIMEs.
        let ok = unsafe { GetThreadTimes(thread.as_raw_handle(), created, exited, kernel, user) };
        assert_ne!(ok, 0);
        let ticks = |t: &FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
        Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
    }

    #[test]
    #[ignore = "hardware: the §10 budgets, on a quiet machine"]
    fn budgets_on_hardware() {
        let (mut rig, _probe, thread) = raw_input(DeviceMask::ALL, 0b11);
        let window = TestWindow::new();
        assert!(window.click(), "an interactive desktop");
        let p95 = delivery(&mut rig, 1000).unwrap();
        eprintln!("delivery p95: {} us", p95.as_micros());
        assert!(p95 <= Duration::from_millis(1), "{p95:?}");

        rig.drain_all();
        let before = cpu(&thread);
        let span = Duration::from_secs(60);
        std::thread::sleep(span);
        let used = cpu(&thread).saturating_sub(before);
        let share = used.as_secs_f64() / (span.as_secs_f64() * 8.0);
        eprintln!("idle capture thread: {} ms over 60 s, {:.4} % of 8 cores", used.as_millis(), share * 100.0);
        assert!(share <= 0.0005, "{share}");
    }
}
