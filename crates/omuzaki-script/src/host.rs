//! The Rhai engine: one fresh engine per run, the limits, cancel, output and error mapping.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, OpInfo, OpKind, Request, TxId, Undo};
use rhai::packages::{
    BasicArrayPackage, BasicMapPackage, BasicMathPackage, CorePackage, LogicPackage, MoreStringPackage, Package,
};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, Engine, EvalAltResult, Map, Module, NativeCallContext, ParseErrorType, Position, Scope, Shared};
use serde_json::Map as JsonMap;

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
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptOutcome {
    /// The script's last value, as JSON.
    pub value: Value,
    /// Everything it printed or logged.
    pub output: Vec<OutputLine>,
    /// Inner op calls sent.
    pub calls: u32,
    /// The transaction the calls implementation committed, if it owns one.
    pub tx: Option<TxId>,
    /// Each call a dry run planned; empty for a real run.
    pub plans: Vec<PlannedCall>,
}

/// Why the engine was told to stop; carried as the `ErrorTerminated` token, which a script cannot catch.
#[derive(Copy, Clone)]
enum Stop {
    Cancelled,
    Limit(LimitKind),
}

/// Why an op is not callable from a script, carried like [`Stop`] so a script can't catch it.
#[derive(Clone)]
struct Refusal(Arc<str>);

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
struct Run {
    /// Cleared by [`Lease`] before `run` returns, so no callback can reach the borrowed services afterwards.
    lent: RwLock<Option<Lent>>,
    limits: Limits,
    request: RunRequest,
    sent: AtomicU32,
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
    EvalAltResult::ErrorTerminated(Dynamic::from(stop), Position::NONE).into()
}

