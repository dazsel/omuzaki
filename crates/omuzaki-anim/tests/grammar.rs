//! The anime grammar stage (FE-05) from the outside: parity of the `smile-blink-arc` decisions with the prototype,
//! hysteresis, the framework's guarantees (order, off means off, strength, humans are never written), determinism and
//! zero allocation per frame.
//!
//! The allocator below counts only allocations made on the measuring thread: the test harness's own threads allocate at
//! unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
// Integration-test helpers may panic (a panic is a clear failure); JSON numbers are f64 and the channels f32.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::grammar::{GrammarConfig, GrammarError, GrammarRule, GrammarStage, RuleCtx};
use omuzaki_anim_api::{Bus, Channel, ChannelBuf, ChannelMask, Cues, Source, Tick};

/// No voice: the framework's cases are about the bus.
const QUIET: Cues = Cues { loudness: 0.0, onset: false, laugh: 0.0, eyes_lead_turn: false, look_shift: false };
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{FaceProfile, GrammarRules, RuleConfig, RuleId};
use serde_json::Value;

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

const ARC_L: Channel = Channel::EYE_ARC_LEFT;
const ARC_R: Channel = Channel::EYE_ARC_RIGHT;
const BLINK_L: Channel = Channel::EYE_BLINK_LEFT;
const BLINK_R: Channel = Channel::EYE_BLINK_RIGHT;
const EYE_CHANNELS: [Channel; 4] = [BLINK_L, BLINK_R, ARC_L, ARC_R];

fn rule_id(id: &str) -> RuleId {
    RuleId::new(id).unwrap()
}

fn stage_with(config: &GrammarConfig) -> GrammarStage {
    GrammarStage::new(config).unwrap()
}

fn stage() -> GrammarStage {
    stage_with(&GrammarConfig::default())
}

/// A config that sets `smile-blink-arc` as given.
fn smile_blink(enabled: bool, strength: f64) -> GrammarConfig {
    let rule = RuleConfig { enabled, strength, ..RuleConfig::default() };
    GrammarConfig::from_rules(GrammarRules([(rule_id("smile-blink-arc"), rule)].into()))
}

fn tick(index: u64) -> Tick {
    Tick::at_rate(index, 60.0)
}

/// A face with both mouth corners at `smile` and the eyes blinking by `left` and `right`.
fn face(smile: f32, left: f32, right: f32) -> Bus {
    let mut bus = Bus::default();
    for c in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT] {
        bus.values[c] = smile;
    }
    bus.values[BLINK_L] = left;
    bus.values[BLINK_R] = right;
    bus
}

fn bits(buf: &ChannelBuf) -> Vec<u32> {
    buf.as_slice().iter().map(|v| v.to_bits()).collect()
}

// ---- parity with the prototype ----------------------------------------------------------------------------------------

/// mulberry32, the prototype's generator, on `u32`. Mirrors the prototype's generator script.
struct Mulberry(u32);

impl Mulberry {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let a = self.0;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(0x3d | t)) ^ t;
        t ^ (t >> 14)
    }

    /// A draw in `0..n`, as an `i32`.
    fn below(&mut self, n: u32) -> i32 {
        (self.next() % n).cast_signed()
    }
}

/// One frame of a trace: channel values as integers over 256, exact in `f32` and in the prototype's `f64`.
struct Row {
    smile: [i32; 2],
    frown: [i32; 2],
    dimple: [i32; 2],
    eye: [i32; 2],
}

