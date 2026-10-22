//! The MIDI backend (#469, PF-05): MIDI controllers as binding sources, through midir (ALSA on Linux, `CoreMIDI` on
//! macOS, `WinMM` on Windows). A note, a controller or a program change is learned as a binding's chord like a key
//! (`ChordStore::learn`), and pressing it presses the binding: a note is down from its note-on to its note-off, a
//! controller is down while at 64 or more (a pad or a footswitch sends 127 and 0), and a program change is a press and
//! a release at once. Channel-mode controllers (120 to 127: all notes off and the like) are not controls.
//!
//! MIDI makes no motion, so its devices are never announced; each controller is one source, so a note held on one
//! and pressed on another holds its binding until both let go. Every port is opened while capture reads MIDI and
//! closed while it does not, so nothing is read while paused; ports are looked for again every second, so a
//! controller plugged in later is picked up, and one unplugged ends what it held.
//!
//! This file holds what does not need a MIDI system (the message decoding and the capture loop over a
//! [`MidiSource`]), so its tests run everywhere; `ports` is the midir side.
#![cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code, reason = "midir runs on the three desktops; the decoding and its tests run everywhere")
)]

#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
pub(crate) mod ports;
#[cfg(feature = "test-hooks")]
pub(crate) mod script;

use std::time::Duration;

use omuzaki_input_api::capture::BackendState;

use crate::normalise::{MidiControl, RawEvent, RawSink};

/// How long the loop parks between looks while capture does not read MIDI.
const PARK: Duration = Duration::from_millis(250);
/// The longest wait for a message; a message ends the wait at once.
const WAIT: Duration = Duration::from_millis(50);
/// The longest message read: a channel message is at most three bytes (system exclusive is ignored).
pub(crate) const MESSAGE: usize = 3;

/// What a source hands the loop.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Polled {
    /// A message from port `port` (the source's own handle for it): its bytes, `len` of them.
    Message { port: u64, bytes: [u8; MESSAGE], len: u8 },
    /// Port `port` went away.
    Gone(u64),
    /// Nothing within the wait.
    Quiet,
    /// The source ended (the tests' script was dropped).
    Ended,
}

/// Where the loop reads MIDI from: midir's ports, or a test's script.
pub(crate) trait MidiSource {
    /// Opens every port there is and starts listening for more.
    ///
    /// # Errors
    /// A fixed reason when there is no MIDI system to open.
    fn open(&mut self) -> Result<(), &'static str>;

    /// Closes every port and drops what they sent; nothing is read until the next `open`.
    fn close(&mut self);

    /// The next message or departure, waiting at most `wait`.
    fn next(&mut self, wait: Duration) -> Polled;
}

/// The controls a message presses or lets go, in order.
pub(crate) fn controls(bytes: &[u8], mut out: impl FnMut(MidiControl, bool)) {
    let (Some(&status), data) = (bytes.first(), bytes.get(1..).unwrap_or_default()) else { return };
    let channel = status & 0x0F;
    let first = data.first().copied().filter(|b| *b < 0x80);
    let second = data.get(1).copied().filter(|b| *b < 0x80);
    match (status & 0xF0, first, second) {
        (0x90, Some(note), Some(velocity)) => out(MidiControl::new(MidiControl::NOTE, channel, note), velocity > 0),
        (0x80, Some(note), Some(_)) => out(MidiControl::new(MidiControl::NOTE, channel, note), false),
        (0xB0, Some(controller), Some(value)) if controller < 120 => {
            out(MidiControl::new(MidiControl::CONTROLLER, channel, controller), value >= 64);
        }
        (0xC0, Some(program), _) => {
            let control = MidiControl::new(MidiControl::PROGRAM, channel, program);
            out(control, true);
            out(control, false);
        }
        _ => {}
    }
}

/// The capture loop over `source`: one run of open ports per run of capture. A source with no MIDI system fails the
/// backend with its reason and ends the loop; a person's next start tries again.
pub(crate) fn run_with(mut sink: RawSink, mut source: impl MidiSource) {
    let mut run = None;
    loop {
        if !sink.wait_reading(PARK) {
            source.close();
            return;
        }
        if !sink.reading() {
            if run.take().is_some() {
                source.close();
            }
            continue;
        }
        if run != Some(sink.epochs()) {
            source.close();
            if let Err(reason) = source.open() {
                sink.set_state(BackendState::failed(reason));
                return;
            }
            run = Some(sink.epochs());
            sink.set_state(BackendState::Running);
        }
        match source.next(WAIT) {
            // A pause came and went while the loop waited: the message belongs to the run that ended.
            Polled::Message { .. } | Polled::Gone(_) if run != Some(sink.epochs()) => {}
            Polled::Message { port, bytes, len } => {
                controls(bytes.get(..usize::from(len)).unwrap_or_default(), |control, down| {
                    sink.push(RawEvent::Midi { device: port, control, down });
                });
            }
            Polled::Gone(port) => sink.push(RawEvent::DeviceGone { device: port }),
            Polled::Quiet => {}
            Polled::Ended => {
                source.close();
                return;
            }
        }
        sink.idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(bytes: &[u8]) -> Vec<(u16, u8, u8, bool)> {
        let mut out = Vec::new();
        controls(bytes, |c, down| out.push((c.kind(), c.channel(), c.number(), down)));
        out
    }

    #[test]
    fn notes_controllers_and_programs_become_presses() {
        assert_eq!(decoded(&[0x90, 60, 100]), [(MidiControl::NOTE, 0, 60, true)]);
        assert_eq!(decoded(&[0x99, 36, 0]), [(MidiControl::NOTE, 9, 36, false)], "a note-on at velocity 0 is an off");
        assert_eq!(decoded(&[0x83, 60, 64]), [(MidiControl::NOTE, 3, 60, false)]);
        assert_eq!(decoded(&[0xB0, 64, 127]), [(MidiControl::CONTROLLER, 0, 64, true)]);
        assert_eq!(decoded(&[0xB0, 64, 63]), [(MidiControl::CONTROLLER, 0, 64, false)]);
        assert_eq!(decoded(&[0xBF, 20, 64]), [(MidiControl::CONTROLLER, 15, 20, true)], "64 is down");
        assert_eq!(
            decoded(&[0xC2, 5]),
            [(MidiControl::PROGRAM, 2, 5, true), (MidiControl::PROGRAM, 2, 5, false)],
            "a program change is a tap"
        );
    }

    #[test]
    fn everything_else_is_not_a_control() {
        for bytes in [
            &[0xB0, 123, 0][..], // all notes off
            &[0xE0, 0, 64],      // pitch bend
            &[0xD0, 40],         // channel pressure
            &[0xA0, 60, 40],     // key pressure
            &[0xF8],             // clock
            &[0xF0, 1, 2],       // system exclusive
            &[0x90, 60],         // truncated
            &[0x90, 0x80, 1],    // a status byte where data belongs
            &[60, 100],          // running status, which midir never hands on
            &[],
        ] {
            assert_eq!(decoded(bytes), [], "{bytes:?}");
        }
    }
}
