//! A driver job's handle as the engine's [`Calls`] and [`Cancel`]: how `script.run` shares the engine with the console
//! and the CLI (`docs/design/scripting.md` section 6). The executor owns the run's transaction, so [`commit`](Calls::commit)
//! and [`rollback`](Calls::rollback) keep their defaults.

use std::collections::BTreeMap;

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{JobCtx, OpError, OpInfo, Outcome, Request, Scope, checks};

use crate::{Calls, Cancel};

/// A [`JobCtx`] seen by the engine: inner requests go through [`JobCtx::call`], reads through [`JobCtx::read`], and the
/// job's cancel flag stops the run.
///
/// The executor checks every inner request against the job's own grants. The grants a request carries here are the
/// script's narrowing (the ops it names and its declared ceiling), which the executor doesn't know, so they are
/// enforced here: a call to an op whose scope the script's grants lack is `Denied` before it is sent.
pub struct JobScript<'a> {
    job: &'a JobCtx<'a>,
    scopes: BTreeMap<&'static str, Scope>,
}

impl<'a> JobScript<'a> {
    /// The adapter for `job`; `ops` says which scope each op needs.
    pub fn new(job: &'a JobCtx<'a>, ops: &[OpInfo]) -> Self {
        Self { job, scopes: ops.iter().map(|info| (info.name, info.scope)).collect() }
    }
}

impl Calls for JobScript<'_> {
    fn call(&self, request: Request) -> Result<Outcome, OpError> {
        if let Some(scope) = self.scopes.get(request.op.as_str()).filter(|scope| !request.grants.allows(**scope)) {
            return Err(checks::denied(&request.actor, &format!("run {}", request.op), *scope)
                .hint("the script's `// omuzaki: grants = [...]` line leaves this scope out"));
        }
        self.job.call(&request.op, request.args, request.mode)
    }

    fn get(&self, path: &DocPath) -> Result<Option<Value>, OpError> {
        Ok(self.job.read(|doc| doc.get(path).cloned()))
    }
}

impl Cancel for JobScript<'_> {
    fn is_cancelled(&self) -> bool {
        self.job.is_cancelled()
    }
}
