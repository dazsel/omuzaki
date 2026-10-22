//! IN-3B's acceptance for bindings (design §5, §13): a release ends what its press started, holders, table swaps,
//! lost releases, the detail switch, learning and conflicts.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::sync::Arc;

use omuzaki_input_api::binding::{BindingAction, BindingId, Edge};
use omuzaki_input_api::capture::{Detail, InputError};
use omuzaki_input_api::event::{DeviceClass, MotionEvent, PadZones};
use omuzaki_input_api::feed::{ChordStore, InputHub};
use omuzaki_input_api::zones::KeyZone;
use support::{A, CTRL, F9, F10, J, KEYBOARD, KEYBOARD_2, LEFT_CTRL, PAD, PAD_2, Rig, W, edges, keys};

const T: fn(u8) -> BindingAction = BindingAction::Trigger;

fn render(label: &omuzaki_input_api::binding::ChordLabel) -> String {
    let mut text = String::new();
    label.render(&mut |t| text.push_str(t));
    text
}

#[test]
fn ctrl_f9_ends_when_f9_goes_up_even_after_ctrl() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(1, F9, CTRL);
    rig.desk.key(KEYBOARD, LEFT_CTRL, true);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(1), Edge::Press)]);
    rig.desk.key(KEYBOARD, LEFT_CTRL, false);
    assert!(rig.hits().is_empty(), "letting Ctrl go first ends nothing");
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, T(1), Edge::Release)]);
}

/// Learns the next chord for `id` on another thread while `press` presses it.
fn learn(rig: &mut Rig, id: BindingId, press: impl FnOnce(&mut Rig)) -> Result<String, InputError> {
    let store: Arc<dyn ChordStore> = rig.hub.chord_store();
    let learning = std::thread::spawn(move || store.learn(id).map(|label| render(&label)));
    while !rig.hub.learning() {
        std::thread::yield_now();
    }
    press(rig);
    learning.join().unwrap()
}

#[test]
fn learning_a_new_chord_while_held_ends_the_old_press_and_the_new_chord_starts_only_on_its_own_press() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(2, F9, 0);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(2), Edge::Press)]);
    let label = learn(&mut rig, id, |rig| rig.desk.key(KEYBOARD, F10, true));
    assert_eq!(label.as_deref(), Ok("F10"));
    assert!(rig.hits().is_empty(), "the learning press starts nothing");
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, T(2), Edge::Release)], "the old keys' release ends the old press");
    rig.desk.key(KEYBOARD, F10, false);
    assert!(rig.hits().is_empty());
    rig.desk.key(KEYBOARD, F9, true);
    assert!(rig.hits().is_empty(), "F9 is no longer the chord");
    rig.desk.key(KEYBOARD, F10, true);
    assert_eq!(rig.hits(), [(id, T(2), Edge::Press)]);
    assert_eq!(rig.hub.chord_store().label(id).map(|l| render(&l)).as_deref(), Some("F10"));
}

#[test]
fn an_intent_changed_to_an_op_while_held_ends_on_the_intent_feed_only() {
    let mut rig = Rig::started();
    let walk = rig.locomotion()[0];
    rig.desk.key(KEYBOARD, W, true);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Press]);
    rig.control.set_action(walk, T(3)).unwrap();
    rig.desk.key(KEYBOARD, W, false);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Release]);
    assert!(rig.hits().is_empty(), "nothing reaches the binding feed");
    rig.desk.key(KEYBOARD, W, true);
    assert_eq!(rig.hits(), [(walk, T(3), Edge::Press)], "the next press runs the op");
}

#[test]
fn an_action_changed_while_held_ends_the_old_action_and_never_starts_the_new() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(4, F9, 0);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(4), Edge::Press)]);
    rig.control.set_action(id, T(9)).unwrap();
    rig.desk.repeat(KEYBOARD, F9);
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, T(4), Edge::Repeat), (id, T(4), Edge::Release)]);
}

