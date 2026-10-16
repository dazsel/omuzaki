//! The conformance suite every [`Plugins`] implementation passes (design `plugins.md` §12): the mock with
//! [`MockGuest`]s, and the wasmtime host with WAT components.
//!
//! A host takes part by implementing [`Subject`], which builds its service from canned guest [`Behaviour`]s: the mock
//! maps each to closures, the wasmtime host to a component. The suite is split into groups so a host runs every group
//! it can serve; [`run`] runs them all. The groups that need the op bridge take an [`OpSubject`].
//!
//! Each case panics with what it expected on failure, like a test.
#![allow(clippy::missing_panics_doc)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{Actor, EndReason, Scope, Sessions, Transport};
use omuzaki_plugin_api::grants::Approval;
use omuzaki_plugin_api::limits::RESTART_BACKOFF_SECONDS;
use omuzaki_plugin_api::manifest::Manifest;
use omuzaki_plugin_api::state::{DisabledReason, FaultKind, LogLevel, PluginState};
use omuzaki_plugin_api::{PluginError, PluginId, Plugins};

use crate::{MockOp, MockSessions, ScriptedCall};

/// What a test plugin does in `init`.
#[derive(Clone, Debug, PartialEq)]
pub enum Behaviour {
    /// Returns `ok`.
    Ok,
    /// Returns `err` with this text.
    InitError(String),
    /// Executes `unreachable`.
    Trap,
    /// Loops forever, so it runs out of fuel (the mock traps out of fuel at once).
    Loop,
    /// Logs `line 0` to `line <n-1>` at `info`, then returns `ok`.
    Logs(u32),
    /// Works long enough for another thread to ask for a reload (the mock sleeps 50 ms; a wasm guest loops within its
    /// CPU share), logs `init done` and returns `ok`.
    Slow,
    /// Logs `flood` this many times at `info`, then returns `ok`: past the log's burst.
    Flood(u32),
    /// Returns `ok` from `init`; its `shutdown` logs `bye` at `info`.
    ByeOnShutdown,
    /// Works like [`Behaviour::Slow`], logs `init failing` and returns `err("slow failure")`.
    SlowFailure,
    /// Calls each op in order, logs `<op>: ok` or `<op>: <code>`, returns `ok`; `approval-resolved` logs
    /// `approval <n>: ok` or `approval <n>: <code>`. Needs the op bridge.
    Calls(Vec<ScriptedCall>),
}

/// The person who enables plugins in the suite.
pub fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

/// An agent, refused every management call.
pub fn agent() -> Actor {
    Actor::Agent { id: "agent#1".into(), model: None }
}

/// A valid manifest for `id`, asking for `scopes`.
pub fn manifest(id: &str, scopes: &[Scope]) -> Manifest {
    let text = format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\napi = \"^0.1\"\nlicence = \"MIT\"\nname = \"{id}\"\ncomponent = \"plugin.wasm\"\n"
    );
    let mut manifest = match Manifest::parse(&text) {
        Ok(manifest) => manifest,
        Err(error) => panic!("the suite's manifest for `{id}` parses: {error}"),
    };
    manifest.requests.scopes = scopes.to_vec();
    manifest
}

/// The shared pieces a subject builds its service on.
pub struct World {
    /// The runner's clock.
    pub clock: Arc<FakeClock>,
    /// The session registry the service opens plugin sessions in.
    pub sessions: Arc<MockSessions>,
}

impl World {
    /// A clock at 100 s and an empty registry.
    pub fn new() -> Self {
        let clock = Arc::new(FakeClock::new(Seconds(100.0)));
        let sessions = Arc::new(MockSessions::new(clock.clone()));
        Self { clock, sessions }
    }

