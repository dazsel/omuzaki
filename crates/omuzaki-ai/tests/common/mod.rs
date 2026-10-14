//! Helpers shared by the `omuzaki-ai` fetch tests: building pins, listing the cache folder and a fetcher that records
//! and breaks the streams it serves.
#![allow(dead_code)] // each test binary uses some of these
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure (clippy.toml only exempts the tests)

use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use omuzaki_ai_api::{Body, FetchError, Fetcher, PinnedFile, Sha256};
use sha2::{Digest, Sha256 as Hasher};

/// A pin for `bytes` at `urls`, with the right size and hash.
pub fn pin(urls: &[&str], bytes: &[u8]) -> PinnedFile {
    PinnedFile::new(urls.iter().copied(), Sha256(Hasher::digest(bytes).into()), bytes.len() as u64)
}

/// The names of the files in the cache's `sha256` folder, sorted (empty if the folder doesn't exist).
pub fn cache_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("sha256"))
        .map(|dir| dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Wraps a fetcher and records what the cache does with it: the `(url, from)` of every open, the size of the
/// biggest single read and the bytes read in all; can make the next body fail part-way.
pub struct Spy<F> {
    inner: F,
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    opens: Mutex<Vec<(String, u64)>>,
    break_after: Mutex<Option<u64>>,
    largest_read: AtomicUsize,
    bytes_read: AtomicU64,
}

impl<F: Fetcher> Spy<F> {
    /// Watches `inner`.
    pub fn new(inner: F) -> Self {
        Self { inner, state: Arc::default() }
    }

    /// The wrapped fetcher.
    pub fn inner(&self) -> &F {
        &self.inner
    }

    /// Every `open` so far, as `(url, from)`.
    pub fn opens(&self) -> Vec<(String, u64)> {
        self.state.opens.lock().unwrap().clone()
    }

    /// The next body fails with a read error after `n` bytes; later bodies are fine again.
    pub fn break_next_after(&self, n: u64) {
        *self.state.break_after.lock().unwrap() = Some(n);
    }

    /// The largest buffer any reader was asked to fill at once.
    pub fn largest_read(&self) -> usize {
        self.state.largest_read.load(Ordering::Relaxed)
    }

    /// Bytes handed out by all readers.
    pub fn bytes_read(&self) -> u64 {
        self.state.bytes_read.load(Ordering::Relaxed)
    }
}

impl<F: Fetcher> Fetcher for Spy<F> {
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError> {
        self.state.opens.lock().unwrap().push((url.to_owned(), from));
        let body = self.inner.open(url, from)?;
        let left = self.state.break_after.lock().unwrap().take();
        Ok(Body {
            start: body.start,
            reader: Box::new(SpyReader { inner: body.reader, left, state: self.state.clone() }),
        })
    }
}

struct SpyReader {
    inner: Box<dyn Read + Send>,
    left: Option<u64>,
    state: Arc<State>,
}

impl Read for SpyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.state.largest_read.fetch_max(buf.len(), Ordering::Relaxed);
        let mut want = buf.len();
        if let Some(left) = self.left {
            if left == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "connection reset"));
            }
            want = want.min(usize::try_from(left).unwrap_or(usize::MAX));
        }
        let n = self.inner.read(&mut buf[..want])?;
        if let Some(left) = &mut self.left {
            *left -= n as u64;
        }
        self.state.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
