//! The store steps that differ on Windows (#795), run on every platform: flushing the partial file through a handle
//! opened for writing, and a rename onto a blob that is already there, which Windows refuses and the cache then
//! accepts only if what is there is the pin. On unix the rename replaces the file instead, so these tests pass there
//! too; under Windows (the nightly's run) they take the refusal path.

use std::fs;
use std::io;

use omuzaki_ai_api::{NoProgress, PinnedFile, Sha256};
use sha2::{Digest, Sha256 as Hasher};

use super::{Paths, accept_existing, import, sha256_file, store_partial};

fn pin_of(bytes: &[u8]) -> PinnedFile {
    PinnedFile::new(["https://files.invalid/blob"], Sha256(Hasher::digest(bytes).into()), bytes.len() as u64)
}

fn paths(root: &std::path::Path, pin: &PinnedFile) -> Paths {
    let paths = Paths::new(root, pin.sha256);
    fs::create_dir_all(&paths.dir).unwrap();
    paths
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "the rename was refused")
}

#[test]
fn a_verified_partial_is_flushed_and_stored_over_the_same_blob_another_writer_stored_first() {
    let root = tempfile::tempdir().unwrap();
    let bytes = b"the pinned bytes";
    let pin = pin_of(bytes);
    let paths = paths(root.path(), &pin);
    fs::write(&paths.blob, bytes).unwrap();
    fs::write(&paths.partial, bytes).unwrap();
    store_partial(&pin, &paths).unwrap();
    assert_eq!(fs::read(&paths.blob).unwrap(), bytes);
    assert!(!paths.partial.exists(), "the partial file is gone either way");
    assert!(paths.rec.exists(), "the blob has its record");
}

#[test]
fn after_a_refused_rename_only_a_regular_file_with_the_pinned_bytes_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let bytes = b"the pinned bytes";
    let pin = pin_of(bytes);
    let paths = paths(root.path(), &pin);
    assert!(accept_existing(&pin, &paths, &refused()).is_err(), "nothing is there");
    fs::write(&paths.blob, b"other bytes, same length!").unwrap();
    assert!(accept_existing(&pin, &paths, &refused()).is_err(), "the wrong bytes");
    fs::write(&paths.blob, b"the pinned bytez").unwrap();
    assert!(accept_existing(&pin, &paths, &refused()).is_err(), "the right length, the wrong hash");
    fs::remove_file(&paths.blob).unwrap();
    fs::create_dir(&paths.blob).unwrap();
    assert!(accept_existing(&pin, &paths, &refused()).is_err(), "a folder");
    fs::remove_dir(&paths.blob).unwrap();
    fs::write(&paths.blob, bytes).unwrap();
    accept_existing(&pin, &paths, &refused()).unwrap();
}

#[test]
fn importing_over_a_blob_keeps_a_good_one_and_replaces_a_corrupted_one() {
    let root = tempfile::tempdir().unwrap();
    let picked = tempfile::tempdir().unwrap();
    let source = picked.path().join("model.bin");
    let bytes = b"a model the person picked";
    fs::write(&source, bytes).unwrap();
    let (sha256, size) = import(root.path(), &source, &NoProgress).unwrap();
    let paths = Paths::new(root.path(), sha256);
    assert_eq!((fs::read(&paths.blob).unwrap(), size), (bytes.to_vec(), bytes.len() as u64));

    import(root.path(), &source, &NoProgress).unwrap();
    assert_eq!(fs::read(&paths.blob).unwrap(), bytes, "the same bytes again");
    fs::write(&paths.blob, b"a corrupted copy of that model").unwrap();
    import(root.path(), &source, &NoProgress).unwrap();
    assert_eq!(sha256_file(&paths.blob).unwrap(), sha256, "the corrupted copy was replaced");
    let leftovers: Vec<_> = fs::read_dir(&paths.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| std::path::Path::new(n).extension().is_some_and(|e| e == "tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temporary file is left: {leftovers:?}");
}
