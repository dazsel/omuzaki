//! The Rhai engine: one fresh engine per run, the limits, cancel, output and error mapping.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, OpInfo, Request, TxId};
use rhai::packages::{
    BasicArrayPackage, BasicMapPackage, BasicMathPackage, CorePackage, LogicPackage, MoreStringPackage, Package,
};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{
    Dynamic, Engine, EvalAltResult, Map, Module, NativeCallContext, OptimizationLevel, ParseErrorType, Position, Scope,
    Shared,
};
use serde::Serialize;
use serde_json::Map as JsonMap;

use crate::bindings::Bindings;
use crate::budget;
use crate::check::{Diagnostic, effective_grants};
use crate::closures;
use crate::convert::{Convert, from_json, oversized, oversized_fields, size_kind, to_json};
use crate::{Calls, Cancel, LimitKind, Limits, OutputKind, OutputLine, PlannedCall, ScriptError, ScriptErrorKind};

/// What to run and as whom.
pub struct RunRequest {
    /// The script's name: the transaction label and the cause text.
    pub name: String,
    /// The Rhai source.
    pub source: String,
    /// Who runs it.
    pub actor: Actor,
    /// What it may do.
    pub grants: Grants,
    /// A caller's open transaction to join (driver jobs only; [`RunnerCalls`](crate::RunnerCalls) refuses it).
    pub tx: Option<TxId>,
    /// Exposed to the script as `params`.
    pub params: JsonMap<String, Value>,
    /// Send every inner call as a dry run and list the plans.
    pub dry_run: bool,
}

/// What a run that finished produced.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScriptOutcome {
    /// The script's last value, as JSON.
    pub value: Value,
    /// Everything it printed or logged.
    pub output: Vec<OutputLine>,
    /// Inner op calls sent.
    pub calls: u32,
    /// The transaction the calls implementation committed, if it owns one.
    pub tx: Option<TxId>,
    /// Each call a dry run planned; empty for a real run. A dry run sends every call against the document as it was
    /// before the script, so a script whose later calls read what earlier ones wrote may plan differently from a real
    /// run; the console says so on the result.
    pub plans: Vec<PlannedCall>,
}

/// Why the engine was told to stop; carried as the `ErrorTerminated` token, which a script cannot catch.
#[derive(Copy, Clone)]
pub(crate) enum Stop {
    Cancelled,
    Limit(LimitKind),
}

/// Why an op is not callable from a script, carried like [`Stop`] so a script can't catch it.
#[derive(Clone)]
pub(crate) struct Refusal(pub(crate) Arc<OpError>);

/// The most ticks a due stop is held back; see [`Run::progress`].
const GRACE_TICKS: u32 = 3;

/// The run's services, borrowed from `run`'s caller.
struct Lent {
    calls: &'static dyn Calls,
    cancel: &'static dyn Cancel,
}

/// Output kept under the limits: lines and bytes, then one marker line.
#[derive(Default)]
struct Output {
    lines: Vec<OutputLine>,
    bytes: usize,
    truncated: bool,
}

/// The state of one run, shared by the engine's callbacks (which must be `'static`).
pub(crate) struct Run {
    /// Cleared by [`Lease`] before `run` returns, so no callback can reach the borrowed services afterwards.
    lent: RwLock<Option<Lent>>,
    limits: Limits,
    request: RunRequest,
    sent: AtomicU32,
    /// `get(path)` reads, which count against [`Limits::calls`] with the calls sent but are not reported as calls.
    reads: AtomicU32,
    /// JSON bytes of the dry-run plans kept so far.
    plan_bytes: AtomicUsize,
    /// A stop a formatting function asked for: 0 none, 1 `StringSize` (its text was cut at the string limit), 2
    /// `Memory` (the run was already over its memory limit when asked to format). Rhai swallows an error from
    /// `to_string` and formats the value in full, so the function answers a short text instead and the run stops at the
    /// next tick or before it commits.
    pending_stop: AtomicU8,
    /// Ticks that found a reason to stop and were let pass; see [`Run::progress`].
    held_ticks: AtomicU32,
    /// The operation count of the last tick.
    last_ops: AtomicU64,
    output: Mutex<Output>,
    plans: Mutex<Vec<PlannedCall>>,
}

/// Empties [`Run::lent`] when dropped. Private to [`Run::lend`], which owns the only one, so it can't be leaked
/// with `mem::forget`; it exists so a panic in the run empties the slot too.
struct Clear<'a>(&'a Run);

impl Drop for Clear<'_> {
    fn drop(&mut self) {
        *self.0.lent.write().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[allow(clippy::unnecessary_box_returns)] // Rhai's callbacks return `Box<EvalAltResult>`
fn terminate(stop: Stop) -> Box<EvalAltResult> {
    terminate_at(stop, Position::NONE)
}

/// Ends the run at `at` with `token`, which a script can't catch.
#[allow(clippy::unnecessary_box_returns)] // as above
pub(crate) fn terminate_at<T: Clone + Send + Sync + 'static>(token: T, at: Position) -> Box<EvalAltResult> {
    EvalAltResult::ErrorTerminated(Dynamic::from(token), at).into()
}

#[allow(clippy::unnecessary_box_returns)] // as above
pub(crate) fn runtime(message: impl Into<String>) -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(message.into().into(), Position::NONE).into()
}

