//! UP-0 spike (Linux half): a Velopack `UpdateSource` that sends no identifier and verifies our own
//! ed25519 signature on the feed and the SHA-256 of every asset before it hands a file back.
//! The "server" is an in-memory double, so nothing here touches a network, a real key or an account.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use velopack::bundle::Manifest;
use velopack::sources::UpdateSource;
use velopack::{Error, VelopackAsset, VelopackAssetFeed};

/// Domain separation (design section 4): a feed signature cannot be replayed as any other role's.
const DOMAIN: &[u8] = b"omuzaki-update-v1\0feed\0";

/// Locks a mutex, recovering the data if another thread panicked while holding it: every value
/// behind these locks is a plain map or string that stays valid, so a poisoned lock is not fatal.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn signed_bytes(feed: &[u8]) -> Vec<u8> {
    [DOMAIN, feed].concat()
}

/// The fake host: static files by path, plus a log of every request path it was asked for.
#[derive(Default)]
pub struct FeedDouble {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl FeedDouble {
    /// Stores `bytes` as the static file served at `path`.
    pub fn put(&self, path: &str, bytes: Vec<u8>) {
        lock(&self.files).insert(path.to_owned(), bytes);
    }
    /// Publishes a feed for `channel` plus its detached signature, and the given asset files.
    pub fn publish(&self, key: &SigningKey, channel: &str, feed_json: &str, assets: &[(&str, &[u8])]) {
        self.put(&format!("/v1/{channel}/feed.json"), feed_json.as_bytes().to_vec());
        let sig = key.sign(&signed_bytes(feed_json.as_bytes()));
        self.put(&format!("/v1/{channel}/feed.json.sig"), sig.to_bytes().to_vec());
        for (name, bytes) in assets {
            self.put(&format!("/v1/{channel}/{name}"), bytes.to_vec());
        }
    }
    /// A plain GET of a static path; the path is all the double ever sees.
    fn get(&self, path: &str) -> Result<Vec<u8>, Error> {
        lock(&self.requests).push(format!("GET {path}"));
        lock(&self.files).get(path).cloned().ok_or_else(|| Error::Other(format!("404 {path}")))
    }
    /// Every request path the host was asked for, in order: the evidence for the no-identifier test.
    pub fn requests(&self) -> Vec<String> {
        lock(&self.requests).clone()
    }
}

/// Velopack source that is private by construction and verifies before it returns anything.
pub struct SignedSource {
    host: Arc<FeedDouble>,
    key: VerifyingKey,
    /// File name -> SHA-256 from the *verified* feed. Only the signed copy is trusted, never the
    /// `SHA256` field of the asset the caller hands back to us.
    trusted: Mutex<HashMap<String, String>>,
    channel: Mutex<String>,
}

impl SignedSource {
    /// A source reading from `host` that trusts feeds signed by `key` only.
    pub fn new(host: Arc<FeedDouble>, key: VerifyingKey) -> Self {
        Self { host, key, trusted: Mutex::default(), channel: Mutex::default() }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl UpdateSource for SignedSource {
    // `app` (id, version) and `staged_user_id` are received and deliberately dropped: the path holds
    // the channel and nothing else, and the client compares versions itself.
    fn get_release_feed(
        &self,
        channel: &str,
        _app: &Manifest,
        _staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, Error> {
        let feed = self.host.get(&format!("/v1/{channel}/feed.json"))?;
        let sig = self.host.get(&format!("/v1/{channel}/feed.json.sig"))?;
        let sig = Signature::from_slice(&sig).map_err(|e| Error::Other(format!("bad signature encoding: {e}")))?;
        self.key
            .verify(&signed_bytes(&feed), &sig)
            .map_err(|_| Error::Other("feed signature does not verify".into()))?; // verify before parsing
        let parsed: VelopackAssetFeed = serde_json::from_slice(&feed)?;
        let mut trusted = lock(&self.trusted);
        trusted.clear();
        for a in &parsed.Assets {
            trusted.insert(a.FileName.clone(), a.SHA256.to_ascii_lowercase());
        }
        *lock(&self.channel) = channel.to_owned();
        Ok(parsed)
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        let want = lock(&self.trusted)
            .get(&asset.FileName)
            .cloned()
            .ok_or_else(|| Error::Other(format!("{} is not in the verified feed", asset.FileName)))?;
        let channel = lock(&self.channel).clone();
        let bytes = self.host.get(&format!("/v1/{channel}/{}", asset.FileName))?;
        let got = hex(&Sha256::digest(&bytes));
        if got != want {
            // Refused before any file exists: `local_file` is untouched.
            return Err(Error::ChecksumInvalid(local_file.to_path_buf(), want, got));
        }
        std::fs::write(local_file, &bytes)?;
        if let Some(p) = progress {
            let _ = p.send(100);
        }
        Ok(())
    }
}

/// A Velopack feed with one `Full` asset per `(file name, version, bytes)`, hashes taken from the bytes.
pub fn feed_json(entries: &[(&str, &str, &[u8])]) -> String {
    let assets: Vec<_> = entries
        .iter()
        .map(|(file, version, bytes)| {
            serde_json::json!({ "PackageId": "omuzakitest", "Version": version, "Type": "Full", "FileName": file,
                "SHA1": "", "SHA256": hex(&Sha256::digest(bytes)), "Size": bytes.len(), "NotesMarkdown": "", "NotesHtml": "" })
        })
        .collect();
    serde_json::json!({ "Assets": assets }).to_string()
}

impl FeedDouble {
    /// Reads a stored file without logging a request (test helper for tampering).
    pub fn get_for_test(&self, path: &str) -> Vec<u8> {
        lock(&self.files).get(path).cloned().unwrap_or_default()
    }
}