/// Trace `i` of the golden: 100 frames. Mirrors `makeTrace` in the prototype's generator script, draw for draw.
fn trace(i: u32) -> Vec<Row> {
    let mut r = Mulberry(0x5eed_0000_u32.wrapping_add(i));
    let (mut lo, mut hi) = (0, 255);
    if i.is_multiple_of(4) {
        let a = 4 + r.below(12);
        (lo, hi) = (94 - a, 94 + a);
    }
    let mut c = lo + r.below((hi - lo + 1).cast_unsigned());
    let (mut blink, mut depth) = ([0; 2], [0; 2]);
    (0..100)
        .map(|_| {
            let step = r.below(31) - 15;
            let jitter = [r.below(9) - 4, r.below(9) - 4];
            let (frown_gate_l, frown_val_l, frown_gate_r, frown_val_r) =
                (r.below(6), r.below(80), r.below(6), r.below(80));
            let (dimple_gate_l, dimple_val_l) = (r.below(6), r.below(160));
            let (dimple_gate_r, dimple_val_r) = (r.below(6), r.below(160));
            let (gate_l, len_l, deep_l) = (r.below(12), 2 + r.below(6), 90 + r.below(166));
            let (sync, skew) = (r.below(2), r.below(41) - 20);
            let (gate_r, len_r, deep_r) = (r.below(12), 2 + r.below(6), 90 + r.below(166));
            let noise = [r.below(20), r.below(20)];

            c += step;
            if c < lo {
                c = 2 * lo - c;
            }
            if c > hi {
                c = 2 * hi - c;
            }
            c = c.clamp(lo, hi);
            if blink[0] == 0 && gate_l == 0 {
                (blink[0], depth[0]) = (len_l, deep_l);
                if sync == 0 {
                    (blink[1], depth[1]) = (len_l, (deep_l + skew).clamp(0, 255));
                }
            }
            if blink[1] == 0 && gate_r == 0 {
                (blink[1], depth[1]) = (len_r, deep_r);
            }
            let eye = [0, 1].map(|s| if blink[s] > 0 { depth[s] } else { noise[s] });
            blink = blink.map(|b| (b - 1).max(0));
            let gated = |gate, value| if gate == 0 { value } else { 0 };
            Row {
                smile: [(c + jitter[0]).clamp(0, 255), (c + jitter[1]).clamp(0, 255)],
                frown: [gated(frown_gate_l, frown_val_l), gated(frown_gate_r, frown_val_r)],
                dimple: [gated(dimple_gate_l, dimple_val_l), gated(dimple_gate_r, dimple_val_r)],
                eye,
            }
        })
        .collect()
}

impl Row {
    /// The integers in the golden's fold order.
    fn numbers(&self) -> [i32; 8] {
        [
            self.smile[0],
            self.smile[1],
            self.frown[0],
            self.frown[1],
            self.dimple[0],
            self.dimple[1],
            self.eye[0],
            self.eye[1],
        ]
    }

    /// The row as a bus. With `shut`, both eyes are held shut instead, which makes the rule's smile latch visible.
    fn bus(&self, shut: bool) -> Bus {
        let unit = |k: i32| k as f32 / 256.0;
        let mut bus = Bus::default();
        for (side, (smile, frown, dimple, blink)) in [
            (Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_LEFT, Channel::MOUTH_DIMPLE_LEFT, BLINK_L),
            (Channel::MOUTH_SMILE_RIGHT, Channel::MOUTH_FROWN_RIGHT, Channel::MOUTH_DIMPLE_RIGHT, BLINK_R),
        ]
        .into_iter()
        .enumerate()
        {
            bus.values[smile] = unit(self.smile[side]);
            bus.values[frown] = unit(self.frown[side]);
            bus.values[dimple] = unit(self.dimple[side]);
            bus.values[blink] = if shut { 1.0 } else { unit(self.eye[side]) };
        }
        bus
    }
}

fn arc(bus: &Bus, channel: Channel) -> u8 {
    u8::from(bus.values[channel] > 0.5)
}