impl Run {
    /// Lends `calls` and `cancel` to the engine's callbacks for the duration of `f`, and only that.
    ///
    /// A scoped closure rather than a returned guard: the slot is emptied before `lend` returns on every path (normal
    /// return or unwind), and no caller can keep the borrow alive past it.
    fn lend<'a, R>(&self, calls: &'a dyn Calls, cancel: &'a dyn Cancel, f: impl FnOnce() -> R) -> R {
        #[allow(unsafe_code)] // see the SAFETY comment
        // SAFETY: invariant: a `Lent` is reachable only through `self.lent`, and only while this call is on the
        // stack. The two references are valid for `'a`, which outlives this call. The slot is filled here and
        // emptied by `_clear` before this function returns or unwinds; `_clear` is a local that nothing else can
        // see, so it can't be forgotten. Every reader (`invoke`, `progress`) holds the slot's read lock for as
        // long as it uses a reference, and emptying takes the write lock, so no use overlaps the end of the
        // borrow. After that the slot is `None`, and a callback that still holds the `Arc<Run>` gets `Cancelled`.
        let lent = unsafe {
            Lent {
                calls: std::mem::transmute::<&'a dyn Calls, &'static dyn Calls>(calls),
                cancel: std::mem::transmute::<&'a dyn Cancel, &'static dyn Cancel>(cancel),
            }
        };
        *self.lent.write().unwrap_or_else(PoisonError::into_inner) = Some(lent);
        let _clear = Clear(self);
        f()
    }

    /// The one call path the bindings use: stops on cancel and at the call limit, then sends the request. A failed op
    /// is raised at `at` as a map of the `OpError` with the `op`, `line` and `column`.
    pub(crate) fn invoke(&self, op: &str, args: Value, at: Position) -> Result<Value, Box<EvalAltResult>> {
        let lent = self.lent.read().unwrap_or_else(PoisonError::into_inner);
        let Some(lent) = lent.as_ref() else { return Err(terminate(Stop::Cancelled)) };
        if lent.cancel.is_cancelled() {
            return Err(terminate(Stop::Cancelled));
        }
        if self.used_calls() >= self.limits.calls {
            return Err(terminate(Stop::Limit(LimitKind::Calls)));
        }
        self.sent.fetch_add(1, Ordering::SeqCst);
        let dry = self.request.dry_run;
        let request = Request {
            op: op.to_owned(),
            args: args.clone(),
            mode: if dry { Mode::DryRun } else { Mode::Apply },
            actor: self.request.actor.clone(),
            grants: self.request.grants.clone(),
            cause: Some(Cause { kind: CauseKind::Script, text: Some(self.request.name.clone()) }),
            tx: self.request.tx,
            base: None,
        };
        match lent.calls.call(request) {
            Ok(outcome) => {
                let value = outcome.plan.output.clone().unwrap_or(Value::Null);
                if dry {
                    let planned = PlannedCall { op: op.to_owned(), args, plan: outcome.plan };
                    let mut plans = self.plans.lock().unwrap_or_else(PoisonError::into_inner);
                    // The plans are bounded like the result: the JSON array of them, brackets and commas included,
                    // may be no larger than `result_bytes`, or the run stops (a dry run of 10,000 calls would
                    // otherwise hold 10,000 plans). `plan_bytes` starts at the two brackets.
                    let comma = usize::from(!plans.is_empty());
                    let size = serde_json::to_vec(&planned).map_or(usize::MAX, |json| json.len());
                    let size = size.saturating_add(comma);
                    let total = self.plan_bytes.fetch_add(size, Ordering::SeqCst).saturating_add(size);
                    if total > self.limits.result_bytes {
                        return Err(terminate_at(Stop::Limit(LimitKind::Result), at));
                    }
                    plans.push(planned);
                }
                Ok(value)
            }
            Err(error) => Err(raise(&error, op, at)),
        }
    }

    /// Calls sent plus reads made: what [`Limits::calls`] limits.
    fn used_calls(&self) -> u32 {
        self.sent.load(Ordering::SeqCst).saturating_add(self.reads.load(Ordering::SeqCst))
    }

    /// `get(path)`: reads the document through [`Calls::get`], as a script value (`()` when there is nothing there).
    pub(crate) fn read(&self, path: &str, at: Position) -> Result<Dynamic, Box<EvalAltResult>> {
        let lent = self.lent.read().unwrap_or_else(PoisonError::into_inner);
        let Some(lent) = lent.as_ref() else { return Err(terminate(Stop::Cancelled)) };
        if lent.cancel.is_cancelled() {
            return Err(terminate(Stop::Cancelled));
        }
        if self.used_calls() >= self.limits.calls {
            return Err(terminate_at(Stop::Limit(LimitKind::Calls), at));
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let path = DocPath::parse(path).map_err(|e| {
            let error =
                OpError::invalid("path", e.to_string()).hint("write a document path such as `characters/aki/face`");
            raise(&error, "get", at)
        })?;
        let Some(value) = lent.calls.get(&path).map_err(|e| raise(&e, "get", at))? else { return Ok(Dynamic::UNIT) };
        if let Some(kind) = oversized(&value, &self.limits) {
            return Err(terminate_at(Stop::Limit(kind), at));
        }
        from_json(&value).map_err(runtime)
    }

    /// Why the run must stop now, if it must: cancelled (or the lease is over), or out of wall-clock time.
    fn stop(&self, started: f64, clock: &dyn Clock) -> Option<Stop> {
        let lent = self.lent.read().unwrap_or_else(PoisonError::into_inner);
        if lent.as_ref().is_none_or(|l| l.cancel.is_cancelled()) {
            return Some(Stop::Cancelled);
        }
        (clock.now().0 - started >= f64::from(self.limits.seconds)).then_some(Stop::Limit(LimitKind::Time))
    }

    /// Records that a text was cut at the string limit; the run ends with `Limit(StringSize)`.
    pub(crate) fn note_string_overflow(&self) {
        self.pending_stop.fetch_max(1, Ordering::SeqCst);
    }

    /// Whether the run already holds more than its memory limit; if so the run is marked to end with `Limit(Memory)`.
    /// Every formatting path asks this first and formats nothing when it is true: a value the run could only copy past
    /// its limit is not worth formatting, and formatting it is what costs several times its size.
    pub(crate) fn over_budget(&self) -> bool {
        let over = budget::used_here() > self.limits.memory_bytes;
        if over {
            self.pending_stop.fetch_max(2, Ordering::SeqCst);
        }
        over
    }

    /// The stop a formatting function asked for, if one did.
    fn requested_stop(&self) -> Option<LimitKind> {
        match self.pending_stop.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(LimitKind::StringSize),
            _ => Some(LimitKind::Memory),
        }
    }

    /// The engine's per-operation check: `Some` ends the run. `ops` is the operation count Rhai passes.
    ///
    /// A stop that is due (cancel, time or memory) is held back for a few ticks, never more than [`GRACE_TICKS`], and
    /// delivered at the first tick whose count repeats the one before it. The reason is Rhai's interpolated string: it
    /// calls `to_string` through a function whose own tick comes first, on a copy of the run state, and it swallows an
    /// error from that call, formatting the value in full instead (`print_with_func`). A stop delivered there would not
    /// stop the run and would make that unbounded formatting happen. That tick is the only one that does not advance
    /// the count, so the next tick, the real one, repeats it: a repeated count proves the tick before was that one.
    /// While a stop is held, the formatting paths see the run is over its limit and format nothing
    /// ([`Run::over_budget`]). Only an adversary that lines the end of the hold up with such a tick gets a value
    /// formatted in full, once; the memory it adds is caught by the peak check before the run commits.
    fn progress(&self, ops: u64, started: f64, clock: &dyn Clock) -> Option<Dynamic> {
        let repeated = self.last_ops.swap(ops, Ordering::Relaxed) == ops;
        if let Some(kind) = self.requested_stop() {
            return Some(Dynamic::from(Stop::Limit(kind)));
        }
        // A callback that outlives its lease stops at once: nothing it can reach is borrowed any more.
        if self.lent.read().unwrap_or_else(PoisonError::into_inner).is_none() {
            return Some(Dynamic::from(Stop::Cancelled));
        }
        let reason = if budget::used_here() > self.limits.memory_bytes {
            Some(Dynamic::from(Stop::Limit(LimitKind::Memory)))
        } else {
            self.stop(started, clock).map(Dynamic::from)
        };
        if reason.is_some() && !repeated && self.held_ticks.fetch_add(1, Ordering::SeqCst) < GRACE_TICKS {
            return None;
        }
        reason
    }

    fn print(&self, kind: OutputKind, text: &str) {
        let mut out = self.output.lock().unwrap_or_else(PoisonError::into_inner);
        if out.truncated {
            return;
        }
        if out.lines.len() >= self.limits.output_lines || out.bytes + text.len() > self.limits.output_bytes {
            out.truncated = true;
            out.lines.push(OutputLine { kind: OutputKind::Note, text: "output truncated".into() });
            return;
        }
        out.bytes += text.len();
        out.lines.push(OutputLine { kind, text: text.to_owned() });
    }
}

