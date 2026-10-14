//! `agent.connection`: whether an agent client is connected to this studio, and how many of its sessions are live (the
//! setup wizard's probe, first-run design §5). It names the client from its handshake and counts sessions. It holds no
//! credential and returns none: an entry is a session serial and a client name, nothing else.

use std::collections::BTreeMap;
use std::sync::Mutex;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::events::lock;
use crate::server::McpServer;

/// The live sessions of one server, keyed by session serial. A session is listed from the moment its handler exists;
/// its value is the client's name once the handshake has given one, and `None` until then. A session leaves the list
/// when its connection ends.
#[derive(Default)]
pub(crate) struct Connections(Mutex<BTreeMap<u64, Option<String>>>);

impl Connections {
    /// A session has begun, before its client has said who it is.
    pub(crate) fn open(&self, serial: u64) {
        lock(&self.0).insert(serial, None);
    }

    /// The session's client named itself in the handshake.
    pub(crate) fn name(&self, serial: u64, client: &str) {
        if let Some(name) = lock(&self.0).get_mut(&serial) {
            *name = Some(client.to_owned());
        }
    }

    /// The session's connection ended, so it no longer counts.
    pub(crate) fn close(&self, serial: u64) {
        lock(&self.0).remove(&serial);
    }

    /// The `agent.connection` answer for the sessions listed now.
    pub(crate) fn answer(&self) -> Value {
        answer(&lock(&self.0))
    }
}

/// `{connected, client_name?, sessions}`: `connected` means some live session has a named client, `client_name` is
/// the newest such client (a later session supersedes an earlier one), and `sessions` counts every live session.
/// `client_name` is left out when nothing is connected.
fn answer(sessions: &BTreeMap<u64, Option<String>>) -> Value {
    let client = sessions.values().rev().find_map(Option::as_deref);
    let mut out = json!({"connected": client.is_some(), "sessions": sessions.len()});
    if let Some(name) = client {
        out["client_name"] = json!(name);
    }
    out
}

/// What the `agent.connection` op reads from the host: the connection state of the server it runs under. The app
/// inserts an implementation into its services, and [`McpServer`] is the one this crate provides.
pub trait ConnectionService: Send + Sync + 'static {
    /// The `{connected, client_name?, sessions}` answer for the sessions live now.
    fn connection(&self) -> Value;
}

impl ConnectionService for McpServer {
    fn connection(&self) -> Value {
        McpServer::connection(self)
    }
}

/// `agent.connection`: whether an agent client is connected, its name, and how many sessions are live. The setup
/// wizard reads `connected` to choose between "Let an assistant set this up" and "Connect an agent". Takes no
/// arguments, changes nothing, and never returns a credential.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentConnection {}

impl Op for AgentConnection {
    fn meta() -> OpMeta {
        OpMeta {
            name: "agent.connection",
            summary: "Say whether an agent client is connected, its client name, and how many sessions are live.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.service::<dyn ConnectionService>()?.connection()))
    }
}

register_op!(AgentConnection);

#[cfg(test)]
mod tests {
    use super::*;

    /// The keys an answer may carry. A scan for anything else keeps tokens and other secrets out of this query.
    const ALLOWED_KEYS: [&str; 3] = ["connected", "client_name", "sessions"];

    #[test]
    fn no_client_is_not_connected_and_names_no_one() {
        let connections = Connections::default();
        assert_eq!(connections.answer(), json!({"connected": false, "sessions": 0}));
    }

    #[test]
    fn a_session_without_a_handshake_counts_but_is_not_connected() {
        let connections = Connections::default();
        connections.open(1);
        assert_eq!(connections.answer(), json!({"connected": false, "sessions": 1}));
    }

    #[test]
    fn one_connected_client_is_named() {
        let connections = Connections::default();
        connections.open(1);
        connections.name(1, "claude-code");
        assert_eq!(connections.answer(), json!({"connected": true, "client_name": "claude-code", "sessions": 1}));
    }

    #[test]
    fn the_newest_named_client_is_reported_and_all_sessions_count() {
        let connections = Connections::default();
        connections.open(1);
        connections.name(1, "claude-code");
        connections.open(2);
        connections.name(2, "codex");
        connections.open(3);
        assert_eq!(connections.answer(), json!({"connected": true, "client_name": "codex", "sessions": 3}));
    }

