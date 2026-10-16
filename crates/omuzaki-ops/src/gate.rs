//! What a request passes under the lock before it applies, and the approval a session's request queues instead when a
//! person could allow it (design `agent-sessions.md` §5.3, §7 and §8; ADR 0020).
//!
//! In order, for one hold of the lock:
//! 1. **Admission:** the session is active (an approved run excepted) and its rate limit has a token.
//! 2. **The op's scope**, before it plans. A session missing a scope its person could grant waits for a person.
//! 3. **The plan.** An op that asks for a person ([`OpError::needs_person`]) waits for one.
//! 4. **The fences**, on the planned patch, for an agent's dry-run and apply alike.
//! 5. For an apply: **the cost** (a paid plan needs `spend`), **the review policies**, then **the budget** (money,
//!    tokens and active time), in that order, so a session that holds `spend` with no budget left is asked for
//!    budget, not for `spend`.
//! 6. **Apply**, then charge the cost to the session.
//!
//! A need is queued only when a person could meet it: the request's own grants and the session's principal hold the
//! scope, an approver is present, and the request does not come from a script (a queued script call would run later
//! as a transaction of its own, after the script ended). Otherwise it is refused with the need as the detail. Before
//! queueing, the request is dry-run with the need met; if that fails, its error is returned and nothing is queued, so
//! a person is only asked about requests that would then succeed. The dry-run's plan is the card's preview. Persons
//! and the system never queue: they are refused as before.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRef, CauseKind, ClashKind, DynOp, ErrorCode, ErrorDetail, Event, Grants, Mode, Need,
    OpError, Outcome, PauseReason, Plan, Request, ReviewRequirement, Scope, SessionChange, Spend, checks,
};

use crate::approvals::Ask;
use crate::budget;
use crate::executor::Executor;
use crate::limits::{self, Bucket};
use crate::sessions::Entry;
use crate::state::State;

/// What an approval satisfied for the one run it allows. The default allows nothing extra: an ordinary request.
#[derive(Clone, Debug, Default)]
pub(crate) struct Allowance {
    /// A scope given for this run only.
    pub(crate) scope: Option<Scope>,
    /// The person who confirmed the run ([`OpCtx::confirmed_by`](omuzaki_ops_api::OpCtx::confirmed_by)).
    pub(crate) confirmed_by: Option<Actor>,
    /// The run may pass the session's budget.
    pub(crate) over_budget: bool,
    /// The run's review is done.
    pub(crate) reviewed: bool,
    /// The approval, stamped on the record. Set for every approved run.
    pub(crate) approval: Option<ApprovalRef>,
}

/// A need about to be queued: what a person must give, the scope a review of a grant gives the run, and how many
/// reviewers a review needs.
struct Asked {
    need: Need,
    grant: Option<Scope>,
    required: u8,
}

impl Asked {
    fn of(need: Need) -> Self {
        Self { need, grant: None, required: 1 }
    }
}

