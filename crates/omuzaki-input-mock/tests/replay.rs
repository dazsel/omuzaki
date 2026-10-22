//! IN-2's acceptance for the replay and the conformance suite: a 60 s replay is identical over two runs and after a
//! text round trip, nothing after `up_to` (proptest), 1,000 due events all arrive in order with every release, and a
//! replay from several backends hands them out in exactly the recorded order.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test scripts

use omuzaki_core::time::Seconds;
use omuzaki_input_api::event::{DeviceClass, MotionEvent, PadId, PadSet, PadState, PadZones, SourceTag};
use omuzaki_input_api::feed::{MotionFeed, MotionFrame};
use omuzaki_input_api::zones::KeyZone;
use omuzaki_input_mock::conformance;
use omuzaki_input_mock::replay::{Entry, ReplaySource};
use proptest::prelude::*;

fn src(i: u8) -> SourceTag {
    SourceTag::new(i).unwrap()
}

/// A deterministic generator (a 64-bit LCG), so the script is the same on every run and machine.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 60 s of typing, mousing and a pad from three backends, at the 1/120 s coalescing period.
fn script() -> ReplaySource {
    let mut rng = Lcg(0x5eed);
    let mut out = ReplaySource::new();
    let (kb, mouse, pad_src) = (src(1), src(2), src(3));
    let pad = PadId::new(0).unwrap();
    out.push("windows.raw_input", Entry::Event(MotionEvent::source_up(kb, DeviceClass::Keyboard, None, Seconds(0.0))));
    out.push("windows.raw_input", Entry::Event(MotionEvent::source_up(mouse, DeviceClass::Mouse, None, Seconds(0.0))));
    out.push(
        "gamepad.gilrs",
        Entry::Event(MotionEvent::source_up(pad_src, DeviceClass::Gamepad, Some(pad), Seconds(0.0))),
    );
    let mut held: Option<KeyZone> = None;
    for tick in 1..=7200u32 {
        let at = Seconds(f64::from(tick) / 120.0);
        match rng.below(6) {
            0 => {
                let zone = KeyZone::ALL[rng.below(16) as usize];
                if let Some(up) = held.take() {
                    out.push("windows.raw_input", Entry::Event(MotionEvent::key(kb, up, false, at)));
                }
                out.push("windows.raw_input", Entry::Event(MotionEvent::key(kb, zone, true, at)));
                held = Some(zone);
            }
            1 => {
                let (dx, dy) = ((rng.below(2001) as f32 - 1000.0) * 1e-6, (rng.below(2001) as f32 - 1000.0) * 1e-6);
                out.push("windows.raw_input", Entry::Event(MotionEvent::pointer(mouse, dx, dy, 0.0, at)));
            }
            2 => {
                let button = 1 << rng.below(3);
                out.push(
                    "windows.raw_input",
                    Entry::Event(MotionEvent::buttons(mouse, DeviceClass::Mouse, button, button, at)),
                );
            }
            3 => {
                let zone = PadZones::NAMED[rng.below(9) as usize].0;
                out.push("gamepad.gilrs", Entry::Event(MotionEvent::pad_edges(pad, zone, PadZones::NONE, at)));
                out.push("gamepad.gilrs", Entry::Event(MotionEvent::pad_edges(pad, PadZones::NONE, zone, at)));
            }
            4 => {
                let stick = || (rng.below(255) as i16 - 127) as i8;
                let mut stick = stick;
                let levels = PadState {
                    pad,
                    sticks: [[stick(), stick()], [stick(), stick()]],
                    triggers: [rng.below(256) as u8, rng.below(256) as u8],
                    zones: PadZones::NONE,
                    at,
                };
                out.push("gamepad.gilrs", Entry::Pad(levels));
            }
            _ => {}
        }
    }
    out
}

/// Everything a consumer sees, frame by frame at 60 fps: the events and the pad's levels.
fn play(feed: &mut dyn MotionFeed, seconds: u32) -> Vec<(Vec<MotionEvent>, Option<PadState>)> {
    let mut frame = MotionFrame::new();
    let pad = PadId::new(0).unwrap();
    (1..=seconds * 60)
        .map(|f| {
            feed.drain(Seconds(f64::from(f) / 60.0), &mut frame);
            (frame.events().to_vec(), feed.pad(pad))
        })
        .collect()
}

