//! The automation services (#774): serde round trips of `Status`, `ValuesSnapshot` and `ShowApprovals`, and each
//! trait usable as `Box<dyn ..>` and `Arc<dyn ..>` across threads.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventId, Source, UtcMillis};
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, PortName, ShowId, VarName};
use omuzaki_live_api::need::{ArgTemplate, Invocation, Need, NetScheme};
use omuzaki_live_api::services::{
    Approvals, ApprovedProgram, AutomationControl, Dropped, EventRow, Firing, GraphStatus, GroupRow, InertNode,
    InertReason, NodeRef, PersistScope, RunInfo, ShowApprovals, ShowKey, Status, SuspendReason, Suspension,
    SystemWallClock, ValuesSnapshot, VarRow, VarStoreError, VariableStore, WallClock,
};
use omuzaki_live_api::show::{VarScope, VarType};
use omuzaki_ops_api::{Actor, OpError, SessionId};
use omuzaki_store_api::ContentHash;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> T {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

/// A string the slug rules accept (no Windows device names such as `con` or `com1`).
fn slug() -> impl Strategy<Value = String> {
    "[a-z]{1,8}".prop_filter("a slug", |s| omuzaki_core::id::Slug::new(s.as_str()).is_ok())
}

fn id<T: std::str::FromStr>(s: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    s.parse().unwrap()
}

fn need() -> impl Strategy<Value = Need> {
    prop_oneof![
        Just(Need::Scope(omuzaki_ops_api::Scope::Broadcast)),
        "[a-z]{1,6}\\.[a-z_]{1,8}".prop_map(Need::op),
        ("[a-z0-9.]{1,12}", any::<u16>()).prop_map(|(host, port)| Need::host(NetScheme::Https, host, port)),
        ("\\PC{1,12}", any::<bool>()).prop_map(|(path, write)| Need::file(path, write)),
        ("\\PC{1,12}", "\\PC{0,6}").prop_map(|(program, arg)| Need::Program(
            Invocation::new(program)
                .with_args(vec![ArgTemplate::Literal(arg), ArgTemplate::Input(PortName::from_static("user"))])
        )),
        ("[a-z]{1,8}", "[a-z0-9.]{1,12}").prop_map(|(name, host)| Need::secret(name, host)),
        "\\PC{1,12}".prop_map(Need::process),
        slug().prop_map(|name| Need::variable(VarName::new(name).unwrap(), VarType::List)),
        slug().prop_map(|group| Need::group(GroupId::new(group).unwrap())),
    ]
}

fn graph_status() -> impl Strategy<Value = GraphStatus> {
    (slug(), any::<bool>(), any::<u64>(), 0..3_usize, proptest::collection::vec(need(), 0..2), -1.0e6..1.0e6_f64)
        .prop_map(|(graph, active, errors, which, needs, at)| {
            let status = GraphStatus::new(id(&graph))
                .with_active(active)
                .with_errors(errors)
                .with_faulted(vec![id("boom")])
                .with_inert(vec![
                    InertNode::new(id("ask"), InertReason::not_approved(needs)),
                    InertNode::new(id("gone"), InertReason::UnknownKind),
                ]);
            match which {
                0 => status,
                1 => status
                    .with_suspension(Suspension::new(SuspendReason::loop_guard(vec![id("a"), id("b")]), Seconds(at))),
                _ => status.with_suspension(Suspension::new(SuspendReason::EmitHops, Seconds(at))),
            }
        })
}

fn dropped() -> impl Strategy<Value = Dropped> {
    proptest::collection::vec(any::<u64>(), 8).prop_map(|n| {
        let mut dropped = Dropped::default();
        (dropped.rate, dropped.inbox, dropped.inbox_full, dropped.inert) = (n[0], n[1], n[2], n[3]);
        (dropped.inactive, dropped.reload, dropped.paused, dropped.producer) = (n[4], n[5], n[6], n[7]);
        dropped
    })
}

fn approvals() -> impl Strategy<Value = ShowApprovals> {
    (
        proptest::collection::vec(need(), 0..4),
        "\\PC{1,12}",
        any::<[u8; 32]>(),
        proptest::option::of(any::<[u8; 32]>()),
        any::<u64>(),
        any::<bool>(),
    )
        .prop_map(|(needs, program, hash, script, cap, autostart)| {
            let approvals = needs.into_iter().fold(ShowApprovals::default(), ShowApprovals::with_granted);
            approvals
                .with_program(ApprovedProgram::new(
                    Invocation::new(program).with_cwd("/tmp"),
                    ContentHash(hash),
                    script.map(ContentHash),
                ))
                .with_root("/streams")
                .with_spend_cap(cap)
                .with_autostart(autostart)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn status_round_trips(graphs in proptest::collection::vec(graph_status(), 0..3), dropped in dropped(),
                          missing in proptest::collection::vec(need(), 0..3), seed in any::<u32>(), paused in any::<bool>()) {
        let run = RunInfo::new(id("main"), seed, SessionId(7), Seconds(12.5));
        let status = Status::default().with_running(run).with_paused(paused).with_graphs(graphs).with_dropped(dropped).with_missing(missing);
        prop_assert_eq!(round_trip(&status), status);
    }

    #[test]
    fn values_snapshots_round_trip(graphs in proptest::collection::vec(graph_status(), 0..2), dropped in dropped(),
                                   text in "\\PC{0,300}", n in -1.0e6..1.0e6_f64) {
        let mut snapshot = ValuesSnapshot::new(Seconds(n));
        snapshot.variables = vec![
            VarRow::new(id("bonks"), VarScope::Show, VarType::Number, json!(n)),
            VarRow::new(id("last"), VarScope::Session, VarType::Text, json!(text.clone()))
                .with_change(Seconds(1.0), Some(NodeRef::new(id("bonk"), id("count")))),
        ];
        snapshot.groups = vec![GroupRow::new(id("redeems"), true)];
        snapshot.graphs = graphs;
        snapshot.firings = vec![Firing::new(Seconds(2.0), NodeRef::new(id("bonk"), id("line")), PortName::from_static("done"))
            .with_value(PortName::from_static("text"), &json!(text.clone()))];
        snapshot.events = vec![EventRow::new(EventId(3), EventKind::from_static("redeem"), Source::Obs, Seconds(1.5), text)];
        snapshot.dropped = dropped;
        prop_assert_eq!(round_trip(&snapshot), snapshot);
    }

    #[test]
    fn show_approvals_round_trip(approvals in approvals()) {
        prop_assert_eq!(round_trip(&approvals), approvals);
    }
}

#[test]
fn missing_lists_what_isnt_granted_and_never_declarations() {
    let host = Need::host(NetScheme::Http, "127.0.0.1", 7474);
    let secret = Need::secret("streamerbot", "127.0.0.1");
    let variable = Need::variable(id("bonks"), VarType::Number);
    let approvals = ShowApprovals::default().with_granted(host.clone());
    assert_eq!(approvals.missing([&host, &secret, &variable]), [secret]);
}

/// A stand-in engine, to prove the trait is object-safe and shareable.
struct Engine;

impl AutomationControl for Engine {
    fn start(&self, show: &ShowId, _: &Actor) -> Result<RunInfo, OpError> {
        Ok(RunInfo::new(show.clone(), 1, SessionId(1), Seconds(0.0)))
    }
    fn stop(&self, _: &Actor) -> Result<(), OpError> {
        Err(OpError::not_found("no show runs"))
    }
    fn pause(&self, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn resume(&self, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn status(&self) -> Status {
        Status::default()
    }
    fn values(&self) -> Option<Arc<ValuesSnapshot>> {
        Some(Arc::new(ValuesSnapshot::new(Seconds(0.0))))
    }
    fn set_variable(&self, _: &VarName, _: Value, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn switch_group(&self, _: &GroupId, _: bool, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn resume_graph(&self, _: &GraphId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn fire(&self, _: EventKind, _: Value, _: &Actor) -> Result<EventId, OpError> {
        Ok(EventId(1))
    }
}

struct Store;

impl Approvals for Store {
    fn show(&self, _: &ShowKey) -> ShowApprovals {
        ShowApprovals::default().with_autostart(true)
    }
    fn program_trusted(&self, _: &Invocation, _: &ContentHash, _: Option<&ContentHash>) -> bool {
        false
    }
}

impl VariableStore for Store {
    fn load(&self, _: &PersistScope) -> Result<BTreeMap<VarName, Value>, VarStoreError> {
        Ok(BTreeMap::new())
    }
    fn save(&self, _: &PersistScope, _: &BTreeMap<VarName, Value>) -> Result<(), VarStoreError> {
        Err(VarStoreError::Io("read-only".into()))
    }
}

fn shared<T: ?Sized + Send + Sync>(_: &Arc<T>) {}

#[test]
fn each_service_is_object_safe_and_shareable() {
    let control: Box<dyn AutomationControl> = Box::new(Engine);
    let control: Arc<dyn AutomationControl> = Arc::from(control);
    let approvals: Arc<dyn Approvals> = Arc::new(Store);
    let variables: Arc<dyn VariableStore> = Arc::new(Store);
    let wall: Arc<dyn WallClock> = Arc::new(SystemWallClock);
    shared(&control);
    shared(&approvals);
    shared(&variables);
    shared(&wall);

    let person = Actor::Person { name: "Kiri".into() };
    let worker = {
        let control = Arc::clone(&control);
        std::thread::spawn(move || control.start(&id("main"), &person).map(|run| run.seed))
    };
    assert_eq!(worker.join().unwrap(), Ok(1));
    assert!(control.values().is_some() && control.stop(&Actor::System).is_err());
    let key = ShowKey::new("/projects/demo", id("main"));
    assert!(approvals.show(&key).autostart);
    assert!(variables.load(&PersistScope::Show(key)).unwrap().is_empty());
    assert!(wall.now() > UtcMillis(0));
}
