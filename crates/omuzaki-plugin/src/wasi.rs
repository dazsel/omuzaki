//! The WASI stub subset (design §4.1): exactly what Rust's standard library on `wasm32-wasip2` imports, granting
//! nothing. No environment, no arguments, no files (no preopens), stdin empty, stdout and stderr into the plugin's log
//! at `debug`, `exit` a trap, clocks from the runner's clock, `random` from OS entropy (a seeded stream in node
//! instances and under the test option), `insecure` from the seeded stream, and a `poll` that waits at most until the
//! entry's deadline in a worker and never waits in a node instance. `wasi:sockets` and `wasi:http` are never linked.

use std::time::{Duration, Instant};

use omuzaki_plugin_api::limits::RANDOM_REQUEST_MAX;
use omuzaki_plugin_api::state::{FaultKind, LogLevel};
use wasmtime::StoreContextMut;
use wasmtime::component::{ComponentType, Lift, Linker, Lower, Resource, ResourceType, Val, WasmList};

use crate::instance::{InstanceKind, StoreData, enter_or_trap, fill, trap};
use crate::limits::MAX_POLLABLES;

/// The WASI release the stubs are defined at; components built for any 0.2 release link to them.
const WASI_VERSION: &str = "0.2.12";

/// How long a waiting `poll` sleeps between looks at the runner's clock.
const POLL_STEP: Duration = Duration::from_millis(1);

/// How much a guest may write to stdout or stderr at once.
const WRITE_BUDGET: u64 = 4096;

/// The `wasi:io/streams` input stream (stdin only).
struct InputStream;
/// The `wasi:io/streams` output stream (stdout and stderr).
struct OutputStream;
/// A `wasi:io/poll` pollable.
struct Pollable;
/// A `wasi:io/error` error; never made, since no stream operation fails.
struct IoError;
/// A `wasi:filesystem` descriptor; never made, since there are no preopens.
struct Descriptor;
/// A `wasi:filesystem` directory-entry stream; never made.
struct DirectoryEntryStream;

/// The handle of stdin.
const STDIN: u32 = 0;
/// The handle of stdout.
const STDOUT: u32 = 1;
/// The handle of stderr.
const STDERR: u32 = 2;

/// When a pollable is ready.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Wait {
    /// Now.
    Ready,
    /// Once the runner's clock reaches this many seconds.
    At(f64),
}

/// The WASI `stream-error`.
#[derive(ComponentType, Lower, Lift)]
#[component(variant)]
enum StreamError {
    #[component(name = "last-operation-failed")]
    LastOperationFailed(Resource<IoError>),
    #[component(name = "closed")]
    Closed,
}

/// The WASI `datetime`.
#[derive(ComponentType, Lower, Lift, Copy, Clone)]
#[component(record)]
struct Datetime {
    seconds: u64,
    nanoseconds: u32,
}

fn name(interface: &str) -> String {
    format!("{interface}@{WASI_VERSION}")
}

fn nanos(seconds: f64) -> u64 {
    // The runner's clock is non-negative seconds; far inside u64 nanoseconds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ns = (seconds.max(0.0) * 1e9) as u64;
    ns
}

#[allow(clippy::cast_precision_loss)]
fn seconds(nanos: u64) -> f64 {
    nanos as f64 / 1e9
}

/// A new pollable in the first free slot (a dropped one's, or a new one up to [`MAX_POLLABLES`]); more at once traps.
fn new_pollable(data: &mut StoreData, wait: Wait) -> wasmtime::Result<Resource<Pollable>> {
    let slot = match data.pollables.iter().position(Option::is_none) {
        Some(free) => free,
        None if data.pollables.len() < MAX_POLLABLES => {
            data.pollables.push(None);
            data.pollables.len() - 1
        }
        None => return Err(trap(FaultKind::Trap, format!("more than {MAX_POLLABLES} pollables at once"))),
    };
    data.pollables[slot] = Some(wait);
    let rep = u32::try_from(slot).map_err(|_| trap(FaultKind::Trap, "a pollable slot past u32"))?;
    Ok(Resource::new_own(rep))
}

fn is_ready(data: &StoreData, rep: u32) -> wasmtime::Result<bool> {
    match data.pollables.get(rep as usize).copied().flatten() {
        Some(Wait::Ready) => Ok(true),
        Some(Wait::At(at)) => Ok(data.clock.now().0 >= at),
        None => Err(trap(FaultKind::Trap, "a pollable that does not exist")),
    }
}

