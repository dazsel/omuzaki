//! The host's tokio runtime, handed to a provider as an opaque [`RuntimeHandle`] (llm-api D1).
//!
//! No crate here starts a hidden thread: the app passes its one runtime with [`from_handle`], and the CLI, tests and
//! the eval driver start a one-worker runtime of their own with [`dedicated`].

use std::sync::Arc;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::RuntimeHandle;
use tokio::runtime::{Builder, Handle, Runtime};

use crate::openai::probe::ProbeCache;

/// What a [`RuntimeHandle`] holds: the tokio handle, the runtime itself when this crate started it (so it lives until
/// the last client built on it is dropped), and the capability answers probed through it.
///
/// The probe answers live here and not in a process-wide table because an answer is true of one server at one moment:
/// a host that is rebuilt, or a test that reuses a loopback port, must not inherit another server's answer.
struct Host {
    handle: Handle,
    _owned: Option<Arc<Runtime>>,
    probes: ProbeCache,
}

/// Wraps the host's runtime `handle` for [`ProviderEnv`](omuzaki_llm_api::provider::ProviderEnv).
pub fn from_handle(handle: Handle) -> RuntimeHandle {
    RuntimeHandle::new(Host { handle, _owned: None, probes: ProbeCache::default() })
}

/// Starts a one-worker runtime and returns the handle that owns it.
///
/// # Errors
/// `Unsupported` if the operating system refuses to start the worker thread.
pub fn dedicated() -> Result<RuntimeHandle, LlmError> {
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("omuzaki-llm")
        .enable_all()
        .build()
        .map_err(|e| LlmError::Unsupported(format!("cannot start the LLM runtime: {e}")))?;
    let runtime = Arc::new(runtime);
    Ok(RuntimeHandle::new(Host {
        handle: runtime.handle().clone(),
        _owned: Some(runtime),
        probes: ProbeCache::default(),
    }))
}

/// The tokio handle inside `runtime`.
///
/// # Errors
/// `Unsupported` if `runtime` is neither from [`from_handle`] nor from [`dedicated`] (the fake's
/// `RuntimeHandle::none()`, for one).
pub(crate) fn handle_of(runtime: &RuntimeHandle) -> Result<Handle, LlmError> {
    host_of(runtime).map(|host| host.handle.clone())
}

/// The probe answers of `runtime`.
///
/// # Errors
/// As [`handle_of`].
pub(crate) fn probes_of(runtime: &RuntimeHandle) -> Result<&ProbeCache, LlmError> {
    host_of(runtime).map(|host| &host.probes)
}

fn host_of(runtime: &RuntimeHandle) -> Result<&Host, LlmError> {
    runtime.downcast_ref::<Host>().ok_or_else(|| {
        LlmError::Unsupported("the provider needs a tokio runtime handle (runtime::dedicated or from_handle)".into())
    })
}

/// Runs `future` on `handle` and waits for its result on the calling thread, which may or may not be inside a
/// runtime. For the blocking calls (model listing, probing) that are never on a hot path.
///
/// Inside a multi-thread runtime the wait is `block_in_place`, which hands the worker's queue to another thread, so
/// even a one-worker runtime makes progress. A current-thread runtime makes progress only while some thread drives it,
/// and this call cannot know that one does (and if the caller is that thread, it never will), so a current-thread
/// target is refused before anything is spawned.
pub(crate) fn block_on<T: Send + 'static>(
    handle: &Handle,
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T, LlmError> {
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
        return Err(LlmError::Unsupported(
            "this call blocks until the LLM runtime has run it, and a current-thread runtime may have no thread driving it; use a multi-thread runtime or runtime::dedicated".into(),
        ));
    }
    let wait_in_place = matches!(Handle::try_current(), Ok(current) if current.runtime_flavor() != tokio::runtime::RuntimeFlavor::CurrentThread);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    handle.spawn(async move {
        // The caller may have gone; nothing to tell it then.
        let _ = tx.send(future.await);
    });
    let received = if wait_in_place { tokio::task::block_in_place(|| rx.recv()) } else { rx.recv() };
    received.map_err(|_| LlmError::Network("the LLM runtime stopped before the call finished".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dedicated_runtime_and_a_host_handle_both_resolve() {
        let owned = dedicated().unwrap_or_else(|e| panic!("{e}"));
        let handle = handle_of(&owned).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(block_on(&handle, async { 7 }), Ok(7));
        let wrapped = from_handle(handle);
        assert!(handle_of(&wrapped).is_ok());
    }

    #[test]
    fn a_blocking_call_from_a_one_worker_runtime_still_completes() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let handle = rt.handle().clone();
        let got = rt.block_on(async move { tokio::spawn(async move { block_on(&handle, async { 5 }) }).await });
        assert_eq!(got.unwrap_or_else(|e| panic!("{e}")), Ok(5));
    }

    #[test]
    fn a_blocking_call_on_an_undriven_current_thread_runtime_is_refused_not_hung() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        // Called from a plain thread: nothing drives `rt`, so waiting for it would never end.
        let got = block_on(rt.handle(), async { 5 });
        assert!(matches!(got, Err(LlmError::Unsupported(_))), "{got:?}");
    }

    #[test]
    fn a_blocking_call_on_the_calling_current_thread_runtime_is_refused_not_hung() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let handle = rt.handle().clone();
        let got = rt.block_on(async move { block_on(&handle, async { 5 }) });
        assert!(matches!(got, Err(LlmError::Unsupported(_))), "{got:?}");
    }

    #[test]
    fn a_handle_that_is_not_a_runtime_is_unsupported() {
        assert!(matches!(handle_of(&RuntimeHandle::none()), Err(LlmError::Unsupported(_))));
    }
}