#[allow(clippy::unnecessary_box_returns)] // as above
fn runtime(message: impl Into<String>) -> Box<EvalAltResult> {
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

    /// The one call path the bindings use: stops on cancel and at the call limit, then sends the request.
    fn invoke(&self, op: &str, args: Value) -> Result<Value, Box<EvalAltResult>> {
        let lent = self.lent.read().unwrap_or_else(PoisonError::into_inner);
        let Some(lent) = lent.as_ref() else { return Err(terminate(Stop::Cancelled)) };
        if lent.cancel.is_cancelled() {
            return Err(terminate(Stop::Cancelled));
        }
        if self.sent.load(Ordering::SeqCst) >= self.limits.calls {
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
                    self.plans.lock().unwrap_or_else(PoisonError::into_inner).push(PlannedCall {
                        op: op.to_owned(),
                        args,
                        plan: outcome.plan,
                    });
                }
                Ok(value)
            }
            Err(error) => {
                let mut map = to_dynamic(&error).map_err(|e| runtime(e.to_string()))?;
                if let Some(fields) = map.write_lock::<Map>().as_mut() {
                    fields.insert("op".into(), Dynamic::from(op.to_owned()));
                }
                Err(EvalAltResult::ErrorRuntime(map, Position::NONE).into())
            }
        }
    }

    /// Why the run must stop now, if it must: cancelled (or the lease is over), or out of wall-clock time.
    fn stop(&self, started: f64, clock: &dyn Clock) -> Option<Stop> {
        let lent = self.lent.read().unwrap_or_else(PoisonError::into_inner);
        if lent.as_ref().is_none_or(|l| l.cancel.is_cancelled()) {
            return Some(Stop::Cancelled);
        }
        (clock.now().0 - started >= f64::from(self.limits.seconds)).then_some(Stop::Limit(LimitKind::Time))
    }

    fn progress(&self, started: f64, clock: &dyn Clock) -> Option<Dynamic> {
        self.stop(started, clock).map(Dynamic::from)
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

fn run_of(context: &NativeCallContext<'_>) -> Result<Arc<Run>, Box<EvalAltResult>> {
    context.tag().and_then(|tag| tag.clone().try_cast::<Arc<Run>>()).ok_or_else(|| runtime("no run is active"))
}

/// Runs scripts. Holds the engine's shared parts; each [`run`](Self::run) builds a fresh engine from them.
pub struct ScriptHost {
    packages: Vec<Shared<Module>>,
    bindings: Vec<(String, Shared<Module>)>,
    limits: Limits,
    clock: Arc<dyn Clock>,
}

impl ScriptHost {
    /// A host whose scripts can call `ops::area::verb({..})` for each op in `ops`, held to `limits`.
    ///
    /// Only queries and edits that undo with their patch ([`Undo::Auto`]) can be called: the run's rollback can't undo
    /// a job or an edit with outside effects, so those are bound to a refusal (`ScriptErrorKind::Refused`) that is
    /// raised before anything is sent.
    ///
    /// The bindings are the plain call path; the schema-aware generated bindings, `get`, `try_op` and the static check
    /// come with the next task.
    pub fn new(ops: &[OpInfo], limits: Limits) -> Self {
        let packages = vec![
            CorePackage::new().as_shared_module(),
            LogicPackage::new().as_shared_module(),
            MoreStringPackage::new().as_shared_module(),
            BasicArrayPackage::new().as_shared_module(),
            BasicMapPackage::new().as_shared_module(),
            BasicMathPackage::new().as_shared_module(),
        ];
        let mut areas: BTreeMap<String, Module> = BTreeMap::new();
        for info in ops {
            let Some((area, verb)) = info.name.split_once('.') else { continue };
            let name = info.name.to_owned();
            let refusal = refusal(info).map(|reason| Refusal(reason.into()));
            let limits = limits.clone();
            areas.entry(format!("ops::{area}")).or_default().set_native_fn(
                verb,
                move |context: NativeCallContext<'_>, args: Map| -> Result<Dynamic, Box<EvalAltResult>> {
                    let at = context.call_position();
                    if let Some(refusal) = &refusal {
                        return Err(EvalAltResult::ErrorTerminated(Dynamic::from(refusal.clone()), at).into());
                    }
                    let args = match to_json(Dynamic::from_map(args), limits.value_depth) {
                        Ok(args) => args,
                        Err(Convert::Depth) => {
                            return Err(EvalAltResult::ErrorTerminated(
                                Dynamic::from(Stop::Limit(LimitKind::Depth)),
                                at,
                            )
                            .into());
                        }
                        Err(Convert::Other(message)) => return Err(runtime(message)),
                    };
                    // Rhai never sizes map keys, so the converted arguments get the same check as `params` and op
                    // outputs before the op sees them.
                    if let Some(kind) = oversized(&args, &limits) {
                        return Err(EvalAltResult::ErrorTerminated(Dynamic::from(Stop::Limit(kind)), at).into());
                    }
                    let value = run_of(&context)?.invoke(&name, args).map_err(|mut e| {
                        e.set_position(at);
                        e
                    })?;
                    // Checked before conversion, as `params` are: Rhai sizes a native result only after building it,
                    // and never checks map keys.
                    if let Some(kind) = oversized(&value, &limits) {
                        return Err(EvalAltResult::ErrorTerminated(Dynamic::from(Stop::Limit(kind)), at).into());
                    }
                    to_dynamic(value).map_err(|e| runtime(e.to_string()))
                },
            );
        }
        let bindings = areas.into_iter().map(|(path, module)| (path, Shared::new(module))).collect();
        Self { packages, bindings, limits, clock: Arc::new(SystemClock::new()) }
    }

    /// Reads the wall-clock limit from `clock`, so tests need no sleep.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn engine(&self, run: &Arc<Run>, started: f64) -> Engine {
        let mut engine = Engine::new_raw();
        for package in &self.packages {
            engine.register_global_module(package.clone());
        }
        for (path, module) in &self.bindings {
            engine.register_static_module(path, module.clone());
        }
        let limits = &self.limits;
        engine
            .set_max_operations(limits.operations)
            .set_max_call_levels(limits.call_depth)
            .set_max_expr_depths(limits.expr_depth, limits.fn_expr_depth)
            .set_max_string_size(limits.string_bytes)
            .set_max_array_size(limits.array_len)
            .set_max_map_size(limits.map_len)
            .set_default_tag(Dynamic::from(run.clone()));
        for symbol in ["eval", "Fn", "call", "curry", "import"] {
            engine.disable_symbol(symbol);
        }
        let sink = run.clone();
        engine.on_print(move |text| sink.print(OutputKind::Print, text));
        let sink = run.clone();
        engine.on_debug(move |text, _, _| sink.print(OutputKind::Note, text));
        let sink = run.clone();
        // A float would otherwise reach the math package's `log(x)` (the logarithm), so it is overloaded too.
        engine.register_fn("log", move |value: Dynamic| sink.print(OutputKind::Note, &value.to_string()));
        let sink = run.clone();
        engine.register_fn("log", move |value: f64| sink.print(OutputKind::Note, &value.to_string()));
        let (watch, clock) = (run.clone(), self.clock.clone());
        engine.on_progress(move |_| watch.progress(started, clock.as_ref()));
        engine
    }

    /// Runs a script. Any failure, limit or cancel rolls back through [`Calls::rollback`].
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
        let run = Arc::new(Run {
            lent: RwLock::new(None),
            limits: self.limits.clone(),
            request,
            sent: AtomicU32::new(0),
            output: Mutex::new(Output::default()),
            plans: Mutex::new(Vec::new()),
        });
        let result = run.lend(calls, cancel, || self.execute(&run, calls, cancel));
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
    fn execute(&self, run: &Arc<Run>, calls: &dyn Calls, cancel: &dyn Cancel) -> Result<ScriptOutcome, ScriptError> {
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
        let params_break = map_breaks(&run.request.params, &self.limits)
            .or_else(|| run.request.params.values().find_map(|value| oversized_in(value, &self.limits, 1)));
        if let Some(kind) = params_break {
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
        let value = evaluate(&engine, &ast, &run.request.params, &self.limits)?;
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
/// `Calls::commit` makes anything durable and before a limit error rolls back. The nesting bound itself (and a worker
/// thread whose stack comes from the operation limit) is open in #1877.
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
    let returned = engine.eval_ast_with_scope::<Dynamic>(&mut scope, ast).map_err(|e| classify(&e))?;
    to_json(returned, limits.value_depth).map_err(|e| match e {
        Convert::Depth => fail(ScriptErrorKind::Limit(LimitKind::Depth), "the result is nested deeper than the limit"),
        Convert::Other(message) => fail(ScriptErrorKind::Runtime, &format!("the result is not JSON: {message}")),
    })
}

/// Why `info` can't be called from a script, if it can't.
fn refusal(info: &OpInfo) -> Option<String> {
    let name = info.name;
    match (info.kind, info.undo) {
        (OpKind::Query, _) | (OpKind::Edit, Undo::Auto) => None,
        (OpKind::Job, _) => Some(format!("`{name}` is a job, which a script can't start: run it as a job instead")),
        (OpKind::Edit, _) => {
            Some(format!("`{name}` has effects outside the document, which a script's rollback can't undo"))
        }
    }
}

/// The first collection limit a JSON value breaks, if any. Values converted outside the engine (`params`, op outputs)
/// are checked with this before conversion: Rhai doesn't size values pushed into the scope, and never checks map keys.
/// Iterative, so a deeply nested value can't overflow the stack.
fn oversized(value: &Value, limits: &Limits) -> Option<LimitKind> {
    oversized_in(value, limits, 0)
}

/// [`oversized`] for a value that sits `outer` containers deep already (a `params` field is inside the `params` map).
/// Nesting beyond [`Limits::value_depth`] is [`LimitKind::Depth`], so the recursive conversion into Rhai is bounded.
fn oversized_in(value: &Value, limits: &Limits, outer: usize) -> Option<LimitKind> {
    let mut pending = vec![(value, outer)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            Value::String(text) if text.len() > limits.string_bytes => return Some(LimitKind::StringSize),
            Value::Array(_) | Value::Object(_) if depth >= limits.value_depth => return Some(LimitKind::Depth),
            Value::Array(items) if items.len() > limits.array_len => return Some(LimitKind::ArraySize),
            Value::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(fields) => {
                if let Some(kind) = map_breaks(fields, limits) {
                    return Some(kind);
                }
                pending.extend(fields.values().map(|field| (field, depth + 1)));
            }
            _ => {}
        }
    }
    None
}

