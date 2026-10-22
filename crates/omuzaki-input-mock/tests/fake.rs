//! `FakeInput` keeps the contract's promises where consumers see them: the indicator gate, pauses that end what is
//! held, presses that own their releases, the gated chord store, the presets, the protected list and the feeds.
#![allow(clippy::unwrap_used)] // a panic is a test failure

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input_api::binding::{BindingAction, BindingId, Edge, Intent};
use omuzaki_input_api::capture::{CaptureState, Detail, InputError, PauseWhy};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, MotionEvent, PadId, PadSet, PadState, SourceMask, SourceTag};
use omuzaki_input_api::feed::{
    BindingFeed, ChordStore, IndicatorHost, InputControl, InputHub, IntentFrame, MotionFeed, MotionFrame, ProtectList,
};
use omuzaki_input_api::zones::KeyZone;
use omuzaki_input_mock::conformance;
use omuzaki_input_mock::fake::FakeInput;

fn src(i: u8) -> SourceTag {
    SourceTag::new(i).unwrap()
}

fn hub() -> (FakeInput, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::new(Seconds(1.0)));
    (FakeInput::new(Arc::clone(&clock)), clock)
}

fn hits(feed: &mut dyn BindingFeed) -> Vec<(BindingId, BindingAction, Edge)> {
    let mut out = Vec::new();
    while let Some(hit) = feed.poll() {
        out.push((hit.id, hit.action.clone(), hit.edge));
    }
    out
}

#[test]
fn capture_starts_only_with_a_shown_indicator_and_a_stale_one_pauses_until_it_returns() {
    let (fake, clock) = hub();
    assert_eq!(fake.start(DeviceMask::ALL), Err(InputError::NoIndicator), "none installed");
    let indicator = fake.install();
    assert_eq!(fake.start(DeviceMask::ALL), Err(InputError::NoIndicator), "installed, never drawn");
    indicator.beat();
    fake.start(DeviceMask::ALL).unwrap();
    assert!(fake.report().state.is_on());
    assert!(indicator.view().state.is_on(), "the view follows");

    clock.advance(Seconds(2.0));
    fake.tick();
    assert_eq!(fake.report().state, CaptureState::paused(Some(PauseWhy::IndicatorLost)));
    indicator.beat();
    fake.tick();
    assert!(fake.report().state.is_on(), "the hub's own pause undoes itself");

    fake.pause();
    indicator.beat();
    fake.tick();
    assert_eq!(fake.report().state, CaptureState::paused(None), "an asked-for pause stays");
    fake.stop();
    fake.stop();
    assert_eq!(fake.report().state, CaptureState::Off, "stop is idempotent");
}

#[test]
fn a_pause_ends_every_held_action_and_resets_the_sources() {
    let (fake, _) = hub();
    let indicator = fake.install();
    indicator.beat();
    fake.start(DeviceMask::ALL).unwrap();
    let pad = PadId::new(0).unwrap();
    fake.connect("windows.raw_input", src(1), DeviceClass::Keyboard, None);
    fake.connect("gamepad.gilrs", src(2), DeviceClass::Gamepad, Some(pad));
    fake.set_pad(PadState { sticks: [[0, 127], [0, 0]], ..PadState::neutral(pad, Seconds(1.0)) });
    let walk = fake.add_binding("walk", DeviceClass::Keyboard, BindingAction::Intent(Intent::moving(0, 127))).unwrap();
    let wave = fake.add_binding("wave", DeviceClass::Keyboard, BindingAction::Trigger(2)).unwrap();
    fake.hit(walk, Edge::Press, Seconds(1.0));
    fake.hit(wave, Edge::Press, Seconds(1.0));
    fake.push("windows.raw_input", MotionEvent::key(src(1), KeyZone::SPACE, true, Seconds(1.0)));

    let (mut motion, mut intents, mut bindings) = fake.take_feeds().unwrap();
    assert!(fake.take_feeds().is_none(), "taken once");
    let (mut frame, mut intent_frame) = (MotionFrame::new(), IntentFrame::new());
    motion.drain(Seconds(1.0), &mut frame);
    intents.drain(Seconds(1.0), &mut intent_frame);
    assert_eq!(frame.events().len(), 3);
    assert_eq!(intent_frame.events().map(|e| e.edge).collect::<Vec<_>>(), [Edge::Press]);
    assert_eq!(hits(bindings.as_mut()), [(wave, BindingAction::Trigger(2), Edge::Press)]);
    assert!(motion.pad(pad).is_some());

    fake.pause();
    motion.drain(Seconds(2.0), &mut frame);
    assert_eq!(frame.reset(), SourceMask::of(src(1)).with(src(2)));
    assert_eq!(motion.pad(pad), None, "a deflected stick cannot keep walking");
    let mut pads = PadSet::new();
    motion.pads(&mut pads);
    assert!(pads.is_empty());
    intents.drain(Seconds(2.0), &mut intent_frame);
    assert_eq!(intent_frame.events().map(|e| (e.id, e.edge)).collect::<Vec<_>>(), [(walk, Edge::Release)]);
    assert_eq!(hits(bindings.as_mut()), [(wave, BindingAction::Trigger(2), Edge::Release)]);

    // Nothing reaches a consumer while paused, and a release whose press was ended by the pause is not handed out.
    fake.push("windows.raw_input", MotionEvent::key(src(1), KeyZone::SPACE, false, Seconds(2.0)));
    fake.hit(wave, Edge::Release, Seconds(2.0));
    motion.drain(Seconds(3.0), &mut frame);
    assert!(frame.events().is_empty() && frame.reset().is_empty());
    assert!(bindings.poll().is_none());
}

