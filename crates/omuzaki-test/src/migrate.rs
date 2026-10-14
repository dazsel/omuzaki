//! The one-off move from the old golden layout (`<store>/<suite>/<name>.png`, shared by every branch) to manifests in
//! git over the content-addressed object store.
//!
//! Each old image is copied, byte for byte, to `<store>/objects/<hash>.png`, and the working tree's
//! `tests/goldens/<suite>.toml` gets an entry for it. The old files are left where they are: the move is safe to repeat
//! and to abandon, and deleting them is the maintainers' decision once the manifests are committed.

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_core::id::Slug;
use omuzaki_store_api::ContentHash;

use crate::compare::Tolerance;
use crate::error::{GoldenError, io_err};
use crate::fsio::sorted_entries;
use crate::goldens::{GoldenId, Goldens, decode_png};
use crate::manifest::{Entry, Manifest};
use crate::objects::object_hash;

/// The folder of the new layout inside the store root. It is never a suite, even though its name is a valid slug.
const OBJECTS_DIR: &str = "objects";

/// What [`migrate`] did, case by case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Migration {
    /// Added to a manifest.
    pub migrated: Vec<GoldenId>,
    /// Already in the manifest with this very image.
    pub unchanged: Vec<GoldenId>,
    /// The manifest already has a different image for the case: the manifest wins and the old file is not used.
    pub conflicts: Vec<GoldenId>,
    /// Files and folders that are not goldens (names that aren't slugs, `.png.new` leftovers, other files, anything in a
    /// legacy suite named `objects`, which is reserved for the object store), listed so nothing is ignored silently.
    pub skipped: Vec<PathBuf>,
}

/// Moves every old-layout golden under `goldens.store()` into the object store and the manifests.
///
/// Old goldens carried no tolerance (it was passed per call), so `tolerance` is what the new entries record; edit a
/// manifest afterwards for cases that need a different one.
///
/// # Errors
/// [`GoldenError`] for a bad tolerance, an old file that isn't a decodable PNG, or a failed read or write. Everything
/// moved before the error stays moved, and running the command again continues.
pub fn migrate(goldens: &Goldens, tolerance: Tolerance) -> Result<Migration, GoldenError> {
    tolerance.validate()?;
    let mut report = Migration::default();
    for dir in sorted_entries(goldens.store())? {
        let Some(suite) = suite_slug(&dir, &mut report) else { continue };
        let mut cases = Vec::new();
        for file in sorted_entries(&dir)? {
            let name = file.extension().is_some_and(|ext| ext == "png").then(|| file.file_stem()).flatten();
            match name.and_then(|stem| Slug::new(stem.to_str()?).ok()).filter(|_| file.is_file()) {
                Some(name) => cases.push((name, import(goldens, &file)?)),
                None => report.skipped.push(file),
            }
        }
        record(goldens, &suite, cases, tolerance, &mut report)?;
    }
    Ok(report)
}

/// The suite a store-root entry stands for, or `None` for entries that are not old-layout suites. Folders with
/// non-slug names go on the skipped list; plain files at the root (a README, say) are not goldens and are left out of
/// the report.
///
/// `objects` is the new object store, not a suite, even though its name is a valid slug. Only what a store never
/// holds is listed from it: anything that isn't `<64 hex>.png` is either a stray or a legacy suite of that name, and
/// is reported rather than silently ignored (it is not migrated: `objects` is reserved).
fn suite_slug(entry: &Path, report: &mut Migration) -> Option<Slug> {
    let name = entry.file_name()?.to_str();
    if !entry.is_dir() {
        return None;
    }
    if name == Some(OBJECTS_DIR) {
        report.skipped.extend(strays_in_object_store(entry));
        return None;
    }
    let slug = name.and_then(|n| Slug::new(n).ok());
    if slug.is_none() {
        report.skipped.push(entry.to_owned());
    }
    slug
}

