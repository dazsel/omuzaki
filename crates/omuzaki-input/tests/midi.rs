//! MIDI controllers as binding sources (#469, PF-05): the MIDI backend's capture loop over a scripted port list,
//! through the hub to the binding feed. A note, a controller or a program change is learned like a key and presses
//! its binding; two controllers holding one binding end it only when both let go; an unplugged controller ends what
//! it held; the ports are open only while capture reads MIDI; a machine with no MIDI system fails the backend alone.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::sync::Arc;

use omuzaki_input::MidiScript;
use omuzaki_input_api::binding::{BindingAction, BindingId, ChordLabel, Edge};
use omuzaki_input_api::capture::BackendState;
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_input_api::feed::{ChordStore, InputHub};
use support::Rig;

/// The scripted MIDI backend is test backend 2.
const MIDI_BACKEND: usize = 2;
/// Two controllers.
const PADS: u64 = 1;
const KEYS: u64 = 2;

const T: fn(u8) -> BindingAction = BindingAction::Trigger;

const NOTE_ON_C4: [u8; 3] = [0x90, 60, 100];
const NOTE_OFF_C4: [u8; 3] = [0x80, 60, 0];

/// A hub with capture on and the MIDI loop running over a script, with `PADS` plugged in.
fn rig() -> (Rig, MidiScript) {
    let rig = Rig::with_midi();
    rig.start();
    let (script, _thread) = rig.hub.run_midi(MIDI_BACKEND).unwrap();
    assert!(script.sync(), "the capture loop runs");
    assert!(script.is_open(), "capture reads MIDI, so the ports are open");
    script.plug(PADS);
    (rig, script)
}

fn render(label: &ChordLabel) -> String {
    let mut text = String::new();
    label.render(&mut |t| text.push_str(t));
    text
}

/// A trigger binding on whatever `port` sends next, learned the way the binding editor does; returns it and its label.
fn learned(rig: &mut Rig, script: &MidiScript, slot: u8, port: u64, message: &[u8]) -> (BindingId, String) {
    let id = rig.control.add_binding("test", DeviceClass::Midi, T(slot)).unwrap();
    let store: Arc<dyn ChordStore> = rig.hub.chord_store();
    let learning = std::thread::spawn(move || store.learn(id).map(|label| render(&label)));
    while !rig.hub.learning() {
        std::thread::yield_now();
    }
    script.send(port, message);
    let label = learning.join().unwrap().unwrap();
    assert!(script.sync());
    assert!(rig.hits().is_empty(), "the learning press starts nothing");
    (id, label)
}

/// Sends `messages` from `port` and returns the hits they made.
fn send(rig: &mut Rig, script: &MidiScript, port: u64, messages: &[&[u8]]) -> Vec<(BindingId, BindingAction, Edge)> {
    for message in messages {
        script.send(port, message);
    }
    assert!(script.sync());
    rig.hits()
}

#[test]
fn a_learned_note_presses_its_binding_from_note_on_to_note_off() {
    let (mut rig, script) = rig();
    let (id, label) = learned(&mut rig, &script, 1, PADS, &NOTE_ON_C4);
    assert_eq!(label, "MIDI note C4, channel 1");
    assert!(send(&mut rig, &script, PADS, &[&NOTE_OFF_C4]).is_empty(), "the learning note's off ends nothing");

    assert_eq!(send(&mut rig, &script, PADS, &[&NOTE_ON_C4]), [(id, T(1), Edge::Press)]);
    assert!(send(&mut rig, &script, PADS, &[&NOTE_ON_C4]).is_empty(), "a second note-on while held is not a press");
    // A note-on at velocity zero is the usual note-off.
    assert_eq!(send(&mut rig, &script, PADS, &[&[0x90, 60, 0]]), [(id, T(1), Edge::Release)]);
    assert!(send(&mut rig, &script, PADS, &[&[0x90, 61, 100], &[0x91, 60, 100]]).is_empty(), "another note or channel");
}

