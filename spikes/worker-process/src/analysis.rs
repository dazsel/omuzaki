//! Turns the run's timeline and the presenter's log into the numbers the research page reports. Pure: no clock, no I/O.

use serde::Serialize;
use spout_out::stats::Summary;

use crate::supervise::{Phase, Timeline};

/// A present is an interval longer than this many output periods; at 30 fps that is a skipped tick, not jitter.
const GAP_PERIODS: f64 = 2.0;
/// How many of the longest present intervals are listed by name.
const WORST_LISTED: usize = 5;

/// One tick of the output.
#[derive(Clone, Copy, Debug)]
pub struct Present {
    /// When the output presented, ms since the run started.
    pub t_ms: f64,
    /// Frame counter of the worker generation that rendered the frame shown.
    pub index: u64,
    /// True when a new frame arrived since the previous tick; false when the last one was held.
    pub fresh: bool,
    /// How old the shown frame was (now minus its arrival), ms.
    pub age_ms: f64,
}

/// One long gap between presents.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Gap {
    /// When the interval ended, ms since the run started.
    pub at_ms: f64,
    /// Its length.
    pub interval_ms: f64,
    /// Time from the nearest kill before it, if there was one.
    pub since_kill_ms: Option<f64>,
}

/// The measured results.
#[derive(Debug, Serialize)]
pub struct Analysis {
    /// Counted kills.
    pub kills: usize,
    /// Workers started (baseline and the final probe included).
    pub workers: usize,
    /// Frames the host refused because the slot did not hold the frame the worker announced.
    pub rejected_frames: u64,
    /// Presents in the kill phase, and how many showed a held frame.
    pub presents_kill_phase: usize,
    /// Presents that re-showed the previous frame because no new one had arrived.
    pub held_presents_kill_phase: usize,
    /// Interval between presents, kill phase, ms.
    pub present_interval_kill_ms: Summary,
    /// Interval between presents, baseline phase, ms.
    pub present_interval_baseline_ms: Summary,
    /// Intervals in the kill phase longer than twice the output period: real gaps, held frames not counted as gaps.
    pub real_gaps: usize,
    /// The longest present intervals in the kill phase.
    pub worst_intervals: Vec<Gap>,
    /// Age of the frame on screen at each present, kill phase, ms.
    pub frame_age_kill_ms: Summary,
    /// Kill to the first frame from a later worker, all kills, ms.
    pub kill_to_first_frame_ms: Summary,
    /// The same for kills that fell after the worker had posted a frame.
    pub kill_to_first_frame_steady_ms: Summary,
    /// The same for kills that fell before the worker's first frame.
    pub kill_to_first_frame_startup_ms: Summary,
    /// Gap between the last frame before the kill and the first from the new worker, ms: how long the output held one picture.
    pub hold_ms: Summary,
    /// Gap between consecutive frames in the kill phase, ms (the longest is the worst hold of the run).
    pub arrival_gap_ms: Summary,
    /// Process creation to the Ready message, ms. Only workers that were left alone until their first frame
    /// (baseline, cold-start and probe workers), so the sample is not censored by the kill schedule.
    pub spawn_to_ready_ms: Summary,
    /// Process creation to the first frame the host could use, ms; the same uncensored workers.
    pub spawn_to_first_frame_ms: Summary,
    /// The same for the very first worker of the run (nothing of the worker was in the page cache).
    pub first_worker_spawn_to_first_frame_ms: Option<f64>,
    /// Inside the worker: instance, adapter and device, ms; the same uncensored workers.
    pub worker_gpu_init_ms: Summary,
    /// Inside the worker: pipeline build, ms; the same uncensored workers.
    pub worker_scene_build_ms: Summary,
    /// Workers the kill schedule started.
    pub kill_phase_workers: usize,
    /// Of those, how many were killed before posting a frame: their start-up time is unknown (censored), not zero.
    pub kill_phase_workers_without_a_frame: usize,
    /// Kill call to the next process creation, ms (the host's own reaction time).
    pub respawn_latency_ms: Summary,
    /// Worker's draw + readback per frame in the baseline phase, ms.
    pub worker_render_ms: Summary,
    /// Worker's copy into the slot per frame in the baseline phase, ms.
    pub worker_write_ms: Summary,
    /// Host's slot read + tag check per frame, ms.
    pub host_ingest_ms: Summary,
}

fn intervals(presents: &[Present]) -> Vec<(f64, f64)> {
    presents.windows(2).map(|w| (w[1].t_ms, w[1].t_ms - w[0].t_ms)).collect()
}