/// Waits until one of `reps` is ready and returns the ready ones' positions. A worker waits at most until the entry's
/// deadline, then traps; a node instance never waits (§4.1).
fn wait(cx: &mut StoreContextMut<'_, StoreData>, reps: &[u32]) -> wasmtime::Result<Vec<u32>> {
    loop {
        let mut ready = Vec::new();
        for (i, rep) in reps.iter().enumerate() {
            if is_ready(cx.data(), *rep)? {
                ready.push(u32::try_from(i).map_err(|_| trap(FaultKind::Trap, "too many pollables"))?);
            }
        }
        if !ready.is_empty() || reps.is_empty() {
            return Ok(ready);
        }
        let data = cx.data();
        if data.kind == InstanceKind::Node {
            return Err(trap(FaultKind::Trap, "would block in a node activation"));
        }
        let Some(call) = data.call else { return Err(trap(FaultKind::Trap, "a wait outside an entry call")) };
        if Instant::now() >= call.deadline {
            return Err(trap(FaultKind::Timeout, format!("deadline in {}", call.kind.entry())));
        }
        std::thread::sleep(POLL_STEP.min(call.deadline.saturating_duration_since(Instant::now())));
    }
}

/// The bytes of a random request, refused past [`RANDOM_REQUEST_MAX`] before anything is allocated.
fn random_len(cx: &StoreContextMut<'_, StoreData>, len: u64) -> wasmtime::Result<usize> {
    if len > RANDOM_REQUEST_MAX {
        return Err(trap(FaultKind::Trap, format!("random request too large in {}", cx.data().entry())));
    }
    usize::try_from(len).map_err(|_| trap(FaultKind::Trap, "random request too large"))
}

fn secure_bytes(cx: &mut StoreContextMut<'_, StoreData>, len: usize) -> wasmtime::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    match cx.data_mut().secure_seeded.as_mut() {
        Some(stream) => fill(stream, &mut buf),
        None => getrandom::fill(&mut buf).map_err(|e| trap(FaultKind::Trap, format!("no OS entropy: {e}")))?,
    }
    Ok(buf)
}

fn insecure_bytes(cx: &mut StoreContextMut<'_, StoreData>, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    fill(&mut cx.data_mut().insecure, &mut buf);
    buf
}

fn u64_of(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(word)
}

/// Writes guest bytes for stdout or stderr into the log, a line at a time.
fn write_out(cx: &StoreContextMut<'_, StoreData>, contents: &[u8]) {
    let text = String::from_utf8_lossy(contents);
    for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
        cx.data().log.push(LogLevel::Debug, line, false);
    }
}

type Data<'a> = StoreContextMut<'a, StoreData>;

/// Links the stub subset for `kind`.
pub(crate) fn add(linker: &mut Linker<StoreData>, kind: InstanceKind) -> wasmtime::Result<()> {
    add_io(linker)?;
    add_cli(linker)?;
    add_clocks(linker, kind)?;
    add_random(linker)?;
    add_filesystem(linker)
}

