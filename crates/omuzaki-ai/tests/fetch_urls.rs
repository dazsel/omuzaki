//! URL fall-through and HTTP range resume of the verified cache (ADR 0017 decisions 2 and 3). Bodies are in memory: no
//! network, no sleeping. The fetch core's smoke tests are in `fetch_core.rs`.

mod common;

use common::{Spy, cache_files, pin};
use omuzaki_ai::{VerifiedCache, sha256_file};
use omuzaki_ai_api::{FetchError, FileSource, NoProgress};
use omuzaki_ai_mock::MemFetcher;

const U1: &str = "https://first.test/model.bin";
const U2: &str = "https://second.test/model.bin";
const U3: &str = "https://third.test/model.bin";
const BODY: usize = 4096;
const CUT: u64 = 1500;
const RESUME_AT: u64 = 1024;

/// A 4096-byte body whose bytes differ from position to position, so a misplaced range shows up in the hash.
fn body() -> Vec<u8> {
    (0..BODY).map(|i| (i % 251).to_le_bytes()[0]).collect()
}

/// The same length as [`body`] with one byte changed.
fn corrupted() -> Vec<u8> {
    let mut bytes = body();
    bytes[7] ^= 1;
    bytes
}

/// The `(url, from)` pairs of opens of one URL, for comparing with [`Spy::opens`].
fn opens_of(url: &str, froms: &[u64]) -> Vec<(String, u64)> {
    froms.iter().map(|&from| (url.to_owned(), from)).collect()
}

#[test]
fn a_dead_first_url_falls_through_to_the_second() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1, U2], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U2, bytes)));

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), [(U1.to_owned(), 0), (U2.to_owned(), 0)]);
}

#[test]
fn a_wrong_hash_first_url_falls_through() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1, U2], &bytes);
    let hex = pin.sha256.to_string();
    let fetcher = MemFetcher::new().serving(U1, corrupted()).serving(U2, bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(fetcher));

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")], "nothing from the first URL is kept");
}

#[test]
fn all_urls_failing_gives_all_failed_with_every_error_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1, U2, U3], &bytes);
    let fetcher = MemFetcher::new().serving(U2, corrupted()).serving(U3, bytes[..100].to_vec());
    let cache = VerifiedCache::new(dir.path(), Spy::new(fetcher));

    let FetchError::AllFailed(errors) = cache.get(&pin, &NoProgress).unwrap_err() else {
        panic!("expected AllFailed");
    };
    assert!(
        matches!(
            errors.as_slice(),
            [
                FetchError::Network { url: first, .. },
                FetchError::Hash { .. },
                FetchError::Size { .. },
            ] if first == U1
        ),
        "{errors:?}"
    );
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());
}

#[test]
fn an_interrupted_resumable_download_resumes_with_a_range_request() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1], &bytes);
    let hex = pin.sha256.to_string();
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U1, bytes.clone())))
        .with_resume_min_size(RESUME_AT);
    cache.fetcher().break_next_after(CUT);

    assert!(cache.get(&pin, &NoProgress).is_err());
    let partial = dir.path().join("sha256").join(format!("{hex}.partial"));
    assert_eq!(std::fs::metadata(&partial).unwrap().len(), CUT, "the bytes received before the failure are kept");

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), opens_of(U1, &[0, CUT]));
    assert!(!partial.exists(), "the partial became the blob");
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")]);
}

#[test]
fn a_server_that_ignores_ranges_restarts_from_zero_and_still_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1], &bytes);
    let hex = pin.sha256.to_string();
    let cache =
        VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U1, bytes))).with_resume_min_size(RESUME_AT);
    cache.fetcher().break_next_after(CUT);
    assert!(cache.get(&pin, &NoProgress).is_err());

    cache.fetcher().inner().set_ignore_ranges(true);
    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), opens_of(U1, &[0, CUT]), "the range was asked for, then ignored");
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")], "no partial remains");
}

#[test]
fn small_files_never_keep_a_partial() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U1, bytes.clone())))
        .with_resume_min_size(1_000_000);
    cache.fetcher().break_next_after(CUT);

    assert!(cache.get(&pin, &NoProgress).is_err());
    assert_eq!(cache_files(dir.path()), Vec::<String>::new(), "a whole-file download keeps nothing");

    assert_eq!(cache.get(&pin, &NoProgress).map(|p| sha256_file(&p).unwrap()).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), opens_of(U1, &[0, 0]), "the second download starts from the start");
}

#[test]
fn a_resumed_file_whose_bytes_are_corrupted_restarts_once() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1], &bytes);
    let hex = pin.sha256.to_string();
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U1, bytes.clone())))
        .with_resume_min_size(RESUME_AT);
    cache.fetcher().break_next_after(CUT);
    assert!(cache.get(&pin, &NoProgress).is_err());

    let partial = dir.path().join("sha256").join(format!("{hex}.partial"));
    let mut kept = std::fs::read(&partial).unwrap();
    kept[100] ^= 0xff;
    std::fs::write(&partial, kept).unwrap();

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), opens_of(U1, &[0, CUT, 0]), "the whole file is fetched again, once");
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")]);
}

#[test]
fn a_persistent_mismatch_on_a_resumable_file_fails_with_hash() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin(&[U1], &body());
    let hex = pin.sha256.to_string();
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(U1, corrupted())))
        .with_resume_min_size(RESUME_AT);

    let FetchError::AllFailed(errors) = cache.get(&pin, &NoProgress).unwrap_err() else {
        panic!("expected AllFailed");
    };
    assert!(matches!(errors.as_slice(), [FetchError::Hash { .. }]), "{errors:?}");
    assert_eq!(cache.fetcher().opens(), opens_of(U1, &[0, 0]), "two complete downloads, both from the start");
    assert_eq!(cache_files(dir.path()), Vec::<String>::new(), "no blob and no partial");
    assert!(!dir.path().join("sha256").join(&hex).exists());
}

#[test]
fn a_resumable_pin_continues_its_partial_at_the_next_url() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body();
    let pin = pin(&[U1, U2], &bytes);
    let hex = pin.sha256.to_string();
    let fetcher = MemFetcher::new().serving(U1, bytes.clone()).serving(U2, bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(fetcher)).with_resume_min_size(RESUME_AT);
    cache.fetcher().break_next_after(CUT);

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);
    assert_eq!(cache.fetcher().opens(), [(U1.to_owned(), 0), (U2.to_owned(), CUT)]);
    assert_eq!(cache_files(dir.path()), [hex.clone(), format!("{hex}.rec")]);
}
