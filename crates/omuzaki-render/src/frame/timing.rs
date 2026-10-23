//! GPU time per pass, from timestamp queries.
//!
//! The frame graph writes one timestamp before and one after each pass on the frame's encoder, so a pass may record any
//! number of render or compute passes and still gets exactly one duration, with no help from the pass itself. A GPU
//! painter's commands (`paint_texture` through `native()`, before the frame) are bracketed the same way on the encoder
//! the renderer lends it, and their durations count towards `PaintTextures`, the face-paint row of the budget. Results
//! arrive a frame or two later: the queries are resolved into one of a few read-back buffers at the end of the frame,
//! and mapped asynchronously; [`GpuTimer::poll`] collects whichever have finished, so the frame loop never waits.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use omuzaki_core::time::Seconds;
use omuzaki_render_api::{GpuTimings, Pass, PassTimes};

/// What a pair of timestamps brackets: a pass, or the step that adds a supersampled round into the average (which the
/// report counts towards `Resolve`, the pass that turns the average into the picture).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    Pass(Pass),
    Accumulate,
}

impl Span {
    /// The pair's place in a round's block. The spans a later round repeats come first (the geometry passes, then the
    /// accumulation), so a later round's written queries are one run from the start of its block.
    fn slot(self) -> u32 {
        match self {
            Self::Pass(Pass::Opaque) => 0,
            Self::Pass(Pass::Outlines) => 1,
            Self::Pass(Pass::Particles) => 2,
            Self::Pass(Pass::Transparent) => 3,
            Self::Accumulate => 4,
            Self::Pass(Pass::PaintTextures) => 5,
            Self::Pass(Pass::Shadows) => 6,
            Self::Pass(Pass::Bloom) => 7,
            Self::Pass(Pass::DepthOfField) => 8,
            Self::Pass(Pass::Resolve) => 9,
        }
    }
}

/// Pairs a round may write: the nine passes and the accumulation.
const SPANS: u32 = 10;
/// Pairs a round after the first writes: the four geometry passes and the accumulation.
const REPEATED: u32 = 5;
/// Queries per round's block: one 256-byte resolve boundary, since each block is resolved where it sits.
const BLOCK: u32 = 32;
/// Rounds of a supersampled frame that are timed: all of them (`supersample::MAX_ROUNDS`).
const ROUNDS: u32 = super::supersample::MAX_ROUNDS;
/// Every round's block.
const PASS_QUERIES: u32 = BLOCK * ROUNDS;
/// GPU paints timed per frame (a face repaints at most its three layers); paints beyond this go unmeasured.
const PAINTS: u32 = 8;
/// Where the paints' timestamps start, in the query set and (times 8 bytes) in the read-back: query results are resolved
/// to 256-byte aligned offsets, so the paints' range starts on the next one after the passes'.
const PAINT_BASE: u32 = PASS_QUERIES;
/// Every query: the rounds' blocks, then the paints' pairs.
const QUERIES: u32 = PAINT_BASE + 2 * PAINTS;
// A block holds a round's pairs and starts on a resolve boundary, and so do the paints, or `resolve` would overlap or be
// refused.
const _: () = assert!(
    2 * SPANS <= BLOCK
        && (BLOCK as u64 * size_of::<u64>() as u64).is_multiple_of(wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT)
        && PAINT_BASE >= PASS_QUERIES
        && (PAINT_BASE as u64 * size_of::<u64>() as u64).is_multiple_of(wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT)
);

/// The query of `span`'s timestamp in `round`: before (`end` false) or after.
fn query(round: u32, span: Span, end: bool) -> u32 {
    BLOCK * round + 2 * span.slot() + u32::from(end)
}

/// Read-back buffers in rotation. The GPU runs at most a couple of frames behind; if all are busy a frame goes
/// unmeasured rather than stalling.
const SLOTS: usize = 3;