fn add_io(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    let mut error = linker.instance(&name("wasi:io/error"))?;
    error.resource("error", ResourceType::host::<IoError>(), |_, _| Ok(()))?;
    error.func_wrap("[method]error.to-debug-string", |mut cx: Data<'_>, (_,): (Resource<IoError>,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((String::from("no error"),))
    })?;

    let mut poll = linker.instance(&name("wasi:io/poll"))?;
    poll.resource("pollable", ResourceType::host::<Pollable>(), |mut cx, rep| {
        if let Some(slot) = cx.data_mut().pollables.get_mut(rep as usize) {
            *slot = None;
        }
        Ok(())
    })?;
    poll.func_wrap("[method]pollable.ready", |mut cx: Data<'_>, (p,): (Resource<Pollable>,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((is_ready(cx.data(), p.rep())?,))
    })?;
    poll.func_wrap("[method]pollable.block", |mut cx: Data<'_>, (p,): (Resource<Pollable>,)| {
        enter_or_trap(&mut cx, 0)?;
        wait(&mut cx, &[p.rep()]).map(|_| ())
    })?;
    poll.func_wrap("poll", |mut cx: Data<'_>, (list,): (Vec<Resource<Pollable>>,)| {
        enter_or_trap(&mut cx, 4 * list.len())?;
        let reps: Vec<u32> = list.iter().map(Resource::rep).collect();
        Ok((wait(&mut cx, &reps)?,))
    })?;

    let mut streams = linker.instance(&name("wasi:io/streams"))?;
    streams.resource("input-stream", ResourceType::host::<InputStream>(), |_, _| Ok(()))?;
    streams.resource("output-stream", ResourceType::host::<OutputStream>(), |_, _| Ok(()))?;
    for method in ["read", "blocking-read"] {
        streams.func_wrap(
            &format!("[method]input-stream.{method}"),
            |mut cx: Data<'_>, (_, _): (Resource<InputStream>, u64)| {
                enter_or_trap(&mut cx, 0)?;
                Ok((Err::<Vec<u8>, StreamError>(StreamError::Closed),))
            },
        )?;
    }
    for method in ["skip", "blocking-skip"] {
        streams.func_wrap(
            &format!("[method]input-stream.{method}"),
            |mut cx: Data<'_>, (_, _): (Resource<InputStream>, u64)| {
                enter_or_trap(&mut cx, 0)?;
                Ok((Err::<u64, StreamError>(StreamError::Closed),))
            },
        )?;
    }
    streams.func_wrap("[method]input-stream.subscribe", |mut cx: Data<'_>, (_,): (Resource<InputStream>,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((new_pollable(cx.data_mut(), Wait::Ready)?,))
    })?;
    streams.func_wrap("[method]output-stream.check-write", |mut cx: Data<'_>, (_,): (Resource<OutputStream>,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Ok::<u64, StreamError>(WRITE_BUDGET),))
    })?;
    for method in ["write", "blocking-write-and-flush"] {
        streams.func_wrap(
            &format!("[method]output-stream.{method}"),
            |mut cx: Data<'_>, (_, contents): (Resource<OutputStream>, WasmList<u8>)| {
                enter_or_trap(&mut cx, contents.len())?;
                // Read in place, and only as much as `check-write` permits; the rest is not kept.
                let bytes = contents.as_le_slice(&cx);
                let budget = usize::try_from(WRITE_BUDGET).unwrap_or(usize::MAX);
                write_out(&cx, &bytes[..bytes.len().min(budget)]);
                Ok((Ok::<(), StreamError>(()),))
            },
        )?;
    }
    for method in ["flush", "blocking-flush"] {
        streams.func_wrap(
            &format!("[method]output-stream.{method}"),
            |mut cx: Data<'_>, (_,): (Resource<OutputStream>,)| {
                enter_or_trap(&mut cx, 0)?;
                Ok((Ok::<(), StreamError>(()),))
            },
        )?;
    }
    for method in ["write-zeroes", "blocking-write-zeroes-and-flush"] {
        streams.func_wrap(
            &format!("[method]output-stream.{method}"),
            |mut cx: Data<'_>, (_, len): (Resource<OutputStream>, u64)| {
                enter_or_trap(&mut cx, usize::try_from(len).unwrap_or(usize::MAX))?;
                Ok((Ok::<(), StreamError>(()),))
            },
        )?;
    }
    for method in ["splice", "blocking-splice"] {
        streams.func_wrap(
            &format!("[method]output-stream.{method}"),
            |mut cx: Data<'_>, (_, _, _): (Resource<OutputStream>, Resource<InputStream>, u64)| {
                enter_or_trap(&mut cx, 0)?;
                Ok((Err::<u64, StreamError>(StreamError::Closed),))
            },
        )?;
    }
    streams.func_wrap("[method]output-stream.subscribe", |mut cx: Data<'_>, (_,): (Resource<OutputStream>,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((new_pollable(cx.data_mut(), Wait::Ready)?,))
    })?;
    Ok(())
}

fn add_cli(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    let mut environment = linker.instance(&name("wasi:cli/environment"))?;
    environment.func_wrap("get-environment", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Vec::<(String, String)>::new(),))
    })?;
    environment.func_wrap("get-arguments", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Vec::<String>::new(),))
    })?;
    environment.func_wrap("initial-cwd", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((None::<String>,))
    })?;

    let mut exit = linker.instance(&name("wasi:cli/exit"))?;
    exit.func_wrap("exit", |cx: Data<'_>, (_,): (Result<(), ()>,)| -> wasmtime::Result<()> {
        Err(trap(FaultKind::Exited, format!("exited in {}", cx.data().entry())))
    })?;

    linker.instance(&name("wasi:cli/stdin"))?.func_wrap("get-stdin", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Resource::<InputStream>::new_own(STDIN),))
    })?;
    linker.instance(&name("wasi:cli/stdout"))?.func_wrap("get-stdout", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Resource::<OutputStream>::new_own(STDOUT),))
    })?;
    linker.instance(&name("wasi:cli/stderr"))?.func_wrap("get-stderr", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Resource::<OutputStream>::new_own(STDERR),))
    })?;
    Ok(())
}

