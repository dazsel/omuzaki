//! The zone table, the hot types' size, the frames, the indicator handle and the serde round trip of every cold type.
#![allow(clippy::unwrap_used)] // a panic is a test failure

use std::collections::HashSet;
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input_api::binding::{BindingAction, BindingId, BindingInfo, Edge, Intent, IntentEvent};
use omuzaki_input_api::capture::{
    BackendReport, BackendState, CaptureCaps, CaptureReport, CaptureState, Detail, IndicatorHandle, IndicatorView,
    PauseWhy, PermissionHow, Support,
};
use omuzaki_input_api::event::{
    DeviceClass, DeviceMask, MotionEvent, PadId, PadSet, PadState, PadZones, SourceMask, SourceTag,
};
use omuzaki_input_api::feed::{INTENT_FRAME_MAX, IntentFrame, MOTION_FRAME_MAX, MotionFrame};
use omuzaki_input_api::zones::{Band, KeyZone, Side};

fn src(i: u8) -> SourceTag {
    SourceTag::new(i).unwrap()
}

#[test]
fn the_sixteen_zones_are_complete_and_lie_on_the_key_plane() {
    assert_eq!(KeyZone::ALL.len(), KeyZone::COUNT);
    let mut names = HashSet::new();
    for (i, zone) in KeyZone::ALL.into_iter().enumerate() {
        assert_eq!(usize::from(zone.index()), i);
        assert!(!zone.is_side_only());
        let info = zone.info();
        assert!(info.centre.iter().all(|c| (0.0..=1.0).contains(c)), "{zone:?} {info:?}");
        assert!(info.finger.is_some(), "{zone:?} has a finger");
        assert!(names.insert(zone.name()), "{zone:?}'s name is unique");
        assert_eq!(KeyZone::from_name(zone.name()), Some(zone));
    }
    // Every zone has its own centre, so two zones never put a finger on the same spot.
    let centres: HashSet<[u32; 2]> = KeyZone::ALL.iter().map(|z| z.info().centre.map(f32::to_bits)).collect();
    assert_eq!(centres.len(), KeyZone::COUNT);
    assert_eq!(KeyZone::new(18), None);
}

#[test]
fn the_letter_zones_split_by_hand_and_mirror_across_the_board() {
    let left =
        [KeyZone::LEFT_OUTER_UPPER, KeyZone::LEFT_OUTER_LOWER, KeyZone::LEFT_INNER_UPPER, KeyZone::LEFT_INNER_LOWER];
    let right = [
        KeyZone::RIGHT_OUTER_UPPER,
        KeyZone::RIGHT_OUTER_LOWER,
        KeyZone::RIGHT_INNER_UPPER,
        KeyZone::RIGHT_INNER_LOWER,
    ];
    for (l, r) in left.into_iter().zip(right) {
        let (li, ri) = (l.info(), r.info());
        assert_eq!((li.side, ri.side), (Some(Side::Left), Some(Side::Right)));
        assert_eq!((li.finger, li.band), (ri.finger, ri.band), "{l:?} mirrors {r:?}");
        assert!(li.centre[0] < ri.centre[0], "{l:?} is left of {r:?}");
    }
    // Upper zones lie behind lower ones.
    assert!(KeyZone::LEFT_OUTER_UPPER.info().centre[1] > KeyZone::LEFT_OUTER_LOWER.info().centre[1]);
    assert_eq!(KeyZone::FUNCTION.info().band, Band::Function);
    assert!(KeyZone::ALL.iter().all(|z| z.info().centre[1] <= KeyZone::FUNCTION.info().centre[1]));
}

#[test]
fn the_side_only_values_name_a_hand_and_no_finger() {
    for (zone, side) in [(KeyZone::LEFT_ONLY, Side::Left), (KeyZone::RIGHT_ONLY, Side::Right)] {
        assert!(zone.is_side_only());
        assert_eq!(KeyZone::side_only(side), zone);
        let info = zone.info();
        assert_eq!((info.side, info.finger), (Some(side), None));
        assert!(info.centre.iter().all(|c| (0.0..=1.0).contains(c)));
        assert_eq!(KeyZone::from_name(zone.name()), Some(zone));
    }
}