#[test]
fn arc_decisions_match_the_prototype_on_every_frame_of_2000_traces() {
    let golden: Value = serde_json::from_str(include_str!("golden/grammar.json")).unwrap();
    let decisions = golden["decisions"].as_array().unwrap();
    assert_eq!((decisions.len(), golden["frames"].as_u64()), (2000, Some(100)));

    let traces: Vec<Vec<Row>> = (0..2000).map(trace).collect();
    // The generator first: if it differs from the Node one, the decisions below mean nothing.
    let inputs = traces
        .iter()
        .flatten()
        .flat_map(Row::numbers)
        .fold(0x811c_9dc5_u32, |h, k| (h ^ k.cast_unsigned()).wrapping_mul(0x0100_0193));
    assert_eq!(
        u64::from(inputs),
        golden["inputs_checksum"].as_u64().unwrap(),
        "the trace generator drifted from the .mjs"
    );

    let (mut arcs, mut smiling) = (0, 0);
    for (i, (rows, expected)) in traces.iter().zip(decisions).enumerate() {
        let expected = expected.as_str().unwrap().as_bytes();
        let (mut real, mut probe) = (stage(), stage());
        for (f, row) in rows.iter().enumerate() {
            let (mut bus, mut shut) = (row.bus(false), row.bus(true));
            let t = Tick::at_rate(f as u64, 30.0);
            real.step(t, &QUIET, &mut bus);
            probe.step(t, &QUIET, &mut shut);
            // Bit 0 and 1: which eyes close as ^. Bit 2: the smile latch, seen through eyes that are always shut.
            let got = arc(&bus, ARC_L) | (arc(&bus, ARC_R) << 1) | (arc(&shut, ARC_L) << 2);
            let want = (expected[f] as char).to_digit(16).unwrap() as u8;
            assert_eq!(got, want, "trace {i}, frame {f}: Rust {got:03b}, prototype {want:03b}");
            arcs += u32::from(got & 3 != 0);
            smiling += u32::from(got >> 2);
        }
    }
    // The traces must exercise both answers, or equal decisions prove nothing.
    assert!(arcs > 10_000 && smiling > 50_000 && smiling < 150_000, "arcs {arcs}, smiling frames {smiling}");
}

// ---- hysteresis -------------------------------------------------------------------------------------------------------

/// Settles the rule smiling (`true`) or not, then holds the smile in the dead band for 5 s at 60 fps with both eyes
/// shut, and returns how many frames drew the arcs.
fn arcs_while_hovering(smiling: bool) -> usize {
    let mut stage = stage();
    let settle = if smiling { 0.8 } else { 0.0 };
    for i in 0..30 {
        stage.step(tick(i), &QUIET, &mut face(settle, 1.0, 1.0));
    }
    (0..300)
        .filter(|&i| {
            // 0.37 +- 0.02, as a triangle wave with a 7 frame period (a naive single threshold at 0.37 would flip 40 times).
            let phase = (i % 7) as f32 / 7.0;
            let smile = 0.37 + 0.02 * (1.0 - 4.0 * (phase - 0.5).abs()).clamp(-1.0, 1.0);
            let mut bus = face(smile, 1.0, 1.0);
            stage.step(tick(30 + i), &QUIET, &mut bus);
            arc(&bus, ARC_L) == 1
        })
        .count()
}

#[test]
fn a_smile_hovering_between_the_thresholds_holds_its_answer() {
    assert_eq!(arcs_while_hovering(true), 300, "smiling stays smiling");
    assert_eq!(arcs_while_hovering(false), 0, "not smiling stays not smiling");
}

#[test]
fn the_thresholds_are_strict_and_a_frown_takes_off_a_smile() {
    let mut stage = stage();
    let mut arcs = |smile: f32, frown: f32| {
        let mut bus = face(smile, 1.0, 1.0);
        bus.values[Channel::MOUTH_FROWN_LEFT] = frown;
        bus.values[Channel::MOUTH_FROWN_RIGHT] = frown;
        stage.step(tick(0), &QUIET, &mut bus);
        arc(&bus, ARC_L) == 1
    };
    assert!(!arcs(0.42, 0.0), "exactly 0.42 is not above it");
    assert!(arcs(0.43, 0.0));
    assert!(arcs(0.32, 0.0), "exactly 0.32 is not below it, so the smile holds");
    assert!(!arcs(0.31, 0.0));
    assert!(arcs(0.5, 0.0));
    assert!(!arcs(0.5, 0.5), "0.5 - 0.8 * 0.5 = 0.1 is no smile");
}

