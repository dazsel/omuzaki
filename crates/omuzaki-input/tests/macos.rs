//! IN-7's acceptance (design §13), on a Mac: `cargo test -p omuzaki-input --features test-hooks --test macos`. Without
//! Input Monitoring the backend asks for it and reads nothing; with it (and the permission to post events, which only
//! this test binary uses), posted keys and motion reach the feed. The rest is the manual checklist in the pull
//! request: the prompt shows once, a disabled tap recovers, and secure input yields no keys. Elsewhere this file is
//! empty, with a message; the translation's tests in `src/macos` run everywhere.

#[cfg(not(target_os = "macos"))]
#[test]
fn macos_only() {
    eprintln!("skipped: the event tap's tests run on macOS");
}

#[cfg(target_os = "macos")]
mod support;

#[cfg(target_os = "macos")]
mod on_macos {
    #![allow(clippy::unwrap_used)]

    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGMouseButton, CGPreflightListenEventAccess,
        CGPreflightPostEventAccess,
    };
    use omuzaki_input_api::capture::{BackendState, PermissionHow};
    use omuzaki_input_api::event::MotionEvent;
    use omuzaki_input_api::zones::KeyZone;

    use super::support::{Rig, keys};

    /// The longest a test waits for the capture thread: a ceiling for a failing test, never an ordering.
    const CEILING: Duration = Duration::from_secs(10);
    /// The desk test backend (keyboard, mouse and pen) runs the tap loop.
    const DESK_BACKEND: usize = 0;

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

    fn state(rig: &Rig) -> Option<BackendState> {
        rig.control.report().backends.first().map(|b| b.state.clone())
    }

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

    fn tap(code: u16) {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(None, code, down).unwrap();
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }

    #[test]
    fn without_input_monitoring_the_tap_asks_for_it_and_reads_nothing() {
        if CGPreflightListenEventAccess() {
            eprintln!("skipped: Input Monitoring is granted here");
            return;
        }
        let mut rig = Rig::new();
        let _thread = rig.hub.run_event_tap(DESK_BACKEND).unwrap();
        rig.start();
        let wanted = BackendState::needs_permission(PermissionHow::InputMonitoring);
        assert!(until(|| state(&rig) == Some(wanted.clone())), "{:?}", state(&rig));
        assert!(rig.motion().0.is_empty(), "nothing is read");
    }

    #[test]
    fn posted_keys_and_motion_reach_the_feed_as_zones_and_deltas() {
        if !CGPreflightListenEventAccess() || !CGPreflightPostEventAccess() {
            eprintln!("skipped: grant Input Monitoring and Accessibility to the terminal running the test");
            return;
        }
        let mut rig = Rig::new();
        let _thread = rig.hub.run_event_tap(DESK_BACKEND).unwrap();
        rig.start();
        assert!(until(|| state(&rig) == Some(BackendState::Running)), "{:?}", state(&rig));
        // kVK_ANSI_A, kVK_ANSI_W and kVK_F9.
        tap(0x00);
        tap(0x0D);
        tap(0x65);
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

        let point = CGPoint { x: 200.0, y: 200.0 };
        let moved = CGEvent::new_mouse_event(None, CGEventType::MouseMoved, point, CGMouseButton::Left).unwrap();
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaX, 40);
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaY, 10);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&moved));
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
}
