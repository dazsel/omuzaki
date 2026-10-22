//! The supervisor to its acceptance (#1025), on fake workers: real processes (the `fake-worker` binary) killed with
//! `kill -9`, hangs, crash loops, the stop, and thread workers that panic or hang.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{Seconds, SystemClock};
use omuzaki_stream::watchdog::launch::WorkerLauncher;
use omuzaki_stream::watchdog::launch::process::{ProcessLauncher, WorkerCommand};
use omuzaki_stream::watchdog::launch::thread::ThreadLauncher;
use omuzaki_stream::watchdog::supervisor::{DEFAULT_TICK, OutputHold, Supervisor};
use omuzaki_stream::watchdog::{Health, HealthEvent, RestartPolicy, RestartReason, WorkerId, WorkerKind, WorkerSpec};

const FAKE_WORKER: &str = env!("CARGO_BIN_EXE_fake-worker");

fn spec(name: &str, kind: WorkerKind, hold: bool) -> WorkerSpec {
    WorkerSpec {
        name: name.to_owned(),
        kind,
        hold_outputs: hold,
        start_grace: Seconds(3.0),
        hung_after: Seconds(0.5),
        restart: RestartPolicy {
            backoff_first: Seconds(0.1),
            backoff_max: Seconds(0.4),
            max_restarts: 3,
            within: Seconds(60.0),
            stable_after: Seconds(30.0),
        },
    }
}

fn worker(args: &[&str]) -> WorkerCommand {
    WorkerCommand::new(FAKE_WORKER, args.iter().copied())
}

/// Records whether the outputs are held.
#[derive(Default)]
struct Hold(AtomicBool);

impl OutputHold for Hold {
    fn hold(&self, on: bool) {
        self.0.store(on, Ordering::Release);
    }
}

fn start(workers: Vec<(WorkerSpec, Box<dyn WorkerLauncher>)>, hold: Arc<Hold>) -> Supervisor {
    Supervisor::start(workers, hold, Arc::new(SystemClock::new()), DEFAULT_TICK)
}

/// Waits up to `limit` for `ok`, polling every 5 ms; returns how long it took.
fn wait_for(limit: Duration, mut ok: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if ok() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn health(sup: &Supervisor, id: WorkerId) -> Health {
    sup.health().into_iter().find(|(w, _)| *w == id).map_or(Health::Starting, |(_, h)| h)
}

fn pid(sup: &Supervisor, id: WorkerId) -> Option<u32> {
    sup.pids().into_iter().find(|(w, _)| *w == id).and_then(|(_, p)| p)
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn kill_9(pid: u32) {
    let status = std::process::Command::new("/bin/kill").args(["-9", &pid.to_string()]).status().unwrap();
    assert!(status.success());
}

/// A fake output host: a frame counter at 30 Hz, a new frame while the renderer is healthy, the last one again while
/// held. Returns, once stopped, the frame slots that went out empty and the count of new frames.
///
/// The output's longest gap is counted in 30 Hz slots, not measured on the wall clock: an empty slot is a gap past
/// 50 ms on a host that runs on time, and the test thread's own scheduling jitter on a busy machine isn't what is
/// under test.
struct OutputHost {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<(u64, u64)>,
}

impl OutputHost {
    fn start(sup: Arc<Supervisor>, renderer: WorkerId, hold: Arc<Hold>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let (mut fresh, mut empty) = (0_u64, 0_u64);
            let period = Duration::from_micros(33_333);
            let mut next = Instant::now() + period;
            while !flag.load(Ordering::Acquire) {
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
                next += period;
                let live = health(&sup, renderer) == Health::Healthy;
                // Held, it re-presents the last frame; live, it shows a new one; otherwise nothing goes out.
                let holding = hold.0.load(Ordering::Acquire);
                if live && !holding {
                    fresh += 1;
                }
                let emitted = holding || live;
                if !emitted {
                    empty += 1;
                }
            }
            (empty, fresh)
        });
        Self { stop, thread }
    }

    fn finish(self) -> (u64, u64) {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap()
    }
}