// ---- what the rule writes ---------------------------------------------------------------------------------------------

#[test]
fn a_blink_while_smiling_closes_into_the_arc_and_a_plain_blink_stays_a_lash_line() {
    let mut smiling = face(0.8, 0.9, 0.9);
    stage().step(tick(0), &QUIET, &mut smiling);
    assert_eq!((smiling.values[ARC_L], smiling.values[ARC_R]), (1.0, 1.0));
    assert_eq!((smiling.values[BLINK_L], smiling.values[BLINK_R]), (0.0, 0.0), "the plain close relaxes");

    let mut plain = face(0.0, 0.9, 0.9);
    let before = plain;
    stage().step(tick(0), &QUIET, &mut plain);
    assert_eq!(plain, before, "not smiling: nothing changes");

    let mut open = face(0.8, 0.55, 0.2);
    let before = open;
    stage().step(tick(0), &QUIET, &mut open);
    assert_eq!(open, before, "a blink must pass 0.55, and a half-closed eye is not a blink");
}

#[test]
fn a_wink_arcs_only_the_winking_eye() {
    let mut bus = face(0.8, 0.9, 0.0);
    stage().step(tick(0), &QUIET, &mut bus);
    assert_eq!((bus.values[ARC_L], bus.values[BLINK_L]), (1.0, 0.0));
    assert_eq!((bus.values[ARC_R], bus.values[BLINK_R]), (0.0, 0.0));
}

#[test]
fn strength_dials_how_far_the_eye_follows_the_arc() {
    let run = |strength: f64| {
        let mut bus = face(0.8, 0.8, 0.8);
        stage_with(&smile_blink(true, strength)).step(tick(0), &QUIET, &mut bus);
        (bus.values[ARC_L], bus.values[BLINK_L])
    };
    let before = face(0.8, 0.8, 0.8);
    assert_eq!(run(0.0), (0.0, 0.8), "strength 0 acts not at all");
    let half = run(0.5);
    assert!((half.0 - 0.5).abs() < 1e-6 && (half.1 - 0.4).abs() < 1e-6, "half way: {half:?}");
    assert_eq!(run(1.0), (1.0, 0.0));
    assert_eq!(run(2.0), (1.0, 0.0), "twice the strength stays inside the channel ranges");
    assert_eq!(run(f64::NAN), (1.0, 0.0), "a corrupt strength reads as the default");
    let mut off = face(0.8, 0.8, 0.8);
    stage_with(&smile_blink(true, 0.0)).step(tick(0), &QUIET, &mut off);
    assert_eq!(off, before);
}

#[test]
fn an_arc_already_on_the_bus_is_never_lowered_and_owners_are_untouched() {
    let mut bus = face(0.8, 0.9, 0.0);
    bus.values[ARC_R] = 0.7; // a laugh's arc, from the life layer
    bus.owners.claim(Source::Life, ChannelMask::of(ARC_R));
    let owners = bus.owners;
    stage().step(tick(0), &QUIET, &mut bus);
    assert_eq!(bus.values[ARC_R], 0.7);
    assert_eq!(bus.owners, owners, "the grammar decorates; it does not take channels");
}

#[test]
fn odd_numbers_on_the_bus_neither_panic_nor_spread() {
    let mut stage = stage();
    for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30] {
        let mut bus = Bus::default();
        for c in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_RIGHT, BLINK_L, BLINK_R] {
            bus.values[c] = poison;
        }
        stage.step(Tick { index: 0, time: omuzaki_core::time::Seconds(0.0), dt: f32::NAN }, &QUIET, &mut bus);
        for c in [ARC_L, ARC_R] {
            assert!(bus.values[c].is_finite(), "{poison}: {c}");
        }
    }
}

