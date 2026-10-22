//! IN-3's acceptance (design §13): the gate and the indicator, the safety loop, the epoch window, what is held across
//! a pause, one backend leaving, single writers, frames that never drop and the classes asked. `test-hooks` staying
//! private and the forbidden symbols of §3 rule 4 are in `tests/fences.rs`.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds, SystemClock};
use omuzaki_input::{FakeProcesses, Hub, HubConfig};
use omuzaki_input_api::binding::{BindingAction, Edge};
use omuzaki_input_api::capture::{CaptureState, InputError, PauseWhy};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, PadId, PadZones};
use omuzaki_input_api::feed::InputHub;
use support::{A, CTRL, DESK, F9, J, KEYBOARD, LEFT_CTRL, PAD, PADS, PEN, Rig, W, edges, keys};

fn paused(why: Option<PauseWhy>) -> CaptureState {
    CaptureState::paused(why)
}

#[test]
fn start_needs_an_installed_indicator_that_is_drawn() {
    let clock = Arc::new(FakeClock::new(Seconds(5.0)));
    let config = HubConfig::new(clock.clone(), None).with_test_backends(&[DESK, PADS]).manual();
    let hub = Hub::new(config).unwrap();
    let control = hub.control();
    assert_eq!(control.start(DeviceMask::ALL), Err(InputError::NoIndicator), "no indicator installed");
    let indicator = hub.indicator_host().install();
    assert_eq!(indicator.view().state, CaptureState::Off, "an installed indicator shows Off before start");
    assert_eq!(control.start(DeviceMask::ALL), Err(InputError::NoIndicator), "installed, never drawn");
    indicator.beat();
    assert_eq!(control.start(DeviceMask::ALL), Ok(()));
    assert!(control.report().state.is_on());
    control.stop();
    clock.advance(Seconds(1.6));
    assert_eq!(control.start(DeviceMask::ALL), Err(InputError::NoIndicator), "a stale heartbeat");
    assert_eq!(control.report().state, CaptureState::Off);
}

#[test]
fn a_stale_indicator_pauses_and_its_return_resumes_but_an_asked_pause_stays() {
    let mut rig = Rig::started();
    rig.advance(1.4);
    rig.hub.tick();
    assert!(rig.state().is_on(), "1.4 s is still shown");
    rig.advance(0.2);
    rig.hub.tick();
    assert_eq!(rig.state(), paused(Some(PauseWhy::IndicatorLost)));
    assert_eq!(rig.indicator.view().state, paused(Some(PauseWhy::IndicatorLost)), "the view says why");
    assert_eq!(rig.control.report().state.redacted(), paused(None), "redacted for actors without `process`");
    rig.indicator.beat();
    rig.hub.tick();
    assert!(rig.state().is_on(), "the hub's own pause undoes itself");

    rig.control.pause();
    assert_eq!(rig.state(), paused(None));
    rig.advance(5.0);
    rig.indicator.beat();
    rig.hub.tick();
    assert_eq!(rig.state(), paused(None), "an asked pause waits for a person's start");
    rig.start();
    assert!(rig.state().is_on());
    rig.control.pause();
    rig.control.pause();
    assert_eq!(rig.state(), paused(None), "pause is idempotent");
    rig.control.stop();
    rig.control.stop();
    assert_eq!(rig.state(), CaptureState::Off, "stop is idempotent");
    rig.drain_all();
}

#[test]
fn a_protected_program_pauses_capture_while_it_runs_and_its_name_stays_on_the_indicator() {
    let mut rig = Rig::started();
    rig.control.protect(&["Vault.exe".to_owned()]);
    rig.processes.set(&["shell", "vault"]);
    rig.hub.tick();
    assert_eq!(rig.state(), paused(Some(PauseWhy::ProtectedApp)));
    assert_eq!(rig.indicator.view().program.as_deref(), Some("Vault.exe"), "the person's own indicator names it");
    assert_eq!(rig.state().redacted(), paused(None));
    rig.processes.set(&["shell"]);
    rig.advance(0.5);
    rig.indicator.beat();
    rig.hub.tick();
    assert_eq!(rig.state(), paused(Some(PauseWhy::ProtectedApp)), "checked again only on the next period");
    rig.advance(0.6);
    rig.indicator.beat();
    rig.hub.tick();
    assert!(rig.state().is_on(), "resumed once the program is gone");
    assert_eq!(rig.indicator.view().program, None);
    rig.drain_all();
}

