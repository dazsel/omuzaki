//! The watchdog policy to its acceptance (#1024): a renderer-like spec through crashes, hangs, crash loops, stability,
//! slow recoveries and a long log, plus property tests over random observation and tick sequences.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use omuzaki_core::time::Seconds;
use omuzaki_stream::watchdog::{
    Action, Health, HealthEvent, Monitor, Observation, RestartPolicy, RestartReason, WorkerId, WorkerKind, WorkerSpec,
};
use proptest::prelude::*;

const W: WorkerId = WorkerId(0);

/// No actions.
const NONE: [Action; 0] = [];

fn renderer() -> WorkerSpec {
    WorkerSpec {
        name: "renderer".to_owned(),
        kind: WorkerKind::Renderer,
        hold_outputs: true,
        start_grace: Seconds(5.0),
        hung_after: Seconds(0.5),
        restart: RestartPolicy {
            backoff_first: Seconds(0.25),
            backoff_max: Seconds(4.0),
            max_restarts: 5,
            within: Seconds(60.0),
            stable_after: Seconds(30.0),
        },
    }
}

/// A monitor over one renderer-like worker, plus the reusable action buffer.
struct Run {
    m: Monitor,
    out: Vec<Action>,
}

impl Run {
    fn new(spec: WorkerSpec) -> Self {
        Self { m: Monitor::new(vec![spec], Seconds(0.0)), out: Vec::with_capacity(16) }
    }

    fn obs(&mut self, at: f64, obs: Observation) -> Vec<Action> {
        self.m.observe(Seconds(at), W, obs, &mut self.out);
        self.out.clone()
    }

    fn tick(&mut self, at: f64) -> Vec<Action> {
        self.m.tick(Seconds(at), &mut self.out);
        self.out.clone()
    }

    fn spawned(&mut self, at: f64) -> Vec<Action> {
        self.obs(at, Observation::Spawned)
    }

    fn beat(&mut self, at: f64, n: u64) -> Vec<Action> {
        self.obs(at, Observation::Beat(n))
    }

    fn exited(&mut self, at: f64, code: Option<i32>) -> Vec<Action> {
        self.obs(at, Observation::Exited { code })
    }

    /// A healthy instance spawned at `at`, beating once at `at + 0.3`.
    fn healthy_at(&mut self, at: f64) {
        self.spawned(at);
        self.beat(at + 0.3, 1);
        assert_eq!(self.m.health(W), Health::Healthy);
    }
}

fn restarted(reason: RestartReason, attempt: u32) -> Action {
    Action::Event(HealthEvent::Restarted { worker: W, reason, attempt })
}

fn recovered(downtime: f64, slow: bool) -> Action {
    Action::Event(HealthEvent::Recovered { worker: W, downtime: Seconds(downtime), slow })
}

fn crashed(code: Option<i32>) -> RestartReason {
    RestartReason::Crashed { exit_code: code }
}

fn close(got: &[Action], want: &[Action]) -> bool {
    let near = |x: f64, y: f64| (x - y).abs() < 1e-9;
    got.len() == want.len()
        && got.iter().zip(want).all(|pair| match pair {
            (
                Action::Event(HealthEvent::Recovered { downtime: x, slow: s, .. }),
                Action::Event(HealthEvent::Recovered { downtime: y, slow: t, .. }),
            ) => near(x.0, y.0) && s == t,
            (
                Action::Event(HealthEvent::Restarted {
                    reason: RestartReason::Hung { silent_for: x }, attempt: a, ..
                }),
                Action::Event(HealthEvent::Restarted {
                    reason: RestartReason::Hung { silent_for: y }, attempt: b, ..
                }),
            ) => near(x.0, y.0) && a == b,
            (x, y) => x == y,
        })
}

