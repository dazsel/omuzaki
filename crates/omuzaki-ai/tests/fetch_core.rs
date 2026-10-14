//! Smoke tests for the verified cache core; the full lists are in `fetch.rs` and `fetch_urls.rs`.

mod common;

use common::{Spy, cache_files, pin};
use omuzaki_ai::{VerifiedCache, sha256_file};
use omuzaki_ai_api::{FetchError, FileSource, NoProgress};
use omuzaki_ai_mock::MemFetcher;

const URL: &str = "https://example.test/model.onnx";
const MIRROR: &str = "https://mirror.test/model.onnx";

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251).to_le_bytes()[0]).collect()
}

#[test]
fn lookup_finds_a_stored_blob_without_downloading_and_forget_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(3000);
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes)));
    assert_eq!(cache.lookup(pin.sha256).unwrap(), None, "nothing stored yet");

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(cache.lookup(pin.sha256).unwrap(), Some(path));
    assert_eq!(cache.fetcher().opens().len(), 1, "a lookup never downloads");

    cache.forget(pin.sha256).unwrap();
    assert_eq!(cache.lookup(pin.sha256).unwrap(), None);
    assert!(cache_files(dir.path()).is_empty(), "the blob and its record are both gone");
    cache.forget(pin.sha256).unwrap();
    assert_eq!(cache.fetcher().opens().len(), 1, "forgetting never downloads either");
}

#[test]
fn lookup_refuses_a_blob_that_fails_its_hash_and_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(3000);
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.clone())));
    let path = cache.get(&pin, &NoProgress).unwrap();
    std::fs::write(&path, vec![0u8; bytes.len()]).unwrap();

    assert_eq!(cache.lookup(pin.sha256).unwrap(), None, "the wrong bytes are not a verified copy");
    assert!(!path.exists(), "the bad blob is deleted, as get would delete it");
    assert_eq!(cache.fetcher().opens().len(), 1, "and never fetched again by the lookup");
}

#[test]
fn a_lookup_in_an_empty_cache_creates_no_folders() {
    let dir = tempfile::tempdir().unwrap();
    let cache = VerifiedCache::new(dir.path().join("cache"), Spy::new(MemFetcher::new()));

    assert_eq!(cache.lookup(pin(&[URL], &body(10)).sha256).unwrap(), None);
    assert!(!dir.path().join("cache").exists(), "nothing to check, so nothing created");
}

#[test]
fn a_bad_blob_that_cannot_be_deleted_is_an_error_not_an_absent_blob() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(3000);
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.clone())));
    let path = cache.get(&pin, &NoProgress).unwrap();
    // Corrupt the blob, and make its record a directory: the blob's deletion goes through, its record's does not.
    std::fs::write(&path, vec![0u8; bytes.len()]).unwrap();
    let record = path.with_extension("rec");
    std::fs::remove_file(&record).unwrap();
    std::fs::create_dir(&record).unwrap();

    assert!(cache.lookup(pin.sha256).is_err(), "a failed cleanup is reported, not read as absent");
}

#[test]
fn a_cache_hit_makes_no_request() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(3000);
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.clone())));

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(cache.get(&pin, &NoProgress).unwrap(), path);

    assert_eq!(cache.fetcher().opens().len(), 1, "the second get came from the cache");
    assert_eq!(cache_files(dir.path()).len(), 2, "the blob and its record, nothing else");
}

#[test]
fn a_wrong_hash_is_refused_and_nothing_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin(&[URL], &body(2000));
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, body(2001)));
    let wrong_length = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&wrong_length, FetchError::AllFailed(e) if matches!(e[..], [FetchError::TooLarge { .. }])));

    let mut other = body(2000);
    other[7] ^= 1;
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, other));
    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::Hash { .. }])), "{err}");
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());
}