#[cfg(unix)]
#[test]
fn kill_9_of_each_worker_leaves_the_stream_up() {
    let (renderer, tracker, encoder) = (WorkerId(0), WorkerId(1), WorkerId(2));
    let launcher = |id| -> Box<dyn WorkerLauncher> {
        Box::new(ProcessLauncher::new().with(id, worker(&["--startup-ms", "100", "--beat-ms", "20"])))
    };
    let hold = Arc::new(Hold::default());
    let sup = Arc::new(start(
        vec![
            (spec("renderer", WorkerKind::Renderer, true), launcher(renderer)),
            (spec("tracker", WorkerKind::Tracker, false), launcher(tracker)),
            (spec("encoder", WorkerKind::Encoder, false), launcher(encoder)),
        ],
        Arc::clone(&hold),
    ));
    let all = [renderer, tracker, encoder];
    wait_for(Duration::from_secs(4), || all.iter().all(|&w| health(&sup, w) == Health::Healthy)).unwrap();
    let host = OutputHost::start(Arc::clone(&sup), renderer, Arc::clone(&hold));
    std::thread::sleep(Duration::from_millis(300));

    for (kills, &victim) in all.iter().enumerate() {
        let before: Vec<_> = all.iter().map(|&w| pid(&sup, w).unwrap()).collect();
        let killed = pid(&sup, victim).unwrap();
        kill_9(killed);
        let took = wait_for(Duration::from_secs(4), || {
            pid(&sup, victim).is_some_and(|p| p != killed) && health(&sup, victim) == Health::Healthy
        })
        .unwrap_or_else(|| panic!("{victim:?} never came back"));
        assert!(took <= Duration::from_secs(2), "{victim:?} back in {took:?}");
        assert!(!alive(killed), "the killed child is reaped");
        for (i, &other) in all.iter().enumerate() {
            if other != victim {
                assert_eq!(pid(&sup, other), Some(before[i]), "{other:?} disturbed");
            }
        }
        let log = sup.log();
        assert_eq!(log.len(), kills + 1);
        let last = log.last().unwrap();
        assert_eq!((last.worker, last.reason, last.attempt), (victim, RestartReason::Crashed { exit_code: None }, 1));
        std::thread::sleep(Duration::from_millis(200));
    }
    let (empty, fresh) = host.finish();
    assert_eq!(empty, 0, "frame slots the output left empty");
    assert!(fresh > 30, "the renderer's frames went out between the kills ({fresh})");
    Arc::try_unwrap(sup).unwrap().stop();
}

#[cfg(unix)]
#[test]
fn a_hung_worker_is_killed_and_replaced() {
    let id = WorkerId(0);
    let hold = Arc::new(Hold::default());
    let launcher = ProcessLauncher::new().with(id, worker(&["--hang-after-ms", "1000"]));
    let sup = start(vec![(spec("tracker", WorkerKind::Tracker, false), Box::new(launcher))], hold);
    let events = sup.events();
    wait_for(Duration::from_secs(3), || pid(&sup, id).is_some()).unwrap();
    let first = pid(&sup, id).unwrap();
    let event = events.recv_timeout(Duration::from_secs(4)).unwrap();
    let HealthEvent::Restarted { reason: RestartReason::Hung { silent_for }, attempt: 1, .. } = event else {
        panic!("{event:?}")
    };
    assert!(silent_for.0 >= 0.5 && silent_for.0 <= 1.0, "silent for {silent_for:?}");
    wait_for(Duration::from_secs(2), || pid(&sup, id).is_some_and(|p| p != first)).unwrap();
    assert!(!alive(first));
    sup.stop();
}

