//! OSC messages as binding sources (#469, PF-05): the OSC backend's capture loop on a loopback socket, through the hub
//! to the binding feed. `/omuzaki/binding/<id>` presses and releases a binding (a key held at the same time is a second
//! holder), no argument taps it, a datagram sent while paused is dropped unread, and capture without OSC reads nothing.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use omuzaki_input_api::binding::{BindingAction, BindingId, Edge};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_osc::{Arg, Writer};
use support::{F9, KEYBOARD, Rig};

/// The OSC backend is test backend 2.
const OSC_BACKEND: usize = 2;
/// How long a test waits for a hit before it fails: a ceiling, never an ordering.
const CEILING: Duration = Duration::from_secs(10);

const T: fn(u8) -> BindingAction = BindingAction::Trigger;

struct Osc {
    rig: Rig,
    /// The backend's socket (it listens on a clone) and the sender's.
    listening: UdpSocket,
    sender: UdpSocket,
    /// How many runs' sockets were made ready.
    runs: Arc<AtomicU32>,
}

impl Osc {
    fn new() -> Self {
        let mut rig = Rig::with_osc();
        rig.start();
        rig.desk.device_up(KEYBOARD, DeviceClass::Keyboard);
        let listening = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (runs, _thread) = rig.hub.run_osc(OSC_BACKEND, &listening).unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let osc = Self { rig, listening, sender, runs };
        assert!(osc.until_runs(1), "the OSC loop runs");
        osc
    }

    /// Waits until `n` runs' sockets were ready.
    fn until_runs(&self, n: u32) -> bool {
        let start = Instant::now();
        while start.elapsed() < CEILING {
            if self.runs.load(Ordering::Acquire) >= n {
                return true;
            }
            std::thread::park_timeout(Duration::from_millis(2));
        }
        false
    }

    fn send(&self, id: BindingId, args: &[Arg<'_>]) {
        let packet = Writer::message(&format!("/omuzaki/binding/{}", id.0), args);
        self.sender.send_to(&packet, self.listening.local_addr().unwrap()).unwrap();
    }

    /// The hits there are once `want` of them arrived, or at the ceiling.
    fn hits(&mut self, want: usize) -> Vec<(BindingId, BindingAction, Edge)> {
        let start = Instant::now();
        let mut hits = Vec::new();
        while hits.len() < want && start.elapsed() < CEILING {
            hits.extend(self.rig.hits());
            std::thread::park_timeout(Duration::from_millis(2));
        }
        hits
    }

    /// A trigger binding on F9, as the binding editor would make one.
    fn binding(&self, slot: u8) -> BindingId {
        let id = self.rig.control.add_binding("wave", DeviceClass::Keyboard, T(slot)).unwrap();
        self.rig.hub.bind_key(id, F9, 0).unwrap();
        id
    }
}

#[test]
fn a_binding_address_presses_and_releases_its_binding_and_no_argument_taps_it() {
    let mut osc = Osc::new();
    let id = osc.binding(1);
    osc.send(id, &[Arg::Float(1.0)]);
    assert_eq!(osc.hits(1), [(id, T(1), Edge::Press)]);
    osc.send(id, &[Arg::Float(0.0)]);
    assert_eq!(osc.hits(1), [(id, T(1), Edge::Release)]);
    osc.send(id, &[]);
    assert_eq!(osc.hits(2), [(id, T(1), Edge::Press), (id, T(1), Edge::Release)]);
    // An id with no binding presses nothing, and the next message still works.
    osc.send(BindingId(999), &[Arg::Int(1)]);
    osc.send(id, &[Arg::Int(1)]);
    assert_eq!(osc.hits(1), [(id, T(1), Edge::Press)]);
}

#[test]
fn osc_and_the_key_hold_one_binding_until_both_let_go() {
    let mut osc = Osc::new();
    let id = osc.binding(2);
    osc.rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(osc.hits(1), [(id, T(2), Edge::Press)]);
    osc.send(id, &[Arg::Bool(true)]);
    // The loop reads in order: once a later tap arrives, the OSC press was had.
    let sentinel = osc.rig.control.add_binding("sentinel", DeviceClass::Osc, T(9)).unwrap();
    osc.send(sentinel, &[]);
    assert_eq!(
        osc.hits(2),
        [(sentinel, T(9), Edge::Press), (sentinel, T(9), Edge::Release)],
        "the second holder starts nothing"
    );
    osc.rig.desk.key(KEYBOARD, F9, false);
    osc.send(id, &[Arg::Bool(false)]);
    assert_eq!(osc.hits(1), [(id, T(2), Edge::Release)], "one release, when the last holder lets go");
}

#[test]
fn a_message_sent_while_paused_is_dropped_unread() {
    let mut osc = Osc::new();
    let id = osc.binding(3);
    osc.rig.control.pause();
    osc.send(id, &[Arg::Int(1)]);
    osc.rig.start();
    assert!(osc.until_runs(2), "the second run's socket is ready");
    osc.send(id, &[]);
    assert_eq!(osc.hits(2), [(id, T(3), Edge::Press), (id, T(3), Edge::Release)], "only the tap after the resume");
}

#[test]
fn capture_without_osc_reads_nothing() {
    let mut osc = Osc::new();
    let id = osc.binding(4);
    osc.rig.control.stop();
    osc.rig.indicator.beat();
    osc.rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard)).unwrap();
    osc.send(id, &[Arg::Int(1)]);
    // The key still works; it is the handshake that everything sent before it was had its chance.
    osc.rig.desk.key(KEYBOARD, F9, true);
    assert_eq!(osc.hits(1), [(id, T(4), Edge::Press)], "the key's press alone");
    osc.rig.desk.key(KEYBOARD, F9, false);
    assert_eq!(osc.hits(1), [(id, T(4), Edge::Release)]);
}
