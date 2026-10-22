//! The supervisor to its acceptance (#1025, #1224; `docs/design/supervisor.md` §14): real processes (the
//! `fake-worker` binary, speaking the worker protocol on its stdin) killed with `kill -9`, hangs, crash loops, protocol
//! faults, configuration errors, latched state replayed to each generation, the stop, orphans, a kill that never
//! finishes, and thread workers that panic or hang.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{Seconds, SystemClock};
use omuzaki_stream::watchdog::launch::thread::ThreadLauncher;
use omuzaki_stream::watchdog::launch::{
    ControlSend, ExitInfo, Fault, LaunchError, Waker, WorkerHandle, WorkerLauncher,
};
use omuzaki_stream::watchdog::{
    Handoff, Health, HealthEvent, HeartbeatCell, RestartPolicy, RestartReason, Scope, SendError, Supervisor,
    SupervisorConfig, WorkerId, WorkerKind, WorkerSpec, WorkerStatus,
};
use omuzaki_stream_api::worker::{ChannelHandle, HostMsg};

fn spec(name: &str, kind: WorkerKind) -> WorkerSpec {
    WorkerSpec {
        name: name.to_owned(),
        kind,
        start_grace: Seconds(3.0),
        hung_after: Seconds(0.5),
        slow_after: Seconds(2.0),
        restart: RestartPolicy {
            backoff_first: Seconds(0.1),
            backoff_max: Seconds(0.4),
            max_restarts: 3,
            within: Seconds(60.0),
            stable_after: Seconds(30.0),
        },
    }
}

/// Records the handoff's calls in order.
#[derive(Default)]
struct Calls(Mutex<Vec<(WorkerId, bool)>>);

impl Handoff for Calls {
    fn down(&self, id: WorkerId, _: WorkerKind) {
        self.0.lock().unwrap().push((id, false));
    }
    fn up(&self, id: WorkerId, _: WorkerKind) {
        self.0.lock().unwrap().push((id, true));
    }
}

fn config() -> SupervisorConfig {
    SupervisorConfig {
        exit_wait: Duration::from_millis(300),
        stop_wait: Duration::from_millis(500),
        ..Default::default()
    }
}

fn start(workers: Vec<(WorkerSpec, Box<dyn WorkerLauncher>)>, handoff: Arc<dyn Handoff>) -> Supervisor {
    Supervisor::start(workers, handoff, Arc::new(SystemClock::new()), config()).unwrap()
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

fn row(sup: &Supervisor, id: WorkerId) -> WorkerStatus {
    sup.status().into_iter().find(|r| r.id == id).unwrap()
}

fn health(sup: &Supervisor, id: WorkerId) -> Health {
    row(sup, id).health
}

fn pid(sup: &Supervisor, id: WorkerId) -> Option<u32> {
    row(sup, id).pid
}

/// Waits for worker `id` to be healthy on a process other than `not`; returns its pid and how long it took.
fn healthy(sup: &Supervisor, id: WorkerId, not: Option<u32>, limit: Duration) -> (u32, Duration) {
    let took = wait_for(limit, || {
        let r = row(sup, id);
        r.health == Health::Healthy && r.pid.is_some() && r.pid != not
    })
    .unwrap_or_else(|| panic!("{id:?} not healthy within {limit:?}: {:?}", row(sup, id)));
    (pid(sup, id).unwrap(), took)
}

#[cfg(unix)]
mod processes {
    use super::*;
    use omuzaki_stream::watchdog::launch::Plain;
    use omuzaki_stream::watchdog::launch::process::{ProcessLauncher, WorkerCommand};
    use omuzaki_stream_api::worker::StartParams;

    const FAKE_WORKER: &str = env!("CARGO_BIN_EXE_fake-worker");

    pub(super) fn params(kind: WorkerKind) -> StartParams {
        match kind {
            WorkerKind::Renderer => StartParams::stage(1, omuzaki_core::glam::UVec2::new(64, 64), (30, 1)),
            WorkerKind::Tracker => StartParams::tracker("synthetic"),
            WorkerKind::Adapter => StartParams::adapter("fixture"),
            other => panic!("no plain parameters for {other:?}"),
        }
    }

    /// A process launcher of the fake worker as a `kind`, with `args`.
    pub(super) fn fake(kind: WorkerKind, args: &[&str]) -> Box<dyn WorkerLauncher> {
        let name = format!("{kind:?}").to_lowercase();
        let mut all = vec!["--kind", name.as_str()];
        all.extend_from_slice(args);
        let command = WorkerCommand::new(FAKE_WORKER, all.iter().copied());
        Box::new(ProcessLauncher::new(command, kind, Arc::new(Plain(params(kind)))))
    }

    /// As [`fake`], saying `Hello` as `says` instead.
    pub(super) fn impostor(kind: WorkerKind, says: &str) -> Box<dyn WorkerLauncher> {
        let command = WorkerCommand::new(FAKE_WORKER, ["--kind", says]);
        Box::new(ProcessLauncher::new(command, kind, Arc::new(Plain(params(kind)))))
    }

    pub(super) fn alive(pid: u32) -> bool {
        // A reaped child is gone; a zombie would still answer signal 0, so this also proves the reap.
        let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()) else { return false };
        rustix::process::test_kill_process(pid).is_ok()
    }

    pub(super) fn kill_9(pid: u32) {
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    }
}