/// Why [`to_json`] failed.
enum Convert {
    /// Arrays and maps nest deeper than [`Limits::value_depth`].
    Depth,
    /// A value with no JSON form.
    Other(String),
}

/// One array or map being converted: what is left to visit and what has been converted so far.
struct Frame {
    rest: Box<dyn Iterator<Item = (String, Dynamic)>>,
    key: String,
    done: Done,
}

enum Done {
    Array(Vec<Value>),
    Object(JsonMap<String, Value>),
}

/// A script value as JSON, walked with an explicit stack: rhai's `from_dynamic` recurses once per level, so a script
/// that wraps a value in an array in a loop could overflow the stack from inside the limits on size and operations.
/// Fails with [`Convert::Depth`] beyond `max_depth` levels of arrays and maps. The value is consumed, and on failure
/// whatever is left is taken apart without recursion as well, so dropping a deep value can't overflow either.
fn to_json(root: Dynamic, max_depth: usize) -> Result<Value, Convert> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut next = Some(root);
    let mut finished: Option<Value> = None;
    loop {
        if let Some(item) = next.take() {
            let item = item.flatten();
            if item.is_array() || item.is_map() {
                if stack.len() >= max_depth {
                    abandon(stack, Some(item));
                    return Err(Convert::Depth);
                }
                stack.push(open(item));
            } else {
                match from_dynamic::<Value>(&item) {
                    Ok(value) => finished = Some(value),
                    Err(e) => {
                        abandon(stack, None);
                        return Err(Convert::Other(e.to_string()));
                    }
                }
            }
        }
        if let Some(value) = finished.take() {
            let Some(top) = stack.last_mut() else { return Ok(value) };
            match &mut top.done {
                Done::Array(items) => items.push(value),
                Done::Object(fields) => {
                    fields.insert(std::mem::take(&mut top.key), value);
                }
            }
        }
        let Some(top) = stack.last_mut() else {
            return Err(Convert::Other("the conversion lost its place".to_owned()));
        };
        match top.rest.next() {
            Some((key, item)) => {
                top.key = key;
                next = Some(item);
            }
            None => {
                finished = stack.pop().map(|frame| match frame.done {
                    Done::Array(items) => Value::Array(items),
                    Done::Object(fields) => Value::Object(fields),
                });
            }
        }
    }
}