#[cfg(unix)]
#[test]
fn a_crash_loop_gives_up_and_a_restart_brings_it_back() {
    let id = WorkerId(0);
    let hold = Arc::new(Hold::default());
    let launcher = ProcessLauncher::new().with(id, worker(&["--crash-after-ms", "50"]));
    let sup = start(vec![(spec("encoder", WorkerKind::Encoder, false), Box::new(launcher))], hold);
    let events = sup.events();
    let seen = Arc::new(Mutex::new(Vec::new()));
    wait_for(Duration::from_secs(6), || {
        seen.lock().unwrap().extend(events.try_iter());
        matches!(health(&sup, id), Health::Failed { .. })
    })
    .unwrap();
    assert_eq!(health(&sup, id), Health::Failed { restarts: 3 });
    std::thread::sleep(Duration::from_millis(300));
    seen.lock().unwrap().extend(events.try_iter());
    let gave_up = seen.lock().unwrap().iter().filter(|e| matches!(e, HealthEvent::GaveUp { .. })).count();
    assert_eq!(gave_up, 1);
    assert_eq!(pid(&sup, id), None, "nothing runs once it gave up");

    let records = sup.log().len();
    sup.restart(id);
    wait_for(Duration::from_secs(2), || sup.log().len() > records).unwrap();
    assert_eq!(sup.log()[records].reason, RestartReason::Requested);
    wait_for(Duration::from_secs(2), || pid(&sup, id).is_some() || sup.log().len() > records + 1).unwrap();
    sup.stop();
}

#[cfg(unix)]
#[test]
fn stop_ends_and_reaps_every_child() {
    let hold = Arc::new(Hold::default());
    let workers = (0..3)
        .map(|i| {
            let id = WorkerId(i);
            let launcher: Box<dyn WorkerLauncher> = Box::new(ProcessLauncher::new().with(id, worker(&[])));
            (spec("worker", WorkerKind::Adapter, false), launcher)
        })
        .collect();
    let sup = start(workers, hold);
    wait_for(Duration::from_secs(3), || sup.pids().iter().all(|(_, p)| p.is_some())).unwrap();
    let pids: Vec<u32> = sup.pids().into_iter().filter_map(|(_, p)| p).collect();
    assert_eq!(pids.len(), 3);
    sup.stop();
    for pid in pids {
        assert!(!alive(pid), "{pid} still runs");
    }
}

#[test]
fn thread_workers_restart_after_a_panic_and_are_abandoned_when_hung() {
    let (panicky, hanging) = (WorkerId(0), WorkerId(1));
    let runs = Arc::new(Mutex::new(0_u32));
    let count = Arc::clone(&runs);
    let launcher = ThreadLauncher::new()
        .with(panicky, move |w| {
            let run = {
                let mut runs = count.lock().unwrap();
                *runs += 1;
                *runs
            };
            w.beat();
            std::thread::sleep(Duration::from_millis(100));
            assert!(run > 1, "the first run panics");
            while !w.should_stop() {
                w.beat();
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .with(hanging, |w| {
            // Beats for 300 ms, then hangs (ignoring its stop flag, as a stuck thread would) for 3 s.
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(300) {
                w.beat();
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_secs(3));
        });
    let counter = launcher.clone();
    let hold = Arc::new(Hold::default());
    let sup = start(
        vec![
            (spec("panicky", WorkerKind::Audio, false), Box::new(launcher.clone())),
            (spec("hanging", WorkerKind::Adapter, false), Box::new(launcher)),
        ],
        hold,
    );
    wait_for(Duration::from_secs(4), || *runs.lock().unwrap() >= 2 && health(&sup, panicky) == Health::Healthy)
        .unwrap();
    let crash = sup.log().into_iter().find(|r| r.worker == panicky).unwrap();
    assert_eq!(crash.reason, RestartReason::Crashed { exit_code: Some(101) });
    wait_for(Duration::from_secs(4), || counter.abandoned() >= 1).unwrap();
    assert_eq!(counter.abandoned(), 1);
    let hang = sup.log().into_iter().find(|r| r.worker == hanging).unwrap();
    assert!(matches!(hang.reason, RestartReason::Hung { .. }));
    wait_for(Duration::from_secs(2), || health(&sup, hanging) == Health::Healthy).unwrap();
    sup.stop();
}
