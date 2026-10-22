//! The script's view of the op registry: `ops::area::verb({..})` for every registered op, plus `try_op` and `get`.
//!
//! One pass over [`OpInfo`] builds the table ([`Bindings`]); the engine's modules are generated from it, and the static
//! check ([`crate::check`]) resolves names through the same table, so the run and the check cannot disagree about what
//! an op call is (`docs/design/scripting.md` sections 2 and 6).

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{OpError, OpInfo, OpKind, Scope, Undo};
use rhai::serde::from_dynamic;
use rhai::{Dynamic, Engine, EvalAltResult, FnPtr, ImmutableString, Map, Module, NativeCallContext, Shared};

use crate::convert::{Convert, from_json, oversized, to_json};
use crate::host::{Refusal, Run, Stop, run_of, runtime, terminate_at};
use crate::{LimitKind, Limits};

/// The namespace every op binding lives under: `ops::area::verb`.
pub(crate) const NAMESPACE: &str = "ops";

/// The registry name of the op that starts a script run; a script can't start another one.
const SCRIPT_RUN: &str = "script.run";

/// One op as scripts and the check see it.
pub(crate) struct OpEntry {
    /// The registered name, `area.verb_noun`.
    pub name: &'static str,
    /// The scope the op needs.
    pub scope: Scope,
    /// Why a script may not call it, if it may not.
    pub refusal: Option<Arc<OpError>>,
    /// The JSON Schema of its arguments, as JSON.
    pub schema: Value,
}

/// Every callable name, and the engine modules generated from them.
pub(crate) struct Bindings {
    /// By `(area, verb)`, the two halves of the registry name around its first dot.
    entries: BTreeMap<(String, String), OpEntry>,
    modules: Vec<(String, Shared<Module>)>,
}

impl Bindings {
    /// The table and the modules for `ops`; ops without an `area.verb` name are not callable from scripts.
    pub(crate) fn new(ops: &[OpInfo], limits: &Limits) -> Self {
        let probe = Engine::new_raw();
        let mut entries = BTreeMap::new();
        for info in ops {
            let Some((area, verb)) = info.name.split_once('.') else { continue };
            let spelled = (spelled_in_script(&probe, area), spelled_in_script(&probe, verb));
            // A schema that can't be read would leave the argument checks off, so the op is refused instead.
            let (schema, unreadable) = match serde_json::to_value(&info.schema) {
                Ok(schema) => (schema, None),
                Err(error) => (
                    Value::Bool(true),
                    Some(
                        OpError::internal(format!("the argument schema of `{}` can't be read: {error}", info.name))
                            .hint("report this: the op is registered with a schema that doesn't serialise"),
                    ),
                ),
            };
            let refusal = refusal(info).or(unreadable).map(Arc::new);
            let entry = OpEntry { name: info.name, scope: info.scope, refusal, schema };
            entries.insert(spelled, entry);
        }
        let mut areas: BTreeMap<String, Module> = BTreeMap::new();
        for ((area, verb), entry) in &entries {
            areas
                .entry(format!("{NAMESPACE}::{area}"))
                .or_default()
                .set_native_fn(verb.as_str(), call_of(entry, limits));
        }
        let modules = areas.into_iter().map(|(path, module)| (path, Shared::new(module))).collect();
        Self { entries, modules }
    }

    /// The op a call `path::verb(..)` names: `path` is the namespace segments (`["ops", "face"]`). The one resolver the
    /// run's modules and the check share.
    pub(crate) fn resolve(&self, path: &[&str], verb: &str) -> Option<&OpEntry> {
        let [namespace, area] = path else { return None };
        if *namespace != NAMESPACE {
            return None;
        }
        self.entries.get(&((*area).to_owned(), verb.to_owned()))
    }

    /// The registry name a call `path::verb(..)` spells, whether or not the registry has it.
    pub(crate) fn spelled(path: &[&str], verb: &str) -> Option<String> {
        let [namespace, area] = path else { return None };
        (*namespace == NAMESPACE).then(|| format!("{area}.{verb}"))
    }

    /// Whether `path` is under the `ops` namespace at all.
    pub(crate) fn in_namespace(path: &[&str]) -> bool {
        path.first().is_some_and(|first| *first == NAMESPACE)
    }

    /// Puts the generated modules, `try_op` and `get` into `engine`.
    pub(crate) fn install(&self, engine: &mut Engine) {
        for (path, module) in &self.modules {
            engine.register_static_module(path, module.clone());
        }
        engine.register_fn("try_op", try_op);
        engine.register_fn("get", get);
    }
}

/// How a script spells one half of an op name. A word Rhai reserves (the keyword for writing a module's items out,
/// `import`, `private`, `match` and the like) can't appear in a path, so it gets a trailing underscore, as in
/// `ops::import_::clip`. Found by asking the parser, so it follows Rhai's own list.
fn spelled_in_script(probe: &Engine, word: &str) -> String {
    if probe.compile(format!("let {word} = 0;")).is_ok() { word.to_owned() } else { format!("{word}_") }
}

/// Why `info` can't be called from a script, if it can't: v1 runs `Query` ops and `Edit` ops that undo with their
/// patch. A job never joins a transaction and an edit with outside effects can't be rolled back with the run.
fn refusal(info: &OpInfo) -> Option<OpError> {
    refusal_for(info.name, info.kind, info.undo)
}