/// What a read-back slot is doing.
#[derive(Copy, Clone)]
enum State {
    Idle,
    /// Resolved this frame; waiting for the submit so mapping can be requested.
    Copied(Measured),
    /// Mapping requested; done when `ready` is set (by a callback that runs during device polling).
    Mapping(Measured),
}

/// What a read-back slot will hold once the GPU is done with it.
#[derive(Copy, Clone)]
struct Measured {
    /// Position among the submissions that were measured: the order results are ranked in.
    sequence: u64,
    /// The caller's frame index, reported as a label.
    frame: u64,
    ran: [bool; Pass::COUNT],
    /// How many paints were timed before the frame.
    paints: u32,
    /// How many rounds the frame drew.
    rounds: u32,
}

/// The newest measurement seen, ranked by submission order.
#[derive(Default)]
struct Newest {
    sequence: u64,
    timings: Option<GpuTimings>,
}

impl Newest {
    /// Keeps `timings` unless a later submission's have already arrived (read-backs can finish out of order).
    fn offer(&mut self, sequence: u64, timings: GpuTimings) {
        if self.timings.is_none() || self.sequence <= sequence {
            (self.sequence, self.timings) = (sequence, Some(timings));
        }
    }
}

struct Slot {
    buffer: wgpu::Buffer,
    state: State,
    ready: Arc<AtomicBool>,
}

/// Measures GPU time per pass. Absent (`None` from [`GpuTimer::new`]) when the device lacks the timestamp features.
pub(crate) struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: [Slot; SLOTS],
    /// Nanoseconds per timestamp tick.
    period_ns: f32,
    /// Submissions measured so far; the next one gets this number.
    submissions: u64,
    /// Paints timed since the last frame was resolved.
    paints: u32,
    newest: Newest,
}

const BYTES: u64 = QUERIES as u64 * size_of::<u64>() as u64;