/// Entries of the object store folder that are not objects. An unreadable folder yields nothing here: the migration
/// writes to it next and reports the real error.
fn strays_in_object_store(dir: &Path) -> Vec<PathBuf> {
    sorted_entries(dir).unwrap_or_default().into_iter().filter(|path| object_hash(path).is_none()).collect()
}

/// Copies one old golden into the object store, after checking that it really is a PNG.
fn import(goldens: &Goldens, file: &Path) -> Result<ContentHash, GoldenError> {
    let bytes = fs::read(file).map_err(io_err(file))?;
    decode_png(&bytes, file)?;
    goldens.objects().put(&bytes)
}

/// Adds `cases` of `suite` to its manifest, never replacing an entry that is already there.
fn record(
    goldens: &Goldens,
    suite: &Slug,
    cases: Vec<(Slug, ContentHash)>,
    tolerance: Tolerance,
    report: &mut Migration,
) -> Result<(), GoldenError> {
    let outcomes = Manifest::update(&goldens.manifest_path(suite.as_str()), suite.as_str(), |manifest| {
        cases
            .into_iter()
            .map(|(name, hash)| {
                let id = GoldenId::from_slugs(suite.clone(), name.clone());
                match manifest.get(&name).map(|entry| entry.hash) {
                    None => {
                        manifest.insert(name, Entry::new(hash, tolerance));
                        (id, Verdict::Migrated)
                    }
                    Some(existing) if existing == hash => (id, Verdict::Unchanged),
                    Some(_) => (id, Verdict::Conflict),
                }
            })
            .collect::<Vec<_>>()
    })?;
    for (id, verdict) in outcomes {
        match verdict {
            Verdict::Migrated => report.migrated.push(id),
            Verdict::Unchanged => report.unchanged.push(id),
            Verdict::Conflict => report.conflicts.push(id),
        }
    }
    Ok(())
}