/// Starts converting an array or map.
fn open(container: Dynamic) -> Frame {
    if container.is_array() {
        let items = container.into_array().unwrap_or_default();
        let done = Done::Array(Vec::with_capacity(items.len()));
        Frame { rest: Box::new(items.into_iter().map(|item| (String::new(), item))), key: String::new(), done }
    } else {
        let fields = container.try_cast::<Map>().unwrap_or_default();
        let done = Done::Object(JsonMap::new());
        Frame { rest: Box::new(fields.into_iter().map(|(k, v)| (k.to_string(), v))), key: String::new(), done }
    }
}

/// Drops what a failed conversion still holds, one level at a time, so no destructor recurses.
fn abandon(stack: Vec<Frame>, extra: Option<Dynamic>) {
    let mut pending: Vec<Dynamic> = extra.into_iter().collect();
    for frame in stack {
        pending.extend(frame.rest.map(|(_, item)| item));
    }
    while let Some(item) = pending.pop() {
        let item = item.flatten();
        if item.is_array() {
            pending.extend(item.into_array().unwrap_or_default());
        } else if item.is_map() {
            pending.extend(item.try_cast::<Map>().unwrap_or_default().into_values());
        }
    }
}

/// The limit a map itself breaks: its size, or a key (which becomes a Rhai string) over the string limit.
fn map_breaks(fields: &JsonMap<String, Value>, limits: &Limits) -> Option<LimitKind> {
    if fields.len() > limits.map_len {
        return Some(LimitKind::MapSize);
    }
    fields.keys().any(|key| key.len() > limits.string_bytes).then_some(LimitKind::StringSize)
}