#[test]
fn case_1_a_crash_holds_restarts_at_once_and_recovers() {
    let mut r = Run::new(renderer());
    r.spawned(0.0);
    assert_eq!(r.m.health(W), Health::Starting);
    r.beat(0.3, 1);
    assert_eq!(r.m.health(W), Health::Healthy);
    let mut t = 0.3;
    let mut n = 1;
    while t < 10.0 {
        t += 0.033;
        n += 1;
        assert_eq!(r.beat(t, n), NONE);
    }
    assert_eq!(r.exited(10.0, Some(137)), [Action::HoldOutputs(W), restarted(crashed(Some(137)), 1), Action::Spawn(W)]);
    assert_eq!(r.spawned(10.0), NONE);
    assert!(close(&r.beat(10.4, 1), &[Action::ReleaseOutputs(W), recovered(0.4, false)]));
}

#[test]
fn case_2_silence_is_a_hang_killed_then_respawned_after_the_exit() {
    let mut r = Run::new(renderer());
    r.healthy_at(0.0);
    r.beat(20.0, 2);
    assert_eq!(r.tick(20.49), NONE);
    let hung = RestartReason::Hung { silent_for: Seconds(0.5) };
    assert!(close(&r.tick(20.50), &[Action::HoldOutputs(W), Action::Kill(W), restarted(hung, 1)]));
    assert_eq!(r.tick(20.51), NONE, "no spawn before the exit");
    assert_eq!(r.exited(20.52, None), [Action::Spawn(W)]);
}

/// A crash loop from 30.0, each respawn dying 0.05 s after it starts; returns the run after the fifth spawn's death
/// is due.
fn crash_loop() -> Run {
    let mut r = Run::new(renderer());
    r.healthy_at(0.0);
    assert_eq!(r.exited(30.0, Some(3)), [Action::HoldOutputs(W), restarted(crashed(Some(3)), 1), Action::Spawn(W)]);
    r.spawned(30.0);
    let mut spawned_at = 30.0;
    for (attempt, due) in [(2, 30.30), (3, 30.85), (4, 31.90), (5, 33.95)] {
        assert_eq!(r.exited(spawned_at + 0.05, Some(3)), [restarted(crashed(Some(3)), attempt)], "attempt {attempt}");
        assert!(matches!(r.m.health(W), Health::Backoff { attempt: a, .. } if a == attempt));
        assert_eq!(r.tick(due - 0.01), NONE, "attempt {attempt} early");
        assert_eq!(r.tick(due), [Action::Spawn(W)], "attempt {attempt} due");
        r.spawned(due);
        spawned_at = due;
    }
    r
}

#[test]
fn case_3_a_crash_loop_backs_off_doubling() {
    crash_loop();
}

#[test]
fn case_4_the_sixth_crash_gives_up_until_a_person_restarts() {
    let mut r = crash_loop();
    assert_eq!(r.exited(34.0, Some(3)), [Action::Event(HealthEvent::GaveUp { worker: W, restarts: 5 })]);
    assert_eq!(r.m.health(W), Health::Failed { restarts: 5 });
    let mut t = 34.0;
    while t < 634.0 {
        t += 0.05;
        assert_eq!(r.tick(t), NONE, "no spawn at {t}");
    }
    r.m.request_restart(Seconds(t), W, &mut r.out);
    assert_eq!(r.out, [Action::Spawn(W)]);
    r.spawned(t);
    r.beat(t + 0.2, 1);
    assert_eq!(r.m.health(W), Health::Healthy);
    // The window is clear: a crash now is the second attempt after the requested one, not the seventh.
    assert_eq!(r.exited(t + 1.0, Some(3)), [Action::HoldOutputs(W), restarted(crashed(Some(3)), 2)]);
}

#[test]
fn case_5_a_stable_stretch_forgives_earlier_restarts() {
    let mut r = Run::new(renderer());
    r.healthy_at(0.0);
    r.exited(30.0, Some(1));
    r.spawned(30.0);
    r.beat(30.4, 1);
    let mut t = 30.4;
    let mut n = 1;
    while t < 99.9 {
        t += 0.1;
        n += 1;
        r.beat(t, n);
        r.tick(t);
    }
    assert_eq!(r.exited(100.0, Some(1)), [Action::HoldOutputs(W), restarted(crashed(Some(1)), 1), Action::Spawn(W)]);
}

#[test]
fn case_6_a_recovery_past_two_seconds_is_slow() {
    let mut r = Run::new(renderer());
    r.healthy_at(0.0);
    r.exited(50.0, Some(1));
    r.spawned(50.0);
    assert!(close(&r.beat(52.5, 1), &[Action::ReleaseOutputs(W), recovered(2.5, true)]));
}

