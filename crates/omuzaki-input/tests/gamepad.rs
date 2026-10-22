//! IN-5's acceptance (design §13): the gamepad backend's capture loop over a scripted gilrs event list, through the
//! hub to every feed, and the registered gilrs backend itself.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use omuzaki_input::{PadAxis, PadButton, PadScript, pad_button_bit};
use omuzaki_input_api::binding::{BindingAction, Edge, Intent};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, MotionEvent, PadId, PadSet, PadState, PadZones, SourceTag};
use omuzaki_input_api::feed::InputHub;
use support::{PAD, PAD_2, Rig, edges};

/// The scripted pads run as test backend 1 (`b.pads`).
const PADS_BACKEND: usize = 1;

/// A hub with capture on and the gamepad loop running over a script, with `connected` pads there from the start.
fn rig(connected: &[u64]) -> (Rig, PadScript) {
    let rig = Rig::new();
    rig.start();
    let (script, _thread) = rig.hub.run_pads(PADS_BACKEND, connected).unwrap();
    assert!(script.sync(), "the capture loop runs");
    (rig, script)
}

/// The pads that came up in `events`, by source and pad.
fn ups(events: &[MotionEvent]) -> Vec<(SourceTag, PadId)> {
    events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::SourceUp { src, class: DeviceClass::Gamepad, pad: Some(pad), .. } => Some((src, pad)),
            _ => None,
        })
        .collect()
}

fn pad_edges(events: &[MotionEvent]) -> Vec<(PadId, PadZones, PadZones)> {
    events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::PadEdges { pad, pressed, released, .. } => Some((pad, pressed, released)),
            _ => None,
        })
        .collect()
}

fn listed(rig: &Rig) -> Vec<PadId> {
    let mut set = PadSet::new();
    rig.motion.pads(&mut set);
    set.iter().collect()
}

fn levels(rig: &Rig, pad: PadId) -> PadState {
    rig.motion.pad(pad).unwrap()
}

/// Connects `device` and returns its pad.
fn connect(rig: &mut Rig, script: &PadScript, device: u64) -> PadId {
    script.connect(device);
    assert!(script.sync());
    let up = ups(&rig.motion().0);
    assert_eq!(up.len(), 1, "one pad came up: {up:?}");
    up[0].1
}

#[test]
fn a_press_and_release_between_two_drains_arrive_as_two_edges_with_the_pad_id() {
    let (mut rig, script) = rig(&[]);
    let pad = connect(&mut rig, &script, PAD);
    assert_eq!(listed(&rig), [pad], "a connected pad is listed before it first moves");
    script.press(PAD, PadButton::South);
    script.release(PAD, PadButton::South);
    assert!(script.sync());
    let events = rig.motion().0;
    assert_eq!(
        pad_edges(&events),
        [(pad, PadZones::FACE, PadZones::NONE), (pad, PadZones::NONE, PadZones::FACE)],
        "{events:?}"
    );
    assert_eq!(levels(&rig, pad).zones, PadZones::NONE);
}

#[test]
fn sticks_triggers_and_zones_map_exactly_at_the_edges() {
    let (mut rig, script) = rig(&[]);
    let pad = connect(&mut rig, &script, PAD);
    let stick = |value: f32| {
        script.axis(PAD, PadAxis::LeftStickX, value);
        assert!(script.sync());
        levels(&rig, pad).sticks[0][0]
    };
    assert_eq!(stick(1.0), 127);
    assert_eq!(stick(-1.0), -127);
    assert_eq!(stick(0.12), 0, "the dead zone's edge");
    assert_eq!(stick(-0.12), 0);
    assert_eq!(stick(0.13), 1, "just past it");
    assert_eq!(stick(2.0), 127, "clamped");
    script.axis(PAD, PadAxis::RightStickY, 1.0);
    script.value(PAD, PadButton::RightTrigger2, 1.0);
    script.value(PAD, PadButton::LeftTrigger2, 0.5);
    assert!(script.sync());
    let state = levels(&rig, pad);
    assert_eq!(state.sticks[1], [0, 127], "up is positive");
    assert_eq!(state.triggers, [128, 255]);
    script.value(PAD, PadButton::LeftTrigger2, 0.0);
    assert!(script.sync());
    assert_eq!(levels(&rig, pad).triggers, [0, 255]);
    let _ = rig.motion();

    let zones = [
        (PadButton::DPadLeft, PadZones::DPAD),
        (PadButton::East, PadZones::FACE),
        (PadButton::LeftThumb, PadZones::LEFT_STICK),
        (PadButton::RightThumb, PadZones::RIGHT_STICK),
        (PadButton::LeftTrigger, PadZones::LEFT_SHOULDER),
        (PadButton::RightTrigger, PadZones::RIGHT_SHOULDER),
        (PadButton::LeftTrigger2, PadZones::LEFT_TRIGGER),
        (PadButton::RightTrigger2, PadZones::RIGHT_TRIGGER),
        (PadButton::Start, PadZones::CENTRE),
    ];
    for (button, zone) in zones {
        script.press(PAD, button);
        assert!(script.sync());
        assert_eq!(levels(&rig, pad).zones, zone, "{button:?}");
        assert_eq!(pad_edges(&rig.motion().0), [(pad, zone, PadZones::NONE)], "{button:?}");
        script.release(PAD, button);
        assert!(script.sync());
        assert_eq!(pad_edges(&rig.motion().0), [(pad, PadZones::NONE, zone)], "{button:?}");
    }
    // A d-pad gilrs reports on axes presses the same zone.
    script.axis(PAD, PadAxis::DPadX, -1.0);
    assert!(script.sync());
    assert_eq!(levels(&rig, pad).zones, PadZones::DPAD);
}

