//! An in-memory [`Fetcher`]: serves fixed bytes per URL, honours range requests (or ignores them, like a server
//! without range support), counts requests and can go offline.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use omuzaki_ai_api::{Body, FetchError, Fetcher};

/// Serves `url → bytes` from memory. Unknown URLs fail like a 404; [`MemFetcher::set_offline`] fails everything.
#[derive(Default)]
pub struct MemFetcher {
    files: BTreeMap<String, Vec<u8>>,
    offline: AtomicBool,
    ignore_ranges: AtomicBool,
    requests: AtomicU64,
}

impl MemFetcher {
    /// Serves nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `bytes` at `url`.
    #[must_use]
    pub fn serving(mut self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.files.insert(url.into(), bytes.into());
        self
    }

    /// Makes every request fail (or succeed again).
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
    }

    /// Answers every request with the whole file, as a server without range support does (`Body::start` is 0).
    pub fn set_ignore_ranges(&self, ignore: bool) {
        self.ignore_ranges.store(ignore, Ordering::Relaxed);
    }

    /// How many times [`Fetcher::open`] was called.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }
}

impl Fetcher for MemFetcher {
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let fail = |reason: &str| FetchError::Network { url: url.to_owned(), reason: reason.to_owned() };
        if self.offline.load(Ordering::Relaxed) {
            return Err(fail("offline"));
        }
        let bytes = self.files.get(url).ok_or_else(|| fail("404 not found"))?;
        let start = if self.ignore_ranges.load(Ordering::Relaxed) { 0 } else { from };
        let skip = usize::try_from(start).map_err(|_| fail("416 range not satisfiable"))?;
        let rest = bytes.get(skip..).ok_or_else(|| fail("416 range not satisfiable"))?;
        Ok(Body { start, reader: Box::new(Cursor::new(rest.to_vec())) })
    }
}
