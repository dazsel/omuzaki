//! An in-memory asset source, so a test can give a part the bytes it reads (a traced map, an image) without a store.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_char_api::{AssetError, AssetSource, ContentHash};

/// Assets by content hash, held in memory. Put the bytes in, wrap it in
/// [`BuildServices::with_assets`](omuzaki_char_api::BuildServices::with_assets), and a part's
/// [`BuildCtx::asset`](omuzaki_char_api::BuildCtx::asset) reads them back.
#[derive(Clone, Debug, Default)]
pub struct MemAssets {
    objects: BTreeMap<ContentHash, Arc<[u8]>>,
}

impl MemAssets {
    /// An empty source: every read is [`AssetError::NotFound`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `bytes` and returns their hash, the same [`ContentHash::of`] a real store computes. Storing the same bytes
    /// twice keeps one copy, as a content-addressed store does.
    pub fn put(&mut self, bytes: &[u8]) -> ContentHash {
        let hash = ContentHash::of(bytes);
        self.objects.entry(hash).or_insert_with(|| Arc::from(bytes));
        hash
    }
}

impl AssetSource for MemAssets {
    fn get(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        self.objects.get(hash).cloned().ok_or(AssetError::NotFound(*hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_come_back_by_hash_and_dedupe() {
        let mut assets = MemAssets::new();
        let a = assets.put(b"radius map");
        assert_eq!(a, ContentHash::of(b"radius map"));
        assert_eq!(assets.put(b"radius map"), a, "the same bytes are the same asset");
        assert_eq!(assets.objects.len(), 1);
        assert_eq!(&*assets.get(&a).unwrap(), b"radius map");
    }

    #[test]
    fn an_absent_hash_is_not_found() {
        let absent = ContentHash::of(b"never stored");
        assert_eq!(MemAssets::new().get(&absent), Err(AssetError::NotFound(absent)));
    }
}