pub(crate) fn run_of(context: &NativeCallContext<'_>) -> Result<Arc<Run>, Box<EvalAltResult>> {
    context.tag().and_then(|tag| tag.clone().try_cast::<Arc<Run>>()).ok_or_else(|| runtime("no run is active"))
}

/// A failed op as the error a script sees: the `OpError` as a map, plus the `op` and the `line` and `column` of the call.
#[allow(clippy::unnecessary_box_returns)] // as above
fn raise(error: &OpError, op: &str, at: Position) -> Box<EvalAltResult> {
    let to_int = |n: Option<usize>| Dynamic::from(i64::try_from(n.unwrap_or(1)).unwrap_or(i64::MAX));
    let mut map = match to_dynamic(error) {
        Ok(map) => map,
        Err(e) => return runtime(e.to_string()),
    };
    if let Some(fields) = map.write_lock::<Map>().as_mut() {
        // `field` and `hint` are always there, `()` when the error has none, so a script can read them unguarded.
        for key in ["field", "hint"] {
            fields.entry(key.into()).or_insert(Dynamic::UNIT);
        }
        fields.insert("op".into(), Dynamic::from(op.to_owned()));
        fields.insert("line".into(), to_int(at.line()));
        fields.insert("column".into(), to_int(at.position()));
    }
    EvalAltResult::ErrorRuntime(map, at).into()
}

/// Runs scripts. Holds the engine's shared parts; each [`run`](Self::run) builds a fresh engine from them.
pub struct ScriptHost {
    packages: Vec<Shared<Module>>,
    pub(crate) bindings: Arc<Bindings>,
    limits: Limits,
    clock: Arc<dyn Clock>,
}

impl ScriptHost {
    /// A host whose scripts can call `ops::area::verb({..})` for each op in `ops`, held to `limits`. The bindings are
    /// generated once from `ops` and shared by every run; each call takes one map, which the op checks against its own
    /// schema. `get(path)` and `try_op(f)` come with them.
    ///
    /// Only queries and edits that undo with their patch ([`Undo::Auto`](omuzaki_ops_api::Undo::Auto)) can be called:
    /// the run's rollback can't undo a job (which also can't join a transaction) or an edit with outside effects, and
    /// a script can't start another `script.run`. Those are bound to a refusal ([`ScriptErrorKind::Refused`], the
    /// `Unsupported` error and its hint in [`ScriptError::op_error`]) that is raised before anything is sent;
    /// [`check`](Self::check) reports the same refusals.
    pub fn new(ops: &[OpInfo], limits: Limits) -> Self {
        let packages = vec![
            CorePackage::new().as_shared_module(),
            LogicPackage::new().as_shared_module(),
            MoreStringPackage::new().as_shared_module(),
            BasicArrayPackage::new().as_shared_module(),
            BasicMapPackage::new().as_shared_module(),
            BasicMathPackage::new().as_shared_module(),
        ];
        let bindings = Arc::new(Bindings::new(ops, &limits));
        Self { packages, bindings, limits, clock: Arc::new(SystemClock::new()) }
    }

    /// A host over bindings built earlier from the same ops and limits (see `registered_host`).
    pub(crate) fn from_bindings(bindings: Arc<Bindings>, limits: Limits) -> Self {
        let host = Self::new(&[], limits);
        Self { bindings, ..host }
    }

    /// The limits this host holds its runs to.
    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Reads the wall-clock limit from `clock`, so tests need no sleep.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The engine a run and the check both parse with: the same packages, bindings, limits and disabled symbols, so
    /// what the check accepts is what the run accepts. No optimizer: it would fold calls away and leave the syntax
    /// tree the check reads different from the code that runs.
    pub(crate) fn base_engine(&self) -> Engine {
        let mut engine = Engine::new_raw();
        for package in &self.packages {
            engine.register_global_module(package.clone());
        }
        self.bindings.install(&mut engine);
        crate::guards::install(&mut engine, &self.limits);
        let limits = &self.limits;
        engine
            .set_max_operations(limits.operations)
            .set_max_call_levels(limits.call_depth)
            .set_max_expr_depths(limits.expr_depth, limits.fn_expr_depth)
            .set_max_string_size(limits.string_bytes)
            .set_max_array_size(limits.array_len)
            .set_max_map_size(limits.map_len)
            .set_optimization_level(OptimizationLevel::None);
        for symbol in ["eval", "Fn", "call", "curry", "import"] {
            engine.disable_symbol(symbol);
        }
        engine
    }

    fn engine(&self, run: &Arc<Run>, started: f64) -> Engine {
        let mut engine = self.base_engine();
        engine.set_default_tag(Dynamic::from(run.clone()));
        let sink = run.clone();
        engine.on_print(move |text| sink.print(OutputKind::Print, text));
        let sink = run.clone();
        engine.on_debug(move |text, _, _| sink.print(OutputKind::Note, text));
        let sink = run.clone();
        // The text is cut at the output limit while it is built, not after: see `guards`.
        let room = self.limits.output_bytes.saturating_add(1);
        // By reference: a by-value argument is a copy of the whole value, which is what this is trying not to make.
        engine.register_fn("log", move |value: &mut Dynamic| {
            if sink.over_budget() {
                return;
            }
            sink.print(OutputKind::Note, &crate::guards::format_bounded(value, false, room).0);
        });
        let sink = run.clone();
        // A float would otherwise reach the math package's `log(x)` (the logarithm), so it is overloaded too.
        engine.register_fn("log", move |value: f64| sink.print(OutputKind::Note, &value.to_string()));
        let (watch, clock) = (run.clone(), self.clock.clone());
        engine.on_progress(move |ops| watch.progress(ops, started, clock.as_ref()));
        engine
    }

