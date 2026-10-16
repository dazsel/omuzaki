//! A connection as a registered agent session (design `agent-sessions.md` §3, §10): how it is admitted, how a call is
//! held through a pause or a rate limit, and how the session ends.
//!
//! The executor's registry is the authority. This module asks it for a session when a client connects, uses the
//! actor it answers with, and ends the session when the connection goes. It watches the event feed for the registry
//! ending the session first (a person revoked it, the project closed) so the connection can close too.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use omuzaki_ops_api::{Actor, EndReason, ErrorCode, ErrorDetail, Event, OpError, SessionId, SessionState, Sessions};
use rmcp::transport::streamable_http_server::SessionManager;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::events::{EventHub, lock};

/// How long a call refused because its session is paused is held, waiting for the person to resume it, before the
/// agent is told (design §4.3). A hold ties up one request, not a thread of the executor.
pub(crate) const PAUSE_HOLD: Duration = Duration::from_secs(20);

/// How long a call refused for its rate limit is slowed in all, before the agent is told it was limited. A burst over
/// the edit limit is slowed, not failed, as long as the wait is short.
pub(crate) const LIMIT_HOLD: Duration = Duration::from_secs(2);

/// What an HTTP access token contributes to a session: its label (which the registry records and the Agent panel
/// shows), the person who issued it, whether the studio has paired it, and how the port ends its sessions.
pub(crate) struct Credential {
    label: String,
    issuer: Actor,
    paired: AtomicBool,
    /// How long a session may be quiet before the port drops it, so a drop is told apart from a disconnect.
    idle_timeout: Duration,
    /// Why the port is closing this token's sessions, when it is the one closing them (revoke, new grants, shutdown).
    ending: Mutex<Option<EndReason>>,
    /// Asks the port to revoke this token. Set by the port; `None` for a credential nothing can revoke.
    revoke: Option<Box<dyn Fn() + Send + Sync>>,
    /// The table of this token's HTTP sessions, which is how one of them is closed from the inside.
    sessions: OnceLock<Arc<LocalSessionManager>>,
}

impl Credential {
    pub(crate) fn new(
        label: String,
        issuer: Actor,
        paired: bool,
        idle_timeout: Duration,
        revoke: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            label,
            issuer,
            paired: AtomicBool::new(paired),
            idle_timeout,
            ending: Mutex::new(None),
            revoke,
            sessions: OnceLock::new(),
        }
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn issuer(&self) -> &Actor {
        &self.issuer
    }

    pub(crate) fn paired(&self) -> bool {
        self.paired.load(Ordering::Acquire)
    }

    pub(crate) fn set_paired(&self, paired: bool) {
        self.paired.store(paired, Ordering::Release);
    }

    pub(crate) fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// The port is about to close this token's sessions for `reason`; each says so when it ends.
    pub(crate) fn will_end_with(&self, reason: EndReason) {
        *lock(&self.ending) = Some(reason);
    }

    /// Remembers the table of this token's sessions (set once, when the port builds the token's service).
    pub(crate) fn attach(&self, sessions: Arc<LocalSessionManager>) {
        let _ = self.sessions.set(sessions);
    }

    /// Closes the HTTP session `id` (the `Mcp-Session-Id` the client uses), as when the registry ended it.
    pub(crate) async fn close_session(&self, id: &str) {
        let Some(sessions) = self.sessions.get() else { return };
        if let Err(error) = sessions.close_session(&Arc::<str>::from(id)).await {
            tracing::debug!(%error, "couldn't close an HTTP session the registry ended");
        }
    }

    pub(crate) fn ending(&self) -> Option<EndReason> {
        lock(&self.ending).clone()
    }

    /// The registry revoked a session's credential: the token stops working and its other sessions end.
    pub(crate) fn revoke(&self) {
        if let Some(revoke) = &self.revoke {
            revoke();
        }
    }
}

/// A connection's registered session.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    /// The registry's id for it; `None` when the host wired no registry and the server names the agent itself.
    pub id: Option<SessionId>,
    /// The agent actor the registry built (or the server's own name for it, without a registry).
    pub actor: Actor,
}

/// How much earlier than the idle timeout a session may have gone quiet and still count as dropped for being idle: the
/// port's own clock starts a moment before this crate sees the request, so a dropped session is a little short of it.
fn idle_slack(idle_timeout: Duration) -> Duration {
    (idle_timeout / 10).min(Duration::from_millis(100))
}

/// Why a connection that went away ended its session: an in-app link is closed by its host; a token's sessions end for
/// the reason the port gave, or as idle when they were quiet for as long as the idle timeout; anything else
/// disconnected.
pub(crate) fn end_reason(in_app: bool, credential: Option<&Credential>, quiet_for: Duration) -> EndReason {
    if in_app {
        return EndReason::Closed;
    }
    match credential {
        Some(credential) => credential.ending().unwrap_or_else(|| {
            if quiet_for + idle_slack(credential.idle_timeout()) >= credential.idle_timeout() {
                EndReason::IdleTimeout
            } else {
                EndReason::Disconnected
            }
        }),
        None => EndReason::Disconnected,
    }
}

