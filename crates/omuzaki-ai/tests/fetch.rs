//! The verified cache's test list: the downloader tests ported to pinned files, and the streaming tests.
//! Every body is in memory; nothing touches the network or sleeps.
#![allow(clippy::unwrap_used, clippy::expect_used)] // helper impls outside `#[test]` fns (clippy.toml only exempts tests)

mod common;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use common::{Spy, cache_files, pin};
use omuzaki_ai::{VerifiedCache, model_ref, sha256_file};
use omuzaki_ai_api::{Body, FetchError, Fetcher, FileSource, ModelManifest, NoProgress, PinnedFile, Progress, Sha256};
use omuzaki_ai_mock::MemFetcher;
use sha2::{Digest, Sha256 as Hasher};

const URL: &str = "https://example.test/Test.ttf";
const ECHO_FAKE: &str = include_str!("../../omuzaki-ai-mock/models/echo-fake.model.toml");

/// The blob the cache keeps for `pin` under `root`.
fn blob(root: &Path, pin: &PinnedFile) -> PathBuf {
    root.join("sha256").join(pin.sha256.to_string())
}

/// Every file under `dir`, at any depth (empty if `dir` doesn't exist).
fn every_file(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() { every_file(&path) } else { vec![path] }
        })
        .collect()
}

/// True if no file under `dir` ends with `suffix`.
fn none_ends_with(dir: &Path, suffix: &str) -> bool {
    every_file(dir).iter().all(|p| !p.to_string_lossy().ends_with(suffix))
}

#[test]
fn the_first_get_downloads_and_later_gets_come_from_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"font bytes";
    let pin = pin(&[URL], bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.to_vec())));

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(path, blob(dir.path(), &pin));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(cache.get(&pin, &NoProgress).unwrap(), path);

    assert_eq!(cache.fetcher().opens().len(), 1, "the second get must not touch the network");
    let hex = pin.sha256.to_string();
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")]);
}

#[test]
fn a_hash_mismatch_is_refused_and_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    // Same length, different content: the length check passes, the hash must catch it.
    let pin = pin(&[URL], b"font bytes");
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, b"evil bytes".to_vec()));

    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::Hash { .. }])), "{err}");
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());
}

#[test]
fn a_short_body_is_refused_before_hashing() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin(&[URL], b"a much longer font");
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, b"short".to_vec()));

    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(
        matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::Size { expected: 18, actual: 5, .. }])),
        "{err}"
    );
}

#[test]
fn an_oversized_body_is_cut_off_one_byte_past_the_pin() {
    /// An endless body; the counter records what the cache pulled from it.
    struct Endless(Arc<AtomicU64>);
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(4096);
            buf[..n].fill(7);
            self.0.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
    }
    struct EndlessFetcher(Arc<AtomicU64>);
    impl Fetcher for EndlessFetcher {
        fn open(&self, _url: &str, _from: u64) -> Result<Body, FetchError> {
            Ok(Body { start: 0, reader: Box::new(Endless(Arc::clone(&self.0))) })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let pulled = Arc::new(AtomicU64::new(0));
    let pin = pin(&[URL], &[7; 10_000]);
    let cache = VerifiedCache::new(dir.path(), EndlessFetcher(Arc::clone(&pulled)));
    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(
        matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::TooLarge { limit: 10_000, .. }])),
        "{err}"
    );
    // At most one read buffer beyond the pin was requested of an endless stream.
    let pulled = pulled.load(Ordering::Relaxed);
    assert!(pulled <= 10_001 + 4096, "pulled {pulled}");
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());

    // A finite body that is merely too long is refused the same way.
    let long = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, vec![7; 10_500]));
    let err = long.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::TooLarge { .. }])), "{err}");
}