/// Polls `f` every 5 ms until it holds or `limit` passes; the time it took.
fn until(limit: Duration, f: impl Fn() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if f() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn the_safety_loop_pauses_within_its_ceilings_with_a_hung_surface_and_stalled_feeds() {
    let processes = FakeProcesses::default();
    let config = HubConfig::new(Arc::new(SystemClock::new()), None)
        .with_test_backends(&[DESK, PADS])
        .with_processes(processes.clone());
    let hub = Hub::new(config).unwrap();
    let control = hub.control();
    // The feeds are taken and never drained: every consumer is stalled.
    let _stalled = hub.take_feeds().unwrap();
    let indicator = hub.indicator_host().install();

    // A surface that draws, then hangs: its heartbeat stops.
    indicator.beat();
    control.start(DeviceMask::ALL).unwrap();
    let last_beat = Instant::now();
    let paused_after =
        until(Duration::from_secs(5), || control.report().state == paused(Some(PauseWhy::IndicatorLost))).unwrap();
    let after_stale = (last_beat.elapsed().saturating_sub(Duration::from_millis(1500))).min(paused_after);
    println!("indicator: paused {} ms after the heartbeat went stale", after_stale.as_millis());
    assert!(after_stale <= Duration::from_millis(1100), "paused {after_stale:?} after the heartbeat went stale");

    // A surface that keeps drawing, and a protected program that starts.
    let drawing = Arc::new(AtomicBool::new(true));
    let surface = {
        let (indicator, drawing) = (indicator.clone(), Arc::clone(&drawing));
        std::thread::spawn(move || {
            while drawing.load(Ordering::Acquire) {
                indicator.beat();
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    assert!(until(Duration::from_secs(3), || control.report().state.is_on()).is_some(), "resumed on its own");
    control.protect(&["vault".to_owned()]);
    // Let the check the protect call asked for run first, with nothing protected running.
    std::thread::sleep(Duration::from_millis(300));
    processes.set(&["vault"]);
    let paused_after =
        until(Duration::from_secs(5), || control.report().state == paused(Some(PauseWhy::ProtectedApp))).unwrap();
    println!("kill switch: paused {} ms after the program started", paused_after.as_millis());
    assert!(paused_after <= Duration::from_millis(2100), "paused {paused_after:?} after the program started");
    processes.set(&[]);
    assert!(until(Duration::from_secs(3), || control.report().state.is_on()).is_some(), "resumed after it ended");
    drawing.store(false, Ordering::Release);
    surface.join().unwrap();
}

#[test]
fn a_press_read_around_a_pause_never_reaches_a_feed() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(1, F9, 0);

    // Read after the bump: nothing is read.
    rig.control.pause();
    assert!(!rig.desk.read_key(KEYBOARD, F9, true), "a paused backend reads nothing");
    rig.start();
    assert!(rig.hits().is_empty());
    assert!(keys(&rig.motion().0).is_empty());

    // Read just before the bump, pushed after it.
    assert!(rig.desk.read_key(KEYBOARD, F9, true));
    rig.control.pause();
    rig.desk.push_read();
    rig.start();
    assert!(rig.hits().is_empty(), "the press carries a closed run's epoch");
    assert!(keys(&rig.motion().0).is_empty());
    // The key is unknown now: its up does nothing, a new press starts the binding.
    rig.desk.key(KEYBOARD, F9, false);
    assert!(rig.hits().is_empty());
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, BindingAction::Trigger(1), Edge::Press)]);
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, BindingAction::Trigger(1), Edge::Release)]);

    // Read in an earlier run of the backend, pushed after it came back.
    assert!(rig.desk.read_key(KEYBOARD, J, true));
    rig.control.start(DeviceMask::of(&[DeviceClass::Gamepad])).unwrap();
    rig.control.start(DeviceMask::ALL).unwrap();
    rig.desk.push_read();
    assert!(keys(&rig.motion().0).is_empty(), "an earlier run's entry is dropped");
}

#[derive(Copy, Clone, Debug)]
enum Way {
    Pause,
    Stop,
    LostIndicator,
    ProtectedApp,
}