#[cfg(unix)]
use processes::{alive, fake, impostor, kill_9};

#[cfg(unix)]
#[test]
fn kill_9_of_each_worker_is_back_within_2_s_and_disturbs_no_other() {
    let calls = Arc::new(Calls::default());
    let sup = start(
        vec![
            (spec("renderer", WorkerKind::Renderer), fake(WorkerKind::Renderer, &["--beat-ms", "20"])),
            (spec("tracker", WorkerKind::Tracker), fake(WorkerKind::Tracker, &["--beat-ms", "20"])),
            (spec("adapter", WorkerKind::Adapter), fake(WorkerKind::Adapter, &["--beat-ms", "20"])),
        ],
        calls.clone(),
    );
    let all = sup.ids().to_vec();
    for &w in &all {
        healthy(&sup, w, None, Duration::from_secs(5));
    }
    let mut worst = Duration::ZERO;
    for (kills, &victim) in all.iter().enumerate() {
        let before: Vec<_> = all.iter().map(|&w| pid(&sup, w).unwrap()).collect();
        let killed = pid(&sup, victim).unwrap();
        kill_9(killed);
        let (_, took) = healthy(&sup, victim, Some(killed), Duration::from_secs(4));
        worst = worst.max(took);
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
        assert_eq!((last.worker, &last.reason, last.attempt), (victim, &RestartReason::crashed(None), 1));
        assert!(last.downtime.is_some_and(|d| d.0 < 2.0), "{:?}", last.downtime);
        let calls = calls.0.lock().unwrap();
        assert_eq!(calls[calls.len() - 2..], [(victim, false), (victim, true)], "down, then up");
    }
    eprintln!("slowest return after kill -9: {worst:?}");
    let report = sup.stop();
    assert_eq!(report.stuck, []);
}