#[test]
fn a_release_carries_the_action_its_press_started() {
    let (fake, _) = hub();
    let mut feed = fake.clone();
    let id = fake.add_binding("bleh", DeviceClass::Keyboard, BindingAction::Trigger(1)).unwrap();
    fake.hit(id, Edge::Release, Seconds(0.5));
    assert!(feed.poll().is_none(), "a release with no press");
    fake.hit(id, Edge::Press, Seconds(1.0));
    fake.set_action(id, BindingAction::op("expression.set", serde_json::json!({ "name": "smile" }))).unwrap();
    fake.hit(id, Edge::Repeat, Seconds(1.1));
    fake.hit(id, Edge::Release, Seconds(1.2));
    fake.hit(id, Edge::Press, Seconds(2.0));
    let got = hits(&mut feed);
    let edges: Vec<(BindingAction, Edge)> = got.into_iter().map(|(_, a, e)| (a, e)).collect();
    assert_eq!(
        edges[..3],
        [
            (BindingAction::Trigger(1), Edge::Press),
            (BindingAction::Trigger(1), Edge::Repeat),
            (BindingAction::Trigger(1), Edge::Release),
        ]
    );
    assert!(matches!(edges[3], (BindingAction::Op { .. }, Edge::Press)), "the new action from the next press");

    // Removing a held binding ends its action.
    fake.remove(id).unwrap();
    assert_eq!(hits(&mut feed).last().map(|h| h.2), Some(Edge::Release));
    assert_eq!(fake.remove(id), Err(InputError::UnknownBinding(id)));
}

#[test]
fn learning_needs_capture_on_and_conflicts_are_reported() {
    let (fake, _) = hub();
    let a = fake.add_binding("a", DeviceClass::Keyboard, BindingAction::Trigger(0)).unwrap();
    let b = fake.add_binding("b", DeviceClass::Keyboard, BindingAction::Trigger(1)).unwrap();
    assert_eq!(fake.learn(a).unwrap_err(), InputError::NotCapturing);
    let indicator = fake.install();
    indicator.beat();
    fake.start(DeviceMask::of(&[DeviceClass::Keyboard])).unwrap();
    fake.script_chord("Ctrl+F9");
    fake.script_chord("Ctrl+F9");
    let label = fake.learn(a).unwrap();
    fake.learn(b).unwrap();
    assert_eq!(format!("{label:?}"), "<chord>");
    let mut conflicts = Vec::new();
    fake.conflicts(a, &mut conflicts);
    assert_eq!(conflicts, [b]);
    assert!(fake.bindings().iter().all(|info| info.conflict));
    let mut text = String::new();
    fake.label(b).unwrap().render(&mut |t| text.push_str(t));
    assert_eq!(text, "Ctrl+F9");
    fake.clear(b);
    assert!(fake.label(b).is_none());
    assert_eq!(fake.learn(BindingId(99)).unwrap_err(), InputError::UnknownBinding(BindingId(99)));
}