#[test]
fn what_is_held_ends_at_every_kind_of_pause_and_nothing_held_across_it_starts_after() {
    for way in [Way::Pause, Way::Stop, Way::LostIndicator, Way::ProtectedApp] {
        let mut rig = Rig::started();
        let ids = rig.locomotion();
        let walk = ids[0];
        let ctrl_f9 = rig.trigger_on(1, F9, CTRL);
        let (keyboard, pen, mouse) =
            (rig.source(DeviceClass::Keyboard), rig.source(DeviceClass::Pen), rig.source(DeviceClass::Mouse));
        let pad = PadId::new(0).unwrap();

        rig.desk.key(KEYBOARD, W, true);
        rig.desk.key(KEYBOARD, J, true);
        rig.desk.key(KEYBOARD, LEFT_CTRL, true);
        rig.pads.pad(PAD, 0, PadZones::NONE, [[0.9, 0.0], [0.0, 0.0]], [0.0, 0.0]);
        rig.desk.pen(PEN, 0.5, 0.5, 0.8, true);
        assert_eq!(edges(&rig.intents(), walk), [Edge::Press], "{way:?}");
        assert_eq!(keys(&rig.motion().0).len(), 3);
        assert!(rig.motion.pad(pad).is_some_and(|p| p.sticks[0][0] > 100), "{way:?}: the stick is deflected");

        match way {
            Way::Pause => rig.control.pause(),
            Way::Stop => rig.control.stop(),
            Way::LostIndicator => {
                rig.advance(1.6);
                rig.hub.tick();
            }
            Way::ProtectedApp => {
                rig.control.protect(&["vault".to_owned()]);
                rig.processes.set(&["vault"]);
                rig.hub.tick();
            }
        }
        // Published before any feed ran.
        let expected = match way {
            Way::Pause => paused(None),
            Way::Stop => CaptureState::Off,
            Way::LostIndicator => paused(Some(PauseWhy::IndicatorLost)),
            Way::ProtectedApp => paused(Some(PauseWhy::ProtectedApp)),
        };
        assert_eq!(rig.state(), expected, "{way:?}");
        assert_eq!(rig.indicator.view().state, expected, "{way:?}");

        assert_eq!(edges(&rig.intents(), walk), [Edge::Release], "{way:?}: the movement ends");
        let (events, reset) = rig.motion();
        assert!(events.is_empty(), "{way:?}");
        for src in [keyboard, pen, mouse] {
            assert!(reset.contains(src), "{way:?}: the keyboard, pen and mouse reset");
        }
        assert!(rig.motion.pad(pad).is_none(), "{way:?}: no levels from a closed epoch");
        // The person lets Ctrl go while capture is paused.
        rig.desk.key(KEYBOARD, LEFT_CTRL, false);

        match way {
            Way::Pause | Way::Stop => rig.start(),
            Way::LostIndicator => {
                rig.indicator.beat();
                rig.hub.tick();
            }
            Way::ProtectedApp => {
                rig.processes.set(&[]);
                rig.advance(1.1);
                rig.indicator.beat();
                rig.hub.tick();
            }
        }
        assert!(rig.state().is_on(), "{way:?}");
        assert!(rig.motion.pad(pad).is_none(), "{way:?}: the old levels stay gone until the pad reports again");
        rig.desk.repeat(KEYBOARD, W);
        rig.desk.repeat(KEYBOARD, W);
        rig.desk.key(KEYBOARD, W, false);
        rig.desk.key(KEYBOARD, F9, true);
        assert!(rig.intents().is_empty(), "{way:?}: the held key, its repeats and its up start nothing");
        assert!(rig.hits().is_empty(), "{way:?}: Ctrl was let go while paused, so F9 is not Ctrl+F9");
        assert!(!ids.contains(&ctrl_f9));
        let (events, _) = rig.motion();
        assert_eq!(keys(&events).len(), 1, "{way:?}: only F9's own down is a key event");
        rig.desk.key(KEYBOARD, W, true);
        assert_eq!(edges(&rig.intents(), walk), [Edge::Press], "{way:?}: a new press walks");
        rig.pads.pad(PAD, 0, PadZones::NONE, [[0.9, 0.0], [0.0, 0.0]], [0.0, 0.0]);
        assert!(rig.motion.pad(pad).is_some(), "{way:?}: new levels arrive");
    }
}

#[test]
fn one_backend_leaving_ends_only_what_its_sources_held() {
    let mut rig = Rig::started();
    let jump = rig.control.add_binding("pad jump", DeviceClass::Gamepad, BindingAction::Trigger(5)).unwrap();
    rig.hub.bind_pad(jump, 0b1).unwrap();
    let wave = rig.trigger_on(6, F9, 0);
    let pad_src = rig.source(DeviceClass::Gamepad);
    let pad = PadId::new(0).unwrap();
    rig.pads.pad(PAD, 0b1, PadZones::NONE, [[0.0, 0.9], [0.0, 0.0]], [0.0, 0.0]);
    rig.desk.key(KEYBOARD, F9, true);
    let hits = rig.hits();
    assert!(hits.contains(&(jump, BindingAction::Trigger(5), Edge::Press)));
    assert!(hits.contains(&(wave, BindingAction::Trigger(6), Edge::Press)));
    let _ = rig.motion();

    rig.hub.fail_backend(1);
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Release)], "only the pad binding ends");
    let (_, reset) = rig.motion();
    assert!(reset.contains(pad_src), "the pad's source resets");
    assert!(!reset.contains(rig.source(DeviceClass::Keyboard)), "the keyboard keeps its state");
    assert!(rig.motion.pad(pad).is_none());
    let report = rig.control.report();
    assert_eq!(report.state, CaptureState::on(DESK.classes, DeviceMask::of(&[DeviceClass::Gamepad])));
    assert!(
        report
            .backends
            .iter()
            .any(|b| b.name == "b.pads" && !matches!(b.state, omuzaki_input_api::capture::BackendState::Running))
    );

    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(wave, BindingAction::Trigger(6), Edge::Release)], "the key was held all along");
}