    fn clock_now(&self) -> f64 {
        use omuzaki_core::time::Clock as _;
        self.clock.now().0
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// A `Plugins` implementation under test.
pub trait Subject {
    /// The service.
    type Service: Plugins + 'static;

    /// A service on `world` with each plugin installed, off, and everything its manifest asks for approved.
    fn build(&self, world: &World, plugins: Vec<(Manifest, Behaviour)>) -> Self::Service;
}

/// A subject whose guests can call ops (the op bridge).
pub trait OpSubject: Subject {
    /// Registers an op the guests may call.
    fn add_op(&self, service: &Self::Service, name: &str, op: MockOp);

    /// Decides the queued request `approval`, as the person.
    ///
    /// # Errors
    /// The service's refusal.
    fn resolve(&self, service: &Self::Service, approval: u64, approve: bool) -> Result<(), PluginError>;
}

fn id(text: &str) -> PluginId {
    match PluginId::new(text) {
        Ok(id) => id,
        Err(error) => panic!("{error}"),
    }
}

fn info<P: Plugins>(plugins: &P, name: &str) -> omuzaki_plugin_api::state::PluginInfo {
    match plugins.info(&id(name)) {
        Ok(info) => info,
        Err(error) => panic!("info({name}): {error}"),
    }
}

fn log_texts<P: Plugins>(plugins: &P, name: &str) -> Vec<String> {
    match plugins.log(&id(name), 0, usize::MAX) {
        Ok(lines) => lines.into_iter().map(|l| l.text).collect(),
        Err(error) => panic!("log({name}): {error}"),
    }
}

/// Every group.
pub fn run<S: OpSubject>(subject: &S) {
    lifecycle(subject);
    faults(subject);
    restart_and_disable(subject);
    reload_drains(subject);
    concurrent_enables(subject);
    logs(subject);
    ceiling_and_queueing(subject);
}

/// States (design §6.2), the person-only rule and the plugin session (§3.1).
pub fn lifecycle<S: Subject>(subject: &S) {
    let world = World::new();
    let plugins = subject.build(
        &world,
        vec![(manifest("zeta", &[Scope::Read]), Behaviour::Ok), (manifest("alpha", &[Scope::Read]), Behaviour::Ok)],
    );
    let ids: Vec<String> = plugins.list().into_iter().map(|p| p.id.to_string()).collect();
    assert_eq!(ids, ["alpha", "zeta"], "list is sorted by id");
    assert_eq!(plugins.info(&id("nope")), Err(PluginError::NotFound));
    assert_eq!(info(&plugins, "alpha").state, PluginState::Installed, "nothing runs before it is enabled");

    for actor in [agent(), Actor::Plugin { id: "zeta#1".into() }, Actor::System] {
        assert_eq!(plugins.enable(&id("alpha"), &actor), Err(PluginError::denied(Scope::EditSettings)), "{actor:?}");
        assert_eq!(plugins.reload(&id("alpha"), &actor), Err(PluginError::denied(Scope::EditSettings)));
        assert_eq!(
            plugins.set_approval(&id("alpha"), Approval::default(), &actor),
            Err(PluginError::denied(Scope::EditSettings))
        );
    }
    assert_eq!(info(&plugins, "alpha").state, PluginState::Installed, "a refused call changes nothing");
    assert_eq!(world.sessions.sessions(), []);

    let running = match plugins.enable(&id("alpha"), &person()) {
        Ok(info) => info,
        Err(error) => panic!("enable: {error}"),
    };
    assert_eq!(running.state, PluginState::Running);
    let Some(session_id) = running.session else { panic!("a running plugin has a session") };
    let Some(session) = world.sessions.session(session_id) else { panic!("the session is registered") };
    assert_eq!(session.transport, Transport::Plugin);
    assert_eq!(session.on_behalf_of, person());
    assert_eq!(session.client, "plugin:alpha@0.1.0");
    assert_eq!(session.actor, Actor::Plugin { id: format!("alpha#{}", session_id.0) });
    assert_eq!(session.grants.scopes().collect::<Vec<_>>(), [Scope::Read]);
    assert_eq!(
        plugins.list().iter().find(|p| p.id == id("alpha")).map(|p| p.state.clone()),
        Some(PluginState::Running)
    );

    assert_eq!(plugins.disable(&id("alpha"), &agent()), Err(PluginError::denied(Scope::EditSettings)));
    let off = match plugins.disable(&id("alpha"), &person()) {
        Ok(info) => info,
        Err(error) => panic!("disable: {error}"),
    };
    assert_eq!((off.state, off.session), (PluginState::disabled(DisabledReason::Person), None));
    assert_eq!(world.sessions.end_reasons(), [(session_id, EndReason::Closed)]);

    let again = plugins.enable(&id("alpha"), &person()).map(|i| i.session);
    assert!(matches!(again, Ok(Some(s)) if s != session_id), "a restart opens a new session: {again:?}");

    // Narrowing the approval narrows the running session at once.
    let narrowed = plugins.set_approval(&id("alpha"), Approval::default(), &person()).map(|i| i.approved);
    assert_eq!(narrowed, Ok(Vec::new()));
    let Some(Some(session)) = plugins.info(&id("alpha")).ok().map(|i| i.session) else { panic!("still running") };
    assert_eq!(world.sessions.session(session).map(|s| s.grants.scopes().count()), Some(0));
}

/// Traps, `init` errors and running out of fuel fault the plugin and end its session `Faulted` (design §4.4).
pub fn faults<S: Subject>(subject: &S) {
    let world = World::new();
    let plugins = subject.build(
        &world,
        vec![
            (manifest("trapper", &[]), Behaviour::Trap),
            (manifest("looper", &[]), Behaviour::Loop),
            (manifest("refuser", &[]), Behaviour::InitError("no thanks".into())),
            (manifest("fine", &[]), Behaviour::Ok),
        ],
    );
    let cases = [("trapper", FaultKind::Trap), ("looper", FaultKind::OutOfFuel), ("refuser", FaultKind::InitError)];
    for (name, fault) in cases {
        assert_eq!(plugins.enable(&id(name), &person()).map(|i| i.state), Err(PluginError::Fault(fault)), "{name}");
        let after = info(&plugins, name);
        assert_eq!((after.state, after.session), (PluginState::faulted(fault), None), "{name}");
        assert_eq!(after.faults.len(), 1, "{name}");
        assert_eq!(after.restart_at, Some(Seconds(world.clock_now() + RESTART_BACKOFF_SECONDS[0])), "{name}");
    }
    let details: Vec<String> = world
        .sessions
        .end_reasons()
        .into_iter()
        .map(|(_, reason)| match reason {
            EndReason::Faulted { detail, .. } => detail,
            other => panic!("a fault ends the session Faulted, not {other:?}"),
        })
        .collect();
    assert_eq!(details.len(), 3);
    assert!(details[0].contains("in init"), "{}", details[0]);
    assert!(details[1].contains("in init"), "the fuel cap or the CPU share, in init: {}", details[1]);
    assert!(details[2].contains("no thanks"), "{}", details[2]);
    assert_eq!(
        plugins.enable(&id("fine"), &person()).map(|i| i.state),
        Ok(PluginState::Running),
        "another plugin still loads and runs after the faults"
    );
}

/// Restart backoff of 1 s, then 10 s, and auto-disable at the third fault within 10 minutes (design §4.4).
pub fn restart_and_disable<S: Subject>(subject: &S) {
    let world = World::new();
    let plugins = subject.build(&world, vec![(manifest("flaky", &[]), Behaviour::Trap)]);
    let flaky = id("flaky");
    assert!(plugins.enable(&flaky, &person()).is_err());
    assert_eq!(plugins.run_due(), Vec::<PluginId>::new(), "not before the backoff");
    world.clock.advance(Seconds(RESTART_BACKOFF_SECONDS[0] - 0.01));
    assert_eq!(plugins.run_due(), Vec::<PluginId>::new());
    world.clock.advance(Seconds(0.01));
    assert_eq!(plugins.run_due(), std::slice::from_ref(&flaky), "restarted after 1 s");
    let second = info(&plugins, "flaky");
    assert_eq!(second.state, PluginState::faulted(FaultKind::Trap));
    assert_eq!(second.restart_at, Some(Seconds(world.clock_now() + RESTART_BACKOFF_SECONDS[1])), "then 10 s");
    world.clock.advance(Seconds(RESTART_BACKOFF_SECONDS[1]));
    assert_eq!(plugins.run_due(), std::slice::from_ref(&flaky));
    let third = info(&plugins, "flaky");
    assert_eq!(third.state, PluginState::disabled(DisabledReason::Faults), "the third fault disables it");
    assert_eq!((third.restart_at, third.faults.len()), (None, 3));
    world.clock.advance(Seconds(3600.0));
    assert_eq!(plugins.run_due(), Vec::<PluginId>::new(), "only the person turns it back on");
    assert_eq!(world.sessions.end_reasons().len(), 3, "each start had its own session, each ended Faulted");
}

/// A reload waits for the running call, ends the old session `Closed` and starts a new one (design §6.3).
pub fn reload_drains<S: Subject>(subject: &S)
where
    S::Service: Send + Sync,
{
    let world = World::new();
    let plugins = Arc::new(subject.build(&world, vec![(manifest("slow", &[]), Behaviour::Slow)]));
    let slow = id("slow");
    let enabling = {
        let plugins = plugins.clone();
        let slow = slow.clone();
        std::thread::spawn(move || plugins.enable(&slow, &person()))
    };
    let started = Instant::now();
    while plugins.info(&slow).map(|i| i.state) != Ok(PluginState::Loading) && started.elapsed() < Duration::from_secs(5)
    {
        if plugins.info(&slow).map(|i| i.state) == Ok(PluginState::Running) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let reloaded = plugins.reload(&slow, &person());
    let first = enabling.join().unwrap_or_else(|_| panic!("the enabling thread panicked"));
    assert_eq!(first.map(|i| i.state), Ok(PluginState::Running), "the running init finished, not cut short");
    let Ok(reloaded) = reloaded else { panic!("reload: {reloaded:?}") };
    assert_eq!(reloaded.state, PluginState::Running);
    let ends = world.sessions.end_reasons();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].1, EndReason::Closed, "the old session closed");
    assert_ne!(reloaded.session, Some(ends[0].0), "with a new session");
    let done = log_texts(plugins.as_ref(), "slow").iter().filter(|t| *t == "init done").count();
    assert_eq!(done, 2, "both instances finished init");
}

/// Enables of one plugin from several threads at once start it once: one session, one instance (design §6.3).
pub fn concurrent_enables<S: Subject>(subject: &S) {
    let world = World::new();
    let plugins = Arc::new(subject.build(&world, vec![(manifest("slow", &[]), Behaviour::Slow)]));
    let slow = id("slow");
    let gate = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (plugins, slow, gate) = (plugins.clone(), slow.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                plugins.enable(&slow, &person())
            })
        })
        .collect();
    let results: Vec<_> =
        threads.into_iter().map(|t| t.join().unwrap_or_else(|_| panic!("an enabling thread panicked"))).collect();
    let sessions: Vec<_> = results
        .iter()
        .map(|r| match r {
            Ok(info) => (info.state.clone(), info.session),
            Err(error) => panic!("enable: {error}"),
        })
        .collect();
    assert!(sessions.iter().all(|s| s.0 == PluginState::Running && s.1.is_some()), "{sessions:?}");
    assert!(sessions.iter().all(|s| s.1 == sessions[0].1), "every enable sees the one session: {sessions:?}");
    let done = log_texts(plugins.as_ref(), "slow").iter().filter(|t| *t == "init done").count();
    assert_eq!(done, 1, "one instance ran init");
    assert!(plugins.disable(&slow, &person()).is_ok());
    let ends = world.sessions.end_reasons();
    assert_eq!(ends.len(), 1, "no session was left open: {ends:?}");

    // The same when the start fails: every waiter gets the one start's fault, and `init` ran once.
    let world = World::new();
    let plugins = Arc::new(subject.build(&world, vec![(manifest("failing", &[]), Behaviour::SlowFailure)]));
    let failing = id("failing");
    let gate = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (plugins, failing, gate) = (plugins.clone(), failing.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                plugins.enable(&failing, &person())
            })
        })
        .collect();
    for thread in threads {
        let result = thread.join().unwrap_or_else(|_| panic!("an enabling thread panicked"));
        assert_eq!(result, Err(PluginError::Fault(FaultKind::InitError)), "every waiter gets the settled fault");
    }
    let runs = log_texts(plugins.as_ref(), "failing").iter().filter(|t| *t == "init failing").count();
    assert_eq!(runs, 1, "one failing start, not one per caller");
    let faults = plugins.info(&failing).map(|i| i.faults.len());
    assert_eq!(faults, Ok(1), "one fault toward the auto-disable count");
}