#[test]
fn presets_bind_with_their_chords_inside_and_list_their_actions() {
    let (fake, _) = hub();
    assert_eq!(fake.presets(), ["locomotion"]);
    let actions = fake.preset_actions("locomotion").unwrap();
    assert!(actions.iter().all(|a| a.as_intent().is_some()));
    let ids = fake.apply_preset("locomotion").unwrap();
    assert_eq!(ids.len(), actions.len());
    assert!(fake.label(ids[0]).is_some(), "the preset's chords are in the store");
    assert_eq!(fake.apply_preset("dance"), Err(InputError::UnknownPreset("dance".into())));
}

#[test]
fn a_protected_program_pauses_capture_and_only_the_persons_indicator_names_it() {
    let (fake, _) = hub();
    let indicator = fake.install();
    indicator.beat();
    fake.start(DeviceMask::ALL).unwrap();
    fake.protect(&["vault.exe".to_owned()]);
    assert_eq!(ProtectList::get(&fake), ["vault.exe"]);
    fake.set_running(Some("Vault.exe"));
    fake.tick();
    let report = fake.report();
    assert_eq!(report.state, CaptureState::paused(Some(PauseWhy::ProtectedApp)));
    assert_eq!(report.state.redacted(), CaptureState::paused(None));
    assert_eq!(indicator.view().program.as_deref(), Some("Vault.exe"));
    assert_eq!(fake.matching().as_deref(), Some("Vault.exe"));
    fake.set_running(None);
    indicator.beat();
    fake.tick();
    assert!(fake.report().state.is_on());
    assert_eq!(indicator.view().program, None);
}

#[test]
fn a_detail_change_resets_the_keyboards_and_drops_their_queued_keys() {
    let (fake, _) = hub();
    fake.connect("windows.raw_input", src(1), DeviceClass::Keyboard, None);
    fake.connect("windows.raw_input", src(2), DeviceClass::Mouse, None);
    fake.push("windows.raw_input", MotionEvent::key(src(1), KeyZone::SPACE, true, Seconds(1.0)));
    let mut feed = fake.clone();
    fake.set_detail(Detail::HandOnly);
    assert_eq!(fake.detail(), Detail::HandOnly);
    let mut frame = MotionFrame::new();
    MotionFeed::drain(&mut feed, Seconds(1.0), &mut frame);
    assert_eq!(frame.reset(), SourceMask::of(src(1)));
    assert!(!frame.events().iter().any(|e| matches!(e, MotionEvent::Key { .. })));
}

#[test]
fn one_backend_failing_resets_only_its_sources() {
    let (fake, _) = hub();
    let pad = PadId::new(0).unwrap();
    fake.connect("windows.raw_input", src(1), DeviceClass::Keyboard, None);
    fake.connect("gamepad.gilrs", src(2), DeviceClass::Gamepad, Some(pad));
    fake.fail_backend("gamepad.gilrs");
    let mut feed = fake.clone();
    let mut frame = MotionFrame::new();
    MotionFeed::drain(&mut feed, Seconds(1.0), &mut frame);
    assert_eq!(frame.reset(), SourceMask::of(src(2)));
    assert_eq!(feed.pad(pad), None);
    assert!(matches!(frame.events(), [MotionEvent::SourceUp { .. }]), "the keyboard's events still arrive");
}

#[test]
fn the_fake_feed_passes_the_conformance_suite_from_three_backends() {
    let (fake, _) = hub();
    let mut sent = Vec::new();
    for i in 0..600u32 {
        let at = Seconds(f64::from(i % 300) / 120.0 + 0.001);
        let (backend, event) = match i % 3 {
            0 => ("windows.raw_input", MotionEvent::key(src(1), KeyZone::ALL[(i % 16) as usize], i % 2 == 0, at)),
            1 => ("windows.raw_input", MotionEvent::pointer(src(2), 0.001, -0.001, 0.0, at)),
            _ => ("gamepad.gilrs", MotionEvent::buttons(src(3), DeviceClass::Pen, 1, 0, at)),
        };
        fake.push(backend, event);
        sent.push(event);
    }
    let mut feed = fake.clone();
    let summary = conformance::run(&mut feed, &sent, Seconds(3.0), Seconds(1.0 / 60.0)).unwrap();
    assert_eq!(summary.events, 600);
}
