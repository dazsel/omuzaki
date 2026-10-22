//! Output pacing (`docs/design/supervisor.md` §12): the presenter's cadence, which Spout, NDI, the virtual camera and
//! the encoders' frame feed all tick from.
//!
//! - **A grid, not a chain:** deadlines stay on `origin + k × period`, and each present carries its grid index `k`, so
//!   receivers and encoders see an exact cadence and `pts` follows `k`.
//! - **The next deadline is chosen after the present completes** ([`next_tick`]): the first grid point at least half a
//!   period after `done`; the grid points skipped are counted as missed.
//! - **A late wake-up skips its tick** ([`on_wake`]): one more than a quarter period past its deadline presents
//!   nothing, counts the tick (and every one it slept through) as missed, and waits for the next grid point.
//! - **The period comes from a rational rate** ([`FramePeriod::from_rate`]) and is between 1 ms and 1 s.
//!
//! Every function here is pure (times are `host_now` nanoseconds passed in); [`Pacer`] holds the state between two
//! ticks.

use std::time::Duration;

use omuzaki_stream_api::error::StreamError;

/// The shortest frame period.
pub const MIN_PERIOD: Duration = Duration::from_millis(1);
/// The longest frame period.
pub const MAX_PERIOD: Duration = Duration::from_secs(1);

/// One output frame period, from a rational rate; at least 1 ms, at most 1 s.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FramePeriod {
    nanos: u64,
}

impl FramePeriod {
    /// The period of `num / den` frames a second, rounded to the nearest nanosecond.
    ///
    /// # Errors
    /// [`StreamError::Config`] if `num` or `den` is 0 or the period is outside `MIN_PERIOD..=MAX_PERIOD`.
    pub fn from_rate(num: u32, den: u32) -> Result<Self, StreamError> {
        if num == 0 || den == 0 {
            return Err(StreamError::Config(format!("a frame rate of {num}/{den}")));
        }
        let nanos = (u128::from(den) * 1_000_000_000 + u128::from(num) / 2) / u128::from(num);
        let (min, max) = (MIN_PERIOD.as_nanos(), MAX_PERIOD.as_nanos());
        if !(min..=max).contains(&nanos) {
            return Err(StreamError::Config(format!("a frame rate of {num}/{den} has a period outside 1 ms to 1 s")));
        }
        Ok(Self { nanos: u64::try_from(nanos).unwrap_or(u64::MAX) })
    }

    /// The period in nanoseconds.
    pub fn nanos(self) -> u64 {
        self.nanos
    }

    /// The least time between a present's completion and the next present's deadline: half a period, rounded up.
    pub fn min_gap(self) -> u64 {
        self.nanos.div_ceil(2)
    }

    /// How late a wake-up may be and still present: a quarter period, rounded down.
    pub fn late_limit(self) -> u64 {
        self.nanos / 4
    }
}

/// The next tick: its grid index, its deadline and the grid points skipped to reach it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NextTick {
    /// The grid index.
    pub index: u64,
    /// When it is due.
    pub deadline: u64,
    /// Grid points skipped.
    pub missed: u64,
}

/// The tick after the one due at grid point `index` (time `deadline`), given that its output work finished at `done`:
/// the first grid point after `deadline` that is at least `min_gap` after `done`.
pub fn next_tick(index: u64, deadline: u64, period: FramePeriod, done: u64) -> NextTick {
    let p = period.nanos();
    let need = done.saturating_add(period.min_gap()).saturating_sub(deadline);
    let j = need.div_ceil(p).max(1);
    NextTick { index: index.saturating_add(j), deadline: deadline.saturating_add(j.saturating_mul(p)), missed: j - 1 }
}

/// What to do on waking for a tick.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Present this tick.
    Present,
    /// Too late: skip to this tick.
    Skip(NextTick),
}

/// What to do on waking at `woke` for the tick due at grid point `index` (time `deadline`): present if less than a
/// quarter period late, else skip to the next grid point after `woke`, counting every one slept through.
pub fn on_wake(index: u64, deadline: u64, period: FramePeriod, woke: u64) -> Wake {
    if woke < deadline.saturating_add(period.late_limit()) {
        return Wake::Present;
    }
    let p = period.nanos();
    let j = (woke - deadline) / p + 1;
    Wake::Skip(NextTick {
        index: index.saturating_add(j),
        deadline: deadline.saturating_add(j.saturating_mul(p)),
        missed: j,
    })
}

/// The presenter's state between ticks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Pacer {
    period: FramePeriod,
    index: u64,
    deadline: u64,
    missed: u64,
    presented: u64,
}

impl Pacer {
    /// Ticks of `period` from grid point 0 at `origin`.
    pub fn new(period: FramePeriod, origin: u64) -> Self {
        Self { period, index: 0, deadline: origin, missed: 0, presented: 0 }
    }

    /// When the next tick is due: sleep until then.
    pub fn deadline(&self) -> u64 {
        self.deadline
    }

    /// The next tick's grid index.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// The period.
    pub fn period(&self) -> FramePeriod {
        self.period
    }

    /// Woke at `woke`: `Some((index, grid time))` to present now, or `None` when it was too late (the tick is counted
    /// as missed and [`deadline`](Self::deadline) has moved on).
    pub fn woke(&mut self, woke: u64) -> Option<(u64, u64)> {
        match on_wake(self.index, self.deadline, self.period, woke) {
            Wake::Present => Some((self.index, self.deadline)),
            Wake::Skip(next) => {
                (self.index, self.deadline) = (next.index, next.deadline);
                self.missed = self.missed.saturating_add(next.missed);
                None
            }
        }
    }

    /// The present finished at `done`: picks the next deadline.
    pub fn presented(&mut self, done: u64) {
        let next = next_tick(self.index, self.deadline, self.period, done);
        (self.index, self.deadline) = (next.index, next.deadline);
        self.missed = self.missed.saturating_add(next.missed);
        self.presented = self.presented.saturating_add(1);
    }

    /// Ticks skipped so far (shown as `OutputStatus.dropped`).
    pub fn missed(&self) -> u64 {
        self.missed
    }

    /// Ticks presented so far.
    pub fn presented_count(&self) -> u64 {
        self.presented
    }
}