/// Log lines keep their order and page by sequence number.
pub fn logs<S: Subject>(subject: &S) {
    let world = World::new();
    let plugins = subject.build(&world, vec![(manifest("talker", &[]), Behaviour::Logs(5))]);
    let talker = id("talker");
    assert!(plugins.enable(&talker, &person()).is_ok());
    let all = match plugins.log(&talker, 0, 100) {
        Ok(lines) => lines,
        Err(error) => panic!("{error}"),
    };
    let texts: Vec<&str> = all.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["line 0", "line 1", "line 2", "line 3", "line 4"]);
    assert!(all.windows(2).all(|w| w[0].seq < w[1].seq), "sequence numbers rise");
    assert!(all.iter().all(|l| l.level == LogLevel::Info && !l.dry_run));
    let page = plugins.log(&talker, all[1].seq, 2).map(|p| p.into_iter().map(|l| l.text).collect::<Vec<_>>());
    assert_eq!(page, Ok(vec!["line 2".to_owned(), "line 3".to_owned()]));
    assert_eq!(plugins.log(&id("nobody"), 0, 1), Err(PluginError::NotFound));

    // A flood that ends with the run still says how much it lost, and `shutdown`'s lines are kept.
    let plugins = subject.build(
        &world,
        vec![(manifest("flood", &[]), Behaviour::Flood(150)), (manifest("bye", &[]), Behaviour::ByeOnShutdown)],
    );
    for name in ["flood", "bye"] {
        assert!(plugins.enable(&id(name), &person()).is_ok(), "enable {name}");
        assert!(plugins.disable(&id(name), &person()).is_ok(), "disable {name}");
    }
    let flood = log_texts(&plugins, "flood");
    assert_eq!(flood.len(), 101, "the burst and one notice: {:?}", flood.last());
    assert_eq!(flood.last().map(String::as_str), Some("50 lines dropped"), "flushed when the plugin stopped");
    assert_eq!(log_texts(&plugins, "bye"), ["bye"], "the line `shutdown` logged");
}