#[test]
fn motion_events_are_copy_and_at_most_32_bytes() {
    fn copy<T: Copy>() {}
    copy::<MotionEvent>();
    copy::<PadState>();
    copy::<IntentEvent>();
    assert!(size_of::<MotionEvent>() <= 32, "{} bytes", size_of::<MotionEvent>());
}

#[test]
fn frames_hold_their_capacity_and_give_the_rest_back() {
    let mut frame = MotionFrame::new();
    for i in 0..MOTION_FRAME_MAX {
        frame.push(MotionEvent::key(src(1), KeyZone::SPACE, i % 2 == 0, Seconds(0.0))).unwrap();
    }
    assert!(frame.is_full());
    let extra = MotionEvent::source_gone(src(1), Seconds(1.0));
    assert_eq!(frame.push(extra), Err(extra));
    frame.add_reset(SourceMask::of(src(3)));
    assert_eq!(frame.events().len(), MOTION_FRAME_MAX);
    frame.clear();
    assert!(frame.events().is_empty() && frame.reset().is_empty());

    let mut intents = IntentFrame::new();
    let event = IntentEvent { id: BindingId(7), intent: Intent::moving(0, 127), edge: Edge::Press, at: Seconds(0.5) };
    for _ in 0..INTENT_FRAME_MAX {
        intents.push(event).unwrap();
    }
    assert_eq!(intents.push(event), Err(event));
    assert!(intents.events().all(|e| e == event));
}

#[test]
fn masks_and_pad_sets_hold_what_was_put_in() {
    let mask = DeviceMask::of(&[DeviceClass::Keyboard, DeviceClass::Gamepad]);
    assert!(mask.contains(DeviceClass::Gamepad) && !mask.contains(DeviceClass::Mouse));
    assert_eq!(mask.iter().collect::<Vec<_>>(), [DeviceClass::Keyboard, DeviceClass::Gamepad]);
    assert_eq!(
        DeviceMask::ALL.without(mask),
        DeviceMask::of(&[DeviceClass::Mouse, DeviceClass::Pen, DeviceClass::Midi, DeviceClass::Osc])
    );
    assert_eq!(DeviceMask::of(&DeviceClass::ALL), DeviceMask::ALL);
    let sources = SourceMask::of(src(0)).with(src(63)).without(src(0));
    assert_eq!(sources.iter().collect::<Vec<_>>(), [src(63)]);
    assert_eq!(SourceTag::new(64), None);

    let mut pads = PadSet::new();
    for i in 0..4 {
        assert!(pads.push(PadId::new(i).unwrap()));
    }
    assert!(!pads.push(PadId::new(0).unwrap()), "no duplicates");
    assert_eq!(pads.len(), 4);
    assert_eq!(PadId::new(4), None);
    assert!(PadZones::ALL.contains(PadZones::CENTRE.or(PadZones::DPAD)));
}

#[test]
fn an_indicator_counts_as_shown_only_while_it_beats() {
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let handle = IndicatorHandle::new(clock.clone());
    assert!(!handle.is_shown(), "never drawn");
    let surface = handle.clone();
    surface.beat();
    assert!(handle.is_shown());
    clock.advance(Seconds(1.4));
    assert!(handle.is_shown());
    clock.advance(Seconds(0.2));
    assert!(!handle.is_shown(), "stale after 1.5 s");

    let before = surface.view_version();
    let view = IndicatorView { state: CaptureState::paused(Some(PauseWhy::ProtectedApp)), ..IndicatorView::default() };
    assert!(handle.publish(&view));
    assert_eq!(surface.view(), view);
    assert!(handle.publish(&view));
    assert_eq!(surface.view_version(), before + 1, "an unchanged view is not a new version");
}