#[test]
fn a_dead_first_url_falls_through_to_the_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(5000);
    let pin = pin(&["https://dead.test/m", MIRROR], &bytes);
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(MIRROR, bytes.clone()));
    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(sha256_file(&path).unwrap(), pin.sha256);

    let none = VerifiedCache::new(tempfile::tempdir().unwrap().path(), MemFetcher::new());
    let FetchError::AllFailed(errors) = none.get(&pin, &NoProgress).unwrap_err() else { panic!("not AllFailed") };
    assert_eq!(errors.len(), 2, "one error per URL");
}

#[test]
fn an_interrupted_download_resumes_with_a_range_request() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(300_000);
    let pin = pin(&[URL], &bytes);
    let spy = Spy::new(MemFetcher::new().serving(URL, bytes.clone()));
    spy.break_next_after(100_000);
    let cache = VerifiedCache::new(dir.path(), spy).with_resume_min_size(1000);

    assert!(cache.get(&pin, &NoProgress).is_err());
    let partial = cache_files(dir.path());
    assert_eq!(partial.len(), 1);
    assert!(partial[0].ends_with(".partial"), "{partial:?}");

    let path = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(cache.fetcher().opens(), [(URL.to_owned(), 0), (URL.to_owned(), 100_000)]);
    assert_eq!(cache_files(dir.path()).len(), 2, "the partial became the blob");
}

#[test]
fn concurrent_gets_download_once_and_leave_a_valid_blob() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(200_000);
    let pin = pin(&[URL], &bytes);
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.clone())))
        .with_resume_min_size(1000);
    let paths: Vec<_> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8).map(|_| scope.spawn(|| cache.get(&pin, &NoProgress).unwrap())).collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert!(paths.iter().all(|p| *p == paths[0]));
    assert_eq!(std::fs::read(&paths[0]).unwrap(), bytes);
    assert_eq!(cache.fetcher().opens().len(), 1, "the others waited and found the blob");
    assert_eq!(cache_files(dir.path()).len(), 2);
}

#[test]
fn a_resumable_download_with_wrong_bytes_is_tried_twice_from_zero() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin(&[URL], &body(5000));
    let mut wrong = body(5000);
    wrong[0] ^= 1;
    let cache =
        VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, wrong))).with_resume_min_size(1000);
    let err = cache.get(&pin, &NoProgress).unwrap_err();
    assert!(matches!(&err, FetchError::AllFailed(e) if matches!(e[..], [FetchError::Hash { .. }])), "{err}");
    assert_eq!(cache.fetcher().opens(), [(URL.to_owned(), 0), (URL.to_owned(), 0)]);
    assert_eq!(cache_files(dir.path()), Vec::<String>::new());
}

#[test]
fn something_that_is_not_the_file_at_the_blob_path_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(3000);
    let pin = pin(&[URL], &bytes);
    let blocker = dir.path().join("sha256").join(pin.sha256.to_string());
    std::fs::create_dir_all(blocker.join("inside")).unwrap();
    let cache = VerifiedCache::new(dir.path(), MemFetcher::new().serving(URL, bytes));
    assert!(matches!(cache.get(&pin, &NoProgress).unwrap_err(), FetchError::Io { .. }));
}

#[test]
fn concurrent_gets_repair_a_corrupt_blob_once() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = body(50_000);
    let pin = pin(&[URL], &bytes);
    let blob = dir.path().join("sha256").join(pin.sha256.to_string());
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, vec![0u8; bytes.len()]).unwrap();
    let cache = VerifiedCache::new(dir.path(), Spy::new(MemFetcher::new().serving(URL, bytes.clone())));
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8).map(|_| scope.spawn(|| cache.get(&pin, &NoProgress).unwrap())).collect();
        for thread in threads {
            let path = thread.join().unwrap();
            assert_eq!(sha256_file(&path).unwrap(), pin.sha256, "a caller was handed a bad file");
        }
    });
    assert_eq!(cache.fetcher().opens().len(), 1, "one caller repaired it, the others reused the result");
}