impl Executor {
    /// Runs `req` under the lock: the steps in the [module](self) docs. `allow` is what an approval satisfied.
    pub(crate) fn run_locked(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        mut req: Request,
        allow: &Allowance,
    ) -> Result<Outcome, OpError> {
        let at = self.env.clock.now();
        let meta = op.meta();
        let applies = !checks::answers_only(meta, req.mode);
        // Admitted again under the lock that plans and applies: a pause or a narrowing that landed since `run` looked
        // must win, and the grants the op plans with are the ones that hold now.
        let entry = match allow.approval {
            Some(_) => Entry::Approved,
            None => Entry::Call(Some(if applies { Bucket::Edits } else { Bucket::Runs })),
        };
        let admitted = state.sessions.admit_run(&req.actor, &req.grants, at, entry)?;
        if let Some(tx) = req.tx {
            state.check_tx(tx, &req.actor)?;
        }
        let granted = match allow.scope {
            Some(scope) => admitted.grants.with(scope),
            None => admitted.grants,
        };
        // What the request was sent with: a queued request keeps it, since it only narrows.
        let sent = std::mem::replace(&mut req.grants, granted);
        let session = admitted.session;
        let ask = |state: &mut State, asked: Asked, req: &Request| match session {
            Some(index) => self.ask(state, index, op, req, &sent, asked, at),
            None => refusal(req, &asked.need, Refused::NotASession),
        };

        // `read` was checked before the lookup; the op's own scope is checked before it plans.
        if checks::authorize(&req.grants, &req.actor, meta, req.mode).is_err() {
            return Err(ask(state, Asked::of(Need::Scope { scope: meta.scope }), &req));
        }
        let plan = match self.plan(state, op, &req, allow.confirmed_by.as_ref()) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(match needs_person(&error) {
                    Some(reason) if session.is_some() => ask(state, Asked::of(Need::Person { reason }), &req),
                    _ => error,
                });
            }
        };
        if let Some(index) = session
            && let Err(refusal) = state.sessions.check_patch(index, &plan.patch)
        {
            // A dry run is how an agent finds out: only a refused apply is a clash worth reviewing.
            if applies {
                let info = state.sessions.info(index);
                let (id, actor) = (info.id, info.actor.clone());
                let clash = Event::clash(id, actor, meta.name, refusal.path.clone(), ClashKind::Fenced, refusal.holder.clone(), at);
                state.queue_event(clash);
            }
            return Err(OpError::fenced(refusal.path, refusal.holder));
        }
        let cost = if applies { budget::usd_micros(meta.name, plan.cost.as_ref())? } else { 0 };
        if cost > 0 && !req.grants.allows(Scope::Spend) {
            return Err(ask(state, Asked::of(Need::Scope { scope: Scope::Spend }), &req));
        }
        if let Some(index) = session {
            if applies
                && !allow.reviewed
                && let Some(requirement) = self.requirement(meta.scope, false)
            {
                let asked = Asked {
                    need: Need::Review { policy: requirement.policy },
                    grant: None,
                    required: requirement.approvals,
                };
                return Err(self.ask(state, index, op, &req, &sent, asked, at));
            }
            if !allow.over_budget
                && let Some((needed, left)) = state.sessions.over_budget(index, cost, at)
            {
                return Err(self.ask(state, index, op, &req, &sent, Asked::of(Need::Budget { needed, left }), at));
            }
        }
        if !applies {
            return Ok(Outcome::new(plan));
        }
        let record =
            self.env.apply_approved(state, op, (&req.actor, &req.cause, req.tx), &plan, allow.approval.clone())?;
        if let Some(index) = session
            && cost > 0
        {
            state.sessions.add_spend(index, cost, 0, at);
            let id = state.sessions.info(index).id;
            if let Some(snapshot) = state.session(id) {
                state.queue_event(Event::session_changed(snapshot, SessionChange::Spent));
            }
        }
        Ok(Outcome::new(plan).with_applied(record))
    }

    /// The review requirement for `scope` (`grant`: for giving it to a session), if policies are installed and ask
    /// for at least one reviewer.
    fn requirement(&self, scope: Scope, grant: bool) -> Option<ReviewRequirement> {
        self.env.review.as_ref()?.requirement(scope, grant).filter(|r| r.approvals > 0)
    }

    /// Queues `asked` for the request `req` of session `index`, or refuses it when no person could meet it. Returns
    /// the error the caller gets: `Waiting` with [`ErrorDetail::Queued`] when it queued.
    #[allow(clippy::too_many_arguments)] // The request, where it came from and what it needs: one call site each.
    fn ask(
        &self,
        state: &mut State,
        index: usize,
        op: &dyn DynOp,
        req: &Request,
        sent: &Grants,
        mut asked: Asked,
        at: Seconds,
    ) -> OpError {
        if let Need::Scope { scope } = asked.need {
            if !state.sessions.may_grant(index, scope, sent) {
                return refusal(req, &asked.need, Refused::NoOneCanGrant);
            }
            if let Some(requirement) = self.requirement(scope, true) {
                asked = Asked {
                    need: Need::Review { policy: requirement.policy },
                    grant: Some(scope),
                    required: requirement.approvals,
                };
            }
        }
        if !self.env.approver {
            return refusal(req, &asked.need, Refused::NoApprover);
        }
        if req.cause.as_ref().is_some_and(|cause| cause.kind == CauseKind::Script) {
            return refusal(req, &asked.need, Refused::Script);
        }
        let preview = match self.preview(state, index, op, req, &asked) {
            Ok(preview) => preview,
            Err(error) => return error,
        };
        let session = state.sessions.info(index).clone();
        if matches!(asked.need, Need::Budget { .. })
            && state.sessions.pause_one(index, session.actor.clone(), PauseReason::Budget, at)
            && let Some(snapshot) = state.session(session.id)
        {
            state.queue_event(Event::session_changed(snapshot, SessionChange::Paused));
        }
        if state.approvals.count(session.id) >= usize::try_from(session.limits.max_pending).unwrap_or(usize::MAX) {
            return limits::queue_full(session.limits.max_pending);
        }
        // An approved request runs as its own transaction (design §4.4): no `tx`, and no `base` to be stale against.
        let stored = Request { grants: sent.clone(), tx: None, base: None, ..req.clone() };
        let need = asked.need.clone();
        let ask = Ask {
            session: session.id,
            actor: session.actor,
            need: asked.need,
            request: Some((stored, preview)),
            grant: asked.grant,
            required: asked.required,
            at,
        };
        let pending = state.approvals.push(ask);
        let id = pending.id;
        state.queue_event(Event::approval_requested(pending));
        queued(id, need)
    }

    /// The card's preview: `req` dry-run with `asked` met, as if the person had approved it (the session's principal
    /// confirms, a scope is given). A request whose preview fails, or whose patch a fence refuses, is not worth
    /// asking about.
    fn preview(&self, state: &State, index: usize, op: &dyn DynOp, req: &Request, asked: &Asked) -> Result<Plan, OpError> {
        let mut grants = req.grants.clone();
        for scope in [asked.grant, scope_of(&asked.need)].into_iter().flatten() {
            grants = grants.with(scope);
        }
        let dry = Request { mode: Mode::DryRun, grants, ..req.clone() };
        let principal = &state.sessions.info(index).on_behalf_of;
        let plan = self.plan(state, op, &dry, Some(principal))?;
        state.sessions.check_patch(index, &plan.patch).map_err(|refusal| OpError::fenced(refusal.path, refusal.holder))?;
        Ok(plan)
    }

    /// Queues a budget approval with no request for session `index` (a charge from outside crossed a cap), when a
    /// person can be asked. One per budget pause, so it is not bounded by `max_pending`.
    pub(crate) fn ask_budget(&self, state: &mut State, index: usize, needed: Spend, at: Seconds) {
        if !self.env.approver {
            return;
        }
        let info = state.sessions.info(index);
        let left = budget::left(&info.budget, &info.spent);
        let ask = Ask {
            session: info.id,
            actor: info.actor.clone(),
            need: Need::Budget { needed, left },
            request: None,
            grant: None,
            required: 1,
            at,
        };
        let pending = state.approvals.push(ask);
        state.queue_event(Event::approval_requested(pending));
    }
}