#[test]
fn the_redacted_state_hides_every_pause_reason() {
    for why in [Some(PauseWhy::IndicatorLost), Some(PauseWhy::ProtectedApp), None] {
        assert_eq!(CaptureState::paused(why).redacted(), CaptureState::paused(None));
    }
    let on = CaptureState::on(DeviceMask::ALL, DeviceMask::NONE);
    assert_eq!(on.clone().redacted(), on);
}

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

#[test]
fn every_cold_type_survives_a_serde_round_trip() {
    let at = Seconds(1.25);
    let pad = PadId::new(1).unwrap();
    for zone in KeyZone::ALL.into_iter().chain([KeyZone::LEFT_ONLY, KeyZone::RIGHT_ONLY]) {
        round_trip(&zone);
    }
    for event in [
        MotionEvent::key(src(2), KeyZone::NUMPAD, true, at),
        MotionEvent::buttons(src(3), DeviceClass::Mouse, 0b01, 0b10, at),
        MotionEvent::pointer(src(3), 0.001, -0.002, 1.0, at),
        MotionEvent::pen(src(4), 31, 0, 200, [-12, 40], true, false, at),
        MotionEvent::pad_edges(pad, PadZones::FACE, PadZones::DPAD, at),
        MotionEvent::source_up(src(5), DeviceClass::Gamepad, Some(pad), at),
        MotionEvent::source_gone(src(5), at),
    ] {
        round_trip(&event);
    }
    round_trip(&PadState { pad, sticks: [[-127, 127], [0, 5]], triggers: [255, 0], zones: PadZones::CENTRE, at });
    for intent in [Intent::moving(-127, 64), Intent::Run, Intent::Jump, Intent::Sit, Intent::Emote(3)] {
        round_trip(&intent);
        round_trip(&BindingAction::Intent(intent));
        round_trip(&IntentEvent { id: BindingId(9), intent, edge: Edge::Repeat, at });
    }
    round_trip(&BindingAction::op("expression.set", serde_json::json!({ "name": "bleh" })));
    round_trip(&BindingAction::Trigger(4));
    round_trip(&BindingInfo {
        id: BindingId(1),
        name: "walk forward".into(),
        device: DeviceClass::Keyboard,
        action: BindingAction::Intent(Intent::moving(0, 127)),
        conflict: true,
    });
    for state in [
        CaptureState::Off,
        CaptureState::paused(Some(PauseWhy::IndicatorLost)),
        CaptureState::on(DeviceMask::of(&[DeviceClass::Keyboard]), DeviceMask::of(&[DeviceClass::Pen])),
        CaptureState::failed("no backend could start"),
    ] {
        round_trip(&state);
    }
    round_trip(&CaptureReport {
        state: CaptureState::on(DeviceMask::ALL, DeviceMask::NONE),
        backends: vec![
            BackendReport {
                name: "linux.evdev".into(),
                classes: DeviceMask::of(&[DeviceClass::Keyboard, DeviceClass::Mouse]),
                state: BackendState::needs_permission(PermissionHow::InputGroup),
            },
            BackendReport { name: "gamepad.gilrs".into(), classes: DeviceMask::ALL, state: BackendState::Running },
            BackendReport { name: "x".into(), classes: DeviceMask::NONE, state: BackendState::failed("gone") },
        ],
        caps: CaptureCaps { typing: Support::NeedsOptIn, bindings: Support::Portal, ..CaptureCaps::default() },
        detail: Detail::HandOnly,
    });
}

#[test]
fn zone_names_are_what_the_recorded_text_and_serde_write() {
    assert_eq!(serde_json::to_string(&KeyZone::LEFT_INNER_LOWER).unwrap(), "\"left_inner_lower\"");
    assert!(serde_json::from_str::<KeyZone>("\"f9\"").is_err());
    assert_eq!(serde_json::to_string(&Detail::HandOnly).unwrap(), "\"hand_only\"");
}
