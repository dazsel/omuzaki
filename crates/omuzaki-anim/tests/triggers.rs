//! Expression triggers on the starter `ExpressionSet` over a neutral rest, and the proof that stepping them allocates
//! nothing (spec §8 "no allocation in the hot loop").
//!
//! The allocator counts only allocations made on the measuring thread, so the harness's own threads cannot make the
//! count flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::float_cmp, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::triggers::{
    MAX_TRIGGERS, TriggerError, TriggerFades, TriggerMode, TriggerSlot, Triggers, expression_slot,
};
use omuzaki_anim_api::{BlendMode, Bus, Channel, ChannelBuf, ChannelMask, Layer, LayerStack, Source};
use omuzaki_face_api::{ExpressionId, ExpressionSet};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
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

const DT: f32 = 1.0 / 60.0;

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).unwrap_or_else(|e| panic!("expression id {name}: {e}"))
}

fn slot(name: &str, mode: TriggerMode) -> TriggerSlot {
    expression_slot(&ExpressionSet::starter(), &id(name), &ChannelBuf::NEUTRAL, mode)
        .unwrap_or_else(|e| panic!("starter expression {name}: {e}"))
}

/// Happy in slot 0 and angry in slot 1.
fn rig(mode: TriggerMode) -> Triggers {
    let mut t = Triggers::new(TriggerFades::default());
    t.set_slot(0, Some(slot("happy", mode))).unwrap_or_else(|e| panic!("slot 0: {e}"));
    t.set_slot(1, Some(slot("angry", mode))).unwrap_or_else(|e| panic!("slot 1: {e}"));
    t
}

fn layer() -> Layer {
    Layer::absent(BlendMode::Override)
}

/// Steps for `seconds` in 1/60 s frames; returns the last weight.
fn run(t: &mut Triggers, out: &mut Layer, seconds: f32) -> f32 {
    let frames = (seconds / DT).ceil() as usize;
    for _ in 0..frames {
        t.step(DT, out);
    }
    out.weight
}

#[test]
fn toggle_fades_in_over_0_12_s_and_out_over_0_2_s() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(0);
    assert!(run(&mut t, &mut out, 0.12 - DT) < 1.0, "not there a step early");
    assert_eq!(run(&mut t, &mut out, DT), 1.0, "full after 0.12 s, within one step");
    assert_eq!(out.mode, BlendMode::Override);
    assert!(out.mask.contains(Channel::MOUTH_SMILE_LEFT));
    assert!(out.values[Channel::MOUTH_SMILE_LEFT] > 0.5);

    t.press(0);
    assert_eq!(t.active(), Some(0), "still fading out");
    for _ in 0..11 {
        t.step(DT, &mut out);
    }
    assert!(out.weight > 0.0, "not gone a step early");
    // 0.2 s is 12 steps; f32 rounding may leave the last one a hair above 0, so allow one more.
    t.step(DT, &mut out);
    t.step(DT, &mut out);
    assert_eq!(out.weight, 0.0, "gone after 0.2 s, within one step");
    assert!(!out.is_active() && out.mask.is_empty());
    assert_eq!(t.active(), None);
}

#[test]
fn hold_shows_while_down_and_fades_out_on_release() {
    let mut t = rig(TriggerMode::Hold);
    let mut out = layer();
    t.press(0);
    assert_eq!(run(&mut t, &mut out, 0.5), 1.0);
    run(&mut t, &mut out, 1.0);
    assert_eq!(out.weight, 1.0, "stays while held");
    t.release(0);
    assert_eq!(run(&mut t, &mut out, 0.25), 0.0);
    t.release(0);
    t.step(DT, &mut out);
    assert!(!out.is_active());
}

#[test]
fn toggle_ignores_release_and_hold_ignores_a_second_press() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(0);
    t.release(0);
    assert_eq!(run(&mut t, &mut out, 0.5), 1.0);
    let mut t = rig(TriggerMode::Hold);
    t.press(0);
    t.press(0);
    assert_eq!(run(&mut t, &mut out, 0.5), 1.0);
}