#[test]
fn a_binding_held_across_six_table_swaps_ends_its_own_action() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(4, F9, 0);
    let other = rig.trigger_on(7, J, 0);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(4), Edge::Press)]);
    for swap in 0..6u8 {
        if swap % 2 == 0 {
            rig.control.set_action(other, T(10 + swap)).unwrap();
        } else {
            rig.control.add_binding("filler", DeviceClass::Keyboard, T(30 + swap)).unwrap();
        }
        assert!(rig.hits().is_empty());
    }
    rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(rig.hits(), [(id, T(4), Edge::Release)]);
}

#[test]
fn a_capture_thread_paused_mid_match_keeps_its_table() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(4, F9, 0);
    assert!(rig.desk.read_key(KEYBOARD, F9, true), "matched against the table of now");
    for slot in 0..12u8 {
        rig.control.add_binding("filler", DeviceClass::Keyboard, T(40 + slot)).unwrap();
    }
    rig.desk.push_read();
    assert_eq!(rig.hits(), [(id, T(4), Edge::Press)], "its own action, after the table it matched was retired");
}

#[test]
fn a_table_swap_with_queued_entries_never_runs_another_binding_s_action() {
    let mut rig = Rig::started();
    let usages = [A, J, F9, F10];
    let ids: Vec<BindingId> = usages.iter().zip(0u8..).map(|(usage, slot)| rig.trigger_on(slot, *usage, 0)).collect();
    for usage in usages {
        rig.desk.key(KEYBOARD, usage, true);
    }
    rig.control.remove(ids[1]).unwrap();
    rig.control.set_action(ids[2], T(20)).unwrap();
    for usage in usages {
        rig.desk.key(KEYBOARD, usage, false);
    }
    let hits = rig.hits();
    assert_eq!(
        hits,
        [
            (ids[0], T(0), Edge::Press),
            (ids[2], T(2), Edge::Press),
            (ids[3], T(3), Edge::Press),
            (ids[0], T(0), Edge::Release),
            (ids[2], T(2), Edge::Release),
            (ids[3], T(3), Edge::Release),
        ],
        "slot 1 is gone and slot 2 keeps the action it was pressed with"
    );
    assert_eq!(rig.hub.stale_entries(), 2, "slot 1's press and release were dropped and counted");
}

#[test]
fn a_binding_held_on_a_removed_table_ends_at_the_swap() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(5, F9, 0);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(5), Edge::Press)]);
    rig.control.remove(id).unwrap();
    assert_eq!(rig.hits(), [(id, T(5), Edge::Release)], "ended at the swap");
    rig.desk.key(KEYBOARD, F9, false);
    assert!(rig.hits().is_empty());
}

#[test]
fn two_holders_end_a_binding_only_when_the_last_lets_go() {
    let mut rig = Rig::started();
    let walk = rig.locomotion()[0];
    rig.desk.device_up(KEYBOARD_2, DeviceClass::Keyboard);
    rig.desk.key(KEYBOARD, W, true);
    rig.desk.key(KEYBOARD_2, W, true);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Press]);
    rig.desk.key(KEYBOARD, W, false);
    rig.desk.key(KEYBOARD, W, false);
    assert!(rig.intents().is_empty(), "the first release (and a release from a source holding nothing) ends nothing");
    rig.desk.key(KEYBOARD_2, W, false);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Release]);

    let jump = rig.control.add_binding("pad jump", DeviceClass::Gamepad, T(5)).unwrap();
    rig.hub.bind_pad(jump, 0b10).unwrap();
    rig.pads.device_up(PAD_2, DeviceClass::Gamepad);
    let still = [[0.0; 2]; 2];
    rig.pads.pad(PAD, 0b10, PadZones::NONE, still, [0.0; 2]);
    rig.pads.pad(PAD_2, 0b10, PadZones::NONE, still, [0.0; 2]);
    assert_eq!(rig.hits(), [(jump, T(5), Edge::Press)]);
    rig.pads.pad(PAD, 0, PadZones::NONE, still, [0.0; 2]);
    assert!(rig.hits().is_empty());
    rig.pads.pad(PAD_2, 0, PadZones::NONE, still, [0.0; 2]);
    assert_eq!(rig.hits(), [(jump, T(5), Edge::Release)]);
}