    #[test]
    fn a_closed_session_no_longer_counts() {
        let connections = Connections::default();
        connections.open(1);
        connections.name(1, "claude-code");
        connections.close(1);
        assert_eq!(connections.answer(), json!({"connected": false, "sessions": 0}));
    }

    #[test]
    fn naming_an_unknown_session_does_nothing() {
        let connections = Connections::default();
        connections.name(9, "ghost");
        assert_eq!(connections.answer(), json!({"connected": false, "sessions": 0}));
    }

    #[test]
    fn the_answer_carries_only_the_listed_keys() {
        let connections = Connections::default();
        connections.open(1);
        connections.name(1, "claude-code");
        let answer = connections.answer();
        let keys: Vec<&str> = answer.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
        assert!(keys.iter().all(|key| ALLOWED_KEYS.contains(key)), "unexpected keys in {answer}");
        assert_eq!(keys.len(), 3);
    }
}

#[cfg(test)]
mod op_tests {
    use std::sync::Arc;

    use omuzaki_core::time::{FakeClock, Seconds};
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{Actor, ErrorCode, Services, ServicesBuilder, find};

    use super::*;

    /// A host whose connection state the test sets up.
    struct Fake(Connections);

    impl ConnectionService for Fake {
        fn connection(&self) -> Value {
            self.0.answer()
        }
    }

    /// Services holding a fake host, with `setup` run against its state first.
    fn services(setup: impl FnOnce(&Connections)) -> Services {
        let connections = Connections::default();
        setup(&connections);
        ServicesBuilder::new().insert::<dyn ConnectionService>(Arc::new(Fake(connections))).build()
    }

    /// Decodes `args` through the op's registration and plans it, as the executor does.
    fn run(services: Option<&Services>, args: Value) -> Result<Value, OpError> {
        let (doc, actor, clock) =
            (MemDoc::default(), Actor::Agent { id: "t".to_owned(), model: None }, FakeClock::new(Seconds(0.0)));
        let mut ctx = OpCtx::new(&doc, &actor, &clock);
        if let Some(services) = services {
            ctx = ctx.with_services(services);
        }
        let reg = find("agent.connection").expect("the op registers itself");
        let plan = (reg.decode)(args)?.plan(&ctx)?;
        Ok(plan.output.expect("a query answers"))
    }

    #[test]
    fn the_op_is_a_read_query_that_changes_nothing() {
        let meta = AgentConnection::meta();
        assert_eq!(
            (meta.name, meta.kind, meta.scope, meta.undo),
            ("agent.connection", OpKind::Query, Scope::Read, Undo::None)
        );
    }

    #[test]
    fn with_no_client_nothing_is_connected() {
        let services = services(|_| {});
        assert_eq!(run(Some(&services), json!({})).unwrap(), json!({"connected": false, "sessions": 0}));
    }

    #[test]
    fn one_connected_client_is_named_and_counted() {
        let services = services(|c| {
            c.open(1);
            c.name(1, "claude-code");
        });
        assert_eq!(
            run(Some(&services), json!({})).unwrap(),
            json!({"connected": true, "client_name": "claude-code", "sessions": 1})
        );
    }

    #[test]
    fn a_closed_session_no_longer_counts() {
        let services = services(|c| {
            c.open(1);
            c.name(1, "claude-code");
            c.close(1);
        });
        assert_eq!(run(Some(&services), json!({})).unwrap(), json!({"connected": false, "sessions": 0}));
    }

    #[test]
    fn the_answer_carries_only_the_listed_keys() {
        let services = services(|c| {
            c.open(1);
            c.name(1, "claude-code");
        });
        let answer = run(Some(&services), json!({})).unwrap();
        let mut keys: Vec<&str> = answer.as_object().expect("an object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["client_name", "connected", "sessions"], "no credential or other field: {answer}");
    }

    #[test]
    fn a_missing_host_service_is_unavailable_not_a_panic() {
        let error = run(None, json!({})).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unavailable);
    }

    #[test]
    fn arguments_are_refused_because_the_op_takes_none() {
        let services = services(|_| {});
        let error = run(Some(&services), json!({"verbose": true})).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
    }
}
