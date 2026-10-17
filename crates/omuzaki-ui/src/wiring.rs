//! The app's wiring module (QG-11, `cargo xtask lint-ui-writes`): the one place in the UI that builds the op executor
//! and its document, and hands out the [`Runner`]. Everything else in the UI gets the `Runner` passed in and changes
//! the document only through it.

use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Scratch, Value};
use omuzaki_ops::Executor;
pub use omuzaki_ops::ExecutorError;
use omuzaki_ops_api::{Runner, Services};

/// The runner of an app with no project open: every op linked into the app, over an empty in-memory document, with
/// `services`.
///
/// # Errors
/// [`ExecutorError`] when two linked crates register one op name (a bug in whichever registered it).
pub fn runner(services: Services) -> Result<Arc<dyn Runner>, ExecutorError> {
    let executor = Executor::new(Box::new(Scratch::of(&Empty)), Arc::new(SystemClock::new()))?;
    Ok(Arc::new(executor.with_services(services)))
}

/// The tree an app with no project open starts from: empty, at version 0. The executor works on an in-memory copy
/// of it ([`Scratch`]), so what ops change lasts until the app quits; opening a project replaces it.
#[derive(Clone, Copy, Debug, Default)]
struct Empty;

impl DocRead for Empty {
    fn get(&self, _path: &DocPath) -> Option<&Value> {
        None
    }

    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}