#[test]
fn a_corrupt_or_oversized_cache_file_is_replaced() {
    let bytes = b"font bytes";
    let pin = pin(&[URL], bytes);
    // (a) Wrong bytes of the pinned length, and no record to vouch for them.
    // (b) A blob far longer than the pin.
    for junk in [b"tampered!!".to_vec(), vec![0_u8; 1_000_000]] {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sha256")).unwrap();
        fs::write(blob(dir.path(), &pin), &junk).unwrap();
        let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.to_vec())));

        assert_eq!(fs::read(cache.get(&pin, &NoProgress).unwrap()).unwrap(), bytes);
        assert_eq!(cache.fetcher().opens().len(), 1, "the bad file was replaced by one download");
        assert_eq!(fs::read(blob(dir.path(), &pin)).unwrap(), bytes);
    }
}

#[test]
fn two_threads_getting_the_same_file_at_once_leave_one_valid_file() {
    // Without a barrier: the per-pin lock makes the second caller wait for the first, then find the blob.
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"font bytes";
    let pin = pin(&[URL], bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.to_vec())));

    let paths: Vec<PathBuf> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2).map(|_| scope.spawn(|| cache.get(&pin, &NoProgress).unwrap())).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(paths[0], paths[1], "both callers get the same path");
    assert_eq!(fs::read(&paths[0]).unwrap(), bytes);
    assert_eq!(cache.fetcher().opens().len(), 1, "one request: the second caller found the blob");
    let hex = pin.sha256.to_string();
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")]);
}

#[test]
fn a_failed_store_is_a_clear_error_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"font bytes";
    let pin = pin(&[URL], bytes);
    // A directory in the blob's place: the store can't put the file there, so it must fail, not replace it.
    let occupied = blob(dir.path(), &pin);
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("keep"), b"x").unwrap();
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, bytes.to_vec()));

    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(err, FetchError::Io { .. }), "{err}");
    assert_eq!(fs::read(occupied.join("keep")).unwrap(), b"x", "the directory's file is untouched");
    assert!(none_ends_with(dir.path(), ".tmp") && none_ends_with(dir.path(), ".partial"));
}

#[test]
fn offline_is_a_network_error() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin(&[URL], b"font bytes");
    let fetcher = MemFetcher::new().serving(URL, b"font bytes".to_vec());
    fetcher.set_offline(true);
    let cache = VerifiedCache::new(dir.path(), fetcher);

    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::Network { .. }])), "{err}");
}

#[test]
fn sha256_file_matches_the_known_vector() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc");
    fs::write(&path, b"abc").unwrap();
    assert_eq!(
        sha256_file(&path).unwrap().to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(matches!(sha256_file(&dir.path().join("missing")), Err(FetchError::Io { .. })));
}

#[test]
fn a_blob_whose_mtime_changed_is_rehashed_and_refetched_if_bad() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"font bytes";
    let pin = pin(&[URL], bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.to_vec())));
    let path = cache.get(&pin, &NoProgress).unwrap();
    let later = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);

    // (a) Same length, other bytes, different mtime: checked again, refused, fetched again.
    fs::write(&path, b"evil bytes").unwrap();
    File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    assert_eq!(fs::read(cache.get(&pin, &NoProgress).unwrap()).unwrap(), bytes);
    assert_eq!(cache.fetcher().opens().len(), 2, "one fresh download for the bad blob");

    // (b) A good blob whose only change is its mtime: checked again, found good, not fetched.
    File::options().write(true).open(&path).unwrap().set_modified(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(cache.get(&pin, &NoProgress).unwrap(), path);
    assert_eq!(cache.fetcher().opens().len(), 2, "a good blob with a new mtime needs no request");
}

/// Stops the download after the first progress report, and keeps the reports.
#[derive(Default)]
struct CancelAfterFirstStage {
    stages: Mutex<Vec<f32>>,
}

impl Progress for CancelAfterFirstStage {
    fn stage(&self, _name: &str, fraction: f32) {
        self.stages.lock().unwrap().push(fraction);
    }
    fn is_cancelled(&self) -> bool {
        !self.stages.lock().unwrap().is_empty()
    }
}