#[test]
#[cfg(debug_assertions)]
fn a_second_thread_writing_a_backend_s_rings_fails_a_debug_build() {
    let mut rig = Rig::started();
    let mut other = rig.hub.injector(0);
    std::thread::spawn(move || {
        other.key(KEYBOARD, A, true);
        other.key(KEYBOARD, A, false);
    })
    .join()
    .unwrap();
    let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rig.desk.key(KEYBOARD, J, true);
        rig.desk.key(KEYBOARD, J, false);
    }));
    assert!(second.is_err(), "the ring recorded its writer and refused another");
}

#[test]
fn a_thousand_due_events_arrive_in_order_over_later_drains_and_no_release_is_lost() {
    let mut rig = Rig::started();
    let usages: Vec<u16> = (0x04..=0x1D).collect();
    let mut sent = Vec::new();
    for i in 0..500 {
        let usage = usages[i % usages.len()];
        rig.desk.key(KEYBOARD, usage, true);
        rig.desk.key(KEYBOARD, usage, false);
        sent.push(true);
        sent.push(false);
    }
    let mut got = Vec::new();
    let mut drains = 0;
    let mut sizes = VecDeque::new();
    while got.len() < sent.len() && drains < 100 {
        rig.desk.idle();
        let (events, _) = rig.motion();
        assert!(events.len() <= 128);
        sizes.push_back(events.len());
        got.extend(keys(&events).into_iter().map(|(_, _, down)| down));
        drains += 1;
    }
    assert_eq!(sizes.front(), Some(&128), "the first drain is full and the rest wait");
    assert_eq!(got, sent, "every event, in order, in {drains} drains");
    assert_eq!(got.iter().filter(|down| !**down).count(), 500, "no release lost");
}

#[test]
fn every_pause_reason_is_hidden_from_actors_without_process_but_not_the_state() {
    for state in [paused(Some(PauseWhy::IndicatorLost)), paused(Some(PauseWhy::ProtectedApp)), paused(None)] {
        assert_eq!(state.redacted(), paused(None));
    }
    let on = CaptureState::on(DeviceMask::ALL, DeviceMask::NONE);
    assert_eq!(on.clone().redacted(), on);
}

#[test]
fn capture_reads_only_the_classes_asked_and_a_new_set_is_a_new_run() {
    let mut rig = Rig::new();
    rig.indicator.beat();
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard)).unwrap();
    rig.desk.device_up(KEYBOARD, DeviceClass::Keyboard);
    rig.desk.device_up(support::MOUSE, DeviceClass::Mouse);
    rig.desk.device_up(PEN, DeviceClass::Pen);
    rig.desk.key(KEYBOARD, A, true);
    rig.desk.motion(support::MOUSE, 400, 0, 1.0);
    rig.desk.buttons(support::MOUSE, DeviceClass::Mouse, 1, 0);
    rig.desk.pen(PEN, 0.5, 0.5, 1.0, true);
    rig.advance(0.02);
    rig.desk.idle();
    let events = rig.motion().0;
    let classes: Vec<DeviceClass> = events
        .iter()
        .filter_map(|e| match *e {
            omuzaki_input_api::event::MotionEvent::SourceUp { class, .. } => Some(class),
            _ => None,
        })
        .collect();
    assert_eq!(classes, [DeviceClass::Keyboard], "only the keyboard is announced: {events:?}");
    assert_eq!(keys(&events).len(), 1);
    assert_eq!(events.len(), 2, "no pointer, button or pen event: {events:?}");

    // Asking for the mouse too starts a new run: the mouse is announced and read, and the key held over stays inert.
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse)).unwrap();
    rig.desk.motion(support::MOUSE, 400, 0, 0.0);
    rig.desk.key(KEYBOARD, A, false);
    rig.advance(0.02);
    rig.desk.idle();
    let events = rig.motion().0;
    let ups = events.iter().filter(|e| matches!(e, omuzaki_input_api::event::MotionEvent::SourceUp { .. })).count();
    assert_eq!(ups, 2, "{events:?}");
    assert!(events.iter().any(|e| matches!(e, omuzaki_input_api::event::MotionEvent::Pointer { .. })), "{events:?}");
    assert!(keys(&events).is_empty(), "the release of a key held over the new run is not seen: {events:?}");
}
