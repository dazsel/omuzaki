//! The base world's imports (design §1.1, §2, §3.4): `ops` through the bridge, `host`, and `data` and `http`, which
//! are refusals unless the manifest asks for them and the host can serve them. Every import goes through
//! [`enter`](crate::instance::enter) first. A node instance gets refusals for `ops`, `data` and `http`.

use omuzaki_ops_api::OpError;
use omuzaki_plugin_api::WIT_PACKAGE;
use omuzaki_plugin_api::limits::{EXPORT_LIST_BYTES_MAX, OP_BYTES_MAX};
use omuzaki_plugin_api::state::{FaultKind, LogLevel};
use wasmtime::StoreContextMut;
use wasmtime::component::{ComponentType, Lift, Linker, Lower, WasmList, WasmStr};

use crate::bridge::{WitErrorCode, WitOpError};
use crate::instance::{InstanceKind, StoreData, enter, enter_or_trap, trap};

/// The WIT `level`. Only ever lifted from a guest, so the compiler sees no constructor.
#[allow(dead_code)]
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
enum WitLevel {
    #[component(name = "trace")]
    Trace,
    #[component(name = "debug")]
    Debug,
    #[component(name = "info")]
    Info,
    #[component(name = "warn")]
    Warn,
    #[component(name = "error")]
    Error,
}

impl From<WitLevel> for LogLevel {
    fn from(level: WitLevel) -> Self {
        match level {
            WitLevel::Trace => Self::Trace,
            WitLevel::Debug => Self::Debug,
            WitLevel::Info => Self::Info,
            WitLevel::Warn => Self::Warn,
            WitLevel::Error => Self::Error,
        }
    }
}

/// The WIT `http.request`, read in place in the guest's memory: nothing of it is copied until its size is known.
#[derive(ComponentType, Lift)]
#[component(record)]
struct WitRequest {
    method: WasmStr,
    url: WasmStr,
    headers: WasmList<(WasmStr, WasmStr)>,
    body: WasmList<u8>,
}

/// The WIT `http.response`.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
struct WitResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// The name of interface `name` in this host's package version.
fn interface(name: &str) -> String {
    let (package, version) = WIT_PACKAGE.split_once('@').unwrap_or((WIT_PACKAGE, ""));
    format!("{package}/{name}@{version}")
}

/// Why `ops`, `data` and `http` refuse right now, or `None` when they may run.
fn refusal(data: &StoreData) -> Option<&'static str> {
    match (data.kind, data.effectful) {
        (InstanceKind::Node, _) => Some("not in a node activation"),
        (InstanceKind::Worker, false) => Some("revoked"),
        (InstanceKind::Worker, true) => None,
    }
}

/// Logs one `warn` line for a refused call (design §2.2: retrying never helps, so one line is the trace).
fn warn_refused(cx: &StoreContextMut<'_, StoreData>, call: &str, why: &str) {
    cx.data().log.push(LogLevel::Warn, &format!("{call} refused: {why}"), false);
}

fn denied(why: &str) -> WitOpError {
    WitOpError { code: WitErrorCode::Denied, message: why.to_owned(), detail: None, hint: None }
}

fn limited() -> WitOpError {
    WitOpError {
        code: WitErrorCode::Limited,
        message: "too many host calls a second".into(),
        detail: None,
        hint: Some("wait a second and try again".into()),
    }
}

/// Faults the plugin when a guest hands the host more than `cap` bytes, before the host does anything with them.
fn cap(len: usize, cap: usize, what: &str, entry: &str) -> wasmtime::Result<()> {
    if len > cap {
        return Err(trap(
            FaultKind::BadReturn,
            format!("bad return value in {entry}: {what} is {len} bytes, over {cap}"),
        ));
    }
    Ok(())
}

/// The byte length of a guest string, read in place in the guest's memory: no copy is made, so a string near the
/// guest's whole memory costs the host nothing before its cap is checked. Every string is UTF-8, read in place:
/// [`load::compile`](crate::load::compile) refuses a component that picks another encoding.
pub(crate) fn guest_len(cx: &StoreContextMut<'_, StoreData>, text: &WasmStr) -> wasmtime::Result<usize> {
    Ok(text.to_str(cx)?.len())
}

/// Copies a guest string of at most `max` bytes into the host; a longer one faults the plugin before any copy.
fn guest_string(
    cx: &StoreContextMut<'_, StoreData>,
    text: &WasmStr,
    max: usize,
    what: &str,
) -> wasmtime::Result<String> {
    let text = text.to_str(cx)?;
    cap(text.len(), max, what, cx.data().entry())?;
    Ok(text.into_owned())
}

/// The shared body of `ops.call` and `ops.plan`.
fn op(
    mut cx: StoreContextMut<'_, StoreData>,
    name: &str,
    op: &WasmStr,
    args: &WasmStr,
    run: fn(&dyn crate::bridge::OpBridge, &str, &str) -> Result<String, OpError>,
) -> wasmtime::Result<(Result<String, WitOpError>,)> {
    let op = guest_string(&cx, op, OP_BYTES_MAX, &format!("{name}'s op name"))?;
    let args = guest_string(&cx, args, OP_BYTES_MAX, &format!("{name} arguments"))?;
    let (op, args) = (op.as_str(), args.as_str());
    let entry = cx.data().entry();
    if !enter(&mut cx, op.len() + args.len())? {
        return Ok((Err(limited()),));
    }
    if let Some(why) = refusal(cx.data()) {
        warn_refused(&cx, &format!("{name} {op}"), why);
        return Ok((Err(denied(why)),));
    }
    let bridge = cx.data().bridge.clone();
    match run(bridge.as_ref(), op, args) {
        Ok(output) => {
            cap(output.len(), OP_BYTES_MAX, &format!("the {name} result"), entry)?;
            enter_or_trap(&mut cx, output.len())?;
            Ok((Ok(output),))
        }
        Err(error) => {
            warn_refused(&cx, &format!("{name} {op}"), &format!("{} ({})", error.message, error.code));
            Ok((Err(WitOpError::from(&error)),))
        }
    }
}