#[test]
fn cancelling_mid_download_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = vec![3_u8; 3 * 1024 * 1024];
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, bytes));
    let progress = CancelAfterFirstStage::default();

    assert_eq!(cache.get(&pin, &progress).unwrap_err(), FetchError::Cancelled, "not wrapped in AllFailed");
    let fractions = progress.stages.lock().unwrap().clone();
    assert!(!fractions.is_empty(), "the download reported progress before it was cancelled");
    assert!(fractions.iter().all(|f| (0.0..=1.0).contains(f)), "{fractions:?}");
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());
    assert!(none_ends_with(dir.path(), ".tmp"));
}

/// A body of [`BIG`] bytes made on the fly: the test holds none of it in memory.
const BIG: u64 = 64 * 1024 * 1024;

/// The byte at offset `i` of the generated file.
fn pattern(i: u64) -> u8 {
    (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
}

/// Generates `left` bytes from offset `next` on, in whatever chunks it is asked for.
struct Generated {
    next: u64,
    left: u64,
}

impl Read for Generated {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = usize::try_from(self.left).map_or(buf.len(), |left| left.min(buf.len()));
        for (k, byte) in buf[..n].iter_mut().enumerate() {
            *byte = pattern(self.next + k as u64);
        }
        self.next += n as u64;
        self.left -= n as u64;
        Ok(n)
    }
}

/// Serves the generated [`BIG`]-byte file from any URL.
struct Generator;

impl Fetcher for Generator {
    fn open(&self, _url: &str, _from: u64) -> Result<Body, FetchError> {
        Ok(Body { start: 0, reader: Box::new(Generated { next: 0, left: BIG }) })
    }
}

#[test]
fn a_64_mib_body_is_written_while_it_streams() {
    // The pin's hash, computed by running the same generator through `sha2` once.
    let mut generated = Generated { next: 0, left: BIG };
    let mut hasher = Hasher::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = generated.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let pin = PinnedFile::new([URL], Sha256(hasher.finalize().into()), BIG);

    let dir = tempfile::tempdir().unwrap();
    let cache = VerifiedCache::new(dir.path(), Spy::new(Generator));
    let path = cache.get(&pin, &NoProgress).unwrap();

    let spy = cache.fetcher();
    assert!(spy.largest_read() < 1024 * 1024, "largest read {}", spy.largest_read());
    assert!(spy.bytes_read() <= BIG + 1, "read {} bytes", spy.bytes_read());
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
}

#[test]
fn model_ref_is_stable_when_the_files_are_reordered() {
    // The echo fixture plus two more files, so the manifest has three.
    let extra = "\n[[files]]\n\
        name = \"weights.bin\"\n\
        sha256 = \"ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb\"\n\
        size = 1000\n\
        urls = [\"https://models.invalid/echo-fake/1.0.0/weights.bin\"]\n\
        \n[[files]]\n\
        name = \"tokenizer.json\"\n\
        sha256 = \"3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d\"\n\
        size = 512\n\
        urls = [\"https://models.invalid/echo-fake/1.0.0/tokenizer.json\"]\n";
    let manifest = ModelManifest::from_toml(&format!("{ECHO_FAKE}{extra}")).unwrap();
    assert_eq!(manifest.files.len(), 3);

    let mut reversed = manifest.clone();
    reversed.files.reverse();
    assert_eq!(model_ref(&reversed), model_ref(&manifest), "the file order does not change the identity");

    let mut changed = manifest.clone();
    changed.files[0].sha256 = Sha256(Hasher::digest(b"other").into());
    assert_ne!(model_ref(&changed).digest, model_ref(&manifest).digest, "a changed file changes the digest");

    let expected = Sha256(Hasher::digest(manifest.digest_text().as_bytes()).into());
    assert_eq!(model_ref(&manifest).digest, expected);
}
