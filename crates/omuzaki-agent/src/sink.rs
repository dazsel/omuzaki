//! [`EntrySink`]: where the host records each fact of a conversation (D7).

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;

/// Receives every [`Entry`] the host produces, in order. The conversation store implements it; tests use a recording
/// sink. Appends happen on the host thread, so an implementation may block on the disk but must not call back into the
/// host.
pub trait EntrySink: Send + Sync {
    /// Records `entry`, flushed before it returns.
    ///
    /// # Errors
    /// [`HostError::Store`] if the entry could not be written.
    fn append(&self, entry: &Entry) -> Result<(), HostError>;
}