#[test]
fn a_second_pad_hot_plugged_leaves_the_first_pad_s_index_alone() {
    let (mut rig, script) = rig(&[]);
    let first = connect(&mut rig, &script, PAD);
    let second = connect(&mut rig, &script, PAD_2);
    assert_ne!(first, second);
    script.disconnect(PAD_2);
    let third = connect(&mut rig, &script, 12);
    assert_eq!(listed(&rig), {
        let mut both = vec![first, third];
        both.sort_by_key(|p| p.index());
        both
    });
    script.axis(PAD, PadAxis::LeftStickX, 1.0);
    assert!(script.sync());
    assert_eq!(levels(&rig, first).sticks[0][0], 127, "the first pad still reads as itself");
}

#[test]
fn pads_lists_a_pad_whose_first_activity_is_a_stick_move() {
    let (rig, script) = rig(&[]);
    assert!(listed(&rig).is_empty());
    script.axis(40, PadAxis::LeftStickY, -1.0);
    assert!(script.sync());
    let pads = listed(&rig);
    assert_eq!(pads.len(), 1);
    assert_eq!(levels(&rig, pads[0]).sticks[0][1], -127);
}

#[test]
fn a_pad_there_from_the_start_comes_up_when_capture_reads() {
    let (mut rig, script) = rig(&[PAD]);
    let up = ups(&rig.motion().0);
    assert_eq!(up.len(), 1);
    script.press(PAD, PadButton::North);
    assert!(script.sync());
    assert_eq!(pad_edges(&rig.motion().0), [(up[0].1, PadZones::FACE, PadZones::NONE)]);
}

#[test]
fn two_pads_holding_one_pad_binding_end_its_action_only_when_the_second_releases() {
    let (mut rig, script) = rig(&[]);
    connect(&mut rig, &script, PAD);
    connect(&mut rig, &script, PAD_2);
    let jump = rig.control.add_binding("pad jump", DeviceClass::Gamepad, BindingAction::Trigger(5)).unwrap();
    rig.hub.bind_pad(jump, pad_button_bit(PadButton::South)).unwrap();
    script.press(PAD, PadButton::South);
    script.press(PAD_2, PadButton::South);
    assert!(script.sync());
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Press)]);
    script.release(PAD, PadButton::South);
    assert!(script.sync());
    assert!(rig.hits().is_empty(), "the second pad still holds it");
    script.release(PAD_2, PadButton::South);
    assert!(script.sync());
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Release)]);
}

