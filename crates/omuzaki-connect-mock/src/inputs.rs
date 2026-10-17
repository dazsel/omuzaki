//! [`MemoryInputs`]: a run's input assets, held in memory.

use std::collections::BTreeMap;

use omuzaki_connect_api::{ConnectError, InputAsset, Inputs};
use omuzaki_store_api::ContentHash;

/// The bytes a test registered, by hash. A hash it does not hold is [`ConnectError::Refused`], like the host's.
#[derive(Clone, Debug, Default)]
pub struct MemoryInputs(BTreeMap<ContentHash, InputAsset>);

impl MemoryInputs {
    /// No inputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds `bytes` and returns their hash, [`ContentHash::of`] them.
    pub fn add(&mut self, bytes: Vec<u8>, media_type: &str) -> ContentHash {
        let hash = ContentHash::of(&bytes);
        self.0.insert(hash, InputAsset { bytes, media_type: media_type.to_owned() });
        hash
    }
}

impl Inputs for MemoryInputs {
    fn get(&self, hash: &ContentHash) -> Result<InputAsset, ConnectError> {
        self.0
            .get(hash)
            .cloned()
            .ok_or_else(|| ConnectError::refused(format!("asset {hash} is not an input of this request")))
    }
}