#[test]
fn unplugging_one_keyboard_of_two_ends_only_its_keys_and_holders() {
    let mut rig = Rig::started();
    let walk = rig.locomotion()[0];
    let wave = rig.trigger_on(6, J, 0);
    rig.desk.device_up(KEYBOARD_2, DeviceClass::Keyboard);
    let second = rig.motion().0.iter().find_map(|e| match *e {
        MotionEvent::SourceUp { src, .. } => Some(src),
        _ => None,
    });
    rig.desk.key(KEYBOARD, W, true);
    rig.desk.key(KEYBOARD_2, W, true);
    rig.desk.key(KEYBOARD_2, J, true);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Press]);
    assert_eq!(rig.hits(), [(wave, T(6), Edge::Press)]);
    let _ = rig.motion();
    rig.desk.device_gone(KEYBOARD_2);
    assert!(rig.intents().is_empty(), "the first keyboard still walks");
    assert_eq!(rig.hits(), [(wave, T(6), Edge::Release)]);
    let (events, _) = rig.motion();
    assert!(events.iter().any(|e| matches!(*e, MotionEvent::SourceGone { src, .. } if Some(src) == second)));
    assert!(keys(&events).is_empty(), "the first keyboard's keys are untouched");
    rig.desk.key(KEYBOARD, W, false);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Release]);
}

#[test]
fn a_release_lost_to_a_full_intent_ring_still_ends_the_movement_within_one_drain() {
    let mut rig = Rig::started();
    let walk = rig.locomotion()[0];
    rig.desk.key(KEYBOARD, W, true);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Press]);
    // The frame thread stalls while the OS repeats W far past the ring.
    for _ in 0..400 {
        rig.desk.repeat(KEYBOARD, W);
    }
    rig.desk.key(KEYBOARD, W, false);
    let first = rig.intents();
    assert_eq!(edges(&first, walk).last(), Some(&Edge::Release), "ended in the first drain after the stall");
    assert_eq!(edges(&first, walk).iter().filter(|e| **e == Edge::Release).count(), 1);
    for _ in 0..20 {
        rig.desk.idle();
        assert!(!edges(&rig.intents(), walk).contains(&Edge::Release), "never a second release");
    }
}

#[test]
fn a_release_lost_to_a_full_binding_ring_still_ends_the_op() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(8, F9, 0);
    rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(rig.hits(), [(id, T(8), Edge::Press)]);
    for _ in 0..400 {
        rig.desk.repeat(KEYBOARD, F9);
    }
    rig.desk.key(KEYBOARD, F9, false);
    let hits = rig.hits();
    assert_eq!(hits.iter().filter(|h| h.2 == Edge::Release).count(), 1, "exactly one release");
    assert_eq!(hits.last().map(|h| h.2), Some(Edge::Release));
    for _ in 0..20 {
        rig.desk.idle();
        assert!(rig.hits().iter().all(|h| h.2 != Edge::Release));
    }
}

