//! The render family: `omuzaki://render/<id>.png`, a PNG the asset store holds, read as an `image/png` blob.
//!
//! A render op puts its PNG in the store and names the hash in its result. The id is that content hash, so a render
//! never changes: nothing in the document feeds these URIs and `list` is empty, because the store cannot tell which of
//! its assets are renders.

use std::sync::Arc;

use base64::{Engine, prelude::BASE64_STANDARD};
use omuzaki_ops_api::{Grants, OpError, Runner};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use rmcp::model::{Resource, ResourceContents, ResourceTemplate};

use super::ResourceProvider;

pub(crate) const RENDER_PREFIX: &str = "omuzaki://render/";
const PNG_SUFFIX: &str = ".png";
const PNG: &str = "image/png";
/// The eight bytes every PNG file starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Serves `omuzaki://render/<id>.png`: a render the store holds, named by its content hash, read as a PNG blob.
///
/// **Only PNGs.** A read checks the PNG signature before it returns anything, so this URI cannot be used to read another
/// asset (a model, audio, a consent document) by its hash: such an asset is `not_found`, as a render that doesn't exist.
///
/// **No licence gate, no size cap.** Reading is not publishing: `licence_gate` guards what leaves in something
/// published, and the server has already checked the session's `read` scope before `read` runs. The bytes are served
/// whole.
pub struct RenderProvider {
    store: Arc<dyn Store>,
}

impl RenderProvider {
    /// A provider over `store`, the App's asset store.
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// The bytes stored under `hash`, with the store's refusals as the errors a client sees.
    fn fetch(&self, hash: &ContentHash) -> Result<Vec<u8>, OpError> {
        self.store.get(hash).map_err(|error| match error {
            StoreError::Missing(_) => OpError::not_found(format!("no render {hash}")),
            StoreError::Corrupt(_) => {
                OpError::internal(format!("the stored render {hash} is damaged: its bytes no longer match its hash"))
            }
            StoreError::Io(e) => OpError::internal(format!("couldn't read the render {hash}: {e}")),
        })
    }
}

impl std::fmt::Debug for RenderProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderProvider").finish_non_exhaustive()
    }
}

impl ResourceProvider for RenderProvider {
    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new(format!("{RENDER_PREFIX}{{id}}{PNG_SUFFIX}"), "render")
                .with_title("Render")
                .with_description(
                    "A render as a PNG image. `id` is the content hash (64 lowercase hex characters) that the render \
                     op's result names.",
                )
                .with_mime_type(PNG),
        ]
    }

    /// Renders are named by hash and the store cannot tell them from other assets, so nothing is listed.
    fn list(&self, _runner: &dyn Runner, _grants: &Grants) -> Result<Vec<Resource>, OpError> {
        Ok(Vec::new())
    }

    fn serves(&self, uri: &str) -> bool {
        uri.starts_with(RENDER_PREFIX)
    }

    fn read(&self, _runner: &dyn Runner, _grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError> {
        let hash = hash_of(uri)?;
        let bytes = self.fetch(&hash)?;
        if !bytes.starts_with(&PNG_SIGNATURE) {
            return Err(OpError::not_found(format!("{hash} is stored but isn't a PNG image, so it isn't a render")));
        }
        Ok(vec![ResourceContents::blob(BASE64_STANDARD.encode(&bytes), uri).with_mime_type(PNG)])
    }
}

/// The content hash a URI names: `omuzaki://render/<64 lowercase hex>.png` and nothing else.
fn hash_of(uri: &str) -> Result<ContentHash, OpError> {
    uri.strip_prefix(RENDER_PREFIX)
        .and_then(|rest| rest.strip_suffix(PNG_SUFFIX))
        .and_then(|id| id.parse::<ContentHash>().ok())
        .ok_or_else(|| {
            OpError::not_found(format!(
                "{uri} names no render: an id is 64 lowercase hex characters followed by {PNG_SUFFIX}"
            ))
            .hint("use the omuzaki://render/ address a render op returned")
        })
}