fn add_clocks(linker: &mut Linker<StoreData>, _kind: InstanceKind) -> wasmtime::Result<()> {
    let mut monotonic = linker.instance(&name("wasi:clocks/monotonic-clock"))?;
    monotonic.func_wrap("now", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((nanos(cx.data().clock.now().0),))
    })?;
    monotonic.func_wrap("resolution", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((1u64,))
    })?;
    monotonic.func_wrap("subscribe-instant", |mut cx: Data<'_>, (when,): (u64,)| {
        enter_or_trap(&mut cx, 0)?;
        Ok((new_pollable(cx.data_mut(), Wait::At(seconds(when)))?,))
    })?;
    monotonic.func_wrap("subscribe-duration", |mut cx: Data<'_>, (after,): (u64,)| {
        enter_or_trap(&mut cx, 0)?;
        let at = cx.data().clock.now().0 + seconds(after);
        Ok((new_pollable(cx.data_mut(), Wait::At(at))?,))
    })?;

    // The runner's clock, read as seconds since 1970-01-01, so a plugin's dates are deterministic under test.
    let mut wall = linker.instance(&name("wasi:clocks/wall-clock"))?;
    wall.func_wrap("now", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        let ns = nanos(cx.data().clock.now().0);
        let nanoseconds = u32::try_from(ns % 1_000_000_000).unwrap_or(0);
        Ok((Datetime { seconds: ns / 1_000_000_000, nanoseconds },))
    })?;
    wall.func_wrap("resolution", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Datetime { seconds: 0, nanoseconds: 1 },))
    })?;
    Ok(())
}

fn add_random(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    let mut random = linker.instance(&name("wasi:random/random"))?;
    random.func_wrap("get-random-bytes", |mut cx: Data<'_>, (len,): (u64,)| {
        let len = random_len(&cx, len)?;
        enter_or_trap(&mut cx, len)?;
        Ok((secure_bytes(&mut cx, len)?,))
    })?;
    random.func_wrap("get-random-u64", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 8)?;
        Ok((u64_of(&secure_bytes(&mut cx, 8)?),))
    })?;

    let mut insecure = linker.instance(&name("wasi:random/insecure"))?;
    insecure.func_wrap("get-insecure-random-bytes", |mut cx: Data<'_>, (len,): (u64,)| {
        let len = random_len(&cx, len)?;
        enter_or_trap(&mut cx, len)?;
        Ok((insecure_bytes(&mut cx, len),))
    })?;
    insecure.func_wrap("get-insecure-random-u64", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 8)?;
        Ok((u64_of(&insecure_bytes(&mut cx, 8)),))
    })?;

    linker.instance(&name("wasi:random/insecure-seed"))?.func_wrap("insecure-seed", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 16)?;
        let bytes = insecure_bytes(&mut cx, 16);
        Ok(((u64_of(&bytes[..8]), u64_of(&bytes[8..])),))
    })?;
    Ok(())
}

/// The `descriptor` methods of `wasi:filesystem/types`. No descriptor or directory stream ever exists (there are no
/// preopens), so each method traps if a guest ever reaches it.
const DESCRIPTOR_METHODS: [&str; 27] = [
    "read-via-stream",
    "write-via-stream",
    "append-via-stream",
    "advise",
    "sync-data",
    "get-flags",
    "get-type",
    "set-size",
    "set-times",
    "read",
    "write",
    "read-directory",
    "sync",
    "create-directory-at",
    "stat",
    "stat-at",
    "set-times-at",
    "link-at",
    "open-at",
    "readlink-at",
    "remove-directory-at",
    "rename-at",
    "symlink-at",
    "unlink-file-at",
    "is-same-object",
    "metadata-hash",
    "metadata-hash-at",
];

fn add_filesystem(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    let mut types = linker.instance(&name("wasi:filesystem/types"))?;
    types.resource("descriptor", ResourceType::host::<Descriptor>(), |_, _| Ok(()))?;
    types.resource("directory-entry-stream", ResourceType::host::<DirectoryEntryStream>(), |_, _| Ok(()))?;
    let methods = DESCRIPTOR_METHODS.iter().map(|m| format!("[method]descriptor.{m}"));
    for method in methods.chain(["[method]directory-entry-stream.read-directory-entry".to_owned()]) {
        types.func_new(&method, |_, _, _: &[Val], _: &mut [Val]| {
            Err(trap(FaultKind::Trap, "no file is reachable from a plugin"))
        })?;
    }
    types.func_new("filesystem-error-code", |_, _, _: &[Val], results: &mut [Val]| {
        if let Some(slot) = results.first_mut() {
            *slot = Val::Option(None);
        }
        Ok(())
    })?;
    linker.instance(&name("wasi:filesystem/preopens"))?.func_wrap("get-directories", |mut cx: Data<'_>, (): ()| {
        enter_or_trap(&mut cx, 0)?;
        Ok((Vec::<(Resource<Descriptor>, String)>::new(),))
    })?;
    Ok(())
}
