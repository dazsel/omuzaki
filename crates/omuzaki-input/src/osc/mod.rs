//! The OSC backend (#469, PF-05): OSC messages press bindings by id, so a control surface, a Stream Deck (through
//! Bitfocus Companion or an OSC plugin), `TouchOSC` or a script can drive any binding. A message to
//! `/omuzaki/binding/<id>` with a number presses the binding at 0.5 or more and releases it below (a button sends 1
//! and 0), with `T` or `F` presses or releases it, and with no argument is a tap (a press and a release at once). Other
//! addresses, and ids with no binding, do nothing.
//!
//! The port listens on this machine only (127.0.0.1, [`PORT`]): a binding runs its op as the person, so nothing from
//! the network may press one; a phone on the local network is PF-11's paired page, not this. The socket is bound while
//! capture reads OSC and closed while it does not, and whatever waited in it from before a run is dropped unread, so a
//! message sent while paused never presses anything later. OSC makes no motion and has no devices: its messages come
//! from one silent source.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use omuzaki_input_api::binding::BindingId;
use omuzaki_input_api::capture::{BackendState, CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_osc::{Arg, for_each_message};

use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::normalise::{RawEvent, RawSink};

const OSC: InputBackend = InputBackend {
    name: "osc.udp",
    classes: DeviceMask::NONE.with(DeviceClass::Osc),
    caps,
    run: Some(run),
    chosen: Chosen::Always,
};
register_input_backend!(OSC);

/// The UDP port the backend listens on, on 127.0.0.1.
pub const PORT: u16 = 39570;
/// The address prefix of a binding; the binding's id follows it.
pub const PREFIX: &str = "/omuzaki/binding/";
/// The handle of the one silent source the messages come from.
const SOURCE: u64 = 1;
/// How long the loop parks between looks while capture does not read OSC.
const PARK: Duration = Duration::from_millis(250);
/// The longest wait for a datagram; one ends the wait at once.
const WAIT: Duration = Duration::from_millis(50);
/// The largest datagram read whole; a larger one is cut off and fails to parse.
const DATAGRAM: usize = 1536;
/// The fixed reason the backend fails with when it cannot listen.
const TAKEN: &str = "the OSC port is in use by another program";

fn caps() -> CaptureCaps {
    CaptureCaps { osc: Support::Available, ..CaptureCaps::default() }
}

fn run(sink: RawSink) {
    run_with(sink, || UdpSocket::bind((Ipv4Addr::LOCALHOST, PORT)), || {});
}

/// The presses and releases a packet asks for, in order. A malformed packet asks for what came before the fault.
pub(crate) fn presses(packet: &[u8], mut out: impl FnMut(BindingId, bool)) {
    let _parsed = for_each_message(packet, &mut |message| {
        let Some(id) = message.address.strip_prefix(PREFIX).and_then(|id| id.parse().ok()).map(BindingId) else {
            return;
        };
        match message.args().next() {
            None => {
                out(id, true);
                out(id, false);
            }
            Some(Ok(Arg::Bool(down))) => out(id, down),
            Some(Ok(arg)) => {
                if let Some(value) = arg.as_f32() {
                    out(id, value >= 0.5);
                }
            }
            Some(Err(_)) => {}
        }
    });
}

/// The capture loop, listening on the socket `bind` makes (the tests hand in their own): one socket per run of
/// capture, with `ready` called once it listens. A socket that cannot be made fails the backend; a person's next start
/// tries again.
pub(crate) fn run_with(mut sink: RawSink, mut bind: impl FnMut() -> io::Result<UdpSocket>, mut ready: impl FnMut()) {
    let mut socket: Option<UdpSocket> = None;
    let mut run = None;
    let mut datagram = [0u8; DATAGRAM];
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        if !sink.reading() {
            socket = None;
            run = None;
            continue;
        }
        if run != Some(sink.epochs()) {
            // The last run's socket closes first, so the port is free to listen on again.
            drop(socket.take());
            let Some(made) = bind().ok().filter(|s| begin(s, &mut datagram).is_ok()) else {
                sink.set_state(BackendState::failed(TAKEN));
                return;
            };
            socket = Some(made);
            run = Some(sink.epochs());
            sink.set_state(BackendState::Running);
            ready();
        }
        if let Some(listening) = &socket
            && let Ok((len, from)) = listening.recv_from(&mut datagram)
            // A pause that came and went while the loop waited: the datagram belongs to the run that ended.
            && run == Some(sink.epochs())
            && from.ip().is_loopback()
        {
            presses(datagram.get(..len).unwrap_or_default(), |id, down| {
                sink.push(RawEvent::Shortcut { device: SOURCE, class: DeviceClass::Osc, id, down });
            });
        }
        sink.idle();
    }
}

/// Readies a run's socket: drops whatever waited in it unread (each datagram read whole, as some systems fail a short
/// read), then waits at most [`WAIT`] per read.
fn begin(socket: &UdpSocket, datagram: &mut [u8]) -> io::Result<()> {
    socket.set_nonblocking(true)?;
    while socket.recv(datagram).is_ok() {}
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(WAIT))
}

#[cfg(test)]
mod tests {
    use omuzaki_osc::Writer;

    use super::*;

    fn asked(packet: &[u8]) -> Vec<(u32, bool)> {
        let mut out = Vec::new();
        presses(packet, |id, down| out.push((id.0, down)));
        out
    }

    #[test]
    fn a_binding_address_presses_releases_or_taps_its_binding() {
        let at = |id: &str| format!("{PREFIX}{id}");
        assert_eq!(asked(&Writer::message(&at("7"), &[Arg::Float(1.0)])), [(7, true)]);
        assert_eq!(asked(&Writer::message(&at("7"), &[Arg::Float(0.0)])), [(7, false)]);
        assert_eq!(asked(&Writer::message(&at("7"), &[Arg::Int(1)])), [(7, true)]);
        assert_eq!(asked(&Writer::message(&at("7"), &[Arg::Double(0.49)])), [(7, false)]);
        assert_eq!(asked(&Writer::message(&at("12"), &[Arg::Bool(true)])), [(12, true)]);
        assert_eq!(asked(&Writer::message(&at("12"), &[])), [(12, true), (12, false)], "no argument is a tap");
        let bundle = Writer::bundle(&[Writer::message(&at("1"), &[Arg::Int(1)]), Writer::message(&at("2"), &[])]);
        assert_eq!(asked(&bundle), [(1, true), (2, true), (2, false)], "a bundle's messages in order");
    }

    #[test]
    fn anything_else_asks_for_nothing() {
        for address in ["/omuzaki/binding/", "/omuzaki/binding/x", "/omuzaki/binding/7/x", "/other/7", "/omuzaki/7"] {
            assert_eq!(asked(&Writer::message(address, &[Arg::Int(1)])), [], "{address}");
        }
        assert_eq!(asked(&Writer::message(&format!("{PREFIX}7"), &[Arg::Str("on")])), [], "a string is no level");
        assert_eq!(asked(b"not osc"), []);
        assert_eq!(asked(&[]), []);
    }
}
