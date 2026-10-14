//! `omuzaki-test verify`: does the object store hold every image the manifests name, undamaged?
//!
//! The store is append-only and lives outside git, so a manifest can name an image that was blessed on another machine
//! or lost from the store, which otherwise shows only when a test happens to run that case. `verify` reads every
//! manifest of the checkout and reports each case whose image is missing or no longer hashes to its name.

use std::collections::BTreeMap;

use omuzaki_store_api::ContentHash;

use crate::error::GoldenError;
use crate::goldens::{GoldenId, Goldens};
use crate::manifest::{Manifest, suite_files};
use crate::objects::ObjectStore;

/// What is wrong with a case's image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The object store has no such image (blessed on another machine, or the store was cleaned).
    Missing,
    /// The file exists but its bytes no longer hash to its name (cut short, or tampered with).
    Corrupt,
    /// The file could not be read; the text is the OS error.
    Unreadable(String),
}

/// A case whose image cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The case.
    pub id: GoldenId,
    /// The hash its manifest names.
    pub hash: ContentHash,
    /// What is wrong with the image.
    pub fault: Fault,
}

/// What [`verify`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verification {
    /// Manifest files read.
    pub manifests: usize,
    /// Cases checked.
    pub cases: usize,
    /// The cases that fail, in suite and name order. Empty when everything checks out.
    pub problems: Vec<Problem>,
}

/// Checks every case of every manifest in `goldens.manifests()` against the object store.
///
/// Each distinct image is read and hashed once, however many cases share it.
///
/// # Errors
/// [`GoldenError::NoManifestsIn`] when the folder is missing or holds no manifest: checking nothing is not a pass.
/// [`GoldenError`] also when the folder can't be read or a manifest is malformed. A bad image is a [`Problem`], not an
/// error.
pub fn verify(goldens: &Goldens) -> Result<Verification, GoldenError> {
    let suites = suite_files(goldens.manifests())?;
    if suites.is_empty() {
        return Err(GoldenError::NoManifestsIn { folder: goldens.manifests().to_path_buf() });
    }
    let mut verdicts: BTreeMap<ContentHash, Option<Fault>> = BTreeMap::new();
    let mut result = Verification::default();
    for (suite, path) in suites {
        result.manifests += 1;
        for (name, entry) in Manifest::load(&path)?.cases() {
            result.cases += 1;
            let fault = verdicts.entry(entry.hash).or_insert_with(|| fault_of(goldens.objects(), &entry.hash));
            if let Some(fault) = fault {
                result.problems.push(Problem {
                    id: GoldenId::from_slugs(suite.clone(), name.clone()),
                    hash: entry.hash,
                    fault: fault.clone(),
                });
            }
        }
    }
    Ok(result)
}

/// Why the image `hash` is unusable, or `None` when it is stored intact.
fn fault_of(objects: &ObjectStore, hash: &ContentHash) -> Option<Fault> {
    match objects.get(hash) {
        Ok(_) => None,
        Err(GoldenError::ObjectMissing { .. }) => Some(Fault::Missing),
        Err(GoldenError::ObjectCorrupt { .. }) => Some(Fault::Corrupt),
        Err(other) => Some(Fault::Unreadable(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::compare::Tolerance;
    use crate::manifest::Entry;

    /// A store and manifest folder in `tmp`, with `cases` (suite, name, image bytes) blessed.
    fn blessed(tmp: &tempfile::TempDir, cases: &[(&str, &str, &[u8])]) -> (Goldens, Vec<ContentHash>) {
        let goldens = Goldens::new(tmp.path().join("store"), tmp.path().join("manifests"), tmp.path().join("out"));
        let hashes = cases
            .iter()
            .map(|(suite, name, bytes)| {
                let hash = goldens.objects().put(bytes).unwrap();
                Manifest::update(&goldens.manifest_path(suite), suite, |m| {
                    m.insert(omuzaki_core::id::Slug::new(*name).unwrap(), Entry::new(hash, Tolerance::EXACT));
                })
                .unwrap();
                hash
            })
            .collect();
        (goldens, hashes)
    }

    #[test]
    fn an_intact_store_verifies_and_counts_what_it_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let (goldens, _) = blessed(&tmp, &[("render", "a", b"one"), ("render", "b", b"two"), ("face", "c", b"one")]);
        let result = verify(&goldens).unwrap();
        assert_eq!((result.manifests, result.cases), (2, 3));
        assert!(result.problems.is_empty(), "{:?}", result.problems);
    }

    #[test]
    fn a_missing_and_a_tampered_object_are_flagged_by_case_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (goldens, hashes) =
            blessed(&tmp, &[("render", "gone", b"one"), ("render", "bent", b"two"), ("face", "fine", b"3")]);
        fs::remove_file(goldens.objects().path(&hashes[0])).unwrap();
        fs::write(goldens.objects().path(&hashes[1]), b"tampered").unwrap();

        let problems = verify(&goldens).unwrap().problems;
        let found: Vec<_> = problems.iter().map(|p| (p.id.to_string(), p.fault.clone())).collect();
        assert_eq!(
            found,
            [("render/bent".to_owned(), Fault::Corrupt), ("render/gone".to_owned(), Fault::Missing)],
            "suites and cases in name order, the intact case not listed"
        );
        assert_eq!(problems[1].hash, hashes[0]);
    }

    #[test]
    fn every_case_sharing_a_damaged_image_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (goldens, hashes) = blessed(&tmp, &[("s", "a", b"same"), ("s", "b", b"same")]);
        fs::remove_file(goldens.objects().path(&hashes[0])).unwrap();
        let names: Vec<_> = verify(&goldens).unwrap().problems.iter().map(|p| p.id.to_string()).collect();
        assert_eq!(names, ["s/a", "s/b"]);
    }

    #[test]
    fn a_missing_or_empty_manifest_folder_is_an_error_naming_the_folder_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let goldens = Goldens::new(tmp.path().join("store"), tmp.path().join("manifests"), tmp.path().join("out"));
        let refused = |err: GoldenError| match err {
            GoldenError::NoManifestsIn { folder } => folder,
            other => panic!("expected NoManifestsIn, got {other}"),
        };
        assert_eq!(refused(verify(&goldens).unwrap_err()), goldens.manifests(), "missing folder");

        fs::create_dir_all(goldens.manifests()).unwrap();
        fs::write(goldens.manifests().join("notes.txt"), "not a manifest").unwrap();
        assert_eq!(refused(verify(&goldens).unwrap_err()), goldens.manifests(), "folder with no manifest");
    }

    #[test]
    fn a_bad_manifest_is_an_error_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let goldens = Goldens::new(tmp.path().join("store"), tmp.path().join("manifests"), tmp.path().join("out"));
        fs::create_dir_all(goldens.manifests()).unwrap();
        fs::write(goldens.manifest_path("broken"), "[a\n").unwrap();
        let err = verify(&goldens).unwrap_err();
        assert!(matches!(err, GoldenError::Manifest { .. }) && err.to_string().contains("broken.toml"), "{err}");
    }
}