/// [`refusal`] from the facts it reads. A recorded macro is held to the same rule, since its replay is a transaction
/// too: a step that is a job or an edit with outside effects can't be played, recorded or saved, and a step can't
/// start `script.run`, which would recurse with no call cap and no clock.
pub(crate) fn refusal_for(name: &str, kind: OpKind, undo: Undo) -> Option<OpError> {
    if name == SCRIPT_RUN {
        return Some(
            OpError::unsupported("a script can't start another script run")
                .hint("scripts don't nest: call the ops you need from this script instead"),
        );
    }
    match (kind, undo) {
        (OpKind::Query, _) | (OpKind::Edit, Undo::Auto) => None,
        (OpKind::Job, _) => Some(
            OpError::unsupported(format!("`{name}` is a job, which a script can't start"))
                .hint("run it as a job on its own, outside the script"),
        ),
        (OpKind::Edit, _) => Some(
            OpError::unsupported(format!(
                "`{name}` has effects outside the document, which a script's rollback can't undo"
            ))
            .hint("run it on its own, outside the script"),
        ),
    }
}

/// The refusal for the op `name` among `ops` (what a runner lists); an op that isn't listed has none here, and its
/// call fails as unknown.
pub(crate) fn refusal_in(ops: &[OpInfo], name: &str) -> Option<OpError> {
    ops.iter().find(|info| info.name == name).and_then(refusal)
}

/// The refusal for the op `name` among the ops linked into this binary, for the places that have no runner to ask.
pub(crate) fn refusal_registered(name: &str) -> Option<OpError> {
    let meta = (omuzaki_ops_api::find(name)?.meta)();
    refusal_for(meta.name, meta.kind, meta.undo)
}

/// The native function behind one `ops::area::verb`.
fn call_of(
    entry: &OpEntry,
    limits: &Limits,
) -> impl Fn(NativeCallContext<'_>, Map) -> Result<Dynamic, Box<EvalAltResult>> + Send + Sync + 'static {
    let name = entry.name;
    let refusal = entry.refusal.clone();
    let limits = limits.clone();
    move |context, args| {
        let at = context.call_position();
        if let Some(refusal) = &refusal {
            return Err(terminate_at(Refusal(refusal.clone()), at));
        }
        let args = match to_json(Dynamic::from_map(args), limits.value_depth) {
            Ok(args) => args,
            Err(Convert::Depth) => return Err(terminate_at(Stop::Limit(LimitKind::Depth), at)),
            Err(Convert::Other(message)) => return Err(runtime(message)),
        };
        // Rhai never sizes map keys, so the converted arguments get the same check as `params` and op outputs before
        // the op sees them.
        if let Some(kind) = oversized(&args, &limits) {
            return Err(terminate_at(Stop::Limit(kind), at));
        }
        let value = run_of(&context)?.invoke(name, args, at).map_err(|e| placed(e, at))?;
        // Checked before conversion, as `params` are: Rhai sizes a native result only after building it, and never
        // checks map keys.
        if let Some(kind) = oversized(&value, &limits) {
            return Err(terminate_at(Stop::Limit(kind), at));
        }
        from_json(&value).map_err(runtime)
    }
}

/// `try_op(|| ops::area::verb(..))`: runs the closure and answers `#{ ok, value, error }`. Only a failed op is caught;
/// a limit, a cancel, a refusal or a script error still ends the run.
#[allow(clippy::needless_pass_by_value)] // Rhai registers a native function's arguments by value
fn try_op(context: NativeCallContext<'_>, call: FnPtr) -> Result<Dynamic, Box<EvalAltResult>> {
    match call.call_within_context::<Dynamic>(&context, ()) {
        Ok(value) => Ok(outcome(true, value, Dynamic::UNIT)),
        Err(error) => match caught(&error) {
            Some(failure) => Ok(outcome(false, Dynamic::UNIT, failure)),
            None => Err(error),
        },
    }
}

/// The op error a failed call carries, looking through the wrappers a call inside a script function adds.
fn caught(error: &EvalAltResult) -> Option<Dynamic> {
    match error {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => caught(inner),
        EvalAltResult::ErrorRuntime(payload, _) if is_op_error(payload) => Some(payload.clone()),
        _ => None,
    }
}

/// Whether a thrown value is the map a failed op call raises: an `OpError` that names the op.
pub(crate) fn is_op_error(payload: &Dynamic) -> bool {
    let named = payload.read_lock::<Map>().is_some_and(|map| map.contains_key("op"));
    named && from_dynamic::<OpError>(payload).is_ok()
}

fn outcome(ok: bool, value: Dynamic, error: Dynamic) -> Dynamic {
    let mut map = Map::new();
    map.insert("ok".into(), Dynamic::from(ok));
    map.insert("value".into(), value);
    map.insert("error".into(), error);
    Dynamic::from_map(map)
}

/// `get("characters/aki/face")`: the document value at the path, or `()` when there is none.
#[allow(clippy::needless_pass_by_value)] // as `try_op`
fn get(context: NativeCallContext<'_>, path: ImmutableString) -> Result<Dynamic, Box<EvalAltResult>> {
    let run: Arc<Run> = run_of(&context)?;
    let at = context.call_position();
    run.read(path.as_str(), at).map_err(|e| placed(e, at))
}

/// `error` positioned at the call, for the ones (a limit, a cancel) that are raised without a place.
#[allow(clippy::unnecessary_box_returns)] // Rhai's callbacks return `Box<EvalAltResult>`
fn placed(mut error: Box<EvalAltResult>, at: rhai::Position) -> Box<EvalAltResult> {
    error.set_position(at);
    error
}