    /// Runs a script on a worker thread whose stack is sized for the deepest value the limits let it build
    /// ([`Limits::worker_stack_bytes`]), so a deeply nested value can't overflow the stack and abort the process. Any
    /// failure, limit or cancel rolls back through [`Calls::rollback`].
    ///
    /// `calls` and `cancel` are used from the worker thread while the caller waits, which their `Send + Sync`
    /// supertraits already allow; the call returns when the run is over, and nothing outlives it. A panic in the run
    /// is raised again in the caller. Limits that would need more stack than [`Limits::MAX_WORKER_STACK_BYTES`] are
    /// refused with [`ScriptErrorKind::Refused`]. On `wasm32`, which has no threads, the run happens on the caller's
    /// thread.
    ///
    /// # Errors
    /// A [`ScriptError`] carrying the position, the kind and the output printed so far.
    #[allow(clippy::result_large_err)] // the error type and this signature are the contract's
    pub fn run(
        &self,
        request: RunRequest,
        calls: &dyn Calls,
        cancel: &dyn Cancel,
    ) -> Result<ScriptOutcome, ScriptError> {
        if !budget::installed() {
            return Err(refused(
                "no memory budget is installed: set `omuzaki_script::MemoryBudget` as the binary's `#[global_allocator]` \
                 so a run's memory can be limited",
            ));
        }
        let Some(stack) = self.limits.worker_stack_bytes() else {
            return Err(refused(
                "the array and map limits allow nesting that needs more stack than a run may have; lower `array_len` or `map_len`",
            ));
        };
        self.run_on_worker(stack, request, calls, cancel)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::result_large_err)] // as `run`
    fn run_on_worker(
        &self,
        stack: usize,
        request: RunRequest,
        calls: &dyn Calls,
        cancel: &dyn Cancel,
    ) -> Result<ScriptOutcome, ScriptError> {
        std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("omuzaki-script".to_owned())
                .stack_size(stack)
                .spawn_scoped(scope, || self.run_here(request, calls, cancel));
            match worker {
                Ok(worker) => worker.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                Err(error) => Err(refused(&format!("the worker thread for the run could not start: {error}"))),
            }
        })
    }

    #[cfg(target_arch = "wasm32")]
    #[allow(clippy::result_large_err)] // as `run`
    fn run_on_worker(
        &self,
        _stack: usize,
        request: RunRequest,
        calls: &dyn Calls,
        cancel: &dyn Cancel,
    ) -> Result<ScriptOutcome, ScriptError> {
        self.run_here(request, calls, cancel)
    }

    /// The run itself, on the calling thread.
    #[allow(clippy::result_large_err)] // as `run`
    fn run_here(
        &self,
        mut request: RunRequest,
        calls: &dyn Calls,
        cancel: &dyn Cancel,
    ) -> Result<ScriptOutcome, ScriptError> {
        let _counting = budget::Counting::start();
        // The run's grants are the caller's, narrowed to what the script names (design section 3). A source that does
        // not analyse cleanly is refused below, so its grants are never used.
        let analysis = self.analyze(&request.source);
        if analysis.report.parse_errors.is_empty() {
            request.grants = effective_grants(&request.grants, &analysis.report);
        }
        let run = Arc::new(Run {
            lent: RwLock::new(None),
            limits: self.limits.clone(),
            request,
            sent: AtomicU32::new(0),
            reads: AtomicU32::new(0),
            plan_bytes: AtomicUsize::new(2),
            pending_stop: AtomicU8::new(0),
            held_ticks: AtomicU32::new(0),
            last_ops: AtomicU64::new(0),
            output: Mutex::new(Output::default()),
            plans: Mutex::new(Vec::new()),
        });
        let result = run.lend(calls, cancel, || self.execute(&run, calls, cancel, analysis.ceiling_error.as_ref()));
        let mut rolled = Ok(());
        if result.is_err() {
            rolled = calls.rollback();
        }
        result.map_err(|mut error| {
            if let Err(e) = rolled {
                error.message = format!("{}; rollback failed: {e}", error.message);
            }
            error.output = std::mem::take(&mut run.output.lock().unwrap_or_else(PoisonError::into_inner).lines);
            error
        })
    }

    #[allow(clippy::result_large_err)] // as `run`
    fn execute(
        &self,
        run: &Arc<Run>,
        calls: &dyn Calls,
        cancel: &dyn Cancel,
        ceiling_error: Option<&Diagnostic>,
    ) -> Result<ScriptOutcome, ScriptError> {
        let fail = |kind, message: &str| ScriptError {
            kind,
            line: 1,
            column: 1,
            op_error: None,
            message: message.to_owned(),
            output: Vec::new(),
        };
        if let Some(name) = self.limits.zero_engine_limit() {
            return Err(fail(
                ScriptErrorKind::Refused,
                &format!("the limit `{name}` is 0, which the engine reads as no limit; a limit must be at least 1"),
            ));
        }
        let source = &run.request.source;
        if source.len() > self.limits.source_bytes {
            return Err(fail(ScriptErrorKind::Limit(LimitKind::Source), "the script is larger than the source limit"));
        }
        if cancel.is_cancelled() {
            return Err(fail(ScriptErrorKind::Cancelled, "the run was cancelled before it started"));
        }
        if let Some(problem) = ceiling_error {
            // A ceiling that can't be read must not silently stop narrowing the run.
            return Err(ScriptError {
                line: problem.line,
                column: problem.column,
                ..fail(ScriptErrorKind::Parse, &problem.message)
            });
        }
        // Measured before `params` is converted: the whole map is held to the limits as one value.
        if let Some(kind) = oversized_fields(&run.request.params, &self.limits) {
            return Err(fail(ScriptErrorKind::Limit(kind), "`params` holds a value larger than the script limits"));
        }
        let started = self.clock.now().0;
        let engine = self.engine(run, started);
        let ast = engine.compile(source).map_err(|e| {
            let kind = match &*e.0 {
                ParseErrorType::ExprTooDeep => ScriptErrorKind::Limit(LimitKind::Depth),
                ParseErrorType::LiteralTooLarge(typ, _) => ScriptErrorKind::Limit(size_kind(typ)),
                _ => ScriptErrorKind::Parse,
            };
            located(kind, e.0.to_string(), e.1, None)
        })?;
        if let Some(stray) = closures::stray(&ast) {
            return Err(located(ScriptErrorKind::Parse, stray.message, stray.at, None));
        }
        let value = evaluate(&engine, &ast, &run.request.params, &self.limits)?;
        // The progress tick reads the live heap between operations; the last operation (or the conversion of the
        // result) can pass the budget and free it again before another tick, so the peak is read here, before anything
        // becomes durable.
        match run.requested_stop() {
            Some(LimitKind::Memory) => {
                return Err(fail(ScriptErrorKind::Limit(LimitKind::Memory), "the run held more memory than the limit"));
            }
            Some(kind) => return Err(fail(ScriptErrorKind::Limit(kind), "a text was longer than the string limit")),
            None => {}
        }
        if budget::peak_here() > self.limits.memory_bytes {
            return Err(fail(
                ScriptErrorKind::Limit(LimitKind::Memory),
                "the run held more memory than the memory limit",
            ));
        }
        let too_big = serde_json::to_vec(&value).map_or(true, |json| json.len() > self.limits.result_bytes);
        if too_big {
            return Err(fail(ScriptErrorKind::Limit(LimitKind::Result), "the result is larger than the result limit"));
        }
        // The last inner call can return straight here, past the call path's check and every progress tick, so a
        // cancel or timeout that arrived during it is caught now, before anything becomes durable.
        match run.stop(started, self.clock.as_ref()) {
            Some(Stop::Cancelled) => {
                return Err(fail(ScriptErrorKind::Cancelled, "the run was cancelled before it committed"));
            }
            Some(Stop::Limit(kind)) => {
                return Err(fail(ScriptErrorKind::Limit(kind), "the wall-clock limit passed before the run committed"));
            }
            None => {}
        }
        let tx = calls.commit().map_err(|e| ScriptError {
            op_error: Some(e.clone()),
            ..fail(ScriptErrorKind::Op, &format!("the transaction did not commit: {}", e.message))
        })?;
        Ok(ScriptOutcome {
            value,
            output: std::mem::take(&mut run.output.lock().unwrap_or_else(PoisonError::into_inner).lines),
            calls: run.sent.load(Ordering::SeqCst),
            tx,
            plans: std::mem::take(&mut run.plans.lock().unwrap_or_else(PoisonError::into_inner)),
        })
    }
}