#[test]
fn a_60_second_replay_is_identical_over_two_runs_and_after_a_text_round_trip() {
    let mut replay = script();
    let first = play(&mut replay, 60);
    assert!(replay.is_done());
    replay.rewind();
    assert_eq!(play(&mut replay, 60), first, "a second run");

    let text = replay.to_text();
    let mut read = ReplaySource::from_text(&text).unwrap();
    assert_eq!(read.to_text(), text, "the text round-trips");
    assert_eq!(play(&mut read, 60), first, "after the round trip");
    let events: usize = first.iter().map(|(events, _)| events.len()).sum();
    assert!(events > 3000, "{events} events");
}

#[test]
fn a_replay_from_several_backends_keeps_the_recorded_order_on_every_run() {
    // Drained order is not time order across backends: the pad's event stamped earlier was drained later.
    let text = "\
# omuzaki recorded input 1
0.01 windows.raw_input key 1 space down
0.02 windows.raw_input key 1 space up
0.005 gamepad.gilrs pad_edges 0 face -
0.015 linux.x11 pointer 2 0.25 -0.5 1
0.006 gamepad.gilrs pad_edges 0 - face
";
    let mut replay = ReplaySource::from_text(text).unwrap();
    let backends: Vec<&str> = replay.entries().map(|(b, _)| b).collect();
    assert_eq!(backends, ["windows.raw_input", "windows.raw_input", "gamepad.gilrs", "linux.x11", "gamepad.gilrs"]);
    for _ in 0..3 {
        replay.rewind();
        let mut frame = MotionFrame::new();
        replay.drain(Seconds(1.0), &mut frame);
        let order: Vec<f64> = frame.events().iter().map(|e| e.at().0).collect();
        assert_eq!(order, [0.01, 0.02, 0.005, 0.015, 0.006]);
    }
}

#[test]
fn bad_lines_name_their_line() {
    for (text, line) in [
        ("0.1 b key 1 f9 down", 1),
        ("# header\n\n0.1 b key 1 space sideways", 3),
        ("0.1 b key 99 space down", 1),
        ("0.1 b warp 1", 1),
        ("0.1 b source_gone 1 extra", 1),
        ("soon b source_gone 1", 1),
    ] {
        assert_eq!(ReplaySource::from_text(text).unwrap_err().line, line, "{text}");
    }
}

#[test]
fn a_thousand_due_events_all_arrive_in_order_with_every_release() {
    let mut replay = ReplaySource::new();
    let mut sent = Vec::new();
    for i in 0..1000u32 {
        let src = src((i % 5) as u8);
        let event = MotionEvent::key(src, KeyZone::ALL[(i % 16) as usize], i % 2 == 0, Seconds(0.001));
        replay.push("b", Entry::Event(event));
        sent.push(event);
    }
    let summary = conformance::run(&mut replay, &sent, Seconds(0.5), Seconds(1.0 / 60.0)).unwrap();
    assert_eq!(summary.events, 1000);
    assert_eq!(summary.fullest, 128, "the first drains are full and the rest wait");
}

#[test]
fn a_source_gone_hides_its_pad() {
    let text = "\
0.0 g source_up 3 gamepad 1
0.0 g pad 1 10 20 30 40 50 60 face
0.5 g source_gone 3
";
    let mut replay = ReplaySource::from_text(text).unwrap();
    let mut frame = MotionFrame::new();
    let pad = PadId::new(1).unwrap();
    replay.drain(Seconds(0.1), &mut frame);
    assert_eq!(replay.pad(pad).unwrap().sticks, [[10, 20], [30, 40]]);
    let mut pads = PadSet::new();
    replay.pads(&mut pads);
    assert_eq!(pads.iter().collect::<Vec<_>>(), [pad]);
    replay.drain(Seconds(1.0), &mut frame);
    assert_eq!(replay.pad(pad), None);
}

proptest! {
    #[test]
    fn a_drain_never_returns_an_event_after_its_up_to(
        mut times in prop::collection::vec(0u32..10_000, 1..400),
        mut drains in prop::collection::vec(0u32..10_000, 1..40),
    ) {
        times.sort_unstable();
        drains.sort_unstable();
        let mut replay = ReplaySource::new();
        for (i, t) in times.iter().enumerate() {
            let event = MotionEvent::key(src((i % 64) as u8), KeyZone::SPACE, i % 2 == 0, Seconds(f64::from(*t) / 1000.0));
            replay.push("b", Entry::Event(event));
        }
        let mut frame = MotionFrame::new();
        let mut seen = 0;
        for d in drains {
            let up_to = Seconds(f64::from(d) / 1000.0);
            replay.drain(up_to, &mut frame);
            prop_assert!(frame.events().iter().all(|e| e.at().0 <= up_to.0));
            seen += frame.events().len();
        }
        prop_assert!(seen <= times.len());
    }
}
