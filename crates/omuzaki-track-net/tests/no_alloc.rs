//! Proves each protocol's decoder allocates nothing per datagram, so the receiver thread runs allocation-free after
//! start (publish and the clock map are proven in `omuzaki-track-api/tests/hot_path.rs`). Only allocations on the
//! measuring thread count.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{DeviceProfile, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_net::Protocol;
use omuzaki_track_net::protocol::live_link::{self, VALUES};
use omuzaki_track_net::protocol::osc::{Arg, Writer};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn packets(protocol: Protocol) -> Vec<Vec<u8>> {
    match protocol {
        Protocol::Ifacialmocap => vec![
            b"mouthSmile_R-12|eyeBlink_L-87|jawOpen&40|tongueOut-0|=head#-2.0,10.0,1.5,0.01,-0.02,-0.45|rightEye#3.0,1.0,0.0|leftEye#4.0,2.0,0.0|"
                .to_vec(),
        ],
        Protocol::VtubeStudio => vec![
            br#"{"Timestamp": 1000.5, "Hotkey": -1, "FaceFound": true, "Rotation": {"x": -3.0, "y": 12.0, "z": 2.0},
                "Position": {"x": 1.0, "y": -2.0, "z": 3.0}, "EyeLeft": {"x": 2.0, "y": 4.0, "z": 0.0},
                "BlendShapes": [{"k": "EyeBlinkLeft", "v": 0.25}, {"k": "JawOpen", "v": 0.5}], "VersionNumber": 2}"#
                .to_vec(),
        ],
        Protocol::LiveLinkFace => vec![live_link::encode("device", "subject", 7, 60, &[0.25; VALUES])],
        Protocol::Vmc => vec![Writer::bundle(&[
            Writer::message("/VMC/Ext/T", &[Arg::Float(1.0)]),
            Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str("JawOpen"), Arg::Float(0.5)]),
            Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str("Blink_L"), Arg::Float(0.5)]),
            Writer::message(
                "/VMC/Ext/Bone/Pos",
                &[Arg::Str("Head"), Arg::Float(0.0), Arg::Float(1.5), Arg::Float(0.0), Arg::Float(0.0),
                  Arg::Float(0.1), Arg::Float(0.0), Arg::Float(0.99)],
            ),
            Writer::message("/VMC/Ext/Blend/Apply", &[]),
        ])],
    }
}

#[test]
fn decoding_allocates_nothing() {
    for protocol in Protocol::ALL {
        let mut decoder = protocol.decoder();
        let packets = packets(protocol);
        let mut out = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit);
        // Warm up once outside the measurement (nothing here is lazily allocated, but the proof should not depend on it).
        decoder.decode(&packets[0], &mut out).unwrap();
        ALLOCATIONS.store(0, Ordering::Relaxed);
        MEASURING.with(|m| m.set(true));
        for _ in 0..1000 {
            for packet in &packets {
                assert!(decoder.decode(packet, &mut out).unwrap().is_some());
            }
        }
        MEASURING.with(|m| m.set(false));
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0, "{protocol:?} allocated while decoding");
    }
}