/// Evaluates `ast` and returns its result as JSON. The scope, the returned value and every local a script built are
/// dropped before this returns, on success and on error alike, so dropping a deeply nested value happens before
/// `Calls::commit` makes anything durable and before a limit error rolls back.
#[allow(clippy::result_large_err)] // as `ScriptHost::run`
fn evaluate(
    engine: &Engine,
    ast: &rhai::AST,
    params: &JsonMap<String, Value>,
    limits: &Limits,
) -> Result<Value, ScriptError> {
    let fail = |kind, message: &str| ScriptError {
        kind,
        line: 1,
        column: 1,
        op_error: None,
        message: message.to_owned(),
        output: Vec::new(),
    };
    let mut scope = Scope::new();
    let params = to_dynamic(params).map_err(|e| fail(ScriptErrorKind::Runtime, &e.to_string()))?;
    scope.push_constant_dynamic("params", params);
    let returned =
        engine.eval_ast_with_scope::<Dynamic>(&mut scope, ast).map_err(|e| classify(&e, limits.string_bytes))?;
    to_json(returned, limits.value_depth).map_err(|e| match e {
        Convert::Depth => fail(ScriptErrorKind::Limit(LimitKind::Depth), "the result is nested deeper than the limit"),
        Convert::Other(message) => fail(ScriptErrorKind::Runtime, &format!("the result is not JSON: {message}")),
    })
}

/// A run that was not started, for a reason in the host's own setup.
fn refused(message: &str) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Refused,
        line: 1,
        column: 1,
        op_error: None,
        message: message.to_owned(),
        output: Vec::new(),
    }
}

/// A position as 1-based `(line, column)`; `(1, 1)` for a failure with no place in the source.
pub(crate) fn line_col(at: Position) -> (u32, u32) {
    let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    (at.line().map_or(1, to_u32), at.position().map_or(1, to_u32))
}

pub(crate) fn located(kind: ScriptErrorKind, message: String, at: Position, op_error: Option<OpError>) -> ScriptError {
    let (line, column) = line_col(at);
    ScriptError { kind, line, column, op_error, message, output: Vec::new() }
}

/// Maps Rhai's error to ours, looking through the wrappers a call inside a script function adds. A thrown value is
/// described in at most `room` bytes: Rhai's own text for it formats the whole value, after the run has stopped
/// counting its memory, and a thrown map can hold more than the budget.
fn classify(error: &EvalAltResult, room: usize) -> ScriptError {
    let at = error.position();
    let (kind, op_error) = match error {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => return classify(inner, room),
        EvalAltResult::ErrorTooManyOperations(_) => (ScriptErrorKind::Limit(LimitKind::Operations), None),
        EvalAltResult::ErrorStackOverflow(_) => (ScriptErrorKind::Limit(LimitKind::Depth), None),
        EvalAltResult::ErrorDataTooLarge(typ, _) => (ScriptErrorKind::Limit(size_kind(typ)), None),
        EvalAltResult::ErrorTerminated(token, _) => match token.clone().try_cast::<Stop>() {
            Some(Stop::Cancelled) => (ScriptErrorKind::Cancelled, None),
            Some(Stop::Limit(kind)) => (ScriptErrorKind::Limit(kind), None),
            None => match token.clone().try_cast::<Refusal>() {
                Some(Refusal(error)) => {
                    return located(ScriptErrorKind::Refused, error.message.clone(), at, Some((*error).clone()));
                }
                None => (ScriptErrorKind::Runtime, None),
            },
        },
        EvalAltResult::ErrorRuntime(payload, _) => match op_error_of(payload) {
            Some(op_error) => (ScriptErrorKind::Op, Some(op_error)),
            None => (ScriptErrorKind::Runtime, None),
        },
        _ => (ScriptErrorKind::Runtime, None),
    };
    let message = op_error.as_ref().map_or_else(|| describe(error, room), |e| e.message.clone());
    located(kind, message, at, op_error)
}