/// Starts the task that notices the registry ending session `id` and says so through `ended` (the connection then
/// closes), and revokes `credential` when the registry asked for that.
///
/// `after` is the event position from before the session was opened, so an end that happens at once is not missed. The
/// registry is asked too on every wake, because the event window is bounded.
pub(crate) fn watch_end(
    hub: Arc<EventHub>,
    sessions: Arc<dyn Sessions>,
    link: Link,
    mut after: u64,
    ended: watch::Sender<Option<EndReason>>,
    credential: Option<Arc<Credential>>,
    alive: CancellationToken,
) {
    tokio::spawn(async move {
        let mut arrivals = hub.watch();
        loop {
            let mut found = None;
            for (n, event) in hub.entries_after(after) {
                after = n;
                if let Event::SessionEnded { session, .. } = event
                    && Some(session.id) == link.id
                    && let SessionState::Ended { reason, .. } = session.state
                {
                    found = Some(reason);
                }
            }
            if found.is_none() {
                found = match sessions.session_of(&link.actor).map(|info| info.state) {
                    Some(SessionState::Ended { reason, .. }) => Some(reason),
                    _ => None,
                };
            }
            if let Some(reason) = found {
                let revoked = matches!(reason, EndReason::Revoked { credential: true, .. });
                ended.send_replace(Some(reason));
                if revoked && let Some(credential) = &credential {
                    credential.revoke();
                }
                return;
            }
            // The handler is gone (its connection ended and it ended the session itself): nothing left to follow.
            tokio::select! {
                () = alive.cancelled() => return,
                changed = arrivals.changed() => if changed.is_err() { return },
            }
        }
    });
}

/// Whether a refusal says the session is paused (the person may resume it).
pub(crate) fn is_paused(error: &OpError) -> bool {
    error.code == ErrorCode::Waiting && matches!(error.detail.as_deref(), Some(ErrorDetail::Paused { .. }))
}

/// How long a refused call should wait before it is retried because of a rate limit, or `None` when it is not one
/// or the wait is longer than what is left of `budget`.
pub(crate) fn limit_wait(error: &OpError, budget: Duration) -> Option<Duration> {
    if error.code != ErrorCode::Limited {
        return None;
    }
    let Some(ErrorDetail::RetryAfter { seconds, .. }) = error.detail.as_deref() else { return None };
    let wait = Duration::try_from_secs_f32(seconds.max(0.0)).ok()?;
    (wait <= budget).then_some(wait)
}

/// Waits up to `hold` for the person to resume the session of `link`, answering whether it was resumed. A session that ended
/// while waiting was not resumed. `after` is the event position from before the refused call, so a resume that landed
/// in between still counts.
pub(crate) async fn held_until_resumed(
    hub: &EventHub,
    sessions: &dyn Sessions,
    link: &Link,
    after: u64,
    hold: Duration,
) -> bool {
    let deadline = Instant::now() + hold;
    let mut arrivals = hub.watch();
    let mut after = after;
    loop {
        for (n, event) in hub.entries_after(after) {
            after = n;
            if let Event::SessionChanged { session, .. } = &event
                && Some(session.id) == link.id
                && matches!(session.state, SessionState::Active)
            {
                return true;
            }
        }
        // The registry is the truth; the event may be behind the hub's worker.
        match sessions.session_of(&link.actor).map(|info| info.state) {
            Some(SessionState::Active) => return true,
            Some(SessionState::Paused { .. }) => {}
            _ => return false,
        }
        if tokio::time::timeout_at(deadline, arrivals.changed()).await.is_err() {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::PauseReason;

    use super::*;

    fn credential(idle: Duration) -> Credential {
        Credential::new("Codex".to_owned(), Actor::System, false, idle, None)
    }

    #[test]
    fn a_connection_that_leaves_disconnects_unless_it_was_quiet_for_the_idle_timeout() {
        let idle = Duration::from_mins(30);
        let token = credential(idle);
        assert_eq!(
            end_reason(false, None, Duration::from_hours(10)),
            EndReason::Disconnected,
            "stdio has no idle drop"
        );
        assert_eq!(end_reason(false, Some(&token), Duration::from_secs(5)), EndReason::Disconnected);
        assert_eq!(end_reason(false, Some(&token), idle), EndReason::IdleTimeout);
        // The port's clock runs a moment ahead of this crate's, so a hair short still counts.
        assert_eq!(
            end_reason(false, Some(&token), idle.saturating_sub(Duration::from_millis(50))),
            EndReason::IdleTimeout
        );
        assert_eq!(
            end_reason(false, Some(&token), idle.saturating_sub(Duration::from_secs(1))),
            EndReason::Disconnected
        );
    }

    #[test]
    fn what_the_port_says_wins_and_an_in_app_link_is_closed() {
        let token = credential(Duration::from_secs(1));
        let revoked = EndReason::Revoked { by: Actor::System, credential: true };
        token.will_end_with(revoked.clone());
        assert_eq!(end_reason(false, Some(&token), Duration::from_secs(60)), revoked);
        assert_eq!(end_reason(true, None, Duration::ZERO), EndReason::Closed);
    }

    #[test]
    fn only_a_pause_is_held_for_a_resume_and_only_a_short_limit_is_waited_out() {
        let mut paused = OpError::conflict("paused")
            .with_detail(ErrorDetail::Paused { by: Actor::System, reason: PauseReason::Person });
        assert!(!is_paused(&paused), "a conflict is not a pause");
        paused.code = ErrorCode::Waiting;
        assert!(is_paused(&paused));

        let mut limited = OpError::conflict("slow down")
            .with_detail(ErrorDetail::RetryAfter { seconds: 0.5, limit: "edits".to_owned() });
        limited.code = ErrorCode::Limited;
        assert_eq!(limit_wait(&limited, Duration::from_secs(2)), Some(Duration::from_millis(500)));
        assert_eq!(limit_wait(&limited, Duration::from_millis(100)), None, "longer than what is left of the budget");
        assert_eq!(limit_wait(&OpError::conflict("x"), Duration::from_secs(2)), None);
    }
}