#[test]
fn a_detail_switch_resets_the_keyboard_zones_and_keeps_the_movement() {
    let mut rig = Rig::started();
    let walk = rig.locomotion()[0];
    let keyboard = rig.source(DeviceClass::Keyboard);
    rig.desk.key(KEYBOARD, W, true);
    rig.desk.key(KEYBOARD, J, true);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Press]);
    let _ = rig.motion();
    assert!(rig.desk.read_key(KEYBOARD, A, true), "A is read under the old detail");
    rig.hub.set_detail(Detail::HandOnly);
    rig.desk.push_read();
    let (events, reset) = rig.motion();
    assert!(reset.contains(keyboard), "the keyboard's zone holders reset");
    assert!(keys(&events).is_empty(), "no key of the old detail reaches motion");
    assert!(edges(&rig.intents(), walk).is_empty(), "the movement keeps going (bindings ignore the detail)");
    rig.desk.key(KEYBOARD, A, false);
    rig.desk.key(KEYBOARD, A, true);
    let zones: Vec<KeyZone> = keys(&rig.motion().0).into_iter().map(|(_, zone, _)| zone).collect();
    assert_eq!(zones.last(), Some(&KeyZone::LEFT_ONLY), "hand-only detail sends the side");
    assert_eq!(rig.control.report().detail, Detail::HandOnly);
    rig.desk.key(KEYBOARD, W, false);
    assert_eq!(edges(&rig.intents(), walk), [Edge::Release]);
}

#[test]
fn learning_needs_capture_on() {
    let mut rig = Rig::new();
    let id = rig.trigger_on(1, F9, 0);
    let store = rig.hub.chord_store();
    assert_eq!(store.learn(id).map(|l| render(&l)), Err(InputError::NotCapturing), "off");
    rig.start();
    rig.control.pause();
    assert_eq!(store.learn(id).map(|l| render(&l)), Err(InputError::NotCapturing), "paused");
    rig.start();
    assert_eq!(store.learn(BindingId(999)).map(|l| render(&l)), Err(InputError::UnknownBinding(BindingId(999))));
    rig.drain_all();
}

#[test]
fn a_pause_while_learning_ends_the_learning() {
    let mut rig = Rig::started();
    let id = rig.trigger_on(1, F9, 0);
    let result = learn(&mut rig, id, |rig| rig.control.pause());
    assert_eq!(result, Err(InputError::NotCapturing));
    assert!(!rig.hub.learning());
}

#[test]
fn chords_with_modifiers_match_and_shared_chords_are_reported_as_conflicts() {
    let rig = Rig::started();
    let a = rig.trigger_on(1, F9, CTRL);
    let b = rig.trigger_on(2, F9, CTRL);
    let c = rig.trigger_on(3, F9, 0);
    let store = rig.hub.chord_store();
    let mut out = Vec::new();
    store.conflicts(a, &mut out);
    assert_eq!(out, [b]);
    store.conflicts(c, &mut out);
    assert!(out.is_empty());
    let infos = rig.control.bindings();
    let conflict = |id| infos.iter().find(|i| i.id == id).map(|i| i.conflict);
    assert_eq!((conflict(a), conflict(b), conflict(c)), (Some(true), Some(true), Some(false)));
    assert_eq!(store.label(a).map(|l| render(&l)).as_deref(), Some("Ctrl+F9"));
    store.clear(b);
    store.conflicts(a, &mut out);
    assert!(out.is_empty());
    assert!(store.label(b).is_none());
}

#[test]
fn bindings_and_the_protected_list_survive_a_restart_and_capture_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let rig = Rig::in_dir(Some(dir.path().to_owned()));
        rig.start();
        rig.control.protect(&["vault".to_owned()]);
        rig.trigger_on(1, F9, CTRL)
    };
    let rig = Rig::in_dir(Some(dir.path().to_owned()));
    assert_eq!(rig.state(), omuzaki_input_api::capture::CaptureState::Off, "never restored");
    assert!(rig.control.bindings().iter().any(|b| b.id == id && b.action == T(1)));
    assert_eq!(rig.hub.chord_store().label(id).map(|l| render(&l)).as_deref(), Some("Ctrl+F9"));
    assert_eq!(rig.hub.protect_list().get(), ["vault"]);
    assert_eq!(rig.control.presets(), ["locomotion"]);
    assert_eq!(rig.control.preset_actions("locomotion").unwrap().len(), 7);
    assert!(matches!(rig.control.apply_preset("nope"), Err(InputError::UnknownPreset(_))));
}