// ---- ownership --------------------------------------------------------------------------------------------------------

#[test]
fn channels_a_person_drives_are_never_written() {
    let mut rng = Rng::new(7);
    let mut stage = stage();
    let (mut writes, mut refusals) = (0, 0);
    for i in 0..4000 {
        let mut bus = face(0.9, 0.95, 0.95);
        let mut human = ChannelMask::EMPTY;
        for c in EYE_CHANNELS {
            if rng.next_u64().is_multiple_of(3) {
                human.insert(c);
            }
        }
        if rng.next_u64().is_multiple_of(4) {
            human |= ChannelMask::MOUTH;
        }
        bus.owners.claim(if i % 2 == 0 { Source::Tracker } else { Source::Trigger }, human);
        let before = bus;
        stage.step(tick(i), &QUIET, &mut bus);
        for c in EYE_CHANNELS {
            if human.contains(c) {
                assert_eq!(bus.values[c].to_bits(), before.values[c].to_bits(), "{c} is driven by a person");
                refusals += 1;
            } else {
                // The smile and the blink are both well past their thresholds, so every free channel is written.
                let want = if c == ARC_L || c == ARC_R { 1.0 } else { 0.0 };
                assert_eq!(bus.values[c], want, "{c} is free");
                writes += 1;
            }
        }
        assert_eq!(bus.owners, before.owners);
    }
    assert!(writes > 4000 && refusals > 4000, "writes {writes}, refusals {refusals}");
}

#[test]
fn a_tracked_blink_while_smiling_still_draws_the_arc() {
    // A tracker drives the ARKit channels, including eyeBlink, but has no eyeArc channel: the painter draws ^ from the arc.
    let mut bus = face(0.8, 0.9, 0.9);
    bus.owners.claim(Source::Tracker, ChannelMask::ARKIT);
    stage().step(tick(0), &QUIET, &mut bus);
    assert_eq!((bus.values[ARC_L], bus.values[ARC_R]), (1.0, 1.0));
    assert_eq!((bus.values[BLINK_L], bus.values[BLINK_R]), (0.9, 0.9), "the person's blink is left alone");
}

#[test]
fn the_smile_latch_keeps_following_while_the_eyes_belong_to_a_person() {
    let mut stage = stage();
    for i in 0..3 {
        stage.step(tick(i), &QUIET, &mut face(0.9, 0.0, 0.0)); // smiling, eyes open and free
    }
    // A person takes the eyes and the smile ends meanwhile: nothing is written...
    for i in 3..8 {
        let mut bus = face(0.0, 1.0, 1.0);
        bus.owners.claim(Source::Tracker, ChannelMask::EYES);
        let before = bus;
        stage.step(tick(i), &QUIET, &mut bus);
        assert_eq!(bus, before);
    }
    // ...and on hand-back the face sits in the dead band, where only a latch that kept following (and so already
    // reads "not smiling") gives a plain blink. A latch frozen at the old smile would draw the arcs: a jump.
    let mut bus = face(0.37, 1.0, 1.0);
    stage.step(tick(8), &QUIET, &mut bus);
    assert_eq!(arc(&bus, ARC_L), 0);
}

// ---- the framework ----------------------------------------------------------------------------------------------------

/// A rule for the framework tests: it halves a channel and adds `value`, and counts its steps and resets.
#[derive(Debug)]
struct Fake {
    id: &'static str,
    priority: i32,
    channel: Channel,
    value: f32,
    steps: Arc<AtomicUsize>,
    resets: Arc<AtomicUsize>,
}

impl GrammarRule for Fake {
    fn id(&self) -> &'static str {
        self.id
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn step(&mut self, ctx: &mut RuleCtx<'_>) {
        self.steps.fetch_add(1, Ordering::Relaxed);
        // Builds on what is there, so the result depends on who ran first.
        ctx.put(self.channel, ctx.get(self.channel) * 0.5 + self.value);
    }

    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
    }
}

