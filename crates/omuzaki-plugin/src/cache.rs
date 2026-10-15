//! The compiled-code cache (design §6.1): `<cache dir>/plugins/<hash>-<wasmtime version>-<engine>.cwasm`, a cache
//! only. Each file is the BLAKE3 digest of its payload followed by the payload wasmtime serialised; a file that is
//! missing, truncated, corrupt or made by another engine is compiled again and replaced.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use omuzaki_plugin_api::PluginError;
use omuzaki_store_api::ContentHash;
use wasmtime::component::Component;

use crate::engine::PluginEngine;
use crate::install::write_atomic;

/// The wasmtime release the cache's files are made by (the workspace pins it exactly).
pub const WASMTIME_VERSION: &str = "49.0.0";

/// The digest that leads each cache file.
const DIGEST_BYTES: usize = 32;

/// Where a component came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CacheOutcome {
    /// Read from a valid cache file.
    Hit,
    /// Compiled, because there was no file or it was unusable; the file is written now.
    Compiled,
}

/// The cache folder.
pub struct CodeCache {
    dir: PathBuf,
}

impl CodeCache {
    /// A cache under `cache_dir`.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self { dir: cache_dir.into().join("plugins") }
    }

    /// The file for `hash` on `engine`: the engine part hashes wasmtime's compatibility data (its version, settings
    /// and the CPU features it compiles for), so another machine's or another release's file is never read.
    pub fn file_for(&self, engine: &PluginEngine, hash: &ContentHash) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        engine.wasmtime().precompile_compatibility_hash().hash(&mut hasher);
        self.dir.join(format!("{hash}-{WASMTIME_VERSION}-{:016x}.cwasm", hasher.finish()))
    }

    /// The component for `bytes` (whose package hash is `hash`): from the cache when its file is valid, else compiled
    /// (with the checks of [`load::compile`](crate::load::compile)) and written.
    ///
    /// # Errors
    /// The compile errors; [`PluginError::Io`] when the cache file can't be written.
    pub fn load(
        &self,
        engine: &PluginEngine,
        hash: &ContentHash,
        bytes: &[u8],
    ) -> Result<(Component, CacheOutcome), PluginError> {
        let file = self.file_for(engine, hash);
        if let Some(component) = read(engine, &file) {
            return Ok((component, CacheOutcome::Hit));
        }
        let component = crate::load::compile(engine, bytes)?;
        let payload = component.serialize().map_err(|e| PluginError::Io(format!("{e:#}")))?;
        let mut contents = blake3::hash(&payload).as_bytes().to_vec();
        contents.extend_from_slice(&payload);
        fs::create_dir_all(&self.dir).map_err(|e| PluginError::Io(e.to_string()))?;
        write_atomic(&file, &contents).map_err(PluginError::from)?;
        Ok((component, CacheOutcome::Compiled))
    }
}

/// The component in a cache file, or `None` when the file is unusable for any reason.
fn read(engine: &PluginEngine, file: &Path) -> Option<Component> {
    let contents = fs::read(file).ok()?;
    let (digest, payload) = contents.split_at_checked(DIGEST_BYTES)?;
    if blake3::hash(payload).as_bytes() != digest {
        return None;
    }
    deserialize(engine, payload)
}

/// Loads machine code wasmtime serialised.
#[allow(unsafe_code)]
fn deserialize(engine: &PluginEngine, payload: &[u8]) -> Option<Component> {
    // SAFETY: `Component::deserialize` needs bytes that `Component::serialize` of a compatible engine produced. The
    // payload is only ever written by `CodeCache::load` from `serialize`, its digest has just matched, its file name
    // carries this engine's compatibility hash, and wasmtime checks the header, version and settings again and
    // refuses a mismatch. A plugin can't write here: its files go through `data`, never into the cache folder.
    unsafe { Component::deserialize(engine.wasmtime(), payload) }.ok()
}