#[test]
fn a_controller_is_down_from_64_and_a_program_change_is_a_tap() {
    let (mut rig, script) = rig();
    let (pedal, label) = learned(&mut rig, &script, 2, PADS, &[0xB0, 64, 127]);
    assert_eq!(label, "MIDI controller 64, channel 1");
    send(&mut rig, &script, PADS, &[&[0xB0, 64, 0]]);
    assert_eq!(
        send(&mut rig, &script, PADS, &[&[0xB0, 64, 100], &[0xB0, 64, 127], &[0xB0, 64, 10]]),
        [(pedal, T(2), Edge::Press), (pedal, T(2), Edge::Release)],
        "a pedal pressed further is still one press"
    );

    let (program, label) = learned(&mut rig, &script, 3, PADS, &[0xC0, 4]);
    assert_eq!(label, "MIDI program 5, channel 1");
    assert_eq!(
        send(&mut rig, &script, PADS, &[&[0xC0, 4]]),
        [(program, T(3), Edge::Press), (program, T(3), Edge::Release)]
    );
}

#[test]
fn two_controllers_hold_one_binding_until_both_let_go_and_an_unplugged_one_ends_what_it_held() {
    let (mut rig, script) = rig();
    script.plug(KEYS);
    let (id, _) = learned(&mut rig, &script, 1, PADS, &NOTE_ON_C4);
    send(&mut rig, &script, PADS, &[&NOTE_OFF_C4]);

    assert_eq!(send(&mut rig, &script, PADS, &[&NOTE_ON_C4]), [(id, T(1), Edge::Press)]);
    assert!(send(&mut rig, &script, KEYS, &[&NOTE_ON_C4]).is_empty(), "the second holder starts nothing");
    assert!(send(&mut rig, &script, PADS, &[&NOTE_OFF_C4]).is_empty(), "the other still holds it");
    script.unplug(KEYS);
    assert!(script.sync());
    assert_eq!(rig.hits(), [(id, T(1), Edge::Release)], "unplugging the last holder ends it");
}

#[test]
fn the_ports_are_open_only_while_capture_reads_midi() {
    let (mut rig, script) = rig();
    let (id, _) = learned(&mut rig, &script, 1, PADS, &NOTE_ON_C4);
    send(&mut rig, &script, PADS, &[&NOTE_OFF_C4]);
    assert_eq!(send(&mut rig, &script, PADS, &[&NOTE_ON_C4]), [(id, T(1), Edge::Press)]);

    rig.control.pause();
    assert!(script.wait_open(false), "paused: the ports are closed");
    assert_eq!(rig.hits(), [(id, T(1), Edge::Release)], "a pause ends what was held");
    script.send(PADS, &NOTE_OFF_C4);
    script.send(PADS, &NOTE_ON_C4);

    rig.start();
    assert!(script.wait_open(true));
    assert!(script.sync());
    assert_eq!(script.opens(), 2);
    assert!(rig.hits().is_empty(), "what was sent while paused is never read");
    assert!(send(&mut rig, &script, PADS, &[&NOTE_OFF_C4]).is_empty(), "the note held across the pause ends nothing");
    assert_eq!(send(&mut rig, &script, PADS, &[&NOTE_ON_C4]), [(id, T(1), Edge::Press)]);

    // Capture of the keyboard alone does not open the MIDI ports.
    rig.control.stop();
    assert!(script.wait_open(false));
    rig.indicator.beat();
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard)).unwrap();
    assert!(!script.is_open(), "the MIDI loop stays parked with its ports closed");
}

#[test]
fn a_machine_with_no_midi_system_fails_the_midi_backend_alone() {
    let rig = Rig::with_midi();
    let (script, thread) = rig.hub.run_midi(MIDI_BACKEND).unwrap();
    script.without_midi();
    rig.start();
    thread.join().unwrap();
    let report = rig.control.report();
    let midi = report.backends.iter().find(|b| b.name == "c.midi").unwrap();
    assert_eq!(midi.state, BackendState::failed("this machine has no MIDI system"));
    assert!(report.backends.iter().filter(|b| b.name != "c.midi").all(|b| b.state == BackendState::Running));
}