/// Links `ops`, `host`, `data`, `http` and the type-only `types`.
pub(crate) fn add(linker: &mut Linker<StoreData>, _kind: InstanceKind) -> wasmtime::Result<()> {
    linker.instance(&interface("types"))?;

    let mut ops = linker.instance(&interface("ops"))?;
    ops.func_wrap("call", |cx, (name, args): (WasmStr, WasmStr)| {
        op(cx, "ops.call", &name, &args, |b, o, a| b.call(o, a))
    })?;
    ops.func_wrap("plan", |cx, (name, args): (WasmStr, WasmStr)| {
        op(cx, "ops.plan", &name, &args, |b, o, a| b.plan(o, a))
    })?;
    ops.func_wrap("describe", |mut cx: StoreContextMut<'_, StoreData>, (name,): (WasmStr,)| {
        let name = guest_string(&cx, &name, OP_BYTES_MAX, "ops.describe's op name")?;
        enter_or_trap(&mut cx, name.len())?;
        if refusal(cx.data()).is_some() {
            return Ok((None,));
        }
        let schema = cx.data().bridge.describe(&name);
        if let Some(schema) = &schema {
            cap(schema.len(), EXPORT_LIST_BYTES_MAX, "a describe schema", cx.data().entry())?;
            // The bytes handed back cost fuel like the bytes handed in, as `ops.call`'s output does.
            enter_or_trap(&mut cx, schema.len())?;
        }
        Ok((schema,))
    })?;
    ops.func_wrap("list", |mut cx: StoreContextMut<'_, StoreData>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        if refusal(cx.data()).is_some() {
            return Ok((Vec::<String>::new(),));
        }
        let names = cx.data().bridge.list();
        let bytes: usize = names.iter().map(String::len).sum();
        cap(bytes, EXPORT_LIST_BYTES_MAX, "the op list", cx.data().entry())?;
        enter_or_trap(&mut cx, bytes)?;
        Ok((names,))
    })?;

    let mut host = linker.instance(&interface("host"))?;
    host.func_wrap("log", |mut cx: StoreContextMut<'_, StoreData>, (level, message): (WitLevel, WasmStr)| {
        let bytes = guest_len(&cx, &message)?;
        enter_or_trap(&mut cx, bytes)?;
        // Read in place: the log keeps at most a line's cap of it.
        let text = message.to_str(&cx)?;
        cx.data().log.push(level.into(), &text, false);
        Ok(())
    })?;
    host.func_wrap("monotonic-seconds", |mut cx: StoreContextMut<'_, StoreData>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((cx.data().clock.now().0,))
    })?;

    let mut data = linker.instance(&interface("data"))?;
    data.func_wrap("read", |mut cx: StoreContextMut<'_, StoreData>, (name,): (WasmStr,)| {
        let bytes = guest_len(&cx, &name)?;
        enter_or_trap(&mut cx, bytes)?;
        warn_refused(&cx, "data.read", data_refusal(cx.data()));
        Ok((None::<Vec<u8>>,))
    })?;
    data.func_wrap("write", |mut cx: StoreContextMut<'_, StoreData>, (name, contents): (WasmStr, WasmList<u8>)| {
        let bytes = guest_len(&cx, &name)? + contents.len();
        enter_or_trap(&mut cx, bytes)?;
        let why = data_refusal(cx.data());
        warn_refused(&cx, "data.write", why);
        Ok((Err::<(), String>(why.to_owned()),))
    })?;
    data.func_wrap("remove", |mut cx: StoreContextMut<'_, StoreData>, (name,): (WasmStr,)| {
        let bytes = guest_len(&cx, &name)?;
        enter_or_trap(&mut cx, bytes)?;
        warn_refused(&cx, "data.remove", data_refusal(cx.data()));
        Ok((false,))
    })?;
    data.func_wrap("list", |mut cx: StoreContextMut<'_, StoreData>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        warn_refused(&cx, "data.list", data_refusal(cx.data()));
        Ok((Vec::<String>::new(),))
    })?;

    let mut http = linker.instance(&interface("http"))?;
    http.func_wrap("fetch", |mut cx: StoreContextMut<'_, StoreData>, (request,): (WitRequest,)| {
        // Each header counts its two lengths' worth of bytes once the bridge serves `fetch`; refused here, only
        // their number is charged, so none of them is read.
        let bytes = guest_len(&cx, &request.method)?
            + guest_len(&cx, &request.url)?
            + request.body.len()
            + request.headers.len();
        enter_or_trap(&mut cx, bytes)?;
        let why = refusal(cx.data()).unwrap_or(if cx.data().net_requested {
            "host not approved"
        } else {
            "the plugin did not ask for net"
        });
        let url = request.url.to_str(&cx)?;
        warn_refused(&cx, &format!("http.fetch {}", crate::log::cut_line(&url)), why);
        Ok((Err::<WitResponse, String>(why.to_owned()),))
    })?;
    Ok(())
}

/// Why a `data` call refuses: no data folder is served yet (its task follows), so every call refuses with the reason
/// that applies first.
fn data_refusal(data: &StoreData) -> &'static str {
    refusal(data).unwrap_or(if data.data_requested { "unavailable" } else { "the plugin did not ask for data" })
}