/// Why a need was refused instead of queued.
#[derive(Copy, Clone)]
enum Refused {
    /// The actor has no session to queue in (a person, the system): refused as before.
    NotASession,
    /// The session's principal, an ancestor or the request itself lacks the scope.
    NoOneCanGrant,
    /// The executor has no approver (headless).
    NoApprover,
    /// The request comes from a script, whose run would be over before anyone decided.
    Script,
}

/// The refusal for a need nobody will be asked about: `Denied`, with the need as the detail where it has one, and a
/// hint that says how to meet it beforehand.
fn refusal(req: &Request, need: &Need, why: Refused) -> OpError {
    let before = match why {
        Refused::Script => "before running the script",
        _ => "in advance",
    };
    match need {
        Need::Scope { scope } => {
            let hint = match why {
                Refused::NoApprover => {
                    format!("use a credential that grants `{scope}`, or open the project in Omuzaki Studio")
                }
                Refused::Script => format!("grant `{scope}` to this session before running the script"),
                Refused::NotASession | Refused::NoOneCanGrant => {
                    format!("grant the `{scope}` scope for this session, then retry")
                }
            };
            checks::denied(&req.actor, &format!("run {}", req.op), *scope).hint(hint)
        }
        Need::Person { reason } => {
            let mut error = OpError::needs_person(reason.clone());
            error.hint = Some(match why {
                Refused::Script => "a person must confirm this: run it outside the script".to_owned(),
                _ => "a person must confirm this in Omuzaki Studio: no one can be asked here".to_owned(),
            });
            error
        }
        Need::Budget { needed, left } => OpError::denied(format!(
            "{} would pass the session's budget: it needs {} and {} is left",
            req.op,
            budget::dollars(needed.usd_micros),
            budget::dollars(left.usd_micros),
        ))
        .hint(format!("raise the session's budget {before}")),
        Need::Review { policy } => OpError::denied(format!("{} waits for a review under the policy {policy}", req.op))
            .hint(format!("have a reviewer approve it in Omuzaki Studio {before}")),
        Need::Pairing { .. } => OpError::denied("this session waits to be paired").hint("pair it in Omuzaki Studio"),
    }
}

/// `Waiting`: nothing ran; approval `id` waits for a person.
pub(crate) fn queued(id: ApprovalId, need: Need) -> OpError {
    let mut error = OpError::denied(format!("nothing ran: approval #{} waits for a person to decide", id.0))
        .hint("do not retry: wait for the resolution")
        .with_detail(ErrorDetail::Queued { approval: id, need });
    error.code = ErrorCode::Waiting;
    error
}

/// The reason an op gave for needing a person, when that is why it failed.
fn needs_person(error: &OpError) -> Option<String> {
    match error.detail.as_deref() {
        Some(ErrorDetail::NeedsPerson { reason }) => Some(reason.clone()),
        _ => None,
    }
}

/// The scope a need asks for, if it is a scope.
fn scope_of(need: &Need) -> Option<Scope> {
    match need {
        Need::Scope { scope } => Some(*scope),
        _ => None,
    }
}
