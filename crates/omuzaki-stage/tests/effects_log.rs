//! The emission log (`effects::log`, particles.md D7, D8, D12): properties over random call sequences (the cap, what
//! prune keeps, contiguous stream indices, determinism), a two-hour stream, the weather share, pre-warm, clear, and
//! zero allocation after construction.

// A `GlobalAlloc` impl is `unsafe` by definition. This one only counts and forwards to `System`.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;

use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::particles::evaluate;
use omuzaki_render_api::{
    Colour, EmitterShape, Orientation, ParticleParams, ParticleSystemDesc, ParticleSystemId, Renderer, Span, Sprite,
    SpriteShape,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_render_mock::particle_sheet::still;
use omuzaki_stage::effects::log::{BurstOutcome, EffectLimits, EmissionLog, Spawn, StreamKey};
use proptest::prelude::*;
use proptest::test_runner::RngSeed;

struct CountingAlloc;

thread_local! {
    static TRACKING: StdCell<bool> = const { StdCell::new(false) };
    static ALLOCATIONS: StdCell<usize> = const { StdCell::new(0) };
}

fn count_allocation() {
    let _ = TRACKING.try_with(|tracking| {
        if tracking.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Allocations `work` makes on this thread.
fn allocations(work: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|c| c.set(0));
    TRACKING.with(|t| t.set(true));
    work();
    TRACKING.with(|t| t.set(false));
    ALLOCATIONS.with(StdCell::get)
}

/// A falling system that lives `life` seconds at most (the minimum is half of it).
fn desc(life: f32) -> ParticleSystemDesc {
    let params = ParticleParams {
        lifetime: Span::new(life * 0.5, life),
        speed: Span::new(0.1, 0.5),
        ..still(0.02, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE)
    };
    ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params }
}

struct Systems {
    _renderer: MockRenderer,
    ids: Vec<(ParticleSystemId, ParticleSystemDesc)>,
}

impl Systems {
    fn new(lives: &[f32]) -> Self {
        let mut renderer = MockRenderer::new();
        let ids = lives
            .iter()
            .map(|&life| {
                let d = desc(life);
                (renderer.create_particle_system(&d).unwrap(), d)
            })
            .collect();
        Self { _renderer: renderer, ids }
    }

    fn desc(&self, id: ParticleSystemId) -> &ParticleSystemDesc {
        &self.ids.iter().find(|(i, _)| *i == id).unwrap().1
    }
}

fn spawn(system: ParticleSystemId, seed: u32) -> Spawn {
    Spawn { shape: EmitterShape::sphere(0.1), seed, ..Spawn::new(system, Mat4::from_translation(Vec3::Y)) }
}

/// One call of a random sequence.
#[derive(Clone, Debug)]
enum Call {
    Burst { system: usize, count: u32 },
    Stream { key: u32, system: usize, rate: f32, weather: bool },
    Clear { fade: f32 },
}

fn call() -> impl Strategy<Value = Call> {
    prop_oneof![
        4 => (0..3usize, 1..1_500u32).prop_map(|(system, count)| Call::Burst { system, count }),
        5 => (0..3u32, 0..3usize, prop_oneof![Just(0.0f32), 1.0..400.0f32], any::<bool>())
            .prop_map(|(key, system, rate, weather)| Call::Stream { key, system, rate, weather }),
        1 => (0.0..2.0f32).prop_map(|fade| Call::Clear { fade }),
    ]
}

/// The particles `evaluate` calls alive at `t`, as (system, seed, index).
fn alive(log: &EmissionLog, systems: &Systems, t: Seconds) -> Vec<(ParticleSystemId, u32, u32)> {
    let mut out = Vec::new();
    for b in log.batches() {
        let desc = systems.desc(b.system);
        for g in b.first..b.first + b.count {
            if evaluate(desc, b, g, t, 1.0).is_some() {
                out.push((b.system, b.seed, g));
            }
        }
    }
    out
}

fn held(log: &EmissionLog, key: (ParticleSystemId, u32, u32)) -> bool {
    log.batches().iter().any(|b| (b.system, b.seed) == (key.0, key.1) && (b.first..b.first + b.count).contains(&key.2))
}

/// Plays `calls`, one per frame of `dt`, pruning every frame, and checks the properties as it goes.
fn play(calls: &[Call], systems: &Systems, dt: f64, check: bool) -> EmissionLog {
    let limits = EffectLimits::live();
    let mut log = EmissionLog::new(limits);
    let life = |system: usize| systems.ids[system].1.params.lifetime.max;
    let mut t = 1.0;
    for c in calls {
        match *c {
            Call::Burst { system, count } => {
                let before = log.slots();
                let out =
                    log.burst(Seconds(t), &spawn(systems.ids[system].0, 100 + system as u32), count, life(system));
                assert_eq!(out.count + out.thinned, count);
                assert!(out.count <= limits.max_slots - before.min(limits.max_slots));
            }
            Call::Stream { key, system, rate, weather } => {
                // One system per key, so a key's segments share a lifetime and its indices stay contiguous.
                let _ = system;
                let system = key as usize % systems.ids.len();
                assert!(log.stream(
                    StreamKey(key),
                    Seconds(t),
                    &spawn(systems.ids[system].0, key),
                    rate,
                    life(system),
                    weather
                ));
            }
            Call::Clear { fade } => log.clear(Seconds(t), fade),
        }
        t += dt;
        let before = if check { alive(&log, systems, Seconds(t)) } else { Vec::new() };
        // A clear that finishes drops live particles on purpose; nothing else may.
        let clearing = log.meta().iter().any(|m| m.fading.is_some());
        log.prune(Seconds(t));
        if check {
            assert!(log.slots() <= limits.max_slots, "{} slots", log.slots());
            if !clearing {
                for particle in &before {
                    assert!(held(&log, *particle), "{particle:?} alive at {t} but pruned");
                }
            }
            contiguous(&log);
        }
    }
    log
}

/// Each stream's batches (one seed per key) cover contiguous global indices.
fn contiguous(log: &EmissionLog) {
    for key in 0..3u32 {
        let mut runs: Vec<(u32, u32)> =
            log.batches().iter().filter(|b| b.seed == key && b.interval > 0.0).map(|b| (b.first, b.count)).collect();
        runs.sort_unstable();
        for pair in runs.windows(2) {
            assert_eq!(pair[0].0 + pair[0].1, pair[1].0, "stream {key}: a gap or overlap between segments {pair:?}");
        }
    }
}

fn config() -> ProptestConfig {
    let mut config = ProptestConfig { failure_persistence: None, cases: 64, ..ProptestConfig::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn any_sequence_keeps_the_cap_keeps_the_living_and_keeps_stream_indices_contiguous(
        calls in proptest::collection::vec(call(), 1..60),
        dt in prop_oneof![Just(1.0 / 60.0), 0.01..0.5f64],
    ) {
        let systems = Systems::new(&[2.0, 4.0, 8.0]);
        let a = play(&calls, &systems, dt, true);
        let b = play(&calls, &systems, dt, false);
        prop_assert_eq!(a, b, "the same calls give the same batches");
    }
}

#[test]
fn a_two_hour_stream_stays_at_rate_times_lifetime() {
    let systems = Systems::new(&[8.0]);
    let mut log = EmissionLog::new(EffectLimits::offline());
    let s = spawn(systems.ids[0].0, 1);
    let mut worst: f64 = 0.0;
    for frame in 0..(2 * 3600 * 30) {
        let t = Seconds(f64::from(frame) / 30.0);
        log.stream(StreamKey(0), t, &s, 1_000.0, 8.0, false);
        log.prune(t);
        if frame > 0 && frame % (60 * 30) == 0 {
            let off = (f64::from(log.slots()) - 8_000.0).abs() / 8_000.0;
            worst = worst.max(off);
            assert!(off <= 0.01, "{} slots at minute {}", log.slots(), frame / 1_800);
        }
    }
    assert_eq!(log.batches().len(), 1, "one open segment, its dead prefix pruned every frame");
    let b = log.batches()[0];
    let last = f64::from(2 * 3_600 * 30 - 1) / 30.0;
    let births = (last * 1_000.0).floor() as u32 + 1;
    assert!(
        b.first + b.count >= births - 1 && b.first + b.count <= births + 1,
        "{} births, worst {worst}",
        b.first + b.count
    );
}

#[test]
fn weather_holds_at_its_share_and_a_burst_takes_the_rest() {
    let systems = Systems::new(&[8.0, 3.0]);
    let mut log = EmissionLog::new(EffectLimits::live());
    let weather = spawn(systems.ids[0].0, 1);
    // 10,000 slots asked for: 1,250 births a second for 8 s.
    for frame in 0..(20 * 60) {
        let t = Seconds(f64::from(frame) / 60.0);
        log.stream(StreamKey(0), t, &weather, 1_250.0, 8.0, true);
        log.prune(t);
    }
    assert!((3_480..=3_500).contains(&log.slots()), "weather holds at its share: {}", log.slots());
    let before = log.slots();
    let out = log.burst(Seconds(20.0), &spawn(systems.ids[1].0, 2), 2_000, 3.0);
    assert_eq!(out, BurstOutcome { count: 5_000 - before, thinned: 2_000 - (5_000 - before) });
    assert_eq!(log.slots(), 5_000);
    assert_eq!(log.thinned(), u64::from(out.thinned));
    // At exactly 3,500 weather slots the burst fits as 1,500 and reports 500 thinned.
    let mut exact = EmissionLog::new(EffectLimits::live());
    exact.burst(Seconds(0.0), &weather, 3_500, 8.0);
    assert_eq!(
        exact.burst(Seconds(0.0), &spawn(systems.ids[1].0, 2), 2_000, 3.0),
        BurstOutcome { count: 1_500, thinned: 500 }
    );
    // A full log thins a burst to nothing.
    assert_eq!(exact.burst(Seconds(0.0), &weather, 10, 8.0), BurstOutcome { count: 0, thinned: 10 });
}

#[test]
fn a_rate_change_continues_the_index_with_the_same_seed() {
    let systems = Systems::new(&[4.0]);
    let mut log = EmissionLog::new(EffectLimits::live());
    let s = spawn(systems.ids[0].0, 9);
    for frame in 0..=60 {
        log.stream(StreamKey(3), Seconds(f64::from(frame) / 60.0), &s, 100.0, 4.0, true);
    }
    log.stream(StreamKey(3), Seconds(1.5), &s, 50.0, 4.0, true);
    let [a, b] = log.batches() else { panic!("two segments: {:?}", log.batches()) };
    assert_eq!((a.first, a.count, a.seed), (0, 151, 9), "births at 0, 0.01, ... 1.5");
    assert_eq!((b.first, b.seed), (151, 9), "the next segment continues the index");
    assert!((b.start.0 - 151.0 * f64::from(a.interval)).abs() < 1e-12, "at the next birth time: {}", b.start.0);
    assert!((b.interval - 0.02).abs() < 1e-7);
    // Rate 0 closes it; nothing more is born.
    log.stream(StreamKey(3), Seconds(3.0), &s, 0.0, 4.0, true);
    let slots = log.slots();
    log.prune(Seconds(3.5));
    assert!(log.slots() <= slots);
}

#[test]
fn prewarm_starts_full_and_the_same_every_time() {
    let systems = Systems::new(&[4.0]);
    let s = spawn(systems.ids[0].0, 4);
    let warm = || {
        let mut log = EmissionLog::new(EffectLimits::live());
        log.prewarm(StreamKey(0), Seconds(10.0), &s, 200.0, 4.0, true);
        log.prune(Seconds(10.0));
        log
    };
    let log = warm();
    assert_eq!(log.slots(), 800, "lifetime 4 s at 200 a second, from the first frame");
    assert!(alive(&log, &systems, Seconds(10.0)).len() > 300, "a full sky, not an empty one filling up");
    assert_eq!(log, warm());
}

#[test]
fn clear_shrinks_everything_away_then_empties_the_log() {
    let systems = Systems::new(&[8.0]);
    let mut log = EmissionLog::new(EffectLimits::live());
    let s = spawn(systems.ids[0].0, 1);
    log.burst(Seconds(0.0), &s, 100, 8.0);
    log.stream(StreamKey(0), Seconds(0.0), &s, 100.0, 8.0, true);
    log.stream(StreamKey(0), Seconds(1.0), &s, 100.0, 8.0, true);
    log.clear(Seconds(1.0), 0.5);
    log.prune(Seconds(1.25));
    assert!(log.batches().iter().all(|b| (b.scale - 0.5).abs() < 1e-6), "half way: half the size");
    log.stream(StreamKey(0), Seconds(1.25), &s, 0.0, 8.0, true);
    log.prune(Seconds(1.5));
    assert!(log.batches().is_empty(), "{:?}", log.batches());
    // Births after a clear are not affected by it.
    log.burst(Seconds(1.6), &s, 10, 8.0);
    log.prune(Seconds(2.0));
    assert_eq!(log.slots(), 10);
    assert!((log.batches()[0].scale - 1.0).abs() < f32::EPSILON);
}

#[test]
fn ten_thousand_frames_of_streams_bursts_prune_and_fill_allocate_nothing() {
    let systems = Systems::new(&[3.0, 8.0]);
    let mut log = EmissionLog::new(EffectLimits::live());
    let mut out = Vec::with_capacity(omuzaki_render_api::limits::MAX_SPAWN_BATCHES);
    let snow = Spawn { shape: EmitterShape::rect(Vec2::new(2.0, 1.0)), ..spawn(systems.ids[1].0, 1) };
    let confetti = spawn(systems.ids[0].0, 2);
    let frame = |log: &mut EmissionLog, out: &mut Vec<_>, i: u32| {
        let t = Seconds(f64::from(i) / 60.0);
        let rate = if (i / 600).is_multiple_of(2) { 300.0 } else { 150.0 };
        log.stream(StreamKey(0), t, &snow, rate, 8.0, true);
        if i.is_multiple_of(45) {
            log.burst(t, &Spawn { seed: i, ..confetti }, 150, 3.0);
        }
        if i == 5_000 {
            log.clear(t, 0.3);
        }
        log.prune(t);
        out.clear();
        log.fill(out);
    };
    for i in 0..600 {
        frame(&mut log, &mut out, i);
    }
    let n = allocations(|| {
        for i in 600..10_600 {
            frame(&mut log, &mut out, i);
        }
    });
    assert_eq!(n, 0, "the log allocates nothing after warm-up");
    assert!(!out.is_empty(), "the frame has batches");
}
