//! IN-3B's privacy acceptance (design §3 rules 3 and 5): two key streams that differ only in which key of a zone was
//! pressed give byte-identical output on every surface the crate has, and a panic on a capture thread writes no key
//! value.
// Helpers outside a #[test] unwrap too; an empty feed is asserted plainly.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod support;

use std::fmt::Write as _;

use omuzaki_input::keys_in_zone;
use omuzaki_input_api::capture::BackendState;
use omuzaki_input_api::event::DeviceClass;
use omuzaki_input_api::zones::KeyZone;
use proptest::prelude::*;
use support::{F9, KEYBOARD, Rig};

/// What a step does to its key.
#[derive(Copy, Clone, Debug)]
enum Act {
    Down,
    Up,
    Repeat,
}

/// Everything the crate shows about a run, as text: every motion frame and its reset, the intent and binding feeds,
/// the capture report, the indicator's view and the stale counter.
fn surfaces(steps: &[(u16, Act)]) -> String {
    let mut rig = Rig::started();
    let mut out = String::new();
    for (i, (usage, act)) in steps.iter().enumerate() {
        match act {
            Act::Down => rig.desk.key(KEYBOARD, *usage, true),
            Act::Up => rig.desk.key(KEYBOARD, *usage, false),
            Act::Repeat => rig.desk.repeat(KEYBOARD, *usage),
        }
        if i % 3 == 0 {
            rig.advance(0.004);
            rig.desk.idle();
        }
        let (events, reset) = rig.motion();
        writeln!(out, "{} {}", serde_json::to_string(&events).unwrap(), reset.bits()).unwrap();
        writeln!(out, "{:?} {:?}", rig.intents(), rig.hits()).unwrap();
    }
    writeln!(out, "{}", serde_json::to_string(&rig.control.report()).unwrap()).unwrap();
    writeln!(out, "{:?} {}", rig.indicator.view(), rig.hub.stale_entries()).unwrap();
    out
}

/// The zones that hold keys, each with its keys.
fn zones() -> Vec<Vec<u16>> {
    (0..16u8).filter_map(KeyZone::new).map(keys_in_zone).filter(|k| !k.is_empty()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn swapping_keys_inside_their_zones_changes_no_output(
        steps in prop::collection::vec((0usize..16, 0usize..40, 0u8..3), 0..80),
        seed in any::<u64>(),
    ) {
        let zones = zones();
        // One random permutation of each zone's keys: the second stream presses the image of each key.
        let mut state = seed | 1;
        let mut next = || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
        let images: Vec<Vec<u16>> = zones
            .iter()
            .map(|keys| {
                let mut image = keys.clone();
                for i in (1..image.len()).rev() {
                    let j = usize::try_from(next() % (i as u64 + 1)).unwrap();
                    image.swap(i, j);
                }
                image
            })
            .collect();
        let act = |a: u8| match a { 0 => Act::Down, 1 => Act::Up, _ => Act::Repeat };
        let first: Vec<(u16, Act)> =
            steps.iter().map(|(z, k, a)| { let keys = &zones[z % zones.len()]; (keys[k % keys.len()], act(*a)) }).collect();
        let second: Vec<(u16, Act)> = steps
            .iter()
            .map(|(z, k, a)| { let image = &images[z % zones.len()]; (image[k % image.len()], act(*a)) })
            .collect();
        prop_assert_eq!(surfaces(&first), surfaces(&second));
    }
}

#[test]
fn a_key_from_another_zone_does_change_the_output() {
    // The property has teeth: A and J are in different zones.
    assert_ne!(surfaces(&[(support::A, Act::Down)]), surfaces(&[(support::J, Act::Down)]));
}

#[test]
fn the_zone_table_has_keys_in_every_zone_but_other() {
    let zones = zones();
    assert!(zones.len() >= 15, "{} zones hold keys", zones.len());
    assert!(zones.iter().map(Vec::len).sum::<usize>() >= 109, "the JIS board's keys at least");
}

const CHILD: &str = "OMUZAKI_INPUT_PANIC_CHILD";

#[test]
fn a_panic_on_a_capture_thread_writes_no_key_value() {
    if std::env::var_os(CHILD).is_some() {
        // The child: press F9 (bound, so the chord is matched) on a capture thread, then panic there.
        let rig = Rig::started();
        rig.trigger_on(1, F9, support::CTRL);
        let thread = rig.hub.run_backend(0, |injector| {
            injector.device_up(KEYBOARD, DeviceClass::Keyboard);
            injector.key(KEYBOARD, support::LEFT_CTRL, true);
            injector.key(KEYBOARD, F9, true);
            panic!("forced capture-thread panic");
        });
        thread.unwrap().join().unwrap();
        let report = rig.control.report();
        let desk = report.backends.iter().find(|b| b.name == "a.desk").unwrap();
        assert_eq!(desk.state, BackendState::failed("the backend stopped unexpectedly"));
        eprintln!("child report: {report:?}");
        eprintln!("child view: {:?}", rig.indicator.view());
        eprintln!("child bindings: {:?}", rig.control.bindings());
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .args(["--exact", "a_panic_on_a_capture_thread_writes_no_key_value", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "the child failed:\n{stdout}\n{stderr}");
    assert!(stderr.contains("forced capture-thread panic"), "the panic was printed:\n{stderr}");
    assert!(stderr.contains("child report"), "the child reported:\n{stderr}");
    for leak in ["F9", "0x42", "0x43", "Ctrl", "0xE0", "0xe0", "usage", "HidUsage"] {
        assert!(!stderr.contains(leak), "stderr names {leak}:\n{stderr}");
        assert!(!stdout.contains(leak), "stdout names {leak}:\n{stdout}");
    }
}