#[test]
fn unplugging_one_of_two_pads_ends_only_its_sticks_buttons_and_actions() {
    let (mut rig, script) = rig(&[]);
    script.connect(PAD);
    script.connect(PAD_2);
    assert!(script.sync());
    let events = rig.motion().0;
    let up = ups(&events);
    assert_eq!(up.len(), 2, "{events:?}");
    let ((gone_src, gone), (_, kept)) = (up[0], up[1]);
    let jump = rig.control.add_binding("pad jump", DeviceClass::Gamepad, BindingAction::Trigger(5)).unwrap();
    rig.hub.bind_pad(jump, pad_button_bit(PadButton::South)).unwrap();
    let run = rig.control.add_binding("pad run", DeviceClass::Gamepad, BindingAction::Intent(Intent::Run)).unwrap();
    rig.hub.bind_pad(run, pad_button_bit(PadButton::East)).unwrap();
    // A pad chord is the set of buttons held, so each pad holds one binding's button.
    for (device, button) in [(PAD, PadButton::South), (PAD_2, PadButton::East)] {
        script.axis(device, PadAxis::LeftStickX, 1.0);
        script.press(device, button);
    }
    assert!(script.sync());
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Press)]);
    assert_eq!(edges(&rig.intents(), run), [Edge::Press]);
    let _ = rig.motion();

    script.disconnect(PAD);
    assert!(script.sync());
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Release)], "the unplugged pad's action ends");
    assert!(rig.intents().is_empty(), "the other pad's intent is untouched");
    let events = rig.motion().0;
    assert!(events.iter().any(|e| matches!(*e, MotionEvent::SourceGone { src, .. } if src == gone_src)));
    assert_eq!(events.iter().filter(|e| matches!(e, MotionEvent::SourceGone { .. })).count(), 1);
    assert!(rig.motion.pad(gone).is_none(), "its sticks and buttons are gone with it");
    assert_eq!(listed(&rig), [kept]);
    let state = levels(&rig, kept);
    assert_eq!(state.sticks[0][0], 127, "the other pad's stick is untouched");
    assert_eq!(state.zones, PadZones::FACE, "and so are its buttons");

    script.disconnect(PAD_2);
    assert!(script.sync());
    assert_eq!(edges(&rig.intents(), run), [Edge::Release], "an intent ends through the intent feed");
}

#[test]
fn a_button_pressed_while_paused_starts_nothing_until_it_is_released() {
    let (mut rig, script) = rig(&[]);
    let pad = connect(&mut rig, &script, PAD);
    let jump = rig.control.add_binding("pad jump", DeviceClass::Gamepad, BindingAction::Trigger(5)).unwrap();
    rig.hub.bind_pad(jump, pad_button_bit(PadButton::South)).unwrap();
    rig.control.pause();
    script.press(PAD, PadButton::South);
    rig.start();
    assert!(script.sync());
    script.axis(PAD, PadAxis::LeftStickX, 1.0);
    assert!(script.sync());
    let hits = rig.hits();
    assert!(hits.is_empty(), "held over the pause: {hits:?}");
    assert_eq!(levels(&rig, pad).zones, PadZones::NONE);
    script.release(PAD, PadButton::South);
    script.press(PAD, PadButton::South);
    assert!(script.sync());
    assert_eq!(rig.hits(), [(jump, BindingAction::Trigger(5), Edge::Press)]);
}

#[test]
fn a_pad_unplugged_while_paused_is_gone_after_the_resume() {
    let (mut rig, script) = rig(&[]);
    let first = connect(&mut rig, &script, PAD);
    script.connect(PAD_2);
    assert!(script.sync());
    let (second_src, _) = ups(&rig.motion().0)[0];
    rig.control.pause();
    script.disconnect(PAD_2);
    rig.start();
    assert!(script.sync());
    let events = rig.motion().0;
    assert!(
        events.iter().any(|e| matches!(*e, MotionEvent::SourceGone { src, .. } if src == second_src)),
        "{events:?}"
    );
    assert_eq!(listed(&rig), [first]);
}

#[test]
fn the_registered_backend_is_gilrs_and_starting_it_never_panics() {
    let clock = std::sync::Arc::new(omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(1.0)));
    let hub = omuzaki_input::Hub::new(omuzaki_input::HubConfig::new(clock, None)).unwrap();
    let indicator = hub.indicator_host().install();
    indicator.beat();
    let control = hub.control();
    assert_eq!(control.report().caps.gamepad, omuzaki_input_api::capture::Support::Available);
    control.start(DeviceMask::NONE.with(DeviceClass::Gamepad)).unwrap();
    let report = control.report();
    assert!(report.backends.iter().any(|b| b.name == "gamepad.gilrs"), "{report:?}");
    control.stop();
    drop(hub);
}

#[cfg(target_os = "linux")]
#[test]
fn libudev_is_linked_dynamically() {
    let exe = std::env::current_exe().unwrap();
    let Ok(output) = std::process::Command::new("ldd").arg(&exe).output() else {
        eprintln!("skipped: no ldd on this runner");
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.lines().any(|l| l.trim_start().starts_with("libudev.so")), "libudev is a shared library:\n{text}");
}