fn fake(id: &'static str, priority: i32, value: f32) -> (Box<dyn GrammarRule>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (steps, resets) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let rule = Fake { id, priority, channel: Channel::BLUSH, value, steps: steps.clone(), resets: resets.clone() };
    (Box::new(rule), steps, resets)
}

#[test]
fn rules_run_by_priority_then_id_whatever_the_registration_order() {
    let build = |order: [(&'static str, i32, f32); 3]| {
        let rules = order.map(|(id, priority, value)| fake(id, priority, value).0);
        GrammarStage::with_rules(rules, &GrammarConfig::default()).unwrap()
    };
    // (id, priority, value): b and a share priority 1, so a goes first; c has priority 0 and goes before both.
    let forward = [("a", 1, 0.125), ("b", 1, 0.25), ("c", 0, 0.5)];
    let backward = [("c", 0, 0.5), ("b", 1, 0.25), ("a", 1, 0.125)];
    let (mut one, mut two) = (build(forward), build(backward));
    assert_eq!(one.rule_ids().collect::<Vec<_>>(), ["c", "a", "b"]);
    assert_eq!(two.rule_ids().collect::<Vec<_>>(), ["c", "a", "b"]);
    let (mut x, mut y) = (Bus::default(), Bus::default());
    one.step(tick(0), &QUIET, &mut x);
    two.step(tick(0), &QUIET, &mut y);
    // c: 0 * 0.5 + 0.5 = 0.5, then a: 0.5 * 0.5 + 0.125 = 0.375, then b: 0.375 * 0.5 + 0.25 = 0.4375. Any other order differs.
    assert_eq!(bits(&x.values), bits(&y.values));
    assert_eq!(x.values[Channel::BLUSH], 0.4375);
}

#[test]
fn a_disabled_rule_is_skipped_and_reset_once_and_the_bus_is_bit_identical() {
    let (rule, steps, resets) = fake("laugh-squint", 0, 0.25);
    let off = GrammarRules([(rule_id("laugh-squint"), RuleConfig { enabled: false, ..RuleConfig::default() })].into());
    let mut stage = GrammarStage::with_rules([rule], &GrammarConfig::from_rules(off.clone())).unwrap();

    let mut rng = Rng::new(3);
    for i in 0..200 {
        let mut bus = Bus::default();
        for info in omuzaki_anim_api::CHANNELS {
            // Wild values on purpose, NaN and infinity included: an untouched bus must not even be sanitised.
            bus.values[info.channel] = match rng.next_u64() % 8 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                _ => rng.range_f32(-2.0, 3.0),
            };
        }
        bus.owners.claim(Source::Score, ChannelMask::from_bits_truncate(u128::from(rng.next_u64())));
        let before = bus;
        stage.step(tick(i), &QUIET, &mut bus);
        assert_eq!((bits(&bus.values), bus.owners), (bits(&before.values), before.owners));
    }
    assert_eq!((steps.load(Ordering::Relaxed), resets.load(Ordering::Relaxed)), (0, 0), "never ran, nothing to reset");

    // Switched on, it runs; switched off again, it is reset once, on the next frame, and not again.
    stage.set_config(&GrammarConfig::default());
    stage.step(tick(0), &QUIET, &mut Bus::default());
    stage.step(tick(1), &QUIET, &mut Bus::default());
    assert_eq!((steps.load(Ordering::Relaxed), resets.load(Ordering::Relaxed)), (2, 0));
    stage.set_config(&GrammarConfig::from_rules(off));
    for i in 0..5 {
        stage.step(tick(i), &QUIET, &mut Bus::default());
    }
    assert_eq!((steps.load(Ordering::Relaxed), resets.load(Ordering::Relaxed)), (2, 1));
}

