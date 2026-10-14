//! A deterministic stream: the reference for the [`BlockSink`] / [`BlockSource`] contract that consumers (the voice
//! conversion stage, streaming speech to text, voice to face) test their fallback against.
//!
//! It runs the session **inside `push`**, so there is no worker thread and no timing: a block is late only when the
//! test says so ([`InlineStream::late_when`]). The real stream host in `omuzaki-ai` runs a worker and must give the same
//! answers for the same pattern of late blocks. Both ends share state behind a mutex, which is fine for a mock (it
//! is never contended); the real host may not lock.

use std::sync::{Arc, Mutex, MutexGuard};

use omuzaki_ai_api::{
    Block, BlockSink, BlockSource, Port, RunError, Session, StreamSpec, StreamStats, TensorBuf, TensorProblem,
    TensorRef,
};

struct State {
    session: Box<dyn Session>,
    spec: StreamSpec,
    queue: u32,
    stall_blocks: u32,
    late: fn(u64) -> bool,
    /// Results by `seq % slots.len()`, tagged with their `seq` when ready.
    slots: Vec<(Option<u64>, Vec<TensorBuf>)>,
    next_seq: u64,
    missed_in_a_row: u32,
    stats: StreamStats,
}

/// Builds the two ends of an inline stream; see the module docs.
pub struct InlineStream;

impl InlineStream {
    /// A stream over `session` with the model's `spec`, `queue` blocks in flight and a stall after `stall_blocks`
    /// missed blocks in a row. No block is late.
    pub fn open(
        session: Box<dyn Session>,
        spec: StreamSpec,
        queue: u32,
        stall_blocks: u32,
    ) -> (Box<dyn BlockSink>, Box<dyn BlockSource>) {
        Self::late_when(session, spec, queue, stall_blocks, |_| false)
    }

    /// As [`InlineStream::open`], with the blocks for which `late(seq)` is true finishing after their deadline.
    pub fn late_when(
        session: Box<dyn Session>,
        spec: StreamSpec,
        queue: u32,
        stall_blocks: u32,
        late: fn(u64) -> bool,
    ) -> (Box<dyn BlockSink>, Box<dyn BlockSource>) {
        let slots = (0..=queue).map(|_| (None, TensorBuf::for_outputs(session.signature()))).collect();
        let state = State {
            session,
            spec,
            queue,
            stall_blocks,
            late,
            slots,
            next_seq: 0,
            missed_in_a_row: 0,
            stats: StreamStats::default(),
        };
        let shared = Arc::new(Mutex::new(state));
        (Box::new(Sink(Arc::clone(&shared))), Box::new(Source(shared)))
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panic while holding the lock can only come from a test assertion; keep going with the data as it is.
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Sink(Arc<Mutex<State>>);
struct Source(Arc<Mutex<State>>);

impl State {
    fn check_block(&self, inputs: &[TensorRef<'_>]) -> Result<(), RunError> {
        self.session.signature().check_inputs(inputs)?;
        let block = usize::try_from(self.spec.block).unwrap_or(usize::MAX);
        match inputs.first().map(|x| x.shape) {
            Some([n]) if *n == block => Ok(()),
            _ => {
                let found = inputs.first().and_then(|x| x.shape.first().copied()).unwrap_or(0);
                let problem = TensorProblem::Dim { axis: 0, expected: block, found };
                Err(RunError::Mismatch { port: Port::Input, index: 0, problem })
            }
        }
    }
}

impl BlockSink for Sink {
    fn push(&mut self, inputs: &[TensorRef<'_>]) -> Result<u64, RunError> {
        let mut s = lock(&self.0);
        s.check_block(inputs)?;
        let seq = s.next_seq;
        s.next_seq += 1;
        s.stats.pushed += 1;
        let index = usize::try_from(seq % s.slots.len() as u64).unwrap_or(0);
        s.slots[index].0 = None;
        if s.stats.stalled || (s.late)(seq) {
            return Ok(seq);
        }
        let State { session, slots, .. } = &mut *s;
        if session.run(inputs, &mut slots[index].1).is_ok() {
            slots[index].0 = Some(seq);
        }
        Ok(seq)
    }

    fn restart(&mut self) {
        let mut s = lock(&self.0);
        s.session.reset();
        for slot in &mut s.slots {
            slot.0 = None;
        }
    }
}

impl BlockSource for Source {
    fn take(&mut self, seq: u64, out: &mut [TensorBuf]) -> Block {
        let mut s = lock(&self.0);
        let index = usize::try_from(seq % s.slots.len() as u64).unwrap_or(0);
        // Once stalled, the slot has given up on the model: even blocks it already finished are not used.
        let ready = !s.stats.stalled
            && s.slots[index].0 == Some(seq)
            && out.len() == s.slots[index].1.len()
            && out.iter_mut().zip(&s.slots[index].1).all(|(o, r)| o.set(&r.as_ref()).is_ok());
        s.slots[index].0 = None;
        if ready {
            s.stats.ready += 1;
            s.missed_in_a_row = 0;
            Block::Ready
        } else {
            s.stats.missed += 1;
            s.missed_in_a_row += 1;
            if s.missed_in_a_row >= s.stall_blocks {
                s.stats.stalled = true;
            }
            Block::Missed
        }
    }

    fn queue(&self) -> u32 {
        lock(&self.0).queue
    }

    fn latency(&self) -> std::time::Duration {
        let s = lock(&self.0);
        s.spec.added_latency(s.queue)
    }

    fn stats(&self) -> StreamStats {
        lock(&self.0).stats
    }
}