fn uncensored_readies<'a>(
    timeline: &'a Timeline,
    uncensored: &'a [&crate::supervise::Spawn],
) -> impl Iterator<Item = &'a crate::supervise::Ready> {
    timeline.readies.iter().filter(move |r| uncensored.iter().any(|s| s.worker == r.worker))
}

fn summary(values: impl Iterator<Item = f64>) -> Summary {
    Summary::of(&values.collect::<Vec<_>>())
}

/// Computes every reported number. `period_ms` is the output period (33.3 ms at 30 fps).
#[must_use]
pub fn analyse(timeline: &Timeline, presents: &[Present], host_ingest_ms: &[f64], period_ms: f64) -> Analysis {
    let boundary = timeline.boundary_ms.unwrap_or(0.0);
    let kill_start = timeline.kill_start_ms.unwrap_or(boundary);
    let baseline_presents: Vec<Present> = presents.iter().copied().filter(|p| p.t_ms < boundary).collect();
    let kill_presents: Vec<Present> = presents.iter().copied().filter(|p| p.t_ms >= kill_start).collect();
    let kill_intervals = intervals(&kill_presents);
    let mut worst: Vec<Gap> = kill_intervals
        .iter()
        .map(|&(at_ms, interval_ms)| Gap {
            at_ms,
            interval_ms,
            since_kill_ms: timeline.kills.iter().rev().find(|k| k.t_ms <= at_ms).map(|k| at_ms - k.t_ms),
        })
        .collect();
    worst.sort_by(|a, b| b.interval_ms.total_cmp(&a.interval_ms));
    let real_gaps = worst.iter().filter(|g| g.interval_ms > GAP_PERIODS * period_ms).count();
    worst.truncate(WORST_LISTED);

    let counted: Vec<_> = timeline.kills.iter().filter(|k| k.counted).collect();
    let mut after_kill = Vec::new(); // (started, kill to first frame, hold)
    for kill in &counted {
        let next = timeline.arrivals.iter().position(|a| a.worker > kill.worker);
        let Some(next) = next else { continue };
        let started = timeline.arrivals.iter().any(|a| a.worker == kill.worker);
        let to_first = timeline.arrivals[next].t_ms - kill.t_ms;
        let hold = next.checked_sub(1).map(|prev| timeline.arrivals[next].t_ms - timeline.arrivals[prev].t_ms);
        after_kill.push((started, to_first, hold));
    }

    let kill_phase_arrivals: Vec<_> = timeline.arrivals.iter().filter(|a| a.t_ms >= kill_start).collect();
    let first_frame_of = |worker: u32| timeline.arrivals.iter().find(|a| a.worker == worker).map(|a| a.t_ms);
    // Start-up timings come only from workers nothing interrupted; a kill-phase worker killed early would leave a short,
    // biased sample (or none).
    let uncensored: Vec<_> = timeline.spawns.iter().filter(|s| s.phase != Phase::Kill).collect();
    let spawn_to_first: Vec<f64> = uncensored.iter().filter_map(|s| Some(first_frame_of(s.worker)? - s.t_ms)).collect();
    let kill_spawns: Vec<_> = timeline.spawns.iter().filter(|s| s.phase == Phase::Kill).collect();
    let baseline_arrivals = timeline.arrivals.iter().filter(|a| a.t_ms < boundary);

    Analysis {
        kills: counted.len(),
        workers: timeline.spawns.len(),
        rejected_frames: timeline.rejected,
        presents_kill_phase: kill_presents.len(),
        held_presents_kill_phase: kill_presents.iter().filter(|p| !p.fresh).count(),
        present_interval_kill_ms: summary(kill_intervals.iter().map(|i| i.1)),
        present_interval_baseline_ms: summary(intervals(&baseline_presents).iter().map(|i| i.1)),
        real_gaps,
        worst_intervals: worst,
        frame_age_kill_ms: summary(kill_presents.iter().map(|p| p.age_ms)),
        kill_to_first_frame_ms: summary(after_kill.iter().map(|k| k.1)),
        kill_to_first_frame_steady_ms: summary(after_kill.iter().filter(|k| k.0).map(|k| k.1)),
        kill_to_first_frame_startup_ms: summary(after_kill.iter().filter(|k| !k.0).map(|k| k.1)),
        hold_ms: summary(after_kill.iter().filter_map(|k| k.2)),
        arrival_gap_ms: summary(kill_phase_arrivals.windows(2).map(|w| w[1].t_ms - w[0].t_ms)),
        spawn_to_ready_ms: summary(
            uncensored
                .iter()
                .filter_map(|s| Some(timeline.readies.iter().find(|r| r.worker == s.worker)?.t_ms - s.t_ms)),
        ),
        spawn_to_first_frame_ms: summary(spawn_to_first.iter().copied()),
        first_worker_spawn_to_first_frame_ms: spawn_to_first.first().copied(),
        worker_gpu_init_ms: summary(uncensored_readies(timeline, &uncensored).map(|r| r.gpu_ms)),
        worker_scene_build_ms: summary(uncensored_readies(timeline, &uncensored).map(|r| r.scene_ms)),
        kill_phase_workers: kill_spawns.len(),
        kill_phase_workers_without_a_frame: kill_spawns.iter().filter(|s| first_frame_of(s.worker).is_none()).count(),
        respawn_latency_ms: summary(
            counted
                .iter()
                .filter_map(|k| Some(timeline.spawns.iter().find(|s| s.worker == k.worker + 1)?.t_ms - k.t_ms)),
        ),
        worker_render_ms: summary(baseline_arrivals.clone().map(|a| a.render_ms)),
        worker_write_ms: summary(baseline_arrivals.map(|a| a.write_ms)),
        host_ingest_ms: Summary::of(host_ingest_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervise::{Arrival, Kill, Ready, Spawn};

    fn arrival(worker: u32, t_ms: f64) -> Arrival {
        Arrival { worker, index: 0, t_ms, render_ms: 1.0, write_ms: 2.0 }
    }

    fn timeline() -> Timeline {
        Timeline {
            // Worker 1 posts every 100 ms until 900, is killed at 1000; worker 2 is spawned at 1010 and posts at 1400.
            arrivals: (1..=9).map(|i| arrival(1, f64::from(i) * 100.0)).chain([arrival(2, 1400.0)]).collect(),
            readies: vec![Ready { worker: 2, t_ms: 1300.0, gpu_ms: 200.0, scene_ms: 50.0 }],
            spawns: vec![
                Spawn { worker: 1, phase: Phase::Kill, t_ms: 0.0 },
                Spawn { worker: 2, phase: Phase::Probe, t_ms: 1010.0 },
            ],
            kills: vec![Kill { worker: 1, t_ms: 1000.0, counted: true }],
            rejected: 0,
            exits: vec![],
            boundary_ms: None,
            kill_start_ms: None,
        }
    }

    #[test]
    fn kill_to_first_frame_and_hold_come_from_the_arrivals() {
        let a = analyse(&timeline(), &[], &[], 33.3);
        assert_eq!(a.kills, 1);
        assert_eq!(a.kill_to_first_frame_ms.max, 400.0);
        assert_eq!(a.kill_to_first_frame_steady_ms.count, 1);
        assert_eq!(a.kill_to_first_frame_startup_ms.count, 0);
        assert_eq!(a.hold_ms.max, 500.0);
        assert_eq!(a.respawn_latency_ms.max, 10.0);
        assert_eq!(a.spawn_to_ready_ms.max, 290.0);
        assert_eq!(a.first_worker_spawn_to_first_frame_ms, Some(390.0));
    }

    #[test]
    fn start_up_timings_ignore_workers_the_schedule_could_interrupt() {
        // Worker 1 (kill phase) started fast; worker 2 (probe) took 390 ms. Only worker 2 is a start-up sample.
        let a = analyse(&timeline(), &[], &[], 33.3);
        assert_eq!(a.spawn_to_first_frame_ms.count, 1);
        assert_eq!(a.spawn_to_first_frame_ms.max, 390.0);
        assert_eq!((a.kill_phase_workers, a.kill_phase_workers_without_a_frame), (1, 0));
        let mut t = timeline();
        t.arrivals.retain(|a| a.worker != 1);
        assert_eq!(analyse(&t, &[], &[], 33.3).kill_phase_workers_without_a_frame, 1);
    }

    #[test]
    fn a_kill_before_the_first_frame_is_a_startup_kill() {
        let mut t = timeline();
        t.arrivals.retain(|a| a.worker != 1);
        let a = analyse(&t, &[], &[], 33.3);
        assert_eq!((a.kill_to_first_frame_steady_ms.count, a.kill_to_first_frame_startup_ms.count), (0, 1));
    }

    #[test]
    fn only_skipped_ticks_count_as_real_gaps() {
        let tick = |t_ms| Present { t_ms, index: 0, fresh: false, age_ms: 0.0 };
        // 33 ms ticks with 40 ms of jitter once, and one 100 ms hole.
        let presents = [0.0, 33.0, 73.0, 106.0, 206.0, 239.0].map(tick);
        let a = analyse(&Timeline::default(), &presents, &[], 33.3);
        assert_eq!(a.real_gaps, 1);
        assert_eq!(a.present_interval_kill_ms.max, 100.0);
        assert_eq!(a.worst_intervals[0].interval_ms, 100.0);
    }
}
