//! The in-process link for the built-in assistant (design `agent-sessions.md` §10.1): the assistant is an MCP client of
//! this same server, over an in-memory stream, in a session of its own. It gets the same tools, results, heads-up,
//! stale-write guard, approvals, pause and limits as any external agent, with no second implementation, and no port
//! is opened.
//!
//! The host (the app) knows who the assistant is, so it says so: the session is opened in the registry at once, with
//! [`Transport::InApp`], and a name in the client's handshake never decides identity. Sub-agents are further links
//! with `parent` set.

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Actor, Budget, EndReason, Grants, Limits, Scope, SessionId, SessionSpec, Transport};
use tokio::io::DuplexStream;
use tokio::sync::watch;

use crate::agent::Link;
use crate::error::ServerError;
use crate::server::{McpServer, serve_session, with_roots};
use crate::session::Session;

/// How much the two ends of the stream may buffer before a writer waits.
const STREAM_BYTES: usize = 64 * 1024;

/// What the in-app host asks for when it opens a session for the assistant or a sub-agent.
///
/// The registry decides what the session may really do: the effective grants are the lesser of `grants` and what the
/// `on_behalf_of` person (or the `parent`) holds, and a spec that asks for more than its person or parent has is
/// refused by [`McpServer::connect_in_app`] before any stream exists.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct InAppSpec {
    /// The client's name as shown in the Agent panel and written into the agent's actor id (`assistant`).
    pub client: String,
    /// The model it runs, when known.
    pub model: Option<String>,
    /// The person the session works for.
    pub on_behalf_of: Actor,
    /// The ceiling of what the session may do.
    pub grants: Grants,
    /// What it may spend.
    pub budget: Budget,
    /// How fast it may edit and run.
    pub limits: Limits,
    /// The session that started this one, for a sub-agent: its ceiling never exceeds the parent's.
    pub parent: Option<SessionId>,
    /// What the session is for (`assistant`, a sub-agent's role).
    pub role: Option<String>,
    /// The parts of the project it may edit; empty means the whole project (design §5.3).
    pub fence: Vec<DocPath>,
}

impl InAppSpec {
    /// The assistant of the studio, working for `person`: it may read, edit and render (AG-01), and nothing that
    /// reaches outside the machine or spends money.
    #[must_use]
    pub fn assistant(person: Actor, model: Option<String>) -> Self {
        let grants = Grants::of([
            Scope::Read,
            Scope::EditCharacter,
            Scope::EditScene,
            Scope::EditLibrary,
            Scope::EditSettings,
            Scope::Render,
        ]);
        Self {
            client: "assistant".to_owned(),
            model,
            on_behalf_of: person,
            grants,
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: Some("assistant".to_owned()),
            fence: Vec::new(),
        }
    }

    fn session_spec(&self) -> SessionSpec {
        SessionSpec {
            transport: Transport::InApp,
            client: self.client.clone(),
            model: self.model.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
            grants: self.grants.clone(),
            budget: self.budget,
            limits: self.limits,
            parent: self.parent,
            role: self.role.clone(),
            fence: self.fence.clone(),
        }
    }
}

/// An open in-app session: the client end of its stream, and a way to learn that it ended.
///
/// The host runs rmcp's client on `client_io`. Dropping the link, or the stream, ends the session with
/// [`EndReason::Closed`], and its open transaction is abandoned through the registry.
#[derive(Debug)]
pub struct InAppLink {
    /// The client end of the in-memory stream the session is served on.
    pub client_io: DuplexStream,
    session: SessionId,
    ended: watch::Receiver<Option<EndReason>>,
}

/// A cloneable way to wait for an in-app session to end, usable after `client_io` has moved into a client.
#[derive(Clone, Debug)]
pub struct InAppEnd {
    ended: watch::Receiver<Option<EndReason>>,
}

impl InAppLink {
    /// The registry's id for the session.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Resolves with why the session ended, whatever the reason (the stream closed, a person revoked it, the project
    /// closed). Take [`InAppLink::end`] first if `client_io` is about to move.
    pub async fn closed(&self) -> EndReason {
        wait_for_end(self.ended.clone()).await
    }

    /// A handle that outlives a move of `client_io`.
    #[must_use]
    pub fn end(&self) -> InAppEnd {
        InAppEnd { ended: self.ended.clone() }
    }
}

impl InAppEnd {
    /// As [`InAppLink::closed`].
    pub async fn closed(&self) -> EndReason {
        wait_for_end(self.ended.clone()).await
    }
}

async fn wait_for_end(mut ended: watch::Receiver<Option<EndReason>>) -> EndReason {
    // The session's handler sets the reason before it goes, so a closed channel still holds it.
    let waited = ended.wait_for(Option::is_some).await.map(|reason| reason.clone());
    match waited {
        Ok(Some(reason)) => reason,
        _ => ended.borrow().clone().unwrap_or(EndReason::Closed),
    }
}

impl McpServer {
    /// Opens an in-app session now and serves it on an in-memory stream: the same tools, results, heads-up, stale-write
    /// guard, approvals, pause and limits as any session. The host runs an rmcp client on the returned
    /// [`InAppLink::client_io`]. No port is opened.
    ///
    /// The tool list is the session's effective grants, as the registry computed them.
    ///
    /// # Errors
    /// [`ServerError::NoRegistry`] if the server was built without a session registry, and
    /// [`ServerError::SessionRefused`] with the registry's reason (an unknown parent, a ceiling above the person's or
    /// the parent's), before any stream exists.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // async so the host awaits it like any link; opening is quick and local today
    pub async fn connect_in_app(&self, spec: InAppSpec) -> Result<InAppLink, ServerError> {
        let host = self.shared().sessions.clone().ok_or(ServerError::NoRegistry)?;
        // Before the open, so an end that follows at once is not missed.
        let after = self.shared().hub.latest();
        // The roots are part of the ceiling the registry narrows, or its effective grants would drop them.
        let mut session_spec = spec.session_spec();
        session_spec.grants = with_roots(session_spec.grants, &self.shared().roots);
        let info = host.sessions.open(session_spec).map_err(ServerError::SessionRefused)?;
        // What this session may do, not what the host asked: the tools and the guide are written for it.
        let server = self.with_credential(info.grants.clone(), None)?;
        let link = Link { id: Some(info.id), actor: info.actor.clone() };
        let session = Session::in_app(server.shared_arc(), server.view_arc(), server.next_serial(), link)
            .with_jobs(server.jobs());
        session.follow_registry(after);
        let ended = session.ended();
        let (client_io, server_io) = tokio::io::duplex(STREAM_BYTES);
        tokio::spawn(async move {
            if let Err(error) = serve_session(session, server_io).await {
                tracing::debug!(%error, "an in-app session ended abnormally");
            }
        });
        Ok(InAppLink { client_io, session: info.id, ended })
    }
}
