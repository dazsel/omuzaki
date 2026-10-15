//! [`EntrySink`]: where the host records each fact of a conversation (D7).

use omuzaki_agent_api::entry::{Entry, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_llm_api::message::ImageMime;

/// Receives every [`Entry`] the host produces, in order. The conversation store implements it; tests use a recording
/// sink. Appends happen on the host thread, so an implementation may block on the disk but must not call back into the
/// host.
pub trait EntrySink: Send + Sync {
    /// Records `entry`, flushed before it returns.
    ///
    /// # Errors
    /// [`HostError::Store`] if the entry could not be written.
    fn append(&self, entry: &Entry) -> Result<(), HostError>;

    /// Keeps an image of a tool result outside the log and returns the block that names it. The default keeps nothing
    /// and returns a note, for sinks that only record text.
    ///
    /// # Errors
    /// [`HostError::Store`] if the image could not be written.
    fn put_blob(&self, bytes: &[u8], mime: ImageMime) -> Result<StoredBlock, HostError> {
        let _ = (bytes, mime);
        Ok(StoredBlock::Text("[image not stored]".into()))
    }

    /// The bytes of the blob `hash` of encoding `mime`, to rebuild a continued conversation's history. The default has
    /// none.
    fn read_blob(&self, hash: &str, mime: ImageMime) -> Option<Vec<u8>> {
        let _ = (hash, mime);
        None
    }
}
