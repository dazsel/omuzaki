//! What an op can see while planning.

use std::any::type_name;

use omuzaki_core::time::Clock;
use omuzaki_doc_api::DocRead;

use crate::actor::NO_GRANTS;
use crate::checks;
use crate::file_access;
use crate::services::{NO_SERVICES, Service, Services};
use crate::{Actor, CanonicalPath, FsMode, Grants, OpError, ProjectDir};

/// What an op can see while planning. Read-only by design: ops describe changes, they don't make them.
///
/// Beyond the document, the actor and the clock, an op reaches the app's injected [`Services`] (the asset store, the
/// renderer, the character builder, ...) through [`service`](Self::service). The rules for using them, in short:
/// a dry-run sees the same services but must not change anything through them (check [`dry_run`](Self::dry_run)),
/// and long work belongs in the op's job, not in `plan`. The rules are in the [`Services`] docs.
///
/// An op that touches a file asks [`fs_access`](Self::fs_access), which answers from the request's grants and the
/// project folder (the rules are in the [`FsMode` docs](crate::FsMode)); ops never see the grants themselves.
///
/// The struct is `#[non_exhaustive]`, so it grows without breaking callers: build one with [`OpCtx::new`] and the
/// `with_*` methods.
#[non_exhaustive]
pub struct OpCtx<'a> {
    /// The document as it is now.
    pub doc: &'a dyn DocRead,
    /// Who is asking.
    pub actor: &'a Actor,
    /// Time, for ops that stamp things (faked in tests).
    pub clock: &'a dyn Clock,
    /// Whether this is a dry-run (only the plan is wanted). Ops must not change anything outside the document, for
    /// example through a service, when it is `true`; assert on it before such a call.
    pub dry_run: bool,
    services: &'a Services,
    /// What the actor may do. Empty until [`with_grants`](Self::with_grants): a context nobody granted anything to
    /// refuses every file.
    grants: &'a Grants,
    /// The project folder, when one is open.
    project_dir: Option<&'a ProjectDir>,
    /// The person who confirmed this run, set by the executor after an approval.
    confirmed_by: Option<&'a Actor>,
}

impl<'a> OpCtx<'a> {
    /// A context over `doc` for `actor` with no services, no grants, no project folder and `dry_run` off. Free to
    /// build: it only borrows. With no grants [`fs_access`](Self::fs_access) refuses every path (fail closed).
    pub fn new(doc: &'a dyn DocRead, actor: &'a Actor, clock: &'a dyn Clock) -> Self {
        Self {
            doc,
            actor,
            clock,
            dry_run: false,
            services: &NO_SERVICES,
            grants: &NO_GRANTS,
            project_dir: None,
            confirmed_by: None,
        }
    }

    /// The same context with the app's `services` available to [`service`](Self::service).
    #[must_use]
    pub fn with_services(mut self, services: &'a Services) -> Self {
        self.services = services;
        self
    }

    /// The same context acting with `grants`, the request's, which [`fs_access`](Self::fs_access) checks paths against.
    #[must_use]
    pub fn with_grants(mut self, grants: &'a Grants) -> Self {
        self.grants = grants;
        self
    }

    /// The same context with the open project's folder, which [`fs_access`](Self::fs_access) allows reading under
    /// and resolves relative paths against.
    #[must_use]
    pub fn with_project_dir(mut self, dir: &'a ProjectDir) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// The same context for a run a person confirmed. Executor-side: it calls this when it re-runs an approved
    /// request, with the approver. Ops and requests cannot name a confirmer.
    #[must_use]
    pub fn with_confirmation(mut self, person: &'a Actor) -> Self {
        self.confirmed_by = Some(person);
        self
    }

    /// The same context marked as a dry-run (or not).
    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// A context over `doc` that keeps everything else of this one: actor, the person who confirmed the run, clock,
    /// services, grants, project folder and dry-run flag. Crate-private so only [`plan_step`](Self::plan_step) can swap the document; the grants stay
    /// out of ops' reach either way.
    pub(crate) fn over<'b>(&'b self, doc: &'b dyn DocRead) -> OpCtx<'b> {
        OpCtx {
            doc,
            actor: self.actor,
            confirmed_by: self.confirmed_by,
            clock: self.clock,
            dry_run: self.dry_run,
            services: self.services,
            grants: self.grants,
            project_dir: self.project_dir,
        }
    }

    /// Whether the request's grants include `scope`.
    pub(crate) fn grants_allow(&self, scope: crate::Scope) -> bool {
        self.grants.allows(scope)
    }

    /// Whether the request's grants include `scope`, for an op whose effect reaches past its own scope: one that
    /// stores another op to run later must refuse an op the caller could not run itself (`input.binding.set_action`),
    /// and one whose answer holds something only a scope may see must leave it out (`input.capture.state`). It answers
    /// for one scope and shows nothing else of the grants. In a dry-run it answers the same.
    pub fn allows(&self, scope: crate::Scope) -> bool {
        self.grants_allow(scope)
    }

    /// The app's implementation of the API trait `S`, for example `ctx.service::<dyn RenderService>()`.
    ///
    /// # Errors
    /// [`ErrorCode::Unavailable`](crate::ErrorCode::Unavailable), naming the trait, when the app did not register
    /// `S`. The op fails cleanly; listing and describing ops never needs a service.
    pub fn service<S: ?Sized + Service>(&self) -> Result<&'a S, OpError> {
        self.services.get::<S>().ok_or_else(|| OpError::unavailable(type_name::<S>()))
    }