/// The ceiling (design §3.2) and per-use queueing with `approval-resolved` (§2.2).
pub fn ceiling_and_queueing<S: OpSubject>(subject: &S) {
    let world = World::new();
    let call = |op: &str| ScriptedCall { op: op.into(), args: serde_json::json!({}) };
    let script = vec![call("test.read"), call("test.edit"), call("test.publish"), call("test.missing")];
    let plugins = subject.build(
        &world,
        vec![(manifest("caller", &[Scope::Read, Scope::EditScene, Scope::Publish]), Behaviour::Calls(script))],
    );
    for (name, scope) in [("test.read", Scope::Read), ("test.edit", Scope::EditScene), ("test.publish", Scope::Publish)]
    {
        subject.add_op(&plugins, name, MockOp { scope, output: Ok("{}".into()) });
    }
    let caller = id("caller");
    let approval = Approval::new(vec![Scope::Read, Scope::Publish], Vec::new(), None);
    assert!(plugins.set_approval(&caller, approval, &person()).is_ok());
    assert_eq!(plugins.enable(&caller, &person()).map(|i| i.state), Ok(PluginState::Running));
    let texts = log_texts(&plugins, "caller");
    for expected in ["test.read: ok", "test.edit: denied", "test.publish: waiting", "test.missing: not_found"] {
        assert!(texts.iter().any(|t| t == expected), "{expected} in {texts:?}");
    }
    assert!(texts.iter().any(|t| t.starts_with("test.edit denied")), "a refused call adds a warn line: {texts:?}");
    assert_eq!(subject.resolve(&plugins, 1, true), Ok(()));
    assert!(log_texts(&plugins, "caller").iter().any(|t| t == "approval 1: ok"), "approval-resolved heard the outcome");
    assert_eq!(subject.resolve(&plugins, 1, true), Err(PluginError::NotFound), "decided exactly once");
}