fn size_kind(typ: &str) -> LimitKind {
    let typ = typ.to_ascii_lowercase();
    if typ.contains("string") {
        LimitKind::StringSize
    } else if typ.contains("array") {
        LimitKind::ArraySize
    } else {
        LimitKind::MapSize
    }
}

fn located(kind: ScriptErrorKind, message: String, at: Position, op_error: Option<OpError>) -> ScriptError {
    let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    ScriptError {
        kind,
        line: at.line().map_or(1, to_u32),
        column: at.position().map_or(1, to_u32),
        op_error,
        message,
        output: Vec::new(),
    }
}

/// Maps Rhai's error to ours, looking through the wrappers a call inside a script function adds.
fn classify(error: &EvalAltResult) -> ScriptError {
    let at = error.position();
    let (kind, op_error) = match error {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => return classify(inner),
        EvalAltResult::ErrorTooManyOperations(_) => (ScriptErrorKind::Limit(LimitKind::Operations), None),
        EvalAltResult::ErrorStackOverflow(_) => (ScriptErrorKind::Limit(LimitKind::Depth), None),
        EvalAltResult::ErrorDataTooLarge(typ, _) => (ScriptErrorKind::Limit(size_kind(typ)), None),
        EvalAltResult::ErrorTerminated(token, _) => match token.clone().try_cast::<Stop>() {
            Some(Stop::Cancelled) => (ScriptErrorKind::Cancelled, None),
            Some(Stop::Limit(kind)) => (ScriptErrorKind::Limit(kind), None),
            None => match token.clone().try_cast::<Refusal>() {
                Some(Refusal(reason)) => return located(ScriptErrorKind::Refused, reason.to_string(), at, None),
                None => (ScriptErrorKind::Runtime, None),
            },
        },
        EvalAltResult::ErrorRuntime(payload, _) => match op_error_of(payload) {
            Some(op_error) => (ScriptErrorKind::Op, Some(op_error)),
            None => (ScriptErrorKind::Runtime, None),
        },
        _ => (ScriptErrorKind::Runtime, None),
    };
    let message = op_error.as_ref().map_or_else(|| error.to_string(), |e| e.message.clone());
    located(kind, message, at, op_error)
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
            let inside = kept.lend(&calls, &Never, || kept.invoke("face.set_profile", Value::Null));
            assert!(inside.is_err(), "the counted call answers an error");
            assert_eq!(reached.load(Ordering::SeqCst), 1, "inside the lease the call reaches the services");
        } // `calls` is dropped here; the lease ended before it.
        assert!(kept.lent.read().unwrap().is_none(), "the slot is empty once `lend` returns");
        let after = kept.invoke("face.set_profile", Value::Null).unwrap_err();
        assert!(is_cancel(&after), "a late call stops as cancelled");
        assert!(kept.progress(0.0, &SystemClock::new()).is_some(), "a late progress tick stops the engine");
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
        assert!(is_cancel(&kept.invoke("face.set_profile", Value::Null).unwrap_err()));
        assert_eq!(reached.load(Ordering::SeqCst), 0);
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