#[test]
fn a_stage_refuses_duplicate_and_unnameable_rules() {
    let two = [fake("same", 0, 0.0).0, fake("same", 5, 0.0).0];
    let err = GrammarStage::with_rules(two, &GrammarConfig::default()).unwrap_err();
    assert_eq!(err, GrammarError::Duplicate("same".into()));
    let err = GrammarStage::with_rules([fake("Not A Slug", 0, 0.0).0], &GrammarConfig::default()).unwrap_err();
    assert!(matches!(err, GrammarError::BadId { id: "Not A Slug", .. }), "{err}");
}

#[test]
fn the_registry_supplies_smile_blink_arc_and_unknown_switches_are_ignored() {
    assert!(stage().rule_ids().any(|id| id == "smile-blink-arc"));
    // A switch for a rule this build does not have (from a newer one) changes nothing.
    let mut rules = GrammarRules::default();
    for id in ["from-a-newer-build", "from-a-much-newer-build"] {
        rules.0.insert(rule_id(id), RuleConfig { enabled: false, strength: 2.0, ..RuleConfig::default() });
    }
    let mut a = face(0.8, 0.9, 0.9);
    let mut b = a;
    stage().step(tick(0), &QUIET, &mut a);
    stage_with(&GrammarConfig::from_rules(rules)).step(tick(0), &QUIET, &mut b);
    assert_eq!(a, b);
}

#[test]
fn the_face_profile_switches_the_rule() {
    let mut profile = FaceProfile::default();
    profile.grammar.0.insert(rule_id("smile-blink-arc"), RuleConfig { enabled: false, ..RuleConfig::default() });
    let config = GrammarConfig::from_profile(&profile);
    let mut bus = face(0.8, 0.9, 0.9);
    let before = bus;
    stage_with(&config).step(tick(0), &QUIET, &mut bus);
    assert_eq!(bus, before, "the profile turned the rule off");
    assert_eq!(GrammarConfig::from_profile(&FaceProfile::default()), GrammarConfig::default());
}

#[test]
fn reset_forgets_the_smile() {
    let mut stage = stage();
    stage.step(tick(0), &QUIET, &mut face(0.9, 0.0, 0.0));
    stage.reset();
    let mut bus = face(0.37, 1.0, 1.0); // in the dead band: only a remembered smile would draw the arcs
    stage.step(tick(1), &QUIET, &mut bus);
    assert_eq!(arc(&bus, ARC_L), 0);
}

// ---- determinism and the hot path -------------------------------------------------------------------------------------

/// A busy frame: a wandering smile, blinks, a tracker taking the eyes for a while.
fn busy_frame(i: u64, rows: &[Row]) -> Bus {
    let mut bus = rows[(i % 100) as usize].bus(false);
    if (i / 100) % 5 == 4 {
        bus.owners.claim(Source::Tracker, ChannelMask::EYES);
    }
    bus
}

#[test]
fn a_replay_is_bit_identical() {
    let rows = trace(1);
    let run = || {
        let mut stage = stage();
        (0..2000).fold(0_u64, |h, i| {
            let mut bus = busy_frame(i, &rows);
            stage.step(tick(i), &QUIET, &mut bus);
            h.rotate_left(5) ^ bus.values.checksum()
        })
    };
    assert_eq!(run(), run());
}

#[test]
fn a_frame_of_the_grammar_stage_allocates_nothing() {
    let rows = trace(2);
    let mut stage = stage();
    let mut off = stage_with(&smile_blink(false, 1.0));
    let mut run = |from: u64, to: u64| {
        for i in from..to {
            for s in [&mut stage, &mut off] {
                let mut bus = busy_frame(i, &rows);
                s.step(tick(i), &QUIET, &mut bus);
                std::hint::black_box(bus);
            }
        }
    };
    run(0, 200); // warm-up
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    run(200, 20_200); // arcs drawn, arcs refused to a person, and a rule that is switched off
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "20,000 frames of the grammar stage made {allocations} allocations");
}