impl GpuTimer {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, enabled: bool) -> Option<Self> {
        if !enabled {
            return None;
        }
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        let slot = || Slot {
            buffer: buffer("timestamps read", wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ),
            state: State::Idle,
            ready: Arc::new(AtomicBool::new(false)),
        };
        Some(Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("pass timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: QUERIES,
            }),
            resolve: buffer("timestamps resolve", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            slots: [slot(), slot(), slot()],
            period_ns: queue.get_timestamp_period(),
            submissions: 0,
            paints: 0,
            newest: Newest::default(),
        })
    }

    /// Writes the timestamp before (`end` false) or after (`end` true) `span` in `round`. Every round writes its
    /// accumulation's pair, even a frame of one round (where it brackets nothing), so each block's written queries are
    /// one run.
    pub(crate) fn write(&self, encoder: &mut wgpu::CommandEncoder, round: u32, span: Span, end: bool) {
        encoder.write_timestamp(&self.queries, query(round.min(ROUNDS - 1), span, end));
    }

    /// Writes the timestamp before a GPU paint's commands on the encoder lent to it, and returns the query its end goes
    /// to ([`end_paint`](Self::end_paint)); `None` once this frame's [`PAINTS`] are used up (that paint is not timed).
    pub(crate) fn begin_paint(&mut self, encoder: &mut wgpu::CommandEncoder) -> Option<u32> {
        if self.paints >= PAINTS {
            return None;
        }
        let begin = PAINT_BASE + 2 * self.paints;
        self.paints += 1;
        encoder.write_timestamp(&self.queries, begin);
        Some(begin + 1)
    }

    /// Writes the timestamp after a GPU paint's commands: `end` is what [`begin_paint`](Self::begin_paint) returned.
    pub(crate) fn end_paint(&self, encoder: &mut wgpu::CommandEncoder, end: u32) {
        encoder.write_timestamp(&self.queries, end);
    }

    /// Gives back the pair [`begin_paint`](Self::begin_paint) handed out for `end`: that paint's commands were dropped,
    /// so nothing writes the pair. Paints run one at a time, so the pair is the last one handed out.
    pub(crate) fn cancel_paint(&mut self, end: u32) {
        debug_assert_eq!(end, PAINT_BASE + 2 * self.paints - 1, "a paint's pair is the last one handed out");
        self.paints = self.paints.saturating_sub(1);
    }

    /// How many paints are timed since the last frame was resolved.
    #[cfg(test)]
    pub(crate) fn paints_timed(&self) -> u32 {
        self.paints
    }

    /// Resolves this frame's timestamps (and those of the paints since the last frame) into a free read-back slot,
    /// labelled `frame`. Returns whether it did; if every slot is still waiting on the GPU, the frame goes unmeasured.
    ///
    /// Only the queries this frame wrote are resolved: all of the first round's block, and the repeated spans of the
    /// rest.
    pub(crate) fn resolve(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
        ran: [bool; Pass::COUNT],
        rounds: u32,
    ) -> bool {
        let paints = std::mem::take(&mut self.paints);
        let rounds = rounds.clamp(1, ROUNDS);
        let Some(slot) = self.slots.iter_mut().find(|s| matches!(s.state, State::Idle)) else { return false };
        encoder.resolve_query_set(&self.queries, 0..2 * SPANS, &self.resolve, 0);
        for round in 1..rounds {
            let first = BLOCK * round;
            let offset = u64::from(first) * size_of::<u64>() as u64;
            encoder.resolve_query_set(&self.queries, first..first + 2 * REPEATED, &self.resolve, offset);
        }
        // Only the paints' queries written since the last frame: the others hold nothing this frame wrote.
        if paints > 0 {
            let offset = u64::from(PAINT_BASE) * size_of::<u64>() as u64;
            encoder.resolve_query_set(&self.queries, PAINT_BASE..PAINT_BASE + 2 * paints, &self.resolve, offset);
        }
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &slot.buffer, 0, BYTES);
        slot.state = State::Copied(Measured { sequence: self.submissions, frame, ran, paints, rounds });
        self.submissions += 1;
        true
    }

    /// Asks for the slots resolved this frame to be mapped. Call after the frame's commands are submitted.
    pub(crate) fn request_maps(&mut self) {
        for slot in &mut self.slots {
            if let State::Copied(measured) = slot.state {
                let ready = Arc::clone(&slot.ready);
                slot.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| ready.store(result.is_ok(), Ordering::Release));
                slot.state = State::Mapping(measured);
            }
        }
    }

    /// Collects every finished measurement and returns the newest one seen so far (by submission, not by frame index).
    pub(crate) fn poll(&mut self) -> Option<GpuTimings> {
        for slot in &mut self.slots {
            let State::Mapping(Measured { sequence, frame, ran, paints, rounds }) = slot.state else { continue };
            if !slot.ready.swap(false, Ordering::AcqRel) {
                continue;
            }
            if let Ok(data) = slot.buffer.slice(..).get_mapped_range() {
                let ticks: &[u64] = bytemuck::cast_slice(&data);
                self.newest.offer(sequence, timings_from(ticks, self.period_ns, frame, ran, paints, rounds));
            }
            slot.buffer.unmap();
            slot.state = State::Idle;
        }
        self.newest.timings
    }
}