/// Rhai's text for `error`, with a thrown value cut at `room` bytes while it is formatted.
fn describe(error: &EvalAltResult, room: usize) -> String {
    match error {
        EvalAltResult::ErrorRuntime(thrown, at) if !thrown.is_unit() && !thrown.is_string() => {
            EvalAltResult::ErrorRuntime(crate::guards::display_bounded(thrown, room).into(), *at).to_string()
        }
        _ => error.to_string(),
    }
}

/// The `OpError` an inner call's failure carried into the script (a map that names the op).
fn op_error_of(payload: &Dynamic) -> Option<OpError> {
    let map = payload.read_lock::<Map>()?;
    map.contains_key("op").then_some(())?;
    drop(map);
    from_dynamic::<OpError>(payload).ok()
}

#[cfg(test)]
mod tests {
    //! The lease's boundary: the borrowed services are reachable while `lend` runs and never after it.

    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use omuzaki_doc_api::DocPath;
    use omuzaki_ops_api::Outcome;

    use super::*;

    /// Counts the calls that reached it, through a counter that outlives it.
    struct Counting<'c>(&'c AtomicUsize);

    impl Calls for Counting<'_> {
        fn call(&self, _: Request) -> Result<Outcome, OpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(OpError::unsupported("counted"))
        }
        fn get(&self, _: &DocPath) -> Result<Option<Value>, OpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    struct Never;

    impl Cancel for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn run() -> Arc<Run> {
        Arc::new(Run {
            lent: RwLock::new(None),
            limits: Limits::console(),
            request: RunRequest {
                name: "lease".into(),
                source: String::new(),
                actor: Actor::Person { name: "tester".into() },
                grants: Grants::person(),
                tx: None,
                params: JsonMap::new(),
                dry_run: false,
            },
            sent: AtomicU32::new(0),
            reads: AtomicU32::new(0),
            plan_bytes: AtomicUsize::new(2),
            pending_stop: AtomicU8::new(0),
            held_ticks: AtomicU32::new(0),
            last_ops: AtomicU64::new(0),
            output: Mutex::new(Output::default()),
            plans: Mutex::new(Vec::new()),
        })
    }

    fn is_cancel(error: &EvalAltResult) -> bool {
        matches!(error, EvalAltResult::ErrorTerminated(t, _) if matches!(t.clone().try_cast::<Stop>(), Some(Stop::Cancelled)))
    }

    #[test]
    fn a_callback_that_outlives_the_lease_cannot_reach_the_services() {
        let reached = AtomicUsize::new(0);
        // A clone of the run, as an engine callback holds one; it lives on after the services are gone.
        let kept = run();
        {
            let calls = Counting(&reached);
            let inside = kept.lend(&calls, &Never, || kept.invoke("face.set_profile", Value::Null, Position::NONE));
            assert!(inside.is_err(), "the counted call answers an error");
            assert_eq!(reached.load(Ordering::SeqCst), 1, "inside the lease the call reaches the services");
        } // `calls` is dropped here; the lease ended before it.
        assert!(kept.lent.read().unwrap().is_none(), "the slot is empty once `lend` returns");
        let after = kept.invoke("face.set_profile", Value::Null, Position::NONE).unwrap_err();
        assert!(is_cancel(&after), "a late call stops as cancelled");
        assert!(kept.progress(1, 0.0, &SystemClock::new()).is_some(), "a late progress tick stops the engine");
        assert_eq!(reached.load(Ordering::SeqCst), 1, "nothing reached the services after the lease");
    }

    #[test]
    fn a_panic_inside_the_lease_still_empties_the_slot() {
        let reached = AtomicUsize::new(0);
        let kept = run();
        let calls = Counting(&reached);
        let panicked = catch_unwind(AssertUnwindSafe(|| kept.lend(&calls, &Never, || panic!("inside the run"))));
        assert!(panicked.is_err());
        assert!(kept.lent.read().unwrap().is_none(), "unwinding empties the slot too");
        assert!(is_cancel(&kept.invoke("face.set_profile", Value::Null, Position::NONE).unwrap_err()));
        assert_eq!(reached.load(Ordering::SeqCst), 0);
    }

    /// Scripts that nest a value one level per step until the size limits stop them.
    const NESTERS: [&str; 3] = [
        "let v = []; loop { v = [v]; }",
        "let v = #{}; loop { v = #{ a: v }; }",
        "let v = []; loop { v = [#{ a: v }]; }",
    ];

    /// Runs `source` on a thread with only `stack` bytes.
    #[allow(clippy::result_large_err)] // the run's own result
    fn run_on_share(host: &ScriptHost, stack: usize, source: &str) -> Result<ScriptOutcome, ScriptError> {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(stack)
                .spawn_scoped(scope, || {
                    let request = RunRequest {
                        name: "nest".into(),
                        source: source.into(),
                        actor: Actor::Person { name: "tester".into() },
                        grants: Grants::person(),
                        tx: None,
                        params: JsonMap::new(),
                        dry_run: false,
                    };
                    host.run_here(request, &Counting(&AtomicUsize::new(0)), &Never)
                })
                .unwrap()
                .join()
                .unwrap()
        })
    }

    fn share_limits() -> Limits {
        Limits { array_len: 5_000, map_len: 5_000, operations: 1_000_000_000, seconds: 120, ..Limits::console() }
    }

    /// Nesting as deep as the limits allow, run on a thread with only the per-level share of the worker stack (the
    /// base is left out), must end with the size limit. This asserts the margin of `Limits::STACK_PER_LEVEL`: if a
    /// level cost more than it is allotted, the thread would overflow and abort the test process.
    #[test]
    fn the_per_level_share_of_the_worker_stack_holds_the_deepest_nesting_the_limits_allow() {
        let limits = share_limits();
        let stack = limits.worker_stack_bytes().unwrap() - Limits::STACK_BASE;
        let host = ScriptHost::new(&[], limits);
        for source in NESTERS {
            let kind = run_on_share(&host, stack, source).unwrap_err().kind;
            assert!(
                matches!(kind, ScriptErrorKind::Limit(LimitKind::ArraySize | LimitKind::MapSize)),
                "{source}: {kind:?}"
            );
        }
    }

    /// What a script can do to a value nested just under the limits (printing it, interpolating it, comparing it,
    /// throwing it) recurses once per level too, so each must fit in the same per-level share.
    #[test]
    fn the_per_level_share_also_holds_what_a_script_does_with_the_deepest_value() {
        let limits = Limits { array_len: 1_000, map_len: 1_000, ..share_limits() };
        let stack = limits.worker_stack_bytes().unwrap() - Limits::STACK_BASE;
        let host = ScriptHost::new(&[], limits);
        let builds = [
            "let v = []; for i in 0..950 { v = [v]; }",
            "let v = #{}; for i in 0..950 { v = #{ a: v }; }",
            "let v = []; for i in 0..480 { v = [#{ a: v }]; }",
        ];
        let uses = [
            "let s = `${v}`; s.len()",
            "print(v); 1",
            "let w = v; v == w",
            "let w = v; v != w",
            "throw v",
            "try { throw v; } catch (e) { 1 }",
            "let w = [v, v]; w.len()",
        ];
        for build in builds {
            for usage in uses {
                let source = format!("{build}\n{usage}");
                // Any outcome but an abort is fine: a value, a runtime error or a size limit.
                let _ = run_on_share(&host, stack, &source);
            }
        }
    }

    /// Sets a flag when it is dropped; a script holds one in a local.
    #[derive(Clone)]
    struct Marker(Arc<AtomicBool>);

    impl Drop for Marker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn marked(source: &str) -> (Arc<AtomicBool>, Result<Value, ScriptError>) {
        let dropped = Arc::new(AtomicBool::new(false));
        let mut engine = Engine::new();
        let held = dropped.clone();
        engine.register_fn("marker", move || Dynamic::from(Marker(held.clone())));
        let ast = engine.compile(source).unwrap();
        let result = evaluate(&engine, &ast, &JsonMap::new(), &Limits::console());
        (dropped, result)
    }

    #[test]
    fn locals_nested_inside_the_limits_are_dropped_before_the_result_is_handed_back() {
        // The returned value is a scalar, so the nested local only goes when the scope does.
        let source = "let v = marker(); for i in 0..500 { v = [v]; } 0";
        let (dropped, result) = marked(source);
        assert_eq!(result.unwrap(), Value::from(0));
        assert!(dropped.load(Ordering::SeqCst), "the scope is gone before `execute` can commit");
    }

    #[test]
    fn locals_are_dropped_before_an_error_is_handed_back_for_rollback() {
        let (dropped, result) = marked("let v = marker(); throw 1;");
        assert!(result.is_err());
        assert!(dropped.load(Ordering::SeqCst), "the scope is gone before `run` rolls back");
    }
}

