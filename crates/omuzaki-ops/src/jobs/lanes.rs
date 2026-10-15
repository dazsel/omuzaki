//! Lanes and the queue: how many jobs run at once and how many may wait (ADR 0036 decision 5).

use std::collections::VecDeque;

use omuzaki_ops_api::{CostHint, JobId, Lane, OpMeta, Scope};

/// Jobs that may wait, over all lanes.
pub(crate) const MAX_QUEUED: usize = 256;
/// Replacement threads a lane lends for detached ones; past it the lane refuses new jobs.
pub(crate) const MAX_LENT: usize = 2;

/// Which lane a job runs in, from its op and its plan's cost.
pub(crate) fn lane_of(meta: &OpMeta, cost: Option<&CostHint>) -> Lane {
    if cost.is_some_and(|c| c.gpu_mib.is_some()) {
        Lane::Gpu
    } else if meta.scope == Scope::Net || cost.is_some_and(|c| c.usd.is_some()) {
        Lane::Net
    } else {
        Lane::Cpu
    }
}

/// How wide the CPU lane is on a machine with `cores`.
pub(crate) fn cpu_width(cores: usize) -> usize {
    (cores / 2).clamp(1, 4)
}

/// Why a job cannot be admitted.
pub(crate) enum Refusal {
    /// 256 jobs are already queued.
    QueueFull,
    /// The lane lent all its replacements and a detached thread has not returned.
    LaneExhausted,
}

struct Slot {
    width: usize,
    running: usize,
    detached: usize,
    queue: VecDeque<JobId>,
}

impl Slot {
    fn new(width: usize) -> Self {
        Self { width, running: 0, detached: 0, queue: VecDeque::new() }
    }
}

/// The three lanes and their queues.
pub(crate) struct Lanes {
    cpu: Slot,
    gpu: Slot,
    net: Slot,
}

impl Lanes {
    pub(crate) fn new(cores: usize) -> Self {
        Self { cpu: Slot::new(cpu_width(cores)), gpu: Slot::new(1), net: Slot::new(8) }
    }

    // `Lane` is non-exhaustive: a lane added later runs on the CPU slots until it gets its own.
    fn slot(&mut self, lane: Lane) -> &mut Slot {
        match lane {
            Lane::Gpu => &mut self.gpu,
            Lane::Net => &mut self.net,
            _ => &mut self.cpu,
        }
    }

    fn queued(&self) -> usize {
        self.cpu.queue.len() + self.gpu.queue.len() + self.net.queue.len()
    }

    /// Queues `id`, or says why not.
    pub(crate) fn admit(&mut self, lane: Lane, id: JobId) -> Result<(), Refusal> {
        if self.queued() >= MAX_QUEUED {
            return Err(Refusal::QueueFull);
        }
        let slot = self.slot(lane);
        if slot.detached > MAX_LENT {
            return Err(Refusal::LaneExhausted);
        }
        slot.queue.push_back(id);
        Ok(())
    }

    /// Takes the next queued job that has a free slot, and marks the slot used.
    pub(crate) fn next_ready(&mut self) -> Option<(Lane, JobId)> {
        for lane in [Lane::Gpu, Lane::Net, Lane::Cpu] {
            let slot = self.slot(lane);
            if slot.running < slot.width
                && let Some(id) = slot.queue.pop_front()
            {
                slot.running += 1;
                return Some((lane, id));
            }
        }
        None
    }

    /// Drops a queued job; `false` if it was not queued.
    pub(crate) fn unqueue(&mut self, lane: Lane, id: JobId) -> bool {
        let queue = &mut self.slot(lane).queue;
        let before = queue.len();
        queue.retain(|queued| *queued != id);
        queue.len() != before
    }

    /// A running job ended and its thread is free: the slot is free again.
    pub(crate) fn finished(&mut self, lane: Lane) {
        let slot = self.slot(lane);
        slot.running = slot.running.saturating_sub(1);
    }

    /// A running job's thread is given up. Returns whether the lane lent a replacement (freed the slot).
    pub(crate) fn detach(&mut self, lane: Lane) -> bool {
        let slot = self.slot(lane);
        slot.detached += 1;
        let lent = slot.detached <= MAX_LENT;
        if lent {
            slot.running = slot.running.saturating_sub(1);
        }
        lent
    }

    /// A detached thread returned; `lent` is what [`detach`](Self::detach) answered for it.
    pub(crate) fn returned(&mut self, lane: Lane, lent: bool) {
        let slot = self.slot(lane);
        slot.detached = slot.detached.saturating_sub(1);
        if !lent {
            slot.running = slot.running.saturating_sub(1);
        }
    }

    /// Detached threads over all lanes.
    pub(crate) fn detached(&self) -> usize {
        self.cpu.detached + self.gpu.detached + self.net.detached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_lane_is_half_the_cores_between_one_and_four() {
        assert_eq!([1, 2, 4, 6, 8, 64].map(cpu_width), [1, 1, 2, 3, 4, 4]);
    }

    #[test]
    fn a_full_lane_queues_and_a_finished_job_frees_its_slot() {
        let mut lanes = Lanes::new(2);
        assert!(lanes.admit(Lane::Gpu, JobId(1)).is_ok() && lanes.admit(Lane::Gpu, JobId(2)).is_ok());
        assert_eq!(lanes.next_ready(), Some((Lane::Gpu, JobId(1))));
        assert_eq!(lanes.next_ready(), None, "the GPU lane is one wide");
        lanes.finished(Lane::Gpu);
        assert_eq!(lanes.next_ready(), Some((Lane::Gpu, JobId(2))));
    }

    #[test]
    fn the_queue_holds_256_over_all_lanes() {
        let mut lanes = Lanes::new(2);
        for n in 0..MAX_QUEUED as u64 {
            assert!(lanes.admit(Lane::Net, JobId(n)).is_ok());
        }
        assert!(matches!(lanes.admit(Lane::Cpu, JobId(999)), Err(Refusal::QueueFull)));
    }

    #[test]
    fn two_detached_threads_get_replacements_and_the_third_closes_the_lane() {
        let mut lanes = Lanes::new(2);
        for _ in 0..3 {
            lanes.slot(Lane::Gpu).running += 1;
        }
        assert!(lanes.detach(Lane::Gpu) && lanes.detach(Lane::Gpu));
        assert!(!lanes.detach(Lane::Gpu), "the third is not replaced");
        assert!(matches!(lanes.admit(Lane::Gpu, JobId(1)), Err(Refusal::LaneExhausted)));
        lanes.returned(Lane::Gpu, true);
        assert!(lanes.admit(Lane::Gpu, JobId(1)).is_ok(), "a thread came back");
        assert_eq!(lanes.detached(), 2);
    }
}
