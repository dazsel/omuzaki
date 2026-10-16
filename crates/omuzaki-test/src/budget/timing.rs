//! Per-system frame timings: systems call a span around their work and the harness keeps the samples.
//!
//! All allocation happens at setup ([`Recorder::register`]); recording a frame is a bounds check, one `push` into a
//! buffer reserved up front and, for a span guard, two reads of the monotonic clock. The clock is read here only, so a
//! system under test never sees a wall clock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::error::BudgetError;

/// Hands out one identity per [`Recorder`], so a handle can be checked against the recorder it is used on.
static NEXT_RECORDER: AtomicU64 = AtomicU64::new(0);

/// Handle of a registered span, valid only for the [`Recorder`] that issued it: using it on another recorder is an
/// error and records nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanId {
    recorder: u64,
    index: usize,
}

/// One name's samples, in nanoseconds.
#[derive(Debug)]
struct Track {
    name: String,
    samples: Vec<u64>,
    /// Samples refused because the buffer was full; a report prints it so a truncated run is never silent.
    dropped: u64,
}

/// Collects named timing spans without allocating per frame.
#[derive(Debug)]
pub struct Recorder {
    identity: u64,
    capacity: usize,
    tracks: Vec<Track>,
}

/// A running measurement; end it with [`Span::end`]. Dropping it unended records nothing.
#[must_use = "a span records only when ended"]
#[derive(Debug)]
pub struct Span {
    id: SpanId,
    start: Instant,
}

impl Recorder {
    /// A recorder keeping up to `capacity` samples per span.
    pub fn new(capacity: usize) -> Self {
        Self { identity: NEXT_RECORDER.fetch_add(1, Ordering::Relaxed), capacity, tracks: Vec::new() }
    }

    /// Register a span (for example `"physics.step"`; use the §10 budget key to get it judged) and reserve its buffer.
    ///
    /// # Errors
    /// [`BudgetError::Span`] when `name` is empty or already registered.
    pub fn register(&mut self, name: &str) -> Result<SpanId, BudgetError> {
        if name.is_empty() || self.tracks.iter().any(|t| t.name == name) {
            return Err(BudgetError::Span(format!("`{name}` is empty or already registered")));
        }
        self.tracks.push(Track { name: name.to_owned(), samples: Vec::with_capacity(self.capacity), dropped: 0 });
        Ok(SpanId { recorder: self.identity, index: self.tracks.len() - 1 })
    }

    /// Start timing a span.
    pub fn span(&self, id: SpanId) -> Span {
        Span { id, start: Instant::now() }
    }

    /// Record a duration measured elsewhere, in nanoseconds. A full buffer counts a drop instead of growing.
    ///
    /// # Errors
    /// [`BudgetError::Span`] when `id` was not issued by this recorder; nothing is recorded then.
    pub fn record_ns(&mut self, id: SpanId, ns: u64) -> Result<(), BudgetError> {
        let capacity = self.capacity;
        let track = match self.tracks.get_mut(id.index) {
            Some(track) if id.recorder == self.identity => track,
            _ => return Err(BudgetError::Span("span handle was not issued by this recorder".to_owned())),
        };
        if track.samples.len() < capacity {
            track.samples.push(ns);
        } else {
            track.dropped += 1;
        }
        Ok(())
    }

    /// Finish a span started by [`Recorder::span`] and record its elapsed time.
    ///
    /// # Errors
    /// As [`Recorder::record_ns`].
    #[allow(clippy::needless_pass_by_value)] // consuming the guard is what stops a span being ended twice
    pub fn end(&mut self, span: Span) -> Result<(), BudgetError> {
        let ns = u64::try_from(span.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.record_ns(span.id, ns)
    }

    /// Every span as `(name, samples in ns, dropped)`, in registration order.
    pub fn tracks(&self) -> impl Iterator<Item = (&str, &[u64], u64)> {
        self.tracks.iter().map(|t| (t.name.as_str(), t.samples.as_slice(), t.dropped))
    }
}

/// Phases of a start-up, timed one after the other from the moment it is created.
#[derive(Debug)]
pub struct StartupTimer {
    begin: Instant,
    last: Instant,
    phases: Vec<(String, u64)>,
}

impl StartupTimer {
    /// Start the clock.
    pub fn begin() -> Self {
        let now = Instant::now();
        Self { begin: now, last: now, phases: Vec::new() }
    }

    /// Close the current phase under `name` (for example `"device"`, `"pipelines"`, `"first_frame"`).
    pub fn phase(&mut self, name: &str) {
        let now = Instant::now();
        self.phases.push((name.to_owned(), nanos(now.duration_since(self.last))));
        self.last = now;
    }

    /// The phases in order, in nanoseconds.
    pub fn phases(&self) -> &[(String, u64)] {
        &self.phases
    }

    /// Nanoseconds from [`StartupTimer::begin`] to the last phase end.
    pub fn total_ns(&self) -> u64 {
        nanos(self.last.duration_since(self.begin))
    }
}

fn nanos(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_and_unknown_spans_are_errors() {
        let mut a = Recorder::new(4);
        a.register("x").unwrap();
        assert!(a.register("x").is_err());
        assert!(a.register("").is_err());
    }

    #[test]
    fn a_handle_from_another_recorder_fails_closed_and_records_nothing() {
        let mut a = Recorder::new(4);
        let mut b = Recorder::new(4);
        let from_a = a.register("x").unwrap();
        let from_b = b.register("x").unwrap();
        assert!(b.record_ns(from_a, 1).is_err());
        let span = a.span(from_b);
        assert!(a.end(span).is_err());
        assert!(a.tracks().chain(b.tracks()).all(|(_, samples, dropped)| samples.is_empty() && dropped == 0));
        assert!(a.record_ns(from_a, 1).is_ok() && b.record_ns(from_b, 1).is_ok());
    }

    #[test]
    fn full_buffer_counts_drops_and_never_reallocates() {
        let mut r = Recorder::new(3);
        let id = r.register("x").unwrap();
        let before = r.tracks[0].samples.capacity();
        for ns in 0..5 {
            r.record_ns(id, ns).unwrap();
        }
        let (name, samples, dropped) = r.tracks().next().unwrap();
        assert_eq!((name, samples, dropped), ("x", &[0, 1, 2][..], 2));
        assert_eq!(r.tracks[0].samples.capacity(), before);
    }

    #[test]
    fn a_span_records_at_least_the_time_it_covered() {
        let mut r = Recorder::new(2);
        let id = r.register("x").unwrap();
        let span = r.span(id);
        std::thread::sleep(std::time::Duration::from_millis(2));
        r.end(span).unwrap();
        assert!(r.tracks().next().unwrap().1[0] >= 2_000_000);
    }

    #[test]
    fn startup_phases_add_up_in_order() {
        let mut t = StartupTimer::begin();
        t.phase("a");
        t.phase("b");
        let names: Vec<_> = t.phases().iter().map(|p| p.0.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(t.total_ns() >= t.phases().iter().map(|p| p.1).sum::<u64>());
    }
}