    /// Every service the op runs with, for a helper that `plan` and a job's `work` share (`JobCtx::services`).
    pub fn services(&self) -> &'a Services {
        self.services
    }

    /// The person who confirmed this run, if the executor ran it after an approval (design §7).
    pub fn confirmed_by(&self) -> Option<&'a Actor> {
        self.confirmed_by
    }

    /// Requires a person: passes when the actor is a person or a person confirmed this run. The answer is the same in
    /// a dry-run and a real run, like [`fs_access`](Self::fs_access), so a dry-run shows what would be asked.
    ///
    /// # Errors
    /// [`OpError::needs_person`] with `reason` (shown to the person on the approval card) for an agent, plugin or
    /// system actor nobody has confirmed. The executor queues it for approval, or returns it when no approver is
    /// present. For an action nobody may confirm, return [`OpError::person_only`] instead.
    pub fn require_person(&self, reason: impl Into<String>) -> Result<(), OpError> {
        let is_person = |actor: &Actor| matches!(actor, Actor::Person { .. });
        if is_person(self.actor) || self.confirmed_by.is_some_and(is_person) {
            Ok(())
        } else {
            Err(OpError::needs_person(reason))
        }
    }

    /// Requires `scope` for what this run is about to do, when that depends on the data an op reads (a merge needs
    /// the scope of every unit it changes). Passes in a dry-run, which needs only `read`, so a dry-run shows what
    /// would be refused without being refused itself. Otherwise it checks the request's effective grants, which stay
    /// hidden: the answer is only yes or a refusal naming `scope`.
    ///
    /// # Errors
    /// `Denied` naming `scope` (see [`checks::denied`]) when this is not a dry-run and the grants lack it.
    pub fn require_scope(&self, scope: crate::Scope) -> Result<(), OpError> {
        if self.dry_run {
            return Ok(());
        }
        if self.grants_allow(scope) { Ok(()) } else { Err(checks::denied(self.actor, "do this", scope)) }
    }

    /// Asks whether the actor may read or write `path`, and resolves it. The answer is the same in a dry-run and a
    /// real run: it depends on the request's grants, the project folder and the disk, never on [`dry_run`](Self::dry_run).
    ///
    /// Reading inside the project folder needs only `read`; anything else needs `fs` and a root that covers the path
    /// (see [`FsMode`]). A relative `path` is relative to the project folder, and `..` is collapsed by name. The
    /// decision is made on the text first: a path outside the project folder and every root is refused without
    /// asking the file system (or, on Windows, the network) anything. Then the path is walked on the disk, following
    /// a symbolic link or junction only into places the request may reach; a link out of them is refused without
    /// its target being looked up.
    ///
    /// Open the returned [`CanonicalPath`] with its own methods
    /// ([`open_read`](CanonicalPath::open_read), [`open_write`](CanonicalPath::open_write),
    /// [`replace`](CanonicalPath::replace), [`create_dir_all`](CanonicalPath::create_dir_all)), never by its text.
    /// They open the allowed folder the path lies in, then each folder below it, refusing a link at every step, so a
    /// file or folder swapped for a link between this check and the open makes the open fail with `Denied`; it cannot
    /// redirect it, inside the allowed folder or out. (A hard link is the file itself, and is read like it; see the
    /// methods for writes.)
    ///
    /// # Errors
    /// - `Denied`, naming the path and the grant that is missing, when the actor may not touch it. The same error
    ///   covers a path that cannot be resolved safely (a broken symlink, a symlink loop), so it never reveals
    ///   whether a file outside the actor's reach exists.
    /// - `Invalid` (field `path`) for text that is not an unambiguous path on this machine, a `..` that climbs
    ///   above the root, or a relative path with no project folder open.
    /// - `NotFound` when `mode` is [`FsMode::Read`], the actor may read the path, and nothing is there.
    pub fn fs_access(&self, path: &str, mode: FsMode) -> Result<CanonicalPath, OpError> {
        file_access::check(self.grants, self.project_dir, path, mode)
    }
}
