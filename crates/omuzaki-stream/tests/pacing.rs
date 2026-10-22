//! Output pacing to `docs/design/supervisor.md` §12: the deadline and wake-up tables, the period's limits, and a
//! presenter driven over 10,000 ticks with scripted present durations and wake-up delays.

#![allow(clippy::unwrap_used)]

use omuzaki_stream::pacing::{FramePeriod, NextTick, Pacer, Wake, next_tick, on_wake};

const MS: u64 = 1_000_000;
/// A 33 ms period: `min_gap` 16.5 ms, `late_limit` 8.25 ms.
fn p33() -> FramePeriod {
    FramePeriod::from_rate(1000, 33).unwrap()
}

#[test]
fn the_period_comes_from_a_rational_rate_within_limits() {
    for (num, den) in [(0, 1), (1, 0), (2000, 1), (1, 2)] {
        assert!(FramePeriod::from_rate(num, den).is_err(), "{num}/{den}");
    }
    assert_eq!(FramePeriod::from_rate(30000, 1001).unwrap().nanos(), 33_366_667);
    assert_eq!(FramePeriod::from_rate(1000, 1).unwrap().nanos(), MS);
    assert_eq!(FramePeriod::from_rate(1, 1).unwrap().nanos(), 1000 * MS);
    let p = p33();
    assert_eq!((p.nanos(), p.min_gap(), p.late_limit()), (33 * MS, 16_500_000, 8_250_000));
}

#[test]
fn the_next_deadline_is_on_the_grid_and_half_a_period_after_the_present() {
    let d = 1_000 * MS;
    let at = |done: u64| next_tick(7, d, p33(), done);
    // (done after D, next deadline after D, missed): the design's table.
    for (done, next, missed) in
        [(3 * MS, 33, 0), (16_500_000, 33, 0), (17 * MS, 66, 1), (32 * MS, 66, 1), (50 * MS, 99, 2)]
    {
        let j = next / 33;
        assert_eq!(at(d + done), NextTick { index: 7 + j, deadline: d + next * MS, missed }, "done D + {done} ns");
    }
    // A present that finished before its own deadline still moves on by one.
    assert_eq!(at(d - 20 * MS), NextTick { index: 8, deadline: d + 33 * MS, missed: 0 });
}

#[test]
fn a_late_wake_up_skips_its_tick() {
    let d = 1_000 * MS;
    let p = p33();
    let wake = |late: u64| on_wake(3, d, p, d + late);
    assert_eq!(wake(0), Wake::Present);
    assert_eq!(wake(p.late_limit() - 1), Wake::Present);
    assert_eq!(wake(p.late_limit()), Wake::Skip(NextTick { index: 4, deadline: d + 33 * MS, missed: 1 }));
    assert_eq!(wake(33 * MS), Wake::Skip(NextTick { index: 5, deadline: d + 66 * MS, missed: 2 }));
    assert_eq!(wake(66 * MS), Wake::Skip(NextTick { index: 6, deadline: d + 99 * MS, missed: 3 }));
}

/// A seeded xorshift: the same script every run.
struct Script(u64);

impl Script {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

/// One present: its grid index and time, start and completion.
#[derive(Clone, Copy, Debug)]
struct Present {
    index: u64,
    grid: u64,
    start: u64,
    done: u64,
}

/// Runs a pacer for `ticks` wake-ups, with present durations below `max_duration` and wake-up delays below
/// `max_delay`.
fn run(ticks: usize, max_duration: u64, max_delay: u64, seed: u64) -> (Pacer, Vec<Present>) {
    let mut pacer = Pacer::new(p33(), 5_000 * MS);
    let mut script = Script(seed);
    let mut presents = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let woke = pacer.deadline() + script.below(max_delay);
        if let Some((index, grid)) = pacer.woke(woke) {
            let done = woke + script.below(max_duration);
            pacer.presented(done);
            presents.push(Present { index, grid, start: woke, done });
        }
    }
    (pacer, presents)
}

#[test]
fn presents_keep_their_spacing_under_any_durations_and_wake_ups() {
    let p = p33();
    let (pacer, presents) = run(10_000, 5 * p.nanos() / 2, 3 * p.nanos() / 2, 0x9E37_79B9_7F4A_7C15);
    assert!(presents.len() > 1_000, "{} presents", presents.len());
    for x in &presents {
        assert!(x.start - x.grid < p.late_limit(), "{x:?} started late");
    }
    for w in presents.windows(2) {
        let (a, b) = (w[0], w[1]);
        assert!(b.start >= a.done + p.min_gap(), "{b:?} too soon after {a:?} finished");
        assert!(b.start - a.start >= p.nanos() - p.late_limit(), "{a:?} then {b:?}");
        assert!(b.grid - a.grid >= p.nanos() && b.index > a.index, "{a:?} then {b:?}");
    }
    // Every grid point is either presented or counted as missed.
    assert_eq!(pacer.index(), pacer.presented_count() + pacer.missed());
    assert!(pacer.missed() > 0);
}

#[test]
fn a_well_behaved_presenter_misses_nothing_at_the_exact_rate() {
    let p = p33();
    let (pacer, presents) = run(10_000, p.min_gap() - p.late_limit(), p.late_limit(), 42);
    assert_eq!(pacer.missed(), 0);
    assert_eq!(presents.len(), 10_000);
    assert!(presents.iter().zip(0_u64..).all(|(x, k)| x.index == k && x.grid == 5_000 * MS + k * p.nanos()));
}
