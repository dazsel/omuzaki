//! The anime-grammar rules `laugh-squint`, `pout-cheek-puff` and `grin-fangs` (FE-05) from the outside: parity of the
//! laugh with the prototype, what each rule writes, hysteresis, strength and the character's own numbers, ownership,
//! the life layer's share of a laugh, a face strip per rule through the mock painter, and zero allocation per frame.
//!
//! The allocator below counts only allocations made on the measuring thread: the test harness's own threads allocate at
//! unpredictable times, which makes a process-wide count flaky under load.
//!
//! Set `OMUZAKI_STRIPS=<dir>` to write the face strips as PNGs (reference / before / after) while the strip test runs.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
// Integration-test helpers may panic (a panic is a clear failure); JSON numbers are f64 and the channels f32.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{Rgba, RgbaImage};
use omuzaki_anim::grammar::rules::{GrinFangs, LaughSquint, PoutCheekPuff};
use omuzaki_anim::grammar::{GrammarConfig, GrammarRule, GrammarStage};
use omuzaki_anim::life::Life;
use omuzaki_anim_api::{Bus, CHANNELS, Channel, ChannelBuf, ChannelMask, Cues, LifeConfig, LifeLayer, Source, Tick};
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FacePainter, FaceProfile, FaceRasterizer, FaceView, FangKind,
    GrammarRules, RESERVED_RULES, Raster, RuleConfig, RuleId,
};
use omuzaki_face_mock::{MockPainter, MockRasterizer, mock_profile};
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

// ---- channels and helpers ---------------------------------------------------------------------------------------------

const SMILE: [Channel; 2] = [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT];
const FROWN: [Channel; 2] = [Channel::MOUTH_FROWN_LEFT, Channel::MOUTH_FROWN_RIGHT];
const SQUINT: [Channel; 2] = [Channel::EYE_SQUINT_LEFT, Channel::EYE_SQUINT_RIGHT];
const CHEEK: [Channel; 2] = [Channel::CHEEK_SQUINT_LEFT, Channel::CHEEK_SQUINT_RIGHT];
const ARC: [Channel; 2] = [Channel::EYE_ARC_LEFT, Channel::EYE_ARC_RIGHT];
const BROW_DOWN: [Channel; 2] = [Channel::BROW_DOWN_LEFT, Channel::BROW_DOWN_RIGHT];
const BLINK: [Channel; 2] = [Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT];
const JAW: Channel = Channel::JAW_OPEN;
const PUCKER: Channel = Channel::MOUTH_PUCKER;
const PUFF: Channel = Channel::CHEEK_PUFF;
const FANGS: Channel = Channel::FANGS;

/// Every channel a rule may write, by rule. A rule that writes outside its own list is a bug.
const LAUGH_WRITES: [Channel; 6] = [SQUINT[0], SQUINT[1], CHEEK[0], CHEEK[1], ARC[0], ARC[1]];
const POUT_WRITES: [Channel; 3] = [PUFF, BROW_DOWN[0], BROW_DOWN[1]];
const GRIN_WRITES: [Channel; 1] = [FANGS];

fn rule_id(id: &str) -> RuleId {
    RuleId::new(id).unwrap()
}

/// A mouth: both corners at `smile` and `frown`, the jaw at `jaw`, the lips puckered by `pucker`.
fn lips(smile: f32, frown: f32, jaw: f32, pucker: f32) -> Bus {
    let mut bus = Bus::default();
    for side in 0..2 {
        bus.values[SMILE[side]] = smile;
        bus.values[FROWN[side]] = frown;
    }
    bus.values[JAW] = jaw;
    bus.values[PUCKER] = pucker;
    bus
}

fn bits(buf: &ChannelBuf) -> Vec<u32> {
    buf.as_slice().iter().map(|v| v.to_bits()).collect()
}

/// A rule's switches: on or off, a strength and its own numbers.
fn rule_config(enabled: bool, strength: f64, params: &[(&str, f64)]) -> RuleConfig {
    let params = params.iter().map(|&(name, value)| (omuzaki_core::id::Slug::new(name).unwrap(), value)).collect();
    RuleConfig { enabled, strength, params }
}

fn config(entries: &[(&str, RuleConfig)]) -> GrammarConfig {
    GrammarConfig::from_rules(GrammarRules(entries.iter().map(|(id, rule)| (rule_id(id), rule.clone())).collect()))
}

/// A stage that holds exactly `rule`, set as `config` says.
fn solo(rule: impl GrammarRule + 'static, config: &GrammarConfig) -> GrammarStage {
    GrammarStage::with_rules([Box::new(rule) as Box<dyn GrammarRule>], config).unwrap()
}

/// The three rules of this task, set as `config` says.
fn trio(config: &GrammarConfig) -> GrammarStage {
    let rules: [Box<dyn GrammarRule>; 3] =
        [Box::new(LaughSquint::default()), Box::new(PoutCheekPuff::default()), Box::new(GrinFangs::default())];
    GrammarStage::with_rules(rules, config).unwrap()
}

fn laugh() -> GrammarStage {
    solo(LaughSquint::default(), &GrammarConfig::default())
}

fn pout() -> GrammarStage {
    solo(PoutCheekPuff::default(), &GrammarConfig::default())
}

fn grin() -> GrammarStage {
    solo(GrinFangs::default(), &GrammarConfig::default())
}

/// Every registered rule, as a character gets them.
fn everything() -> GrammarStage {
    GrammarStage::new(&GrammarConfig::default()).unwrap()
}

fn tick(index: u64) -> Tick {
    Tick::at_rate(index, 60.0)
}

/// A stage and the frame it is on: frames come at 60 fps unless asked otherwise.
struct Run {
    stage: GrammarStage,
    frame: u64,
    fps: f32,
}

impl Run {
    fn new(stage: GrammarStage) -> Self {
        Self::at(stage, 60.0)
    }

    fn at(stage: GrammarStage, fps: f32) -> Self {
        Self { stage, frame: 0, fps }
    }

    /// One frame of `input`; returns what the stage made of it.
    fn step(&mut self, input: &Bus) -> Bus {
        let mut out = *input;
        self.stage.step(Tick::at_rate(self.frame, self.fps), &mut out);
        self.frame += 1;
        out
    }