#[test]
fn switching_fades_the_first_out_before_the_second_comes_in() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(0);
    run(&mut t, &mut out, 0.5);
    t.press(1);
    assert_eq!(t.active(), Some(1));
    let happy = slot("happy", TriggerMode::Toggle);
    let angry = slot("angry", TriggerMode::Toggle);
    let mut reached_zero = false;
    for _ in 0..60 {
        t.step(DT, &mut out);
        if !out.is_active() {
            reached_zero = true;
            continue;
        }
        // Whatever shows is one expression whole: every masked channel matches that slot.
        let (own, other) =
            if out.mask == happy.mask && out.values == happy.target { (happy, angry) } else { (angry, happy) };
        assert_eq!(out.mask, own.mask);
        assert!(own.mask.into_iter().all(|c| out.values[c] == own.target[c]));
        if own.target == angry.target {
            assert!(reached_zero, "angry appeared before the weight reached 0");
        } else {
            assert!(!reached_zero, "happy came back");
        }
        assert_ne!(own.target, other.target);
    }
    assert!(reached_zero);
    assert_eq!(out.weight, 1.0);
    assert_eq!(out.values[Channel::BROW_DOWN_LEFT], angry.target[Channel::BROW_DOWN_LEFT]);
}

#[test]
fn pressing_another_while_fading_in_reverses_from_the_current_weight() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(0);
    let before = run(&mut t, &mut out, 0.06);
    assert!(before > 0.0 && before < 1.0);
    t.press(1);
    t.step(DT, &mut out);
    assert!(out.weight < before, "happy fades out from where it was");
    assert!(out.mask.contains(Channel::MOUTH_SMILE_LEFT));
}

#[test]
fn ownership_changes_hands_at_half_weight() {
    let mut t = rig(TriggerMode::Toggle);
    let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
    stack.tracker.mask = ChannelMask::of(Channel::MOUTH_SMILE_LEFT);
    stack.tracker.weight = 1.0;
    let mut bus = Bus::default();
    t.press(0);
    let mut seen = [false; 2];
    for _ in 0..12 {
        t.step(DT, &mut stack.trigger);
        stack.resolve(&mut bus);
        let owner = bus.owners.of(Channel::MOUTH_SMILE_LEFT);
        if stack.trigger.weight > 0.5 {
            assert_eq!(owner, Source::Trigger);
            seen[1] = true;
        } else if stack.trigger.weight < 0.5 {
            assert_eq!(owner, Source::Tracker);
            seen[0] = true;
        }
    }
    assert_eq!(seen, [true, true]);
}

#[test]
fn bad_dt_is_clamped() {
    let mut out = layer();
    let mut t = Triggers::new(TriggerFades { in_s: 1.0, out_s: 1.0 });
    t.set_slot(0, Some(slot("happy", TriggerMode::Toggle))).unwrap_or_else(|e| panic!("slot 0: {e}"));
    t.press(0);
    t.step(f32::NAN, &mut out);
    assert!(!out.is_active(), "NaN is a zero step");
    t.step(-1.0, &mut out);
    assert!(!out.is_active(), "negative is a zero step");
    t.step(100.0, &mut out);
    assert_eq!(out.weight, 0.25, "a long step counts as 0.25 s");
}

#[test]
fn set_slot_checks_its_arguments() {
    let mut t = Triggers::new(TriggerFades::default());
    let ok = slot("happy", TriggerMode::Toggle);
    assert_eq!(t.set_slot(MAX_TRIGGERS, Some(ok)), Err(TriggerError::SlotOutOfRange { slot: 16 }));
    assert_eq!(t.set_slot(MAX_TRIGGERS, None), Err(TriggerError::SlotOutOfRange { slot: 16 }));
    let empty = TriggerSlot { mask: ChannelMask::EMPTY, ..ok };
    assert_eq!(t.set_slot(0, Some(empty)), Err(TriggerError::EmptyMask));
    assert_eq!(t.set_slot(15, Some(ok)), Ok(()));
}

#[test]
fn an_unknown_expression_is_an_error() {
    let r = expression_slot(&ExpressionSet::starter(), &id("nope"), &ChannelBuf::NEUTRAL, TriggerMode::Hold);
    assert_eq!(r.map(|_| ()).map_err(|e| e.0), Err(id("nope")));
}