/// Turns raw tick pairs into per-pass durations; passes that did not run stay `None`. A pass that repeats in a
/// supersampled frame adds up its `rounds`, and `Resolve` adds the rounds' accumulation. The first `paints` paint spans
/// count towards `PaintTextures`.
fn timings_from(
    ticks: &[u64],
    period_ns: f32,
    frame: u64,
    ran: [bool; Pass::COUNT],
    paints: u32,
    rounds: u32,
) -> GpuTimings {
    // Timestamps are counters of a few seconds at most, so the f64 conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    let seconds = |begin: u64, end: u64| end.saturating_sub(begin) as f64 * f64::from(period_ns) * 1e-9;
    let span = |round: u32, span: Span| {
        let at = |end| ticks.get(query(round, span, end) as usize).copied().unwrap_or(0);
        seconds(at(false), at(true))
    };
    let rounds = rounds.clamp(1, ROUNDS);
    let mut passes = PassTimes::default();
    for pass in Pass::ALL.into_iter().filter(|p| ran[p.index()]) {
        let repeated = if super::supersample::repeats(pass) { rounds } else { 1 };
        let mut total: f64 = (0..repeated).map(|round| span(round, Span::Pass(pass))).sum();
        if pass == Pass::Resolve && rounds > 1 {
            total += (0..rounds).map(|round| span(round, Span::Accumulate)).sum::<f64>();
        }
        passes.set(pass, Seconds(total));
    }
    let mut painters = None;
    if let Some(Seconds(own)) = passes.get(Pass::PaintTextures) {
        let base = PAINT_BASE as usize;
        let painted: f64 = (0..paints as usize).map(|i| seconds(ticks[base + 2 * i], ticks[base + 2 * i + 1])).sum();
        passes.set(Pass::PaintTextures, Seconds(own + painted));
        painters = (paints > 0).then_some(Seconds(painted));
    }
    GpuTimings { frame, passes, painters }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_come_from_tick_pairs_and_skip_passes_that_did_not_run() {
        let mut ticks = [0u64; QUERIES as usize];
        set(&mut ticks, 0, Span::Pass(Pass::Opaque), (1_000, 3_000));
        set(&mut ticks, 0, Span::Pass(Pass::Resolve), (3_000, 3_500));
        let mut ran = [false; Pass::COUNT];
        (ran[Pass::Opaque.index()], ran[Pass::Resolve.index()]) = (true, true);

        let t = timings_from(&ticks, 2.0, 9, ran, 0, 1);
        assert_eq!(t.frame, 9);
        assert_eq!(t.passes.get(Pass::Opaque), Some(Seconds(4_000.0 * 1e-9)), "2000 ticks at 2 ns");
        assert_eq!(t.passes.get(Pass::Resolve), Some(Seconds(1_000.0 * 1e-9)));
        assert_eq!(t.passes.get(Pass::Bloom), None);
    }

    fn set(ticks: &mut [u64], round: u32, span: Span, (begin, end): (u64, u64)) {
        (ticks[query(round, span, false) as usize], ticks[query(round, span, true) as usize]) = (begin, end);
    }

    #[test]
    fn a_supersampled_frame_adds_up_its_rounds_and_resolve_takes_the_accumulation() {
        let mut ticks = [0u64; QUERIES as usize];
        for round in 0..4 {
            set(&mut ticks, round, Span::Pass(Pass::Opaque), (0, 100));
            set(&mut ticks, round, Span::Accumulate, (0, 10));
            // Left over from an earlier frame in a block this frame did not reach: never counted.
            set(&mut ticks, round + 4, Span::Pass(Pass::Opaque), (0, 1_000_000));
        }
        set(&mut ticks, 0, Span::Pass(Pass::Bloom), (0, 50));
        // A once-per-frame pass's pair in a later block is stale: only the first round's counts.
        set(&mut ticks, 1, Span::Pass(Pass::Bloom), (0, 1_000_000));
        set(&mut ticks, 0, Span::Pass(Pass::Resolve), (0, 20));
        let mut ran = [false; Pass::COUNT];
        for pass in [Pass::Opaque, Pass::Bloom, Pass::Resolve] {
            ran[pass.index()] = true;
        }
        let t = timings_from(&ticks, 1.0, 0, ran, 0, 4);
        let ns = |pass| t.passes.get(pass).map(|s| (s.0 * 1e9).round());
        assert_eq!(ns(Pass::Opaque), Some(400.0), "four rounds of 100");
        assert_eq!(ns(Pass::Bloom), Some(50.0));
        assert_eq!(ns(Pass::Resolve), Some(60.0), "its own 20 and four accumulations of 10");
        let single = timings_from(&ticks, 1.0, 0, ran, 0, 1);
        assert_eq!(single.passes.get(Pass::Resolve).map(|s| (s.0 * 1e9).round()), Some(20.0), "one round adds nothing");
    }

    #[test]
    fn every_span_has_its_own_pair_and_the_repeated_ones_lead_each_block() {
        let mut seen = std::collections::HashSet::new();
        for span in Pass::ALL.map(Span::Pass).into_iter().chain([Span::Accumulate]) {
            assert!(span.slot() < SPANS);
            assert!(seen.insert(span.slot()), "{span:?} shares a slot");
            let repeated = matches!(span, Span::Accumulate)
                || matches!(span, Span::Pass(p) if super::super::supersample::repeats(p));
            assert_eq!(span.slot() < REPEATED, repeated, "{span:?}");
        }
    }

    fn labelled(frame: u64) -> GpuTimings {
        GpuTimings { frame, passes: PassTimes::default(), painters: None }
    }

    #[test]
    fn the_newest_submission_wins_even_when_the_frame_index_goes_backwards() {
        // A looping timeline: frames 1000..=1014, then back to 0..=14. Submissions are numbered 0..30 in order.
        let mut newest = Newest::default();
        for (sequence, frame) in (0..).zip((1000..1015).chain(0..15)) {
            newest.offer(sequence, labelled(frame));
            assert_eq!(newest.timings.map(|t| t.frame), Some(frame), "submission {sequence} is the newest so far");
        }
    }

    #[test]
    fn a_read_back_that_finishes_late_does_not_replace_a_newer_one() {
        let mut newest = Newest::default();
        newest.offer(5, labelled(50));
        newest.offer(4, labelled(40));
        assert_eq!(newest.timings.map(|t| t.frame), Some(50));
        newest.offer(6, labelled(7));
        assert_eq!(newest.timings.map(|t| t.frame), Some(7), "a lower label from a later submission is still newer");
    }

    #[test]
    fn the_paints_timed_before_a_frame_count_towards_paint_textures() {
        let mut ticks = [0u64; QUERIES as usize];
        let paint = Pass::PaintTextures.index();
        set(&mut ticks, 0, Span::Pass(Pass::PaintTextures), (10_000, 10_100));
        let base = PAINT_BASE as usize;
        (ticks[base], ticks[base + 1]) = (1_000, 1_400);
        (ticks[base + 2], ticks[base + 3]) = (2_000, 2_250);
        // A third pair the frame did not write: it must not count.
        (ticks[base + 4], ticks[base + 5]) = (0, 1_000_000);
        let mut ran = [false; Pass::COUNT];
        ran[paint] = true;
        let t = timings_from(&ticks, 1.0, 3, ran, 2, 1).passes.get(Pass::PaintTextures).map(|s| s.0);
        assert!(t.is_some_and(|t| (t - 750e-9).abs() < 1e-15), "the mips' 100 ticks and the two paints' 650: {t:?}");
        let painters = timings_from(&ticks, 1.0, 3, ran, 2, 1).painters.map(|s| s.0);
        assert!(painters.is_some_and(|p| (p - 650e-9).abs() < 1e-15), "the painters' share alone: {painters:?}");
        ran[paint] = false;
        assert_eq!(timings_from(&ticks, 1.0, 3, ran, 2, 1).passes.get(Pass::PaintTextures), None, "no paint, no row");
    }

    #[test]
    fn a_counter_that_goes_backwards_reads_as_zero_not_a_huge_time() {
        let mut ticks = [0u64; QUERIES as usize];
        set(&mut ticks, 0, Span::Pass(Pass::PaintTextures), (500, 100));
        let mut ran = [false; Pass::COUNT];
        ran[0] = true;
        assert_eq!(timings_from(&ticks, 1.0, 0, ran, 0, 1).passes.get(Pass::PaintTextures), Some(Seconds(0.0)));
    }
}
