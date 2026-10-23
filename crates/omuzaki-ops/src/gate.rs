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
use crate::executor::Env;
use crate::limits::{self, Bucket};
use crate::sessions::Entry;
use crate::state::State;

/// What an approval satisfied for the one run it allows. The default allows nothing extra: an ordinary request.
#[derive(Clone, Debug, Default)]
pub(crate) struct Allowance {
    /// Scopes given for this run only.
    pub(crate) scopes: Vec<Scope>,
    /// The person who confirmed the run ([`OpCtx::confirmed_by`](omuzaki_ops_api::OpCtx::confirmed_by)).
    pub(crate) confirmed_by: Option<Actor>,
    /// The run may pass the session's budget.
    pub(crate) over_budget: bool,
    /// The run's review is done.
    pub(crate) reviewed: bool,
    /// The approval, stamped on the record. Set for every approved run.
    pub(crate) approval: Option<ApprovalRef>,
    /// The run is an effect's inverse call during an undo, a redo or a rollback (#1091): it may not change the
    /// document and never queues for approval.
    pub(crate) effect_call: bool,
    /// The run is an inner call of this driver job (#1092): its event waits for the transaction's commit, and it never
    /// queues for approval (it would run later, outside the job's transaction).
    pub(crate) driver: Option<omuzaki_ops_api::JobId>,
}

/// A request admitted and planned: the grants it may use replaced the ones it was `sent` with.
struct Prepared {
    req: Request,
    sent: Grants,
    session: Option<usize>,
    plan: Plan,
    at: Seconds,
}

/// Where a request runs: its session, the grants it was sent with, what approvals met, and when.
struct Run<'a> {
    session: Option<usize>,
    sent: &'a Grants,
    allow: &'a Allowance,
    at: Seconds,
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

impl Env {
    /// Runs `req` under the lock: the steps in the [module](self) docs. `allow` is what an approval satisfied.
    pub(crate) fn run_locked(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        req: Request,
        allow: &Allowance,
    ) -> Result<Outcome, OpError> {
        let applies = !checks::answers_only(op.meta(), req.mode);
        let bucket = if applies { Bucket::Edits } else { Bucket::Runs };
        let Prepared { req, sent, session, plan, at } = self.prepare(state, op, req, allow, bucket)?;
        let run = Run { session, sent: &sent, allow, at };
        // An Explicit op performed its effect while it planned (ADR 0036 decision 8): if anything now refuses the
        // apply, that effect is undone at once.
        let effect = (req.mode == Mode::Apply).then(|| plan.effect.clone()).flatten();
        let result = match self.checked(state, op, &req, &run, &plan, true) {
            Err(error) => Err(error),
            Ok(_) if !applies => Ok(Outcome::new(plan)),
            Ok(cost) => {
                let stamps = (allow.approval.clone(), allow.driver);
                self.apply_approved(state, op, (&req.actor, &req.cause, req.tx), &plan, stamps).map(|record| {
                    Self::charge_cost(state, session, cost, at);
                    Outcome::new(plan).with_applied(record)
                })
            }
        };
        match (result, effect) {
            (Err(error), Some(effect)) => Err(self.undo_now(state, &req, &effect, error)),
            (result, _) => result,
        }
    }

    /// The checks of a job start (#1218), under the lock: admission (a start counts as one edit against the
    /// session's rate limit), a driver's transaction, the op's scope, the plan, the cost's `spend` and the review
    /// policies, with the same queueing as any request. The money is the job manager's to reserve against the
    /// session's budget. Returns the request with the grants it may use, and the plan.
    pub(crate) fn check_job(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        req: Request,
        allow: &Allowance,
    ) -> Result<(Request, Plan), OpError> {
        let Prepared { req, sent, session, plan, at } = self.prepare(state, op, req, allow, Bucket::Edits)?;
        let run = Run { session, sent: &sent, allow, at };
        self.checked(state, op, &req, &run, &plan, false)?;
        Ok((req, plan))
    }