#[test]
fn pressing_an_empty_slot_is_ignored() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(5);
    t.press(99);
    t.release(99);
    t.step(DT, &mut out);
    assert_eq!(t.active(), None);
    assert!(!out.is_active());
}

#[test]
fn replacing_a_shown_slot_fades_the_old_one_out_and_clear_fades_out_too() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    t.press(0);
    run(&mut t, &mut out, 0.5);
    let old = out.values;
    t.set_slot(0, Some(slot("angry", TriggerMode::Toggle))).unwrap_or_else(|e| panic!("{e}"));
    t.step(DT, &mut out);
    assert!(out.weight < 1.0 && out.weight > 0.0);
    assert_eq!(out.values, old, "the old expression finishes its fade");
    run(&mut t, &mut out, 0.5);
    assert!(!out.is_active());

    t.press(1);
    run(&mut t, &mut out, 0.5);
    t.clear();
    assert_eq!(run(&mut t, &mut out, 0.25), 0.0);
}

/// Shows happy in slot 0, lets `replace` set slot 0 again mid fade-out, presses 0, and checks happy only fades out
/// (it never climbs back) and angry then fades in from zero to full.
fn repress_after_replacing(mode: TriggerMode, replace: impl FnOnce(&mut Triggers)) {
    let mut t = rig(mode);
    let mut out = layer();
    t.press(0);
    run(&mut t, &mut out, 0.5);
    let happy = out.values;
    replace(&mut t);
    run(&mut t, &mut out, 0.05);
    let mut last = out.weight;
    assert!(last > 0.0 && last < 1.0 && out.values == happy, "still fading happy out");

    t.press(0);
    while out.is_active() && out.values == happy {
        t.step(DT, &mut out);
        assert!(out.weight <= last, "the replaced happy must not fade back in");
        last = out.weight;
    }
    let angry = slot("angry", mode);
    run(&mut t, &mut out, 0.5);
    assert_eq!(out.weight, 1.0, "the replacement comes in while {mode:?} is down");
    assert_eq!((out.values, out.mask), (angry.target, angry.mask));
    assert_eq!(t.active(), Some(0));
}

#[test]
fn pressing_a_replaced_slot_mid_fade_out_shows_the_replacement() {
    for mode in [TriggerMode::Toggle, TriggerMode::Hold] {
        repress_after_replacing(mode, |t| {
            t.set_slot(0, Some(slot("angry", mode))).unwrap_or_else(|e| panic!("{e}"));
        });
    }
}

#[test]
fn emptying_and_refilling_a_slot_mid_fade_out_shows_the_new_value() {
    for mode in [TriggerMode::Toggle, TriggerMode::Hold] {
        repress_after_replacing(mode, |t| {
            t.set_slot(0, None).unwrap_or_else(|e| panic!("{e}"));
            t.step(DT, &mut layer());
            t.press(0);
            t.set_slot(0, Some(slot("angry", mode))).unwrap_or_else(|e| panic!("{e}"));
        });
    }
}

#[test]
fn expression_slot_masks_the_expression_channels() {
    let s = slot("happy", TriggerMode::Toggle);
    assert!(s.mask.contains(Channel::MOUTH_SMILE_LEFT) && s.mask.contains(Channel::BLUSH));
    assert!(!s.mask.contains(Channel::MOUTH_SMILE_LEFT) || s.target[Channel::MOUTH_SMILE_LEFT] > 0.0);
    assert!(!s.mask.contains(Channel::EYE_BLINK_LEFT));
}

#[test]
fn stepping_allocates_nothing() {
    let mut t = rig(TriggerMode::Toggle);
    let mut out = layer();
    for i in 0..600 {
        t.press(i % 2);
        t.step(DT, &mut out);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 0..10_000_usize {
        if i % 37 == 0 {
            t.press(i / 37 % 3);
        }
        if i % 53 == 0 {
            t.release(i / 53 % 2);
        }
        t.step(DT, &mut out);
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed) - before, 0);
}
