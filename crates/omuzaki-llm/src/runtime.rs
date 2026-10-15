//! The host's tokio runtime, handed to a provider as an opaque [`RuntimeHandle`] (llm-api D1).
//!
//! No crate here starts a hidden thread: the app passes its one runtime with [`from_handle`], and the CLI, tests and
//! the eval driver start a one-worker runtime of their own with [`dedicated`].

use std::sync::Arc;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::RuntimeHandle;
use tokio::runtime::{Builder, Handle, Runtime};

/// A runtime this crate started; the handle keeps it alive until the last client built on it is dropped.
struct Dedicated(Arc<Runtime>);

/// Wraps the host's runtime `handle` for [`ProviderEnv`](omuzaki_llm_api::provider::ProviderEnv).
pub fn from_handle(handle: Handle) -> RuntimeHandle {
    RuntimeHandle::new(handle)
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
    Ok(RuntimeHandle::new(Dedicated(Arc::new(runtime))))
}

/// The tokio handle inside `runtime`.
///
/// # Errors
/// `Unsupported` if `runtime` is neither from [`from_handle`] nor from [`dedicated`] (the fake's
/// `RuntimeHandle::none()`, for one).
pub(crate) fn handle_of(runtime: &RuntimeHandle) -> Result<Handle, LlmError> {
    if let Some(handle) = runtime.downcast_ref::<Handle>() {
        return Ok(handle.clone());
    }
    if let Some(Dedicated(runtime)) = runtime.downcast_ref::<Dedicated>() {
        return Ok(runtime.handle().clone());
    }
    Err(LlmError::Unsupported("the provider needs a tokio runtime handle (runtime::dedicated or from_handle)".into()))
}

/// Runs `future` on `handle` and waits for its result on the calling thread, which may or may not be inside a
/// runtime. For the blocking calls (model listing, probing) that are never on a hot path.
pub(crate) fn block_on<T: Send + 'static>(
    handle: &Handle,
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T, LlmError> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    handle.spawn(async move {
        // The caller may have gone; nothing to tell it then.
        let _ = tx.send(future.await);
    });
    rx.recv().map_err(|_| LlmError::Network("the LLM runtime stopped before the call finished".into()))
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
    fn a_handle_that_is_not_a_runtime_is_unsupported() {
        assert!(matches!(handle_of(&RuntimeHandle::none()), Err(LlmError::Unsupported(_))));
    }
}