    /// Steps 1 to 3 of the [module](self) docs: admission, the transaction, the op's scope, the journal's `admit`
    /// for an apply, and the plan, queueing the needs a person could meet.
    fn prepare(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        mut req: Request,
        allow: &Allowance,
        bucket: Bucket,
    ) -> Result<Prepared, OpError> {
        let at = self.clock.now();
        let meta = op.meta();
        // Admitted again under the lock that plans and applies: a pause or a narrowing that landed since `run` looked
        // must win, and the grants the op plans with are the ones that hold now.
        let entry =
            if allow.approval.is_some() || allow.effect_call { Entry::Inner } else { Entry::Call(Some(bucket)) };
        let admitted = state.sessions.admit_run(&req.actor, &req.grants, at, entry)?;
        if let Some(tx) = req.tx {
            state.check_tx(tx, &req.actor)?;
        }
        let granted = allow.scopes.iter().fold(admitted.grants, |grants, scope| grants.with(*scope));
        // What the request was sent with: a queued request keeps it, since it only narrows.
        let sent = std::mem::replace(&mut req.grants, granted);
        let session = admitted.session;
        let run = Run { session, sent: &sent, allow, at };

        // `read` was checked before the lookup; the op's own scope is checked before it plans.
        if checks::authorize(&req.grants, &req.actor, meta, req.mode).is_err() {
            return Err(self.need(state, op, &req, &run, Asked::of(Need::Scope { scope: meta.scope })));
        }
        if !checks::answers_only(meta, req.mode) {
            state.journal.admit()?;
        }
        let plan = match self.plan(state, op, &req, allow.confirmed_by.as_ref()) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(match needs_person(&error) {
                    Some(reason) if session.is_some() => {
                        self.need(state, op, &req, &run, Asked::of(Need::Person { reason }))
                    }
                    _ => error,
                });
            }
        };
        Ok(Prepared { req, sent, session, plan, at })
    }

    /// Steps 4 and 5 of the [module](self) docs, on the plan: fences, the stale-write guard, the cost, review and (for
    /// an edit, `budget`) the session's budget. Returns the cost to charge once applied, in micro-dollars.
    fn checked(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        req: &Request,
        run: &Run<'_>,
        plan: &Plan,
        budget: bool,
    ) -> Result<u64, OpError> {
        let meta = op.meta();
        let applies = !checks::answers_only(meta, req.mode);
        if run.allow.effect_call && !plan.patch.is_empty() {
            return Err(OpError::internal(format!(
                "{} is an effect's inverse call but planned a change to the document",
                meta.name
            ))
            .hint("this is a bug in the op: inverse calls change only what is outside the document"));
        }
        if let Some(index) = run.session {
            Self::check_claims(state, index, req, plan, applies, run.at)?;
        }
        let cost = if applies { budget::usd_micros(meta.name, plan.cost.as_ref())? } else { 0 };
        if cost > 0 && !req.grants.allows(Scope::Spend) {
            return Err(self.need(state, op, req, run, Asked::of(Need::Scope { scope: Scope::Spend })));
        }
        if let Some(index) = run.session {
            if applies
                && !run.allow.reviewed
                && let Some(requirement) = self.requirement(meta.scope, false)
            {
                let asked = Asked {
                    need: Need::Review { policy: requirement.policy },
                    grant: None,
                    required: requirement.approvals,
                };
                return Err(self.need(state, op, req, run, asked));
            }
            if budget
                && !run.allow.over_budget
                && let Some((needed, left)) = state.sessions.over_budget(index, cost, run.at)
            {
                return Err(self.need(state, op, req, run, Asked::of(Need::Budget { needed, left })));
            }
        }
        Ok(cost)
    }

    /// Charges an applied run's `cost` to its session, with a `Spent` event.
    fn charge_cost(state: &mut State, session: Option<usize>, cost: u64, at: Seconds) {
        let Some(index) = session.filter(|_| cost > 0) else { return };
        state.sessions.add_spend(index, cost, 0, at);
        let id = state.sessions.info(index).id;
        if let Some(snapshot) = state.session(id) {
            state.queue_event(Event::session_changed(snapshot, SessionChange::Spent));
        }
    }

    /// The fences (design §5.3) and the stale-write guard (§5.1) on session `index`'s planned patch, dry-run and apply
    /// alike. A refused apply is a `Clash` for the Theatre's review items; a refused dry run is how the agent finds out.
    fn check_claims(
        state: &mut State,
        index: usize,
        req: &Request,
        plan: &Plan,
        applies: bool,
        at: Seconds,
    ) -> Result<(), OpError> {
        let (error, path, kind, other) = match state.sessions.check_patch(index, &plan.patch) {
            Err(refusal) => {
                let error = OpError::fenced(refusal.path.clone(), refusal.holder.clone());
                (error, refusal.path, ClashKind::Fenced, refusal.holder)
            }
            Ok(()) => match req.base.and_then(|base| stale(state, &plan.patch, &req.actor, base)) {
                Some((error, path, by)) => (error, path, ClashKind::Stale, Some(by)),
                None => return Ok(()),
            },
        };
        if applies {
            let info = state.sessions.info(index);
            let clash = Event::clash(info.id, info.actor.clone(), req.op.clone(), path, kind, other, at);
            state.queue_event(clash);
        }
        Err(error)
    }

    /// Undoes an Explicit op's effect right after its apply was refused, and returns the refusal, with a hint when the
    /// effect is still in place.
    fn undo_now(&self, state: &mut State, req: &Request, effect: &omuzaki_ops_api::Effect, error: OpError) -> OpError {
        match self.call(state, &effect.undo, &req.actor, &req.grants) {
            Ok(()) => error,
            Err(failed) => {
                let hint = format!(
                    "nothing changed in the document, but its effect outside it ({}) is still in place: undoing it \
                     failed: {}",
                    effect.summary, failed.message
                );
                error.hint(hint)
            }
        }
    }

    /// Queues `asked` for `req`, or refuses it when it has no session to queue in.
    fn need(&self, state: &mut State, op: &dyn DynOp, req: &Request, run: &Run<'_>, asked: Asked) -> OpError {
        match run.session {
            Some(index) => self.ask(state, index, op, (req, run.sent, run.allow), asked, run.at),
            None => refusal(req, &asked.need, Refused::NotASession),
        }
    }

    /// The review requirement for `scope` (`grant`: for giving it to a session), if policies are installed and ask
    /// for at least one reviewer.
    fn requirement(&self, scope: Scope, grant: bool) -> Option<ReviewRequirement> {
        self.review.as_ref()?.requirement(scope, grant).filter(|r| r.approvals > 0)
    }

    /// Queues `asked` for the request of session `index`, or refuses it when no person could meet it. `run` is the
    /// request as planned, the grants it was sent with, and what earlier approvals of it met. Returns the error the
    /// caller gets: `Waiting` with [`ErrorDetail::Queued`] when it queued.
    fn ask(
        &self,
        state: &mut State,
        index: usize,
        op: &dyn DynOp,
        run: (&Request, &Grants, &Allowance),
        mut asked: Asked,
        at: Seconds,
    ) -> OpError {
        let (req, sent, allow) = run;
        // With nobody to ask, the only way is to say yes in advance, whoever could have granted it here.
        if !self.approver {
            return refusal(req, &asked.need, Refused::NoApprover);
        }
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
        if allow.effect_call {
            return refusal(req, &asked.need, Refused::Inner);
        }
        // A driver job's inner call is a script's (or a macro's) step: queued, it would run after the run ended.
        if allow.driver.is_some() || req.cause.as_ref().is_some_and(|cause| cause.kind == CauseKind::Script) {
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
            carried: Allowance { approval: None, ..allow.clone() },
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
    fn preview(
        &self,
        state: &State,
        index: usize,
        op: &dyn DynOp,
        req: &Request,
        asked: &Asked,
    ) -> Result<Plan, OpError> {
        let mut grants = req.grants.clone();
        for scope in [asked.grant, scope_of(&asked.need)].into_iter().flatten() {
            grants = grants.with(scope);
        }
        let dry = Request { mode: Mode::DryRun, grants, ..req.clone() };
        let principal = &state.sessions.info(index).on_behalf_of;
        let plan = self.plan(state, op, &dry, Some(principal))?;
        state
            .sessions
            .check_patch(index, &plan.patch)
            .map_err(|refusal| OpError::fenced(refusal.path, refusal.holder))?;
        Ok(plan)
    }

    /// Whether session `index` may start a job that reserves `money.0` micro-dollars while its other live jobs hold
    /// `money.1`: `None` when its budget allows (two concurrent jobs never both pass a check only one fits); otherwise
    /// the refusal, after pausing the session and queueing a `Budget` approval with the request, as for an edit.
    pub(crate) fn job_budget(
        &self,
        state: &mut State,
        index: usize,
        op: &dyn DynOp,
        req: &Request,
        money: (u64, u64),
    ) -> Option<OpError> {
        let (cost, held) = money;
        let at = self.clock.now();
        let (_, mut left) = state.sessions.over_budget(index, cost.saturating_add(held), at)?;
        left.usd_micros = left.usd_micros.saturating_sub(held);
        let needed = Spend { usd_micros: cost, ..Spend::default() };
        let allow = Allowance::default();
        let run = Run { session: Some(index), sent: &req.grants, allow: &allow, at };
        Some(self.need(state, op, req, &run, Asked::of(Need::Budget { needed, left })))
    }

    /// Queues a budget approval with no request for session `index` (a charge from outside crossed a cap), when a
    /// person can be asked. One per budget pause, so it is not bounded by `max_pending`.
    pub(crate) fn ask_budget(&self, state: &mut State, index: usize, needed: Spend, at: Seconds) -> Option<ApprovalId> {
        if !self.approver {
            return None;
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
            carried: Allowance::default(),
            at,
        };
        let pending = state.approvals.push(ask);
        let id = pending.id;
        state.queue_event(Event::approval_requested(pending));
        Some(id)
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
    /// The request comes from a script (or a driver job's inner call), whose run would be over before anyone decided.
    Script,
    /// The request is an effect call inside an undo, a redo or a rollback, which would be over before anyone decided.
    Inner,
}

/// The refusal for a need nobody will be asked about: `Denied`, with the need as the detail where it has one, and a
/// hint that says how to meet it beforehand.
fn refusal(req: &Request, need: &Need, why: Refused) -> OpError {
    let before = match why {
        Refused::Script => "before running the script",
        Refused::Inner => "before undoing again",
        _ => "in advance",
    };
    match need {
        Need::Scope { scope } => {
            let hint = match why {
                Refused::NoApprover if *scope == Scope::Spend => {
                    "nobody can be asked here: the person can let it spend \
                     at launch (`omuzaki mcp --spend-cap <USD>`) or with a credential whose budget covers it, or open \
                     the project in Omuzaki Studio"
                        .to_owned()
                }
                Refused::NoApprover => {
                    format!(
                        "nobody can be asked here: the person can grant it at launch (`omuzaki mcp --grant {scope}`) \
                         or with a credential that grants it, or open the project in Omuzaki Studio"
                    )
                }
                Refused::Script => format!("grant `{scope}` to this session before running the script"),
                Refused::Inner => format!("grant `{scope}` to this session, then undo again"),
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
                Refused::Inner => "a person must undo this step".to_owned(),
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
        .hint(match why {
            Refused::NoApprover => "nobody can be asked here: start the agent with a higher `--spend-cap`, or with a \
                                    credential whose budget covers it"
                .to_owned(),
            _ => format!("raise the session's budget {before}"),
        }),
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

/// The stale-write guard (design §5.1): the newest change, by an actor other than `actor`, after version `base` at,
/// above or below a path `planned` changes. Returns the refusal, the path and who changed it.
fn stale(
    state: &State,
    planned: &omuzaki_doc_api::Patch,
    actor: &Actor,
    base: omuzaki_doc_api::DocVersion,
) -> Option<(OpError, omuzaki_doc_api::DocPath, Actor)> {
    let newest = planned
        .changes
        .iter()
        .filter_map(|change| state.changes.newest(&change.path, Some(actor)))
        .filter(|last| last.version > base)
        .max_by_key(|last| last.version)?;
    let (path, by, version) = (newest.path.clone(), newest.by.clone(), newest.version);
    let mut error = OpError::conflict(format!(
        "`{path}` was changed by {} after the version you planned against",
        crate::error::who(&by)
    ))
    .hint(format!("re-read {path}; your next call carries the newer base"))
    .with_detail(ErrorDetail::Changed { path: path.clone(), by: by.clone(), version });
    error.code = ErrorCode::Conflict;
    Some((error, path, by))
}

/// The scope a need asks for, if it is a scope.
fn scope_of(need: &Need) -> Option<Scope> {
    match need {
        Need::Scope { scope } => Some(*scope),
        _ => None,
    }
}