#[cfg(unix)]
#[test]
fn a_hung_worker_is_killed_and_replaced() {
    let sup = start(
        vec![(spec("tracker", WorkerKind::Tracker), fake(WorkerKind::Tracker, &["--hang-after-ms", "800"]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    let events = sup.events();
    let (first, _) = healthy(&sup, id, None, Duration::from_secs(4));
    let event = events.recv_timeout(Duration::from_secs(4)).unwrap();
    let HealthEvent::Restarted { reason: RestartReason::Hung { silent_for, .. }, attempt: 1, .. } = event else {
        panic!("{event:?}")
    };
    // Killed within hung_after plus a tick and a beat period.
    assert!(silent_for.0 >= 0.5 && silent_for.0 <= 0.65, "silent for {silent_for:?}");
    wait_for(Duration::from_secs(2), || pid(&sup, id).is_some_and(|p| p != first)).unwrap();
    assert!(!alive(first));
    sup.stop();
}

#[cfg(unix)]
#[test]
fn a_crash_loop_gives_up_and_a_restart_brings_it_back() {
    let sup = start(
        vec![(spec("adapter", WorkerKind::Adapter), fake(WorkerKind::Adapter, &["--crash-after-ms", "50"]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    let events = sup.events();
    wait_for(Duration::from_secs(6), || matches!(health(&sup, id), Health::Failed { .. })).unwrap();
    assert_eq!(health(&sup, id), Health::failed(3));
    std::thread::sleep(Duration::from_millis(300));
    let seen: Vec<_> = events.try_iter().collect();
    assert_eq!(seen.iter().filter(|e| matches!(e, HealthEvent::GaveUp { .. })).count(), 1, "{seen:?}");
    assert_eq!(pid(&sup, id), None, "nothing runs once it gave up");
    assert!(sup.log().iter().all(|r| r.reason == RestartReason::crashed(Some(3))));
    // A command to a failed worker is refused; state is still taken.
    assert_eq!(sup.send(id, &HostMsg::frame(0, 0, 0)), Err(SendError::Down));
    assert_eq!(sup.send(id, &HostMsg::scene(1)), Ok(()));

    let records = sup.log().len();
    sup.restart(id).unwrap();
    wait_for(Duration::from_secs(2), || sup.log().len() > records).unwrap();
    assert_eq!(sup.log()[records].reason, RestartReason::Requested);
    wait_for(Duration::from_secs(2), || pid(&sup, id).is_some()).unwrap();
    sup.stop();
}

#[cfg(unix)]
#[test]
fn a_malformed_frame_is_a_protocol_fault_and_the_worker_is_killed() {
    let sup = start(
        vec![(spec("tracker", WorkerKind::Tracker), fake(WorkerKind::Tracker, &["--bad-frame"]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    let first = wait_for(Duration::from_secs(3), || !sup.log().is_empty()).map(|_| sup.log()[0].clone()).unwrap();
    assert!(matches!(&first.reason, RestartReason::Protocol { .. }), "{:?}", first.reason);
    // The faulting process stays alive on its own (it sleeps 30 s); the supervisor killed and reaped it.
    let gone =
        wait_for(Duration::from_secs(2), || sup.log().len() >= 2 || matches!(health(&sup, id), Health::Failed { .. }));
    assert!(gone.is_some(), "the next generation also faulted, so the first was replaced");
    sup.stop();
}

#[cfg(unix)]
#[test]
fn gone_is_a_failed_launch_with_its_reason() {
    let sup = start(
        vec![(spec("adapter", WorkerKind::Adapter), fake(WorkerKind::Adapter, &["--gone", "no such account"]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    wait_for(Duration::from_secs(6), || matches!(health(&sup, id), Health::Failed { .. })).unwrap();
    let log = sup.log();
    assert_ne!(log.len(), 0);
    assert!(log.iter().all(|r| r.reason == RestartReason::launch_failed("no such account")), "{log:?}");
    sup.stop();
}

#[cfg(unix)]
#[test]
fn a_worker_of_another_kind_gives_up_at_once() {
    let sup = start(
        vec![(spec("tracker", WorkerKind::Tracker), impostor(WorkerKind::Tracker, "adapter"))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    wait_for(Duration::from_secs(3), || matches!(health(&sup, id), Health::Failed { .. })).unwrap();
    assert_eq!(health(&sup, id), Health::failed(0), "retrying can't help");
    let log = sup.log();
    assert_eq!(log.len(), 1);
    assert!(matches!(&log[0].reason, RestartReason::LaunchFailed { .. }), "{:?}", log[0].reason);
    sup.stop();
}

#[cfg(unix)]
#[test]
fn stdout_spam_never_reaches_the_protocol() {
    let sup = start(
        vec![(spec("renderer", WorkerKind::Renderer), fake(WorkerKind::Renderer, &["--spam-stdout"]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    let (first, _) = healthy(&sup, id, None, Duration::from_secs(4));
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(pid(&sup, id), Some(first));
    assert_eq!(health(&sup, id), Health::Healthy);
    assert!(sup.log().is_empty(), "{:?}", sup.log());
    sup.stop();
}

#[cfg(unix)]
#[test]
fn latched_state_is_replayed_to_every_generation() {
    let sup = start(
        vec![(spec("renderer", WorkerKind::Renderer), fake(WorkerKind::Renderer, &[]))],
        Arc::new(Calls::default()),
    );
    let id = sup.ids()[0];
    let (first, _) = healthy(&sup, id, None, Duration::from_secs(4));
    sup.send(id, &HostMsg::scene(1)).unwrap();
    sup.send(id, &HostMsg::scene(2)).unwrap();
    let other = WorkerId::new(Scope::Host, 99, 0);
    sup.send(id, &HostMsg::source(other, true)).unwrap();
    sup.send(id, &HostMsg::source(other, false)).unwrap();
    wait_for(Duration::from_secs(2), || row(&sup, id).work == 4).unwrap();
    kill_9(first);
    healthy(&sup, id, Some(first), Duration::from_secs(4));
    // The newest scene and the newest word on the source: two messages, not four.
    wait_for(Duration::from_secs(2), || row(&sup, id).work == 2).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(row(&sup, id).work, 2);
    // An id from another supervisor is refused.
    assert!(matches!(sup.send(other, &HostMsg::scene(3)), Err(SendError::Unknown(_))));
    assert!(sup.restart(other).is_err());
    sup.stop();
}

#[cfg(unix)]
#[test]
fn stop_ends_and_reaps_every_child() {
    let workers = (0..3).map(|_| (spec("adapter", WorkerKind::Adapter), fake(WorkerKind::Adapter, &[]))).collect();
    let sup = start(workers, Arc::new(Calls::default()));
    let ids = sup.ids().to_vec();
    let pids: Vec<u32> = ids.iter().map(|&id| healthy(&sup, id, None, Duration::from_secs(4)).0).collect();
    let started = Instant::now();
    let report = sup.stop();
    // Each exits on `Stop`: well inside stop_wait.
    assert!(started.elapsed() < Duration::from_millis(500), "stop took {:?}", started.elapsed());
    assert_eq!(report.stuck, []);
    for pid in pids {
        assert!(!alive(pid), "{pid} still runs");
    }
}

/// The orphan check's host, run as a child of the test: a supervisor over one worker, its pid on stdout, then it waits
/// to be killed.
#[cfg(target_os = "linux")]
#[test]
fn orphan_host() {
    if std::env::var_os("OMUZAKI_SUPERVISOR_HOST").is_none() {
        return;
    }
    // Started from a thread that ends once the worker is up: the parent-death signal must not follow that thread.
    let (tx, rx) = std::sync::mpsc::channel();
    let starter = std::thread::spawn(move || {
        let sup = start(
            vec![(spec("tracker", WorkerKind::Tracker), fake(WorkerKind::Tracker, &[]))],
            Arc::new(Calls::default()),
        );
        healthy(&sup, sup.ids()[0], None, Duration::from_secs(4));
        tx.send(sup).unwrap();
    });
    let sup = rx.recv().unwrap();
    starter.join().unwrap();
    let id = sup.ids()[0];
    let (worker, _) = healthy(&sup, id, None, Duration::from_secs(4));
    std::thread::sleep(Duration::from_millis(300));
    println!("WORKER {worker} {} {}", pid(&sup, id).unwrap(), sup.log().len());
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(target_os = "linux")]
#[test]
fn a_dead_host_leaves_no_worker_behind() {
    use std::io::BufRead as _;
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "orphan_host", "--nocapture", "--test-threads", "1"])
        .env("OMUZAKI_SUPERVISOR_HOST", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = std::io::BufReader::new(host.stdout.take().unwrap());
    let line = stdout.lines().map_while(Result::ok).find(|l| l.contains("WORKER ")).unwrap();
    let mut words = line.split_whitespace().skip_while(|w| *w != "WORKER").skip(1);
    let mut number = || -> u32 { words.next().unwrap().parse().unwrap() };
    let (worker, again, restarts) = (number(), number(), number());
    assert_eq!((worker, restarts), (again, 0), "the worker outlived the thread that started the supervisor");
    assert!(alive(worker));
    host.kill().unwrap();
    host.wait().unwrap();
    // Not our child: it is gone once it has exited and init has reaped it.
    let gone = wait_for(Duration::from_secs(2), || !alive(worker));
    assert!(gone.is_some(), "worker {worker} outlived its host");
}

// ── a kill that never finishes, and thread workers ──────────────────────────────────────────────────────────────

/// A process-like worker that never beats and ignores kills until released.
struct Stubborn {
    spawns: Arc<AtomicU32>,
    release: Arc<AtomicBool>,
}

struct StubbornHandle {
    heartbeat: HeartbeatCell,
    release: Arc<AtomicBool>,
}

struct Nowhere;

impl ControlSend for Nowhere {
    fn send(&self, _: &HostMsg, _: &[&ChannelHandle]) -> Result<(), LaunchError> {
        Ok(())
    }
}

impl WorkerLauncher for Stubborn {
    fn kind(&self) -> WorkerKind {
        WorkerKind::Audio
    }

    fn spawn(&self, _: WorkerId, _: u32, _: Waker) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(StubbornHandle { heartbeat: HeartbeatCell::new(), release: Arc::clone(&self.release) }))
    }
}

impl WorkerHandle for StubbornHandle {
    fn heartbeat(&self) -> &HeartbeatCell {
        &self.heartbeat
    }
    fn try_wait(&mut self) -> Option<ExitInfo> {
        self.release.load(Ordering::SeqCst).then_some(ExitInfo { code: None })
    }
    fn kill(&mut self) {}
    fn needs_hello(&self) -> bool {
        false
    }
    fn start(&mut self) -> Result<(), LaunchError> {
        Ok(())
    }
    fn control(&self) -> Arc<dyn ControlSend> {
        Arc::new(Nowhere)
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
    fn revocable(&self) -> bool {
        false
    }
    fn fault(&self) -> Option<Fault> {
        None
    }
}

#[test]
fn a_kill_that_never_finishes_is_reported_stuck_and_never_doubled() {
    let spawns = Arc::new(AtomicU32::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let mut audio = spec("audio", WorkerKind::Audio);
    audio.start_grace = Seconds(0.2);
    let launcher = Stubborn { spawns: Arc::clone(&spawns), release: Arc::clone(&release) };
    let sup = start(vec![(audio, Box::new(launcher))], Arc::new(Calls::default()));
    let events = sup.events();
    let stuck = wait_for(Duration::from_secs(3), || events.try_iter().any(|e| matches!(e, HealthEvent::Stuck { .. })));
    assert!(stuck.is_some(), "no Stuck event");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(spawns.load(Ordering::SeqCst), 1, "a second instance while the first lives");
    release.store(true, Ordering::SeqCst);
    wait_for(Duration::from_secs(2), || spawns.load(Ordering::SeqCst) == 2).unwrap();
    sup.stop();
}

#[test]
fn thread_workers_restart_after_a_panic_and_are_abandoned_when_hung() {
    let runs = Arc::new(AtomicU32::new(0));
    let count = Arc::clone(&runs);
    let panicky = ThreadLauncher::new(WorkerKind::Audio, move |w| {
        let run = count.fetch_add(1, Ordering::SeqCst) + 1;
        w.beat();
        std::thread::sleep(Duration::from_millis(100));
        assert!(run > 1, "the first run panics");
        while !w.should_stop() {
            w.beat();
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let hangs = Arc::new(AtomicU32::new(0));
    let hung = Arc::clone(&hangs);
    let hanging = ThreadLauncher::new(WorkerKind::Adapter, move |w| {
        // The first run beats for 300 ms, then hangs (ignoring its stop flag, as a stuck thread would) for 3 s.
        let first = hung.fetch_add(1, Ordering::SeqCst) == 0;
        let start = Instant::now();
        while !w.should_stop() && (!first || start.elapsed() < Duration::from_millis(300)) {
            w.beat();
            std::thread::sleep(Duration::from_millis(20));
        }
        if first {
            std::thread::sleep(Duration::from_secs(3));
        }
    });
    let sup = start(
        vec![
            (spec("panicky", WorkerKind::Audio), Box::new(panicky)),
            (spec("hanging", WorkerKind::Adapter), Box::new(hanging)),
        ],
        Arc::new(Calls::default()),
    );
    let (p, h) = (sup.ids()[0], sup.ids()[1]);
    let events = sup.events();
    wait_for(Duration::from_secs(4), || runs.load(Ordering::SeqCst) >= 2 && health(&sup, p) == Health::Healthy)
        .unwrap();
    let crash = sup.log().into_iter().find(|r| r.worker == p).unwrap();
    assert_eq!(crash.reason, RestartReason::crashed(Some(101)));
    let mut seen = Vec::new();
    wait_for(Duration::from_secs(4), || {
        seen.extend(events.try_iter());
        seen.iter().any(|e| matches!(e, HealthEvent::Abandoned { .. }))
    })
    .unwrap();
    let record = sup.log().into_iter().find(|r| r.worker == h).unwrap();
    assert!(matches!(record.reason, RestartReason::Hung { .. }), "{:?}", record.reason);
    wait_for(Duration::from_secs(2), || hangs.load(Ordering::SeqCst) >= 2 && health(&sup, h) == Health::Healthy)
        .unwrap();
    assert_eq!(seen.iter().filter(|e| matches!(e, HealthEvent::Abandoned { .. })).count(), 1);
    sup.stop();
}

#[test]
fn a_launcher_of_another_kind_is_refused() {
    let launcher = ThreadLauncher::new(WorkerKind::Audio, |_| {});
    let err = Supervisor::start(
        vec![(spec("tracker", WorkerKind::Tracker), Box::new(launcher))],
        Arc::new(Calls::default()),
        Arc::new(SystemClock::new()),
        config(),
    )
    .unwrap_err();
    assert!(err.is_permanent());
}
