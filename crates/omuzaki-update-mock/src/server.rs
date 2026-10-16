//! An in-memory feed server: a [`Fetcher`] that serves fixed bytes per URL and misbehaves on cue.

use std::collections::BTreeMap;
use std::io::Cursor;

use omuzaki_ai_api::{Body, FetchError, Fetcher};

use crate::locked;

/// One [`Fetcher::open`] request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The address asked for.
    pub url: String,
    /// The byte offset asked for.
    pub from: u64,
}

#[derive(Default)]
struct Served {
    bytes: Vec<u8>,
    /// The server stops sending after this many bytes.
    cut_at: Option<usize>,
}

#[derive(Default)]
struct Inner {
    files: BTreeMap<String, Served>,
    calls: Vec<Call>,
    offline: bool,
    ignore_range: bool,
}

/// Serves `url -> bytes` from memory and records every request. An unknown URL fails like a 404.
///
/// The signing and manifest builders arrive with the trust task; this skeleton serves whatever bytes it is given.
#[derive(Default)]
pub struct FakeServer {
    inner: std::sync::Mutex<Inner>,
}

impl FakeServer {
    /// Serves nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `bytes` at `url`, replacing what was there (and any cut made by [`FakeServer::truncate_at`]).
    pub fn put(&self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        locked(&self.inner).files.insert(url.into(), Served { bytes: bytes.into(), cut_at: None });
    }

    /// Makes every request fail with [`FetchError::Network`] (or succeed again).
    pub fn offline(&self, offline: bool) {
        locked(&self.inner).offline = offline;
    }

    /// From now on the body at `url` ends after `n` bytes, as when the connection drops. Returns whether `url` is served.
    #[must_use]
    pub fn truncate_at(&self, url: &str, n: usize) -> bool {
        locked(&self.inner).files.get_mut(url).map(|f| f.cut_at = Some(n)).is_some()
    }

    /// Inverts every bit of the byte at `offset` in the file at `url`: exactly one byte changes. Returns whether
    /// `url` is served and has that byte.
    #[must_use]
    pub fn flip_byte(&self, url: &str, offset: usize) -> bool {
        let mut inner = locked(&self.inner);
        inner.files.get_mut(url).and_then(|f| f.bytes.get_mut(offset)).map(|b| *b = !*b).is_some()
    }

    /// Answers every request with the whole body and `Body::start == 0`, as a server without range support does.
    pub fn ignore_range(&self, ignore: bool) {
        locked(&self.inner).ignore_range = ignore;
    }

    /// Every [`Fetcher::open`] so far, in order, failed ones included.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        locked(&self.inner).calls.clone()
    }
}

impl Fetcher for FakeServer {
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError> {
        let mut inner = locked(&self.inner);
        inner.calls.push(Call { url: url.to_owned(), from });
        let fail = |reason: &str| FetchError::Network { url: url.to_owned(), reason: reason.to_owned() };
        if inner.offline {
            return Err(fail("offline"));
        }
        let ignore_range = inner.ignore_range;
        let file = inner.files.get(url).ok_or_else(|| fail("404 not found"))?;
        let served = file.bytes.get(..file.cut_at.unwrap_or(usize::MAX).min(file.bytes.len())).unwrap_or_default();
        let start = if ignore_range { 0 } else { from };
        let skip = usize::try_from(start).map_err(|_| fail("416 range not satisfiable"))?;
        let rest = served.get(skip..).ok_or_else(|| fail("416 range not satisfiable"))?;
        Ok(Body { start, reader: Box::new(Cursor::new(rest.to_vec())) })
    }
}