#[test]
fn case_7_a_worker_that_never_beats_is_hung_after_the_start_grace() {
    let mut r = Run::new(renderer());
    assert_eq!(r.tick(0.0), [Action::Spawn(W)]);
    r.spawned(60.0);
    assert_eq!(r.tick(64.99), NONE);
    let hung = RestartReason::Hung { silent_for: Seconds(5.0) };
    assert!(close(&r.tick(65.0), &[Action::HoldOutputs(W), Action::Kill(W), restarted(hung, 1)]));
}

#[test]
fn case_8_the_log_keeps_the_newest_256_with_their_downtimes() {
    let mut spec = renderer();
    spec.restart.max_restarts = 10_000;
    spec.restart.within = Seconds(1e9);
    let mut r = Run::new(spec);
    r.healthy_at(0.0);
    let mut t = 1.0;
    for _ in 0..300 {
        r.exited(t, Some(9));
        let due = match r.m.health(W) {
            Health::Backoff { until, .. } => until.0,
            _ => t,
        };
        r.tick(due);
        r.spawned(due);
        r.beat(due + 0.1, 1);
        t = due + 1.0;
    }
    let log = r.m.log();
    assert_eq!(log.len(), 256);
    let records: Vec<_> = log.records().collect();
    assert!(records.iter().all(|rec| rec.reason == crashed(Some(9)) && rec.downtime.is_some()));
    let attempts: Vec<_> = records.iter().map(|rec| rec.attempt).collect();
    assert_eq!(attempts.first(), Some(&45), "the oldest kept is the 45th of 300");
    assert_eq!(attempts.last(), Some(&300));
}

// ── properties ─────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Step {
    Spawned,
    Beat,
    Exited,
    Tick,
    Restart,
}

fn step() -> impl Strategy<Value = (Step, f64)> {
    (
        prop_oneof![Just(Step::Spawned), Just(Step::Beat), Just(Step::Exited), Just(Step::Tick), Just(Step::Restart)],
        0.0..3.0f64,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn case_9_spawns_holds_and_states_stay_consistent(steps in prop::collection::vec(step(), 1..200)) {
        let mut m = Monitor::new(vec![renderer()], Seconds(0.0));
        let mut out = Vec::new();
        let (mut now, mut beats) = (0.0, 0_u64);
        let (mut alive, mut spawn_pending, mut held) = (false, false, false);
        for (step, dt) in steps {
            now += dt;
            let failed_before = matches!(m.health(W), Health::Failed { .. });
            let restart = matches!(step, Step::Restart);
            match step {
                Step::Spawned if spawn_pending => {
                    m.observe(Seconds(now), W, Observation::Spawned, &mut out);
                    (alive, spawn_pending, beats) = (true, false, 0);
                }
                Step::Beat if alive => {
                    beats += 1;
                    m.observe(Seconds(now), W, Observation::Beat(beats), &mut out);
                }
                Step::Exited if alive => {
                    m.observe(Seconds(now), W, Observation::Exited { code: Some(1) }, &mut out);
                    alive = false;
                }
                Step::Restart => m.request_restart(Seconds(now), W, &mut out),
                _ => m.tick(Seconds(now), &mut out),
            }
            for action in &out {
                match action {
                    Action::Spawn(_) => {
                        prop_assert!(!alive && !spawn_pending, "a second instance");
                        prop_assert!(!failed_before || restart, "a failed worker spawned");
                        spawn_pending = true;
                    }
                    Action::HoldOutputs(_) => {
                        prop_assert!(!held);
                        held = true;
                    }
                    Action::ReleaseOutputs(_) => {
                        prop_assert!(held);
                        held = false;
                    }
                    Action::Kill(_) | Action::Event(_) => {}
                }
            }
            let declared = matches!(
                m.health(W),
                Health::Starting
                    | Health::Healthy
                    | Health::Restarting { .. }
                    | Health::Backoff { .. }
                    | Health::Failed { .. }
            );
            prop_assert!(declared);
        }
    }
}