/// The peak heap a run reaches, read after the run on the same thread (`run_here`), against the budget.
#[cfg(test)]
mod peak {
    use super::*;

    /// Answers nothing: the scripts here call no op.
    struct NoCalls;

    impl Calls for NoCalls {
        fn call(&self, _: Request) -> Result<omuzaki_ops_api::Outcome, OpError> {
            Err(OpError::internal("no op is expected"))
        }

        fn get(&self, _: &DocPath) -> Result<Option<Value>, OpError> {
            Ok(None)
        }
    }

    struct Never;

    impl Cancel for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    /// Runs `source` under the console limits and returns how it ended and the most heap it held, in bytes.
    fn peak_of(source: &str) -> (Result<ScriptOutcome, ScriptError>, usize) {
        peak_with(Limits::console(), source)
    }

    /// As [`peak_of`], under `limits`.
    fn peak_with(limits: Limits, source: &str) -> (Result<ScriptOutcome, ScriptError>, usize) {
        let host = ScriptHost::new(&[], limits);
        let request = RunRequest {
            name: "peak".into(),
            source: source.into(),
            actor: Actor::Person { name: "tester".into() },
            grants: Grants::person(),
            tx: None,
            params: JsonMap::new(),
            dry_run: false,
        };
        let result = host.run_here(request, &NoCalls, &Never);
        (result, budget::peak_here())
    }

    const LONG_KEY: &str = r#"let k = "x"; while k.len < 500000 { k += k; }"#;

    /// A native call that copies a value many times must be refused before it copies, not after: the budget is only
    /// read between operations, so the peak is what shows whether a call got through.
    #[test]
    fn a_native_call_cannot_copy_past_the_budget_before_the_next_read() {
        let limit = Limits::console().memory_bytes;
        // `pad` clones a map whose keys no data limit counts: 100 copies of ten half-megabyte keys is 500 MiB.
        let pad = format!("{LONG_KEY} let m = #{{}}; for i in 0..10 {{ m[k + i] = 1; }} let a = []; a.pad(100, m); 1");
        // `replace` builds a result as long as the matches times the replacement, 20,000 x 20,000 here.
        let replace = r#"let a = "a"; while a.len < 20000 { a += a; } let b = a.replace("a", a); b.len"#;
        for (name, source) in [("pad", pad.as_str()), ("replace", replace)] {
            let (result, peak) = peak_of(source);
            let kind = result.expect_err(name).kind;
            assert!(matches!(kind, ScriptErrorKind::Limit(_)), "{name}: {kind:?}");
            assert!(peak < limit / 4, "{name}: held {} MiB before it was refused", peak >> 20);
        }
    }

    /// Without any one call to blame, the run still ends at the budget, within one operation of it.
    #[test]
    fn map_keys_stop_within_one_operation_of_the_budget() {
        let limit = Limits::console().memory_bytes;
        let (result, peak) = peak_of(&format!("{LONG_KEY} let m = #{{}}; for i in 0..2000 {{ m[k + i] = 1; }}"));
        assert_eq!(result.expect_err("stops").kind, ScriptErrorKind::Limit(LimitKind::Memory));
        assert!(peak <= limit + (4 << 20), "held {} MiB with a {} MiB budget", peak >> 20, limit >> 20);
    }