    /// `frames` frames of `input`; returns the last result.
    fn hold(&mut self, input: &Bus, frames: u64) -> Bus {
        let mut out = *input;
        for _ in 0..frames {
            out = self.step(input);
        }
        out
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

// ---- parity with the prototype ----------------------------------------------------------------------------------------

/// mulberry32, the prototype's generator, on `u32`. Mirrors the prototype's generator script (and `grammar.rs`).
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

/// One frame of a laugh trace: channel values as integers over 256, exact in `f32` and in the prototype's `f64`.
struct Row {
    smile: [i32; 2],
    frown: [i32; 2],
    jaw: i32,
}

/// Reflects `c` back into `lo..=hi` after a step.
fn bounce(mut c: i32, lo: i32, hi: i32) -> i32 {
    if c < lo {
        c = 2 * lo - c;
    }
    if c > hi {
        c = 2 * hi - c;
    }
    c.clamp(lo, hi)
}

/// Trace `i` of the golden: 100 frames. Mirrors `makeTrace` in the prototype's generator script, draw for draw.
fn laugh_trace(i: u32) -> Vec<Row> {
    let mut r = Mulberry(0x5eed_1000_u32.wrapping_add(i));
    let (mut s_lo, mut s_hi, mut j_lo, mut j_hi) = (0_i32, 255_i32, 0_i32, 255_i32);
    match i % 4 {
        0 => (s_lo, s_hi, j_lo, j_hi) = (140, 170, 80, 255),
        2 => (s_lo, s_hi, j_lo, j_hi) = (100, 175, 40, 110),
        _ => {}
    }
    let mut smile = s_lo + r.below((s_hi - s_lo + 1).cast_unsigned());
    let mut jaw = j_lo + r.below((j_hi - j_lo + 1).cast_unsigned());
    let mut dip = 0;
    (0..100)
        .map(|_| {
            let (s_step, j_step) = (r.below(31) - 15, r.below(41) - 20);
            let jitter = [r.below(9) - 4, r.below(9) - 4];
            let (frown_gate_l, frown_val_l, frown_gate_r, frown_val_r) =
                (r.below(6), r.below(80), r.below(6), r.below(80));
            let (dip_gate, dip_len, dip_jaw) = (r.below(12), 1 + r.below(6), r.below(40));

            smile = bounce(smile + s_step, s_lo, s_hi);
            jaw = bounce(jaw + j_step, j_lo, j_hi);
            if dip == 0 && dip_gate == 0 {
                dip = dip_len;
            }
            let jaw_now = if dip > 0 { dip_jaw } else { jaw };
            dip = (dip - 1).max(0);
            let gated = |gate, value| if gate == 0 { value } else { 0 };
            Row {
                smile: [(smile + jitter[0]).clamp(0, 255), (smile + jitter[1]).clamp(0, 255)],
                frown: [gated(frown_gate_l, frown_val_l), gated(frown_gate_r, frown_val_r)],
                jaw: jaw_now,
            }
        })
        .collect()
}

impl Row {
    /// The integers in the golden's fold order.
    fn numbers(&self) -> [i32; 5] {
        [self.smile[0], self.smile[1], self.frown[0], self.frown[1], self.jaw]
    }

    fn bus(&self) -> Bus {
        let unit = |k: i32| k as f32 / 256.0;
        let mut bus = Bus::default();
        for side in 0..2 {
            bus.values[SMILE[side]] = unit(self.smile[side]);
            bus.values[FROWN[side]] = unit(self.frown[side]);
        }
        bus.values[JAW] = unit(self.jaw);
        bus
    }
}

/// The weight the laugh has faded to in frame `n` of the golden: the ramp is `n / 9`, eased by a smoothstep.
fn eased(n: u32) -> f32 {
    let u = n as f32 / 9.0;
    u * u * (3.0 - 2.0 * u)
}

#[test]
fn the_laugh_matches_the_prototype_on_every_frame_of_2000_traces() {
    let golden: Value = serde_json::from_str(include_str!("golden/grammar_rules.json")).unwrap();
    let laughs = golden["laughs"].as_array().unwrap();
    assert_eq!((laughs.len(), golden["frames"].as_u64()), (2000, Some(100)));

    let traces: Vec<Vec<Row>> = (0..2000).map(laugh_trace).collect();
    // The generator first: if it differs from the Node one, the weights below mean nothing.
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

    let (mut starts, mut ends, mut full, mut ridden_out) = (0, 0, 0, 0);
    for (i, (rows, expected)) in traces.iter().zip(laughs).enumerate() {
        let expected = expected.as_str().unwrap().as_bytes();
        let mut run = Run::at(laugh(), 30.0); // the golden's frame rate
        let mut previous = 0;
        for (f, row) in rows.iter().enumerate() {
            let out = run.step(&row.bus());
            let n = (expected[f] as char).to_digit(10).unwrap();
            // Squint and cheek both rise to 0.6 of the eased weight, on both sides.
            for channel in [SQUINT[0], SQUINT[1], CHEEK[0], CHEEK[1]] {
                let want = 0.6 * eased(n);
                let got = out.values[channel];
                assert!(
                    (got - want).abs() < 1e-4,
                    "trace {i}, frame {f}, {channel}: Rust {got}, prototype {want} (n = {n})"
                );
            }
            starts += u32::from(previous == 0 && n > 0);
            ends += u32::from(previous > 0 && n == 0);
            full += u32::from(n == 9);
            // A frame whose jaw dipped under its keep level while the laugh stayed fully on: the hold at work.
            ridden_out += u32::from(n == 9 && row.jaw < 51 && previous == 9);
            previous = n;
        }
    }
    // The traces must exercise starting, ending, holding and riding out a dip, or equal weights prove nothing.
    assert!(starts > 2000 && ends > 1500 && full > 50_000 && ridden_out > 5000, "{starts} {ends} {full} {ridden_out}");
}

// ---- what each rule writes: tables ------------------------------------------------------------------------------------

/// One row of a table: the faces are held for the given number of 60 fps frames in turn, then the channels are read.
struct Case {
    name: &'static str,
    phases: Vec<(Bus, u64)>,
    expect: Vec<(Channel, f32)>,
}

fn case(name: &'static str, phases: Vec<(Bus, u64)>, expect: Vec<(Channel, f32)>) -> Case {
    Case { name, phases, expect }
}

/// `value` on both sides of a pair.
fn both(pair: [Channel; 2], value: f32) -> Vec<(Channel, f32)> {
    pair.iter().map(|&c| (c, value)).collect()
}

fn run_table(stage: fn() -> GrammarStage, cases: Vec<Case>) {
    for case in cases {
        let mut run = Run::new(stage());
        let mut out = Bus::default();
        for (input, frames) in &case.phases {
            out = run.hold(input, *frames);
        }
        for &(channel, want) in &case.expect {
            let got = out.values[channel];
            assert!(close(got, want), "{}: {channel} is {got}, want {want}", case.name);
        }
    }
}

/// Everything a laugh may write, expected at `squint`/`cheek`/`arc`.
fn laugh_look(squint: f32, cheek: f32, arc: f32) -> Vec<(Channel, f32)> {
    [both(SQUINT, squint), both(CHEEK, cheek), both(ARC, arc)].concat()
}

#[test]
fn laugh_squint_table() {
    let (big, huge, shut) = (lips(0.8, 0.0, 0.5, 0.0), lips(1.0, 0.0, 0.6, 0.0), lips(0.0, 0.0, 0.0, 0.0));
    let keeping = lips(0.5, 0.0, 0.25, 0.0);
    let mut squinting = big;
    squinting.values[SQUINT[0]] = 0.9; // a source that already squints harder than a laugh would
    let mut half = big;
    half.values[SQUINT[0]] = 0.3;
    let mut arced = big;
    arced.values[ARC[1]] = 1.0; // a smile-blink-arc earlier in the frame
    run_table(
        laugh,
        vec![
            case("a big laugh squints and lifts the cheeks", vec![(big, 60)], laugh_look(0.6, 0.6, 0.0)),
            case("a huge one closes into ^^ as well", vec![(huge, 60)], laugh_look(0.6, 0.6, 1.0)),
            case(
                "a smile with a shut mouth is no laugh",
                vec![(lips(0.9, 0.0, 0.0, 0.0), 60)],
                laugh_look(0.0, 0.0, 0.0),
            ),
            case(
                "an open mouth without a smile is no laugh",
                vec![(lips(0.2, 0.0, 0.8, 0.0), 60)],
                laugh_look(0.0, 0.0, 0.0),
            ),
            case("a frown takes off the smile", vec![(lips(0.8, 0.5, 0.5, 0.0), 60)], laugh_look(0.0, 0.0, 0.0)),
            case("just under the start level", vec![(lips(0.59, 0.0, 0.5, 0.0), 60)], laugh_look(0.0, 0.0, 0.0)),
            case("just over the start level", vec![(lips(0.61, 0.0, 0.5, 0.0), 60)], laugh_look(0.6, 0.6, 0.0)),
            case("kept above the release level", vec![(big, 30), (keeping, 60)], laugh_look(0.6, 0.6, 0.0)),
            case(
                "let go below the release level",
                vec![(big, 30), (lips(0.4, 0.0, 0.25, 0.0), 60)],
                laugh_look(0.0, 0.0, 0.0),
            ),
            case(
                "a dip shorter than the hold is ridden out",
                vec![(big, 30), (shut, 5), (big, 1)],
                laugh_look(0.6, 0.6, 0.0),
            ),
            case("a dip longer than the hold ends it", vec![(big, 30), (shut, 60)], laugh_look(0.0, 0.0, 0.0)),
            case("a harder squint is never lowered", vec![(squinting, 60)], {
                let mut want = laugh_look(0.6, 0.6, 0.0);
                want[0] = (SQUINT[0], 0.9);
                want
            }),
            case("a half squint is raised to the laugh's, not added to", vec![(half, 60)], laugh_look(0.6, 0.6, 0.0)),
            case("an arc drawn earlier stays", vec![(arced, 60)], {
                let mut want = laugh_look(0.6, 0.6, 0.0);
                want[5] = (ARC[1], 1.0);
                want
            }),
        ],
    );
}

#[test]
fn laugh_squint_eases_in_and_out_instead_of_snapping() {
    let mut run = Run::new(laugh());
    let first = run.step(&lips(0.9, 0.0, 0.5, 0.0)).values[SQUINT[0]];
    assert!(first > 0.0 && first < 0.1, "the first frame of a laugh is the start of a ramp: {first}");
    let mut last = first;
    for _ in 0..8 {
        let now = run.step(&lips(0.9, 0.0, 0.5, 0.0)).values[SQUINT[0]];
        assert!(now > last, "rising: {last} then {now}");
        last = now;
    }
    run.hold(&lips(0.9, 0.0, 0.5, 0.0), 30);
    let ends = run.hold(&lips(0.0, 0.0, 0.0, 0.0), 10).values[SQUINT[0]];
    assert!(ends > 0.0 && ends < 0.6, "ten frames after the laugh it is still fading out: {ends}");
}

#[test]
fn pout_cheek_puff_table() {
    let (pouting, shut) = (lips(0.0, 0.6, 0.0, 0.6), lips(0.0, 0.0, 0.0, 0.0));
    let mut cheeky = pouting;
    cheeky.values[PUFF] = 0.95; // a person's own puff
    let look = |puff: f32, brows: f32| [vec![(PUFF, puff)], both(BROW_DOWN, brows)].concat();
    run_table(
        pout,
        vec![
            case("a pout puffs the cheeks and tilts the brows down", vec![(pouting, 60)], look(0.8, 0.25)),
            case("a kiss (pucker, no frown) is no pout", vec![(lips(0.0, 0.0, 0.0, 0.9), 60)], look(0.0, 0.0)),
            case("a sulk (frown, no pucker) is no pout", vec![(lips(0.0, 0.9, 0.0, 0.0), 60)], look(0.0, 0.0)),
            case("the weaker of the two decides", vec![(lips(0.0, 0.9, 0.0, 0.35), 60)], look(0.8, 0.25)),
            case("just under the start level", vec![(lips(0.0, 0.9, 0.0, 0.29), 60)], look(0.0, 0.0)),
            case("an open mouth is no pout", vec![(lips(0.0, 0.6, 0.5, 0.6), 60)], look(0.0, 0.0)),
            case("a nearly shut mouth still pouts", vec![(lips(0.0, 0.9, 0.05, 0.9), 60)], look(0.8, 0.25)),
            case("kept above the release level", vec![(pouting, 30), (lips(0.0, 0.9, 0.0, 0.25), 60)], look(0.8, 0.25)),
            case(
                "let go below the release level",
                vec![(pouting, 30), (lips(0.0, 0.9, 0.0, 0.15), 60)],
                look(0.0, 0.0),
            ),
            case(
                "a dip shorter than the hold is ridden out",
                vec![(pouting, 30), (shut, 5), (pouting, 1)],
                look(0.8, 0.25),
            ),
            case("a dip longer than the hold ends it", vec![(pouting, 30), (shut, 90)], look(0.0, 0.0)),
            case("a person's own puff is never lowered", vec![(cheeky, 60)], look(0.95, 0.25)),
        ],
    );
}

#[test]
fn a_pout_is_let_out_more_slowly_than_it_is_made() {
    let pouting = lips(0.0, 0.6, 0.0, 0.6);
    let mut run = Run::new(pout());
    let rise = (1..=40).find(|_| run.step(&pouting).values[PUFF] > 0.79).unwrap();
    run.hold(&pouting, 30);
    let fall = (1..=60).find(|_| run.step(&lips(0.0, 0.0, 0.0, 0.0)).values[PUFF] < 0.01).unwrap();
    assert!(rise <= 16, "a pout is believed after 0.1 s and made in 0.12 s more ({rise} frames)");
    assert!(fall > rise + 6, "and let out over a hold and 0.25 s, which is slower still ({fall} frames)");
}

/// A profile that says "no fangs" switches `grin-fangs` off by itself (#637): no per-character `show` is needed, and a
/// `grammar` entry that asks for the rule does not bring it back.
#[test]
fn a_profile_without_fangs_never_shows_them() {
    let grinning = lips(0.8, 0.0, 0.4, 0.0);
    let mut profile = FaceProfile::default();
    let fanged = Run::new(solo(GrinFangs::default(), &GrammarConfig::from_profile(&profile))).hold(&grinning, 60);
    assert!(close(fanged.values[FANGS], 1.0), "a profile with fangs shows them: {}", fanged.values[FANGS]);

    profile.mouth.fang.kind = FangKind::None;
    profile.grammar =
        GrammarRules([(rule_id("grin-fangs"), rule_config(true, 2.0, &[("show", 1.0)]))].into_iter().collect());
    let config = GrammarConfig::from_profile(&profile);
    let bare = Run::new(solo(GrinFangs::default(), &config)).hold(&grinning, 60);
    assert_eq!(bare.values[FANGS], 0.0, "no fangs, whatever the grammar section asks");
    assert!(!profile.has_fangs());
    // The other rules are untouched.
    let laughing = Run::new(trio(&config)).hold(&lips(0.9, 0.0, 0.6, 0.0), 60);
    assert!(laughing.values[SQUINT[0]] > 0.0, "laugh-squint still runs");
}

#[test]
fn grin_fangs_table() {
    let (grinning, shut) = (lips(0.8, 0.0, 0.4, 0.0), lips(0.0, 0.0, 0.0, 0.0));
    let mut fanged = grinning;
    fanged.values[FANGS] = 0.3;
    run_table(
        grin,
        vec![
            case("a big smile with an open mouth shows the fangs", vec![(grinning, 60)], vec![(FANGS, 1.0)]),
            case("a closed mouth shows none", vec![(lips(0.9, 0.0, 0.05, 0.0), 60)], vec![(FANGS, 0.0)]),
            case("a small smile shows none", vec![(lips(0.45, 0.0, 0.4, 0.0), 60)], vec![(FANGS, 0.0)]),
            case("a frown takes off the smile", vec![(lips(0.8, 0.4, 0.4, 0.0), 60)], vec![(FANGS, 0.0)]),
            case("just over the start level", vec![(lips(0.51, 0.0, 0.4, 0.0), 60)], vec![(FANGS, 1.0)]),
            case("just under it", vec![(lips(0.49, 0.0, 0.4, 0.0), 60)], vec![(FANGS, 0.0)]),
            case(
                "kept above the release level",
                vec![(grinning, 30), (lips(0.45, 0.0, 0.1, 0.0), 60)],
                vec![(FANGS, 1.0)],
            ),
            case(
                "let go when the smile drops",
                vec![(grinning, 30), (lips(0.3, 0.0, 0.4, 0.0), 60)],
                vec![(FANGS, 0.0)],
            ),
            case(
                "let go when the mouth shuts",
                vec![(grinning, 30), (lips(0.8, 0.0, 0.05, 0.0), 60)],
                vec![(FANGS, 0.0)],
            ),
            case(
                "a dip shorter than the hold is ridden out",
                vec![(grinning, 30), (shut, 4), (grinning, 1)],
                vec![(FANGS, 1.0)],
            ),
            case("more fang already on the bus is not lowered, less is raised", vec![(fanged, 60)], vec![(FANGS, 1.0)]),
        ],
    );
}

// ---- hysteresis -------------------------------------------------------------------------------------------------------

/// One look and the number that starts and releases it: how to build a face with that number at `x`, and a face on
/// each side of the band to settle on.
struct Probe {
    name: &'static str,
    stage: fn() -> GrammarStage,
    face: fn(f32) -> Bus,
    /// The channel to watch and its value at full effect.
    out: (Channel, f32),
    start: f32,
    keep: f32,
    on: Bus,
    off: Bus,
}

fn probes() -> Vec<Probe> {
    let (shut, kept) = (lips(0.0, 0.0, 0.0, 0.0), lips(0.9, 0.0, 0.6, 0.0));
    vec![
        Probe {
            name: "laugh: smile",
            stage: laugh,
            face: |x| lips(x, 0.0, 0.5, 0.0),
            out: (SQUINT[0], 0.6),
            start: 0.6,
            keep: 0.45,
            on: kept,
            off: shut,
        },
        Probe {
            name: "laugh: jaw",
            stage: laugh,
            face: |x| lips(0.9, 0.0, x, 0.0),
            out: (SQUINT[0], 0.6),
            start: 0.3,
            keep: 0.2,
            on: kept,
            off: shut,
        },
        Probe {
            name: "laugh: the smile that closes into ^^",
            stage: laugh,
            face: |x| lips(x, 0.0, 0.6, 0.0),
            out: (ARC[0], 1.0),
            start: 0.85,
            keep: 0.75,
            on: lips(1.0, 0.0, 0.6, 0.0),
            off: lips(0.8, 0.0, 0.6, 0.0),
        },
        Probe {
            name: "pout: pucker with a frown",
            stage: pout,
            face: |x| lips(0.0, 0.9, 0.0, x),
            out: (PUFF, 0.8),
            start: 0.3,
            keep: 0.2,
            on: lips(0.0, 0.9, 0.0, 0.6),
            off: shut,
        },
        Probe {
            name: "grin: smile",
            stage: grin,
            face: |x| lips(x, 0.0, 0.4, 0.0),
            out: (FANGS, 1.0),
            start: 0.5,
            keep: 0.4,
            on: lips(0.8, 0.0, 0.4, 0.0),
            off: shut,
        },
        Probe {
            name: "grin: jaw",
            stage: grin,
            face: |x| lips(0.9, 0.0, x, 0.0),
            out: (FANGS, 1.0),
            start: 0.15,
            keep: 0.08,
            on: lips(0.9, 0.0, 0.4, 0.0),
            off: shut,
        },
    ]
}

/// How many times the look crosses half its full level while the number hovers 0.02 either side of `centre` in a
/// triangle wave of `period` frames for 5 s at 60 fps, after settling on `settle`. `face(x)` builds the face with the
/// number at `x`; `out` is the channel to watch and its value at full effect.
fn hover_flips(
    stage: GrammarStage,
    face: &dyn Fn(f32) -> Bus,
    out: (Channel, f32),
    settle: &Bus,
    centre: f32,
    period: u64,
) -> usize {
    let mut run = Run::new(stage);
    let (channel, full) = out;
    let mut high = run.hold(settle, 60).values[channel] > 0.5 * full;
    let mut flips = 0;
    for i in 0..300 {
        let phase = (i % period) as f32 / period as f32;
        let x = centre + 0.02 * (1.0 - 4.0 * (phase - 0.5).abs());
        let now = run.step(&face(x)).values[channel] > 0.5 * full;
        flips += usize::from(now != high);
        high = now;
    }
    flips
}

fn crossings(probe: &Probe, settle: &Bus, centre: f32, period: u64) -> usize {
    hover_flips((probe.stage)(), &probe.face, probe.out, settle, centre, period)
}

#[test]
fn a_number_hovering_at_a_threshold_does_not_chatter() {
    for probe in probes() {
        for period in [7, 13, 60] {
            let name = probe.name;
            // Showing, and hovering around where it would start: it is kept, so nothing changes.
            assert_eq!(
                crossings(&probe, &probe.on, probe.start, period),
                0,
                "{name}: on, hovering at the start, period {period}"
            );
            // Not showing, and hovering around where it would be released: it cannot start, so nothing changes.
            assert_eq!(
                crossings(&probe, &probe.off, probe.keep, period),
                0,
                "{name}: off, hovering at the release, period {period}"
            );
            // Not showing, and hovering around the start: it starts at most once and then stays. (A short period
            // may never hold the number above the start long enough for the attack hold of the pout and the grin.)
            let starts = crossings(&probe, &probe.off, probe.start, period);
            assert!(
                starts <= 1 && (period < 60 || starts == 1),
                "{name}: off, hovering at the start, period {period}: {starts}"
            );
            // Showing, and hovering around the release: at most one release (a slow dip), never a flicker.
            assert!(
                crossings(&probe, &probe.on, probe.keep, period) <= 1,
                "{name}: on, hovering at the release, period {period}"
            );
        }
    }
}

#[test]
fn a_start_level_set_alone_keeps_the_default_gap_below_it() {
    /// A rule of which the character moves one start level and leaves everything else.
    struct Moved {
        id: &'static str,
        /// The one number the character sets, and where.
        number: (&'static str, f64),
        /// The face with the thresholded value at `x`.
        face: fn(f32) -> Bus,
        /// The channel to watch and its value at full effect.
        out: (Channel, f32),
    }
    let moved = [
        Moved {
            id: "laugh-squint",
            number: ("smile-on", 0.3),
            face: |x| lips(x, 0.0, 0.5, 0.0),
            out: (SQUINT[0], 0.6),
        },
        Moved { id: "laugh-squint", number: ("jaw-on", 0.2), face: |x| lips(0.9, 0.0, x, 0.0), out: (SQUINT[0], 0.6) },
        Moved { id: "pout-cheek-puff", number: ("pout-on", 0.1), face: |x| lips(0.0, 0.9, 0.0, x), out: (PUFF, 0.8) },
        Moved { id: "grin-fangs", number: ("smile-on", 0.3), face: |x| lips(x, 0.0, 0.4, 0.0), out: (FANGS, 1.0) },
    ];
    for Moved { id, number: (name, start), face, out } in moved {
        let stage = || stage_of(id, &config(&[(id, rule_config(true, 1.0, &[(name, start)]))]));
        let (shut, high) = (lips(0.0, 0.0, 0.0, 0.0), face(1.0));
        let start = start as f32;
        for period in [7, 13, 60] {
            // Hovering at the one level the character moved: once showing it stays, and from off it starts at most once.
            let on = hover_flips(stage(), &face, out, &high, start, period);
            assert_eq!(on, 0, "{id} {name}: on, hovering at the start, period {period}");
            let off = hover_flips(stage(), &face, out, &shut, start, period);
            assert!(off <= 1, "{id} {name}: off, hovering at the start, period {period}: {off} flips");
        }
        assert_eq!(hover_flips(stage(), &face, out, &shut, start, 60), 1, "{id} {name}: it does start");
    }
    // The gap itself: a laugh started at 0.35 with only `smile-on = 0.3` is kept at 0.2 (above 0.3 - 0.15) and
    // let go at 0.1; clamping the release onto the start level would have let it go at 0.2.
    let mut run =
        Run::new(stage_of("laugh-squint", &config(&[("laugh-squint", rule_config(true, 1.0, &[("smile-on", 0.3)]))])));
    run.hold(&lips(0.35, 0.0, 0.5, 0.0), 30);
    assert!(close(run.hold(&lips(0.2, 0.0, 0.5, 0.0), 60).values[SQUINT[0]], 0.6));
    assert!(close(run.hold(&lips(0.1, 0.0, 0.5, 0.0), 60).values[SQUINT[0]], 0.0));
}

#[test]
fn a_spike_shorter_than_the_attack_shows_nothing_and_a_sustained_look_starts_within_it() {
    // (rule, the face that makes the look, its channel)
    let looks = [("pout-cheek-puff", lips(0.0, 0.7, 0.0, 0.7), PUFF), ("grin-fangs", lips(0.8, 0.0, 0.4, 0.0), FANGS)];
    let shut = lips(0.0, 0.0, 0.0, 0.0);
    for (id, face, channel) in looks {
        let stage = |params: &[(&str, f64)]| stage_of(id, &config(&[(id, rule_config(true, 1.0, params))]));
        // A spike of one to four frames (0.07 s at 60 fps, under both attack holds) then nothing: the bus is untouched.
        for spike in [1, 2, 4] {
            let mut run = Run::new(stage(&[]));
            run.hold(&shut, 10);
            for frame in 0..spike + 60 {
                let input = if frame < spike { &face } else { &shut };
                let out = run.step(input);
                assert_eq!(bits(&out.values), bits(&input.values), "{id}: a spike of {spike} frames, frame {frame}");
            }
        }
        // A spike that keeps coming back but never lasts the attack is no look either.
        let mut run = Run::new(stage(&[]));
        for frame in 0..300 {
            let input = if frame % 4 < 3 { &face } else { &shut };
            assert_eq!(run.step(input).values[channel], 0.0, "{id}: a 3-frame spike every 4 frames, frame {frame}");
        }
        // Held, the look starts after a few frames, not on the first, and within 8 frames (0.13 s).
        let mut run = Run::new(stage(&[]));
        let first = (1..=30).find(|_| run.step(&face).values[channel] > 0.0).unwrap();
        assert!((3..=8).contains(&first), "{id}: first shows on frame {first}");
        // `attack = 0` starts on the first frame (the old behaviour); a longer one is later; and it is capped.
        let first_with = |attack: f64| {
            let mut run = Run::new(stage(&[("attack", attack)]));
            (1..=60).find(|_| run.step(&face).values[channel] > 0.0).unwrap()
        };
        assert_eq!(first_with(0.0), 1, "{id}: attack 0");
        assert!(
            first_with(0.3) >= 18 && first_with(0.3) <= 20,
            "{id}: attack 0.3 s is about 18 frames: {}",
            first_with(0.3)
        );
        assert_eq!(first_with(60.0), first_with(0.5), "{id}: the longest attack is 0.5 s");
    }
}

#[test]
fn a_flicker_of_a_frame_or_two_never_changes_a_look() {
    for probe in probes() {
        let mut run = Run::new((probe.stage)());
        let (channel, full) = probe.out;
        run.hold(&probe.on, 60);
        for i in 0..300 {
            // One frame in five is a dip to the far side of the release level.
            let face = if i % 5 == 0 { &probe.off } else { &probe.on };
            let value = run.step(face).values[channel];
            assert!(value > 0.5 * full, "{}: frame {i} dropped to {value}", probe.name);
        }
    }
}

// ---- switches, strength and the character's own numbers ---------------------------------------------------------------

/// A face that makes each rule write, with the rule's id.
fn triggers() -> [(&'static str, Bus); 3] {
    [
        ("laugh-squint", lips(1.0, 0.0, 0.6, 0.0)),
        ("pout-cheek-puff", lips(0.0, 0.7, 0.0, 0.7)),
        ("grin-fangs", lips(0.8, 0.0, 0.4, 0.0)),
    ]
}

fn stage_of(id: &str, config: &GrammarConfig) -> GrammarStage {
    match id {
        "laugh-squint" => solo(LaughSquint::default(), config),
        "pout-cheek-puff" => solo(PoutCheekPuff::default(), config),
        _ => solo(GrinFangs::default(), config),
    }
}

#[test]
fn switching_a_rule_off_leaves_the_bus_bit_identical() {
    let mut rng = Rng::new(11);
    for (id, trigger) in triggers() {
        let on = GrammarConfig::default();
        let off = config(&[(id, rule_config(false, 1.0, &[]))]);
        let (mut acting, mut idle) = (Run::new(stage_of(id, &on)), Run::new(stage_of(id, &off)));
        assert_ne!(bits(&acting.hold(&trigger, 60).values), bits(&trigger.values), "{id} acts when it is on");
        for _ in 0..200 {
            // Wild values, NaN and infinity included: an untouched bus must not even be sanitised.
            let mut wild = trigger;
            for info in CHANNELS {
                wild.values[info.channel] = match rng.next_u64() % 10 {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    _ => rng.range_f32(-2.0, 3.0),
                };
            }
            wild.owners.claim(Source::Score, ChannelMask::from_bits_truncate(u128::from(rng.next_u64())));
            let out = idle.step(&wild);
            assert_eq!((bits(&out.values), out.owners), (bits(&wild.values), wild.owners), "{id} is off");
        }
    }
    // Off together with the whole of FE-05 off: also nothing.
    let all_off = config(&RESERVED_RULES.map(|id| (id, rule_config(false, 1.0, &[]))));
    let mut run = Run::new(GrammarStage::new(&all_off).unwrap());
    for (_, trigger) in triggers() {
        assert_eq!(run.hold(&trigger, 30), trigger);
    }
}

#[test]
fn strength_dials_how_far_a_look_goes() {
    // (rule, the face, the channel to read, its value at strength 1)
    let looks = [
        ("laugh-squint", lips(0.9, 0.0, 0.5, 0.0), SQUINT[0], 0.6),
        ("pout-cheek-puff", lips(0.0, 0.7, 0.0, 0.7), PUFF, 0.8),
        ("grin-fangs", lips(0.8, 0.0, 0.4, 0.0), FANGS, 1.0),
    ];
    for (id, face, channel, full) in looks {
        let at = |strength: f64| {
            let mut run = Run::new(stage_of(id, &config(&[(id, rule_config(true, strength, &[]))])));
            run.hold(&face, 60)
        };
        assert_eq!(bits(&at(0.0).values), bits(&face.values), "{id}: strength 0 acts not at all");
        assert!(close(at(0.5).values[channel], 0.5 * full), "{id}: half strength is half way");
        assert!(close(at(1.0).values[channel], full), "{id}: strength 1 is the rule as designed");
        assert!(
            close(at(2.0).values[channel], (2.0 * full).min(1.0)),
            "{id}: twice goes twice as far, inside the range"
        );
        assert!(close(at(f64::NAN).values[channel], full), "{id}: a corrupt strength reads as the default");
    }
}

#[test]
fn a_character_sets_the_thresholds_and_levels_of_its_own_rules() {
    let run = |id: &str, params: &[(&str, f64)], face: &Bus, channel: Channel| {
        let mut run = Run::new(stage_of(id, &config(&[(id, rule_config(true, 1.0, params))])));
        run.hold(face, 60).values[channel]
    };
    // A lower start level: this smile is not a laugh by default, and is for a character that laughs easily.
    let easy = lips(0.35, 0.0, 0.5, 0.0);
    assert_eq!(run("laugh-squint", &[], &easy, SQUINT[0]), 0.0);
    assert!(close(run("laugh-squint", &[("smile-on", 0.3), ("smile-off", 0.2)], &easy, SQUINT[0]), 0.6));
    // The levels a full laugh raises to.
    assert!(close(run("laugh-squint", &[("squint", 0.3), ("cheek", 0.9)], &easy, CHEEK[0]), 0.0), "not a laugh yet");
    let laughing = lips(0.9, 0.0, 0.5, 0.0);
    assert!(close(run("laugh-squint", &[("squint", 0.3), ("cheek", 0.9)], &laughing, SQUINT[0]), 0.3));
    assert!(close(run("laugh-squint", &[("squint", 0.3), ("cheek", 0.9)], &laughing, CHEEK[0]), 0.9));
    // The smile that closes into ^^ moves.
    let broad = lips(0.8, 0.0, 0.5, 0.0);
    assert!(close(run("laugh-squint", &[("arc-smile", 0.5)], &broad, ARC[0]), 1.0));
    assert!(close(run("laugh-squint", &[], &broad, ARC[0]), 0.0));
    // A pout's size and how shut the mouth must be.
    let pouting = lips(0.0, 0.6, 0.0, 0.6);
    assert!(close(run("pout-cheek-puff", &[("puff", 0.4), ("brow-down", 0.5)], &pouting, PUFF), 0.4));
    assert!(close(run("pout-cheek-puff", &[("puff", 0.4), ("brow-down", 0.5)], &pouting, BROW_DOWN[0]), 0.5));
    let open = lips(0.0, 0.9, 0.4, 0.9);
    assert!(close(run("pout-cheek-puff", &[], &open, PUFF), 0.0), "0.4 of a 0.5 jaw is mostly open");
    assert!(
        close(run("pout-cheek-puff", &[("jaw-max", 1.0)], &open, PUFF), 0.8),
        "a character that pouts with a looser jaw"
    );
    // Fangs: a character with small fangs, and one without.
    let grinning = lips(0.8, 0.0, 0.4, 0.0);
    assert!(close(run("grin-fangs", &[("show", 0.5)], &grinning, FANGS), 0.5));
    assert_eq!(run("grin-fangs", &[("show", 0.0)], &grinning, FANGS), 0.0, "no fangs: the rule never writes");
    // Numbers out of range are kept in range; names no rule reads are ignored.
    assert!(close(run("grin-fangs", &[("show", 5.0)], &grinning, FANGS), 1.0));
    assert!(close(run("grin-fangs", &[("show", -1.0)], &grinning, FANGS), 0.0));
    assert!(close(run("grin-fangs", &[("from-a-newer-build", 7.0)], &grinning, FANGS), 1.0));
}

#[test]
fn a_release_level_above_the_start_level_is_held_at_it() {
    // Written the wrong way round (start 0.5, release 0.9): without the guard a laugh would be released on the very
    // frame it started. With it the release is 0.5, so a smile of 0.55 starts it and 0.52 keeps it.
    let swapped = config(&[("laugh-squint", rule_config(true, 1.0, &[("smile-on", 0.5), ("smile-off", 0.9)]))]);
    let mut run = Run::new(solo(LaughSquint::default(), &swapped));
    run.hold(&lips(0.55, 0.0, 0.5, 0.0), 30);
    assert!(close(run.hold(&lips(0.52, 0.0, 0.5, 0.0), 60).values[SQUINT[0]], 0.6));
}

#[test]
fn new_numbers_keep_a_running_look_instead_of_restarting_it() {
    let mut stage = solo(LaughSquint::default(), &GrammarConfig::default());
    let laughing = lips(0.9, 0.0, 0.5, 0.0);
    for i in 0..60 {
        let mut frame = laughing;
        stage.step(tick(i), &mut frame);
    }
    stage.set_config(&config(&[("laugh-squint", rule_config(true, 1.0, &[("squint", 0.3)]))]));
    let mut next = laughing;
    stage.step(tick(60), &mut next);
    assert!(close(next.values[SQUINT[0]], 0.3), "the next frame already uses the new level, at the full weight");
}

#[test]
fn the_face_profile_carries_the_numbers() {
    let mut profile = FaceProfile::default();
    profile.grammar.0.insert(rule_id("grin-fangs"), rule_config(true, 1.0, &[("show", 0.25)]));
    let mut run = Run::new(solo(GrinFangs::default(), &GrammarConfig::from_profile(&profile)));
    assert!(close(run.hold(&lips(0.8, 0.0, 0.4, 0.0), 60).values[FANGS], 0.25));
}

#[test]
fn the_registry_supplies_every_reserved_rule_in_a_fixed_order() {
    let stage = everything();
    let ids: Vec<&str> = stage.rule_ids().collect();
    assert_eq!(ids, ["smile-blink-arc", "laugh-squint", "pout-cheek-puff", "grin-fangs"]);
    for reserved in RESERVED_RULES {
        assert!(ids.contains(&reserved), "{reserved} is reserved by the face profile and must be implemented");
    }
}

// ---- ownership --------------------------------------------------------------------------------------------------------

/// A bus of random values for every channel on frame `i`. The mouth is held as a laugh, a pout or a grin for 40 frames
/// at a time (so the rules, which wait out an attack hold, get to fire) and random in between.
fn wild_bus(rng: &mut Rng, i: u64) -> Bus {
    let mut bus = Bus::default();
    for info in CHANNELS {
        bus.values[info.channel] = match rng.next_u64() % 16 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            _ => rng.range_f32(-0.3, 1.3),
        };
    }
    let held = match (i / 40) % 4 {
        0 => Some(lips(0.9, 0.0, 0.5, 0.0)),
        1 => Some(lips(0.0, 0.7, 0.0, 0.7)),
        2 => Some(lips(0.8, 0.0, 0.2, 0.0)),
        _ => None,
    };
    for channel in [SMILE[0], SMILE[1], FROWN[0], FROWN[1], JAW, PUCKER] {
        bus.values[channel] = held.map_or_else(|| rng.next_f32(), |mouth| mouth.values[channel]);
    }
    bus
}

#[test]
fn the_rules_write_only_their_own_channels_and_never_a_persons() {
    let mut rng = Rng::new(7);
    let own: Vec<Channel> = [LAUGH_WRITES.to_vec(), POUT_WRITES.to_vec(), GRIN_WRITES.to_vec()].concat();
    let mut stage = trio(&GrammarConfig::default());
    let (mut writes, mut refusals) = (0, 0);
    for i in 0..6000 {
        let mut input = wild_bus(&mut rng, i);
        let mut human = ChannelMask::EMPTY;
        for &channel in &own {
            if rng.next_u64().is_multiple_of(3) {
                human.insert(channel);
            }
        }
        if rng.next_u64().is_multiple_of(5) {
            human |= ChannelMask::ARKIT;
        }
        input.owners.claim(if i % 2 == 0 { Source::Tracker } else { Source::Trigger }, human);
        let mut out = input;
        stage.step(tick(i), &mut out);
        assert_eq!(out.owners, input.owners, "a rule decorates; it does not take channels");
        for info in CHANNELS {
            let c = info.channel;
            let (before, after) = (input.values[c], out.values[c]);
            if before.to_bits() == after.to_bits() {
                continue;
            }
            assert!(own.contains(&c), "{c} is not one of the rules' channels");
            assert!(!human.contains(c), "{c} is driven by a person");
            assert!(after.is_finite() && (info.min..=info.max).contains(&after), "{c} left its range: {after}");
            // Looks only add: nothing a source or an earlier rule put on a channel is lowered (but an illegal value
            // the rule writes over is brought into range).
            assert!(
                !before.is_finite() || after >= before.clamp(info.min, info.max),
                "{c} was lowered from {before} to {after}"
            );
            writes += 1;
        }
        refusals += own.iter().filter(|c| human.contains(**c)).count();
    }
    assert!(writes > 3000 && refusals > 3000, "writes {writes}, refusals {refusals}");
}

#[test]
fn under_a_tracker_the_rules_still_decorate_what_it_cannot_drive() {
    let tracked = |mut face: Bus| {
        face.owners.claim(Source::Tracker, ChannelMask::ARKIT);
        face
    };
    // A tracker drives the ARKit channels but has no eyeArc or fangs: those still follow the mouth.
    let out = Run::new(laugh()).hold(&tracked(lips(1.0, 0.0, 0.6, 0.0)), 60);
    assert_eq!((out.values[ARC[0]], out.values[SQUINT[0]], out.values[CHEEK[0]]), (1.0, 0.0, 0.0));
    let out = Run::new(grin()).hold(&tracked(lips(0.8, 0.0, 0.4, 0.0)), 60);
    assert_eq!(out.values[FANGS], 1.0);
    // The cheeks and brows of a pout are ARKit channels, so a tracked person's own face stays theirs.
    let face = tracked(lips(0.0, 0.7, 0.0, 0.7));
    assert_eq!(Run::new(pout()).hold(&face, 60), face);
}

#[test]
fn a_look_keeps_following_the_mouth_while_the_eyes_belong_to_a_person() {
    let laughing = lips(0.9, 0.0, 0.5, 0.0);
    let mut run = Run::new(laugh());
    let mut taken = laughing;
    taken.owners.claim(Source::Tracker, ChannelMask::EYES);
    assert_eq!(run.hold(&taken, 60), taken, "nothing is written while a person has the eyes");
    // On hand-back the laugh is already fully faded in, because the latch and the ramp kept following: no jump from zero.
    let first = run.step(&laughing).values[SQUINT[0]];
    assert!(close(first, 0.6), "{first}");
}

// ---- the life layer's share of a laugh --------------------------------------------------------------------------------

/// One frame of a run through the life layer and then the grammar stage.
struct Played {
    t: f32,
    /// The bus as the life layer left it.
    from_life: Bus,
    /// The bus as the grammar stage left it.
    after: Bus,
}

/// An 8 s run at 60 fps: the mouth laughs from 1 s to 4 s (`smile`, a jaw of 0.55, and `loud` speech), and the bus goes
/// through a seeded life layer and then every registered grammar rule.
fn play(seed: u64, smile: f32, loud: f32) -> Vec<Played> {
    let (mut life, mut stage) = (Life::new(LifeConfig::new(seed)), everything());
    (0..480)
        .map(|i| {
            let t = i as f32 / 60.0;
            let laughing = (1.0..4.0).contains(&t);
            let mut bus = if laughing { lips(smile, 0.0, 0.55, 0.0) } else { lips(0.1, 0.0, 0.0, 0.0) };
            life.step(
                tick(i),
                &Cues { loudness: if laughing { loud } else { 0.0 }, onset: false, laugh: 0.0 },
                &mut bus,
            );
            let from_life = bus;
            stage.step(tick(i), &mut bus);
            Played { t, from_life, after: bus }
        })
        .collect()
}

/// The laugh once it has settled, and before it fades again.
fn in_the_laugh(frame: &Played) -> bool {
    (1.6..3.9).contains(&frame.t)
}

const HEAD: [Channel; 5] =
    [Channel::HEAD_YAW, Channel::HEAD_PITCH, Channel::HEAD_ROLL, Channel::HEAD_SHIFT_X, Channel::HEAD_SHIFT_Y];

/// The things that hold for every frame of every run: a legal bus, the same owners, and the head exactly as the life
/// layer left it (the bob is the life layer's, never a second one from the grammar).
fn assert_coherent(seed: u64, run: &[Played]) {
    for frame in run {
        let (before, after, t) = (&frame.from_life, &frame.after, frame.t);
        let mut clean = after.values;
        clean.sanitize();
        assert_eq!(bits(&clean), bits(&after.values), "seed {seed}, t {t}: a channel is outside its range");
        assert_eq!(after.owners, before.owners, "seed {seed}, t {t}");
        for head in HEAD {
            assert_eq!(
                after.values[head].to_bits(),
                before.values[head].to_bits(),
                "seed {seed}, t {t}: the grammar moved {head}"
            );
        }
    }
}

#[test]
fn a_huge_laugh_through_the_life_layer_squints_closes_into_arcs_and_leaves_the_bob_to_the_life_layer() {
    let mean_pitch = |run: &[Played], from: f32, to: f32| {
        let pitches: Vec<f32> =
            run.iter().filter(|f| (from..to).contains(&f.t)).map(|f| f.from_life.values[Channel::HEAD_PITCH]).collect();
        pitches.iter().sum::<f32>() / pitches.len() as f32
    };
    for seed in 1..=6 {
        let run = play(seed, 0.97, 0.9);
        assert_coherent(seed, &run);
        for frame in run.iter().filter(|f| in_the_laugh(f)) {
            let v = &frame.after.values;
            for side in 0..2 {
                let t = frame.t;
                assert!(
                    v[SQUINT[side]] >= 0.6 - 1e-3 && v[CHEEK[side]] >= 0.6 - 1e-3,
                    "seed {seed}, t {t}: the laugh squints"
                );
                assert!(v[ARC[side]] > 0.99, "seed {seed}, t {t}: a huge laugh is ^^");
            }
        }
        // The life layer's bob leans the head back (negative pitch) while the laugh lasts; that is its doing alone.
        assert!(
            mean_pitch(&run, 1.6, 3.9) < mean_pitch(&run, 0.0, 0.9) - 0.02,
            "seed {seed}: the life layer bobs the head back"
        );
    }
}

#[test]
fn a_blink_in_a_big_laugh_closes_into_an_arc_over_the_squint() {
    let mut blinks = 0;
    for seed in 1..=8 {
        // A big laugh that is not loud and not huge: the life layer neither closes it into ^^ nor stops its blinks.
        let run = play(seed, 0.75, 0.7);
        assert_coherent(seed, &run);
        for frame in run.iter().filter(|f| in_the_laugh(f)) {
            let (before, after, t) = (&frame.from_life.values, &frame.after.values, frame.t);
            for side in 0..2 {
                assert!(after[SQUINT[side]] >= 0.6 - 1e-3, "seed {seed}, t {t}: the squint stays through a blink");
                if before[BLINK[side]] > 0.55 {
                    blinks += 1;
                    assert!(
                        after[ARC[side]] > 0.99 && after[BLINK[side]] == 0.0,
                        "seed {seed}, t {t}: a blink in a smile is the arc"
                    );
                }
            }
        }
    }
    assert!(blinks >= 6, "the seeds must put blinks into the laugh: {blinks}");
}

#[test]
fn a_laugh_the_life_layer_already_squinted_is_not_squinted_twice() {
    // The life layer adds 0.2 to the squint of a laugh (capped at 0.6) and the rule raises to 0.6: the two never add.
    let run = play(3, 0.75, 0.7);
    let mut life_top = 0.0_f32;
    let mut grammar_top = 0.0_f32;
    for frame in &run {
        life_top = life_top.max(frame.from_life.values[SQUINT[0]]);
        grammar_top = grammar_top.max(frame.after.values[SQUINT[0]]);
        assert!(frame.after.values[SQUINT[0]] >= frame.from_life.values[SQUINT[0]], "a rule only adds");
    }
    assert!(life_top < 0.45, "the life layer's own squint is small: {life_top}");
    assert!(
        (0.6 - 1e-3..0.7).contains(&grammar_top),
        "and the rule's is 0.6 (plus the lid drift), not their sum: {grammar_top}"
    );
}

// ---- the design, over thousands of traces -----------------------------------------------------------------------------

/// A random walk of the mouth for the pout and the grin, which have no prototype to compare with: 100 frames of
/// `(smile, frown, jaw, pucker)`. A third of the traces stay in the pout's range, a third in the grin's.
fn mouth_walk(i: u32) -> Vec<[f32; 4]> {
    let mut r = Mulberry(0x5eed_2000_u32.wrapping_add(i));
    // (lo, hi) per channel in 256ths: smile, frown, jaw, pucker.
    let ranges: [(i32, i32); 4] = match i % 3 {
        0 => [(0, 40), (50, 230), (0, 90), (20, 230)],
        1 => [(80, 255), (0, 40), (0, 140), (0, 30)],
        _ => [(0, 255), (0, 255), (0, 255), (0, 255)],
    };
    let mut at = ranges.map(|(lo, hi)| lo + r.below((hi - lo + 1).cast_unsigned()));
    (0..100)
        .map(|_| {
            for (value, (lo, hi)) in at.iter_mut().zip(ranges) {
                *value = bounce(*value + r.below(41) - 20, lo, hi);
            }
            at.map(|k| k as f32 / 256.0)
        })
        .collect()
}

#[test]
fn the_pout_and_the_grin_keep_their_design_on_2000_traces_each() {
    let (mut pouts, mut grins) = (0, 0);
    for i in 0..2000 {
        let walk = mouth_walk(i);
        let (mut pout_run, mut grin_run) = (Run::new(pout()), Run::new(grin()));
        let (mut pout_for, mut grin_for) = (0_u32, 0_u32);
        let (mut pout_seen, mut grin_seen) = (0_u32, 0_u32);
        for (f, &[smile, frown, jaw, pucker]) in walk.iter().enumerate() {
            let face = lips(smile, frown, jaw, pucker);
            let (p, g) = (pout_run.step(&face), grin_run.step(&face));
            // Never lowers, never writes outside its channels, never leaves its range.
            for (out, stage) in [(&p, "pout"), (&g, "grin")] {
                for info in CHANNELS {
                    let (a, b) = (face.values[info.channel], out.values[info.channel]);
                    let theirs = if stage == "pout" {
                        POUT_WRITES.contains(&info.channel)
                    } else {
                        GRIN_WRITES.contains(&info.channel)
                    };
                    assert!(
                        theirs || a.to_bits() == b.to_bits(),
                        "{stage} trace {i} frame {f}: {} changed",
                        info.channel
                    );
                    assert!(b >= a && b <= info.max, "{stage} trace {i} frame {f}: {} went {a} -> {b}", info.channel);
                }
            }
            // The conditions each look starts on, as the design states them.
            let shut = 1.0 - (jaw / 0.5).clamp(0.0, 1.0);
            let level = pucker.min(frown) * shut;
            let net = smile - frown;
            pout_for = if level > 0.3 { pout_for + 1 } else { 0 };
            grin_for = if net > 0.5 && jaw > 0.15 { grin_for + 1 } else { 0 };
            // Held for 20 frames (0.33 s, longer than the attack hold and the rise) the look is at full effect.
            if pout_for >= 20 {
                assert!(close(p.values[PUFF], 0.8), "pout trace {i} frame {f}: {}", p.values[PUFF]);
                pouts += 1;
            }
            if grin_for >= 20 {
                assert!(close(g.values[FANGS], 1.0), "grin trace {i} frame {f}: {}", g.values[FANGS]);
                grins += 1;
            }
            // A look needs a reason: it only shows within 40 frames of the face having met its release condition.
            pout_seen = if level > 0.2 { 0 } else { pout_seen + 1 };
            grin_seen = if net > 0.4 && jaw > 0.08 { 0 } else { grin_seen + 1 };
            if p.values[PUFF] > 0.0 {
                assert!(pout_seen < 40, "pout trace {i} frame {f}: a puff with no pout for {pout_seen} frames");
            }
            if g.values[FANGS] > 0.0 {
                assert!(grin_seen < 40, "grin trace {i} frame {f}: fangs with no grin for {grin_seen} frames");
            }
        }
    }
    assert!(pouts > 5000 && grins > 5000, "the traces must reach both looks: {pouts} pout frames, {grins} grin frames");
}

// ---- face strips ------------------------------------------------------------------------------------------------------

const PX_PER_M: f32 = 1000.0;
const SKIN: [u8; 3] = [249, 211, 197];
/// Frames of the strip: snapshots at these 60 fps frame numbers.
const STRIP_FRAMES: [u64; 6] = [0, 15, 24, 33, 54, 84];

/// What a strip shows of one rule: how the mouth moves in, which channel its bar plots.
struct Strip {
    name: &'static str,
    /// The incoming face at time `t` seconds: the mouth eases in over 0.3 s, starting at 0.2 s.
    face: fn(f32) -> Bus,
    bar: Channel,
}

fn onset(t: f32) -> f32 {
    ((t - 0.2) / 0.3).clamp(0.0, 1.0)
}

fn strips() -> [Strip; 3] {
    [
        Strip { name: "laugh-squint", face: |t| lips(0.97 * onset(t), 0.0, 0.55 * onset(t), 0.0), bar: SQUINT[0] },
        Strip { name: "pout-cheek-puff", face: |t| lips(0.0, 0.6 * onset(t), 0.0, 0.6 * onset(t)), bar: PUFF },
        Strip { name: "grin-fangs", face: |t| lips(0.8 * onset(t), 0.0, 0.2 * onset(t), 0.0), bar: FANGS },
    ]
}

/// Paints `face` through the mock painter into a premultiplied raster.
fn paint(painter: &mut MockPainter, profile: &FaceProfile, face: &Bus) -> Raster {
    let params = FaceMapping::default().apply(&face.values);
    let input = FaceInput { profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let spec = *painter.layers().iter().find(|l| l.id == FaceLayerId::Base).unwrap();
    let mut out = Raster::new(1, 1);
    MockRasterizer.rasterize(&spec, &drawing[FaceLayerId::Base], PX_PER_M, &mut out).unwrap();
    out
}

/// Composites a premultiplied raster over the sheet at `(ox, oy)`, and draws a bar of `level` under it.
fn place(sheet: &mut RgbaImage, r: &Raster, (ox, oy): (u32, u32), level: f32) {
    for (i, px) in r.pixels.chunks(4).enumerate() {
        let i = i as u32;
        let dst = sheet.get_pixel_mut(ox + i % r.width, oy + i / r.width);
        let inv = 1.0 - f32::from(px[3]) / 255.0;
        for (d, s) in dst.0.iter_mut().zip(&px[..3]) {
            *d = (f32::from(*s) + f32::from(*d) * inv).clamp(0.0, 255.0).round() as u8;
        }
    }
    let filled = (level.clamp(0.0, 1.0) * r.width as f32) as u32;
    for x in 0..r.width {
        for y in 0..8 {
            let on = x < filled;
            sheet.put_pixel(
                ox + x,
                oy + r.height - 1 - y,
                if on { Rgba([60, 60, 60, 255]) } else { Rgba([225, 225, 225, 255]) },
            );
        }
    }
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn each_rule_has_a_strip_that_shows_what_it_does() {
    let (profile, mut painter) = (mock_profile(), MockPainter::default());
    let dir = std::env::var_os("OMUZAKI_STRIPS");
    for strip in strips() {
        let mut run = Run::new(everything());
        // Row 0 is the incoming face (before), row 1 what the stage made of it (after).
        let mut cells: Vec<(Bus, Bus)> = Vec::new();
        for f in 0..=*STRIP_FRAMES.last().unwrap() {
            let before = (strip.face)(f as f32 / 60.0);
            let after = run.step(&before);
            if STRIP_FRAMES.contains(&f) {
                cells.push((before, after));
            }
        }
        let rasters: Vec<(Raster, Raster)> =
            cells.iter().map(|(b, a)| (paint(&mut painter, &profile, b), paint(&mut painter, &profile, a))).collect();
        let (cw, ch) = (rasters[0].0.width, rasters[0].0.height);
        let mut sheet = RgbaImage::from_pixel(cw * 6, ch * 2, Rgba([SKIN[0], SKIN[1], SKIN[2], 255]));
        for (k, ((before, after), (b, a))) in cells.iter().zip(&rasters).enumerate() {
            let x = cw * k as u32;
            place(&mut sheet, b, (x, 0), before.values[strip.bar]);
            place(&mut sheet, a, (x, ch), after.values[strip.bar]);
        }
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).unwrap();
            sheet.save(std::path::Path::new(dir).join(format!("{}.png", strip.name))).unwrap();
        }

        let differs = |(b, a): &(Raster, Raster)| b.pixels.iter().zip(&a.pixels).filter(|(x, y)| x != y).count();
        // Before anything happens the rule has nothing to say; at the end of the strip it has said it.
        assert_eq!(differs(&rasters[0]), 0, "{}: the first cell is the neutral face", strip.name);
        let last = cells.len() - 1;
        assert!(close(cells[0].1.values[strip.bar], 0.0) && cells[last].1.values[strip.bar] > 0.5, "{}", strip.name);
        match strip.name {
            // The mock painter draws squints, brows and the blush of a puff, but not fangs: for those the paint
            // parameters the real painter would get are checked instead.
            "grin-fangs" => {
                let params = FaceMapping::default().apply(&cells[last].1.values);
                assert!(
                    params.mouth.fangs > 0.99 && FaceMapping::default().apply(&cells[last].0.values).mouth.fangs == 0.0
                );
            }
            name => assert!(
                differs(&rasters[last]) > 500,
                "{name}: the last cell differs from the incoming face by {} px",
                differs(&rasters[last])
            ),
        }
    }
}

// ---- determinism and the hot path -------------------------------------------------------------------------------------

/// A busy frame: laughs, pouts and grins in turn, wild numbers around them, a tracker taking the eyes for a while.
fn busy_frame(i: u64, rng: &mut Rng) -> Bus {
    let mut bus = wild_bus(rng, i);
    let mouth = match (i / 90) % 3 {
        0 => lips(0.9, 0.0, 0.5, 0.0),
        1 => lips(0.0, 0.7, 0.0, 0.7),
        _ => lips(0.8, 0.0, 0.4, 0.0),
    };
    for channel in [SMILE[0], SMILE[1], FROWN[0], FROWN[1], JAW, PUCKER] {
        bus.values[channel] = mouth.values[channel];
    }
    if (i / 270) % 3 == 2 {
        bus.owners.claim(Source::Tracker, ChannelMask::EYES);
    }
    bus
}

#[test]
fn a_replay_is_bit_identical() {
    let run = || {
        let (mut stage, mut rng) = (everything(), Rng::new(5));
        (0..3000).fold(0_u64, |h, i| {
            let mut bus = busy_frame(i, &mut rng);
            stage.step(tick(i), &mut bus);
            h.rotate_left(5) ^ bus.values.checksum()
        })
    };
    assert_eq!(run(), run());
}

#[test]
fn a_frame_of_the_grammar_rules_allocates_nothing() {
    let mut stage = everything();
    let mut off = GrammarStage::new(&config(&RESERVED_RULES.map(|id| (id, rule_config(false, 1.0, &[]))))).unwrap();
    let tuned = config(&[
        ("laugh-squint", rule_config(true, 1.5, &[("smile-on", 0.5), ("squint", 0.4)])),
        ("pout-cheek-puff", rule_config(true, 0.5, &[("puff", 0.6)])),
        ("grin-fangs", rule_config(true, 1.0, &[("show", 0.7)])),
    ]);
    let defaults = GrammarConfig::default();
    let mut rng = Rng::new(9);
    let mut run = |from: u64, to: u64| {
        for i in from..to {
            // Reconfiguring is not on the real-time path, but it does not allocate either.
            if i % 50 == 0 {
                stage.set_config(if (i / 50) % 2 == 0 { &defaults } else { &tuned });
            }
            for s in [&mut stage, &mut off] {
                let mut bus = busy_frame(i, &mut rng);
                s.step(tick(i), &mut bus);
                std::hint::black_box(bus);
            }
        }
    };
    run(0, 400); // warm-up
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    run(400, 20_400); // laughs, pouts and grins, refused to a person, reconfigured, and switched off
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "20,000 frames of the grammar rules made {allocations} allocations");
}