enum Verdict {
    Migrated,
    Unchanged,
    Conflict,
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};
    use tempfile::TempDir;

    use super::*;
    use crate::goldens::{Mode, Outcome, save_png};

    fn flat(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(8, 8, Rgba(color))
    }

    fn id(text: &str) -> GoldenId {
        GoldenId::parse(text).unwrap()
    }

    /// A store in the old layout: `store/render/front-view.png` and `store/render/side-view.png`.
    fn legacy() -> (TempDir, Goldens) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        save_png(&flat([255, 0, 0, 255]), &store.join("render/front-view.png")).unwrap();
        save_png(&flat([0, 0, 255, 255]), &store.join("render/side-view.png")).unwrap();
        save_png(&flat([1, 1, 1, 255]), &store.join("face/neutral.png")).unwrap();
        let goldens = Goldens::new(&store, dir.path().join("manifests"), dir.path().join("out"));
        (dir, goldens)
    }

    #[test]
    fn old_goldens_move_into_manifests_and_keep_passing() {
        let (_dir, goldens) = legacy();
        let tolerance = Tolerance { per_channel: 2, max_bad_fraction: 0.001 };
        let report = migrate(&goldens, tolerance).unwrap();
        assert_eq!(report.migrated, [id("face/neutral"), id("render/front-view"), id("render/side-view")]);
        assert_eq!((report.unchanged.len(), report.conflicts.len(), report.skipped.len()), (0, 0, 0));

        // The manifests are written, with the tolerance, and the images are in the object store byte for byte.
        let manifest = Manifest::load(&goldens.manifest_path("render")).unwrap();
        let entry = manifest.get(&Slug::new("front-view").unwrap()).unwrap();
        assert_eq!(entry.tolerance(), tolerance);
        let old_bytes = fs::read(goldens.store().join("render/front-view.png")).unwrap();
        assert_eq!(fs::read(goldens.objects().path(&entry.hash)).unwrap(), old_bytes);

        // Ordinary checks pass against the migrated goldens, with no re-bless.
        for (case, color) in [("render/front-view", [255, 0, 0, 255]), ("face/neutral", [1, 1, 1, 255])] {
            let outcome = goldens.check(&id(case), &flat(color), Tolerance::EXACT, Mode::Check).unwrap();
            assert!(matches!(outcome, Outcome::Passed(_)), "{case}");
        }
    }

    #[test]
    fn migrating_twice_changes_nothing_and_leaves_the_old_files() {
        let (_dir, goldens) = legacy();
        migrate(&goldens, Tolerance::EXACT).unwrap();
        let manifest = fs::read(goldens.manifest_path("render")).unwrap();

        let again = migrate(&goldens, Tolerance::EXACT).unwrap();
        assert_eq!((again.migrated.len(), again.unchanged.len()), (0, 3));
        assert_eq!(fs::read(goldens.manifest_path("render")).unwrap(), manifest);
        assert!(goldens.store().join("render/front-view.png").exists(), "old files are the user's to delete");
    }

    #[test]
    fn an_existing_manifest_entry_is_never_overwritten() {
        let (_dir, goldens) = legacy();
        // The branch already blessed a different image for this case.
        goldens.check(&id("render/front-view"), &flat([9, 9, 9, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        let blessed = fs::read(goldens.manifest_path("render")).unwrap();

        let report = migrate(&goldens, Tolerance::EXACT).unwrap();
        assert_eq!(report.conflicts, [id("render/front-view")]);
        assert_eq!(report.migrated, [id("face/neutral"), id("render/side-view")]);
        let after = fs::read_to_string(goldens.manifest_path("render")).unwrap();
        assert!(after.contains("[side-view]") && after.contains("[front-view]"));
        let entry = Manifest::load(&goldens.manifest_path("render")).unwrap();
        let kept = entry.get(&Slug::new("front-view").unwrap()).unwrap();
        assert!(String::from_utf8(blessed).unwrap().contains(&kept.hash.to_string()), "the blessed hash survived");
    }

    #[test]
    fn what_is_not_a_golden_is_listed_not_ignored_silently() {
        let (_dir, goldens) = legacy();
        let store = goldens.store();
        fs::write(store.join("render/front-view.png.new"), b"leftover").unwrap();
        fs::write(store.join("render/Notes.txt"), b"hi").unwrap();
        fs::write(store.join("README.md"), b"a plain file at the root").unwrap();
        fs::create_dir_all(store.join("Bad Suite")).unwrap();

        // The new object store sits beside the suites and is not one of them: its objects are not reported, but a
        // legacy suite that happens to be called `objects` is (it is reserved, and not migrated).
        goldens.objects().put(b"x").unwrap();
        save_png(&flat([5, 5, 5, 255]), &store.join("objects/legacy-case.png")).unwrap();

        let report = migrate(&goldens, Tolerance::EXACT).unwrap();
        let skipped: Vec<_> = report.skipped.iter().map(|p| p.strip_prefix(store).unwrap().to_owned()).collect();
        assert_eq!(
            skipped,
            [
                PathBuf::from("Bad Suite"),
                "objects/legacy-case.png".into(),
                "render/Notes.txt".into(),
                "render/front-view.png.new".into()
            ]
        );
        assert_eq!(report.migrated.len(), 3);
    }

    #[test]
    fn an_old_file_that_is_not_a_png_stops_the_move_with_its_name() {
        let (_dir, goldens) = legacy();
        fs::write(goldens.store().join("render/front-view.png"), b"not a png").unwrap();
        let err = migrate(&goldens, Tolerance::EXACT).unwrap_err();
        assert!(matches!(err, GoldenError::Image { .. }) && err.to_string().contains("front-view.png"), "{err}");
    }

    #[test]
    fn a_bad_tolerance_moves_nothing() {
        let (_dir, goldens) = legacy();
        let bad = Tolerance { per_channel: 0, max_bad_fraction: 7.0 };
        assert!(matches!(migrate(&goldens, bad), Err(GoldenError::Compare(_))));
        assert!(!goldens.manifests().exists());
    }
}