    /// Turning a value into text builds the whole text before any output limit looks at it, unless it is bounded.
    #[test]
    fn formatting_a_huge_value_stops_at_the_limit_instead_of_building_it() {
        // 300 half-megabyte keys: about 150 MiB, under the budget, and each key is more than the output limit.
        let build = format!("{LONG_KEY} let m = #{{}}; for i in 0..300 {{ m[k + i] = 1; }}");
        let (_, held) = peak_of(&build);
        // The last number is the copies of the value the statement makes before it formats anything.
        for (name, statement, ends, copies) in [
            ("log", "log(m);", "ok", 0),
            ("print", "print(m);", "ok", 0),
            ("debug", "debug(m);", "ok", 0),
            ("to_string", "let s = m.to_string();", "limit", 0),
            // Rhai builds these by calling `to_string` and, if that fails, formatting the whole value itself.
            ("interpolation", "let s = `a${m}b`;", "limit", 1),
            ("string plus value", r#"let s = "a" + m;"#, "limit", 1),
            ("value plus string", r#"let s = m + "a";"#, "limit", 1),
            ("string append", r#"let s = "a"; s += m;"#, "limit", 1),
            ("interpolated array", "let s = `${[m]}`;", "limit", 1),
        ] {
            let (result, peak) = peak_of(&format!("{build} {statement}"));
            match (ends, &result) {
                ("ok", Ok(outcome)) => {
                    assert!(
                        outcome.output.iter().any(|line| line.text == "output truncated"),
                        "{name}: {:?}",
                        outcome.output
                    );
                }
                ("limit", Err(error)) => {
                    assert!(
                        matches!(error.kind, ScriptErrorKind::Limit(LimitKind::StringSize | LimitKind::Memory)),
                        "{name}: {error}"
                    );
                }
                _ => panic!("{name}: {:?}", result.as_ref().map(|o| &o.output)),
            }
            // Formatting in full is several times the value; the copy a script makes to interpolate or add a value
            // is the one extra it is allowed (and is what the memory limit then stops).
            let allowed = copies * held + (8 << 20);
            assert!(
                peak <= held + allowed,
                "{name}: held {} MiB over the {} MiB it started with",
                peak >> 20,
                held >> 20
            );
        }
    }

    /// A run that is already over its memory limit formats nothing, on any path, and ends with `Limit(Memory)`.
    #[test]
    fn a_run_over_its_limit_formats_nothing_and_stops_with_the_memory_limit() {
        let tight = Limits { memory_bytes: 2_500_000, ..Limits::console() };
        // Three half-megabyte keys: 1.5 MB held, so one more copy of the map is over the limit.
        let build = format!("{LONG_KEY} let m = #{{}}; for i in 0..3 {{ m[k + i] = 1; }} let over = m;");
        let (_, base) = peak_with(tight.clone(), &build);
        for (name, statement) in [
            ("interpolation", "let s = `a${m}b`;"),
            ("interpolated array", "let s = `${[m]}`;"),
            ("string plus value", r#"let s = "a" + m;"#),
            ("value plus string", r#"let s = m + "a";"#),
            ("string append", r#"let s = "a"; s += m;"#),
            ("to_string", "let s = m.to_string();"),
            ("to_debug", "let s = m.to_debug();"),
            ("print", "print(m);"),
            ("debug", "debug(m);"),
            ("log", "log(m);"),
        ] {
            let (result, peak) = peak_with(tight.clone(), &format!("{build} {statement}"));
            assert_eq!(result.expect_err(name).kind, ScriptErrorKind::Limit(LimitKind::Memory), "{name}");
            // The limit plus the copy the statement itself makes plus a tick's worth of small work.
            assert!(peak <= base + 1_500_000 + (1 << 20), "{name}: peak {} KiB from {} KiB", peak >> 10, base >> 10);
        }
    }

    /// The message of an uncaught `throw` is Rhai's text for the thrown value, built after the run stopped counting.
    #[test]
    fn throwing_a_huge_value_reports_it_cut_at_the_limit() {
        let build = format!("{LONG_KEY} let m = #{{}}; for i in 0..300 {{ m[k + i] = 1; }}");
        let (_, held) = peak_of(&build);
        let (result, peak) = peak_of(&format!("{build} throw m;"));
        let error = result.expect_err("a thrown value is an error");
        assert_eq!(error.kind, ScriptErrorKind::Runtime, "{}", error.message.len());
        assert!(error.message.starts_with("Runtime error: #{"), "{}", &error.message[..40]);
        assert!(error.message.len() <= Limits::console().string_bytes + 64, "{} bytes", error.message.len());
        // `throw m` copies the value once (a copy of what the script already holds, as the module docs of `guards` say);
        // formatting it in full would be several more.
        assert!(peak <= 2 * held + (8 << 20), "held {} MiB with {} MiB to start", peak >> 20, held >> 20);
        // A small thrown value keeps Rhai's own text.
        let (result, _) = peak_of("throw #{ a: 1 };");
        let mut small = Map::new();
        small.insert("a".into(), Dynamic::from(1_i64));
        let rhais = EvalAltResult::ErrorRuntime(Dynamic::from_map(small), Position::new(1, 1)).to_string();
        assert_eq!(result.expect_err("thrown").message, rhais);
    }

    /// The last operation can pass the budget and free what it took before another progress tick looks.
    #[test]
    fn a_final_call_that_passed_the_budget_cannot_commit() {
        // 256 KiB of text; the last `+` of four copies makes a 768 KiB string while a 512 KiB one and the original are
        // alive: about 1.5 MiB at once, against a 1.2 MiB budget, released again when the call returns.
        let build = r#"let s = "x"; while s.len < 200000 { s += s; } "#;
        let tight = Limits { memory_bytes: 1_200_000, ..Limits::console() };
        let (result, _) = peak_with(tight.clone(), &format!("{build} (s + s + s).len()"));
        assert_eq!(result.expect_err("over budget").kind, ScriptErrorKind::Limit(LimitKind::Memory));
        let (result, _) = peak_with(tight, &format!("{build} (s + s).len()"));
        assert_eq!(result.expect("a smaller last call commits").value, 524_288);
    }

    /// The bounded versions say what Rhai's own do for values that fit.
    #[test]
    fn bounded_formatting_matches_rhais_own_for_values_that_fit() {
        let script = r#"let v = [1, "a", #{ k: 2.5, j: [()] }, (), true, 'c', 1.0e10];
            print(v); debug(v); print(v.to_string()); print(v.to_debug()); print(`${v}`);"#;
        let (ours, _) = peak_of(script);
        let ours: Vec<String> = ours.expect("runs").output.into_iter().map(|line| line.text).collect();
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut plain = Engine::new();
        let capture = lines.clone();
        plain.on_print(move |text| capture.lock().unwrap().push(text.to_owned()));
        let capture = lines.clone();
        plain.on_debug(move |text, _, _| capture.lock().unwrap().push(text.to_owned()));
        plain.run(script).unwrap();
        assert_eq!(ours, *lines.lock().unwrap());
    }

    #[test]
    fn padding_and_replacing_within_the_limits_still_work() {
        let (result, _) = peak_of(r#"let a = [1]; a.pad(5, "x"); let s = "a-b-c"; s.replace("-", "+"); [a.len(), s]"#);
        assert_eq!(result.expect("runs").value, serde_json::json!([5, "a+b+c"]));
        let (result, _) =
            peak_of(r#"let s = "abc"; s.replace('b', "XY"); s.replace("XY", 'z'); s.replace('a', 'q'); s"#);
        assert_eq!(result.expect("runs").value, "qzc");
    }
}
