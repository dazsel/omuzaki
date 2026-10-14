//! Golden manifests: the part of the golden store that lives in git.
//!
//! `tests/goldens/<suite>.toml` maps every case name to the BLAKE3 hash of its reference image and the tolerance it is
//! compared with:
//!
//! ```toml
//! [front-view]
//! hash = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
//! per_channel = 2
//! max_bad_fraction = 0.001
//! ```
//!
//! Because the manifest is in git, every branch has its own: a bless changes the manifest in the working tree, so the
//! change is in the PR diff, and two branches can bless different images for the same case without touching each
//! other (the images themselves are content-addressed, see [`crate::objects`]).
//!
//! Cases are kept sorted by name and one table per case, so two branches adding different cases merge cleanly.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use omuzaki_core::id::Slug;
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::compare::Tolerance;
use crate::error::{GoldenError, io_err};
use crate::fsio::{ensure_parent, sorted_entries, write_atomic};

/// One case: which image is its reference, and how far a render may drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Hash of the reference PNG in the object store.
    pub hash: ContentHash,
    /// See [`Tolerance::per_channel`]. Left out means 0.
    #[serde(default)]
    pub per_channel: u8,
    /// See [`Tolerance::max_bad_fraction`]. Left out means 0.0.
    #[serde(default)]
    pub max_bad_fraction: f64,
}

impl Entry {
    /// An entry for `hash` compared under `tolerance`.
    pub fn new(hash: ContentHash, tolerance: Tolerance) -> Self {
        Self { hash, per_channel: tolerance.per_channel, max_bad_fraction: tolerance.max_bad_fraction }
    }

    /// The tolerance a render of this case is compared under.
    pub fn tolerance(&self) -> Tolerance {
        Tolerance { per_channel: self.per_channel, max_bad_fraction: self.max_bad_fraction }
    }
}

/// All cases of one suite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Manifest {
    cases: BTreeMap<Slug, Entry>,
}

impl Manifest {
    /// The entry for `name`, if the suite has one.
    pub fn get(&self, name: &Slug) -> Option<&Entry> {
        self.cases.get(name)
    }

    /// Adds or replaces the entry for `name`, returning the one it replaced.
    pub fn insert(&mut self, name: Slug, entry: Entry) -> Option<Entry> {
        self.cases.insert(name, entry)
    }

    /// Every case with its entry, in name order.
    pub fn cases(&self) -> impl Iterator<Item = (&Slug, &Entry)> {
        self.cases.iter()
    }

    /// Reads the manifest at `path`. A missing file is an empty manifest: a suite with no blessed case yet.
    ///
    /// # Errors
    /// [`GoldenError::Manifest`] when the file isn't valid TOML of the expected shape or holds a nonsense tolerance,
    /// [`GoldenError::Io`] when it can't be read.
    pub fn load(path: &Path) -> Result<Self, GoldenError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_err(path)(e)),
        }
    }

    /// Parses manifest `text`. `path` only names the source in errors, so text read from a git commit can say where it
    /// came from.
    ///
    /// # Errors
    /// [`GoldenError::Manifest`] when the text isn't valid TOML of the expected shape or holds a nonsense tolerance.
    pub fn parse(text: &str, path: &Path) -> Result<Self, GoldenError> {
        let bad = |reason: String| GoldenError::Manifest { path: path.to_owned(), reason };
        let manifest: Self = toml::from_str(text).map_err(|e| bad(e.to_string()))?;
        // Catch a hand-edited tolerance here, with the file name, rather than as a bare comparison error later.
        for (name, entry) in &manifest.cases {
            entry.tolerance().validate().map_err(|e| bad(format!("case '{name}': {e}")))?;
        }
        Ok(manifest)
    }

    /// The file's text: a header saying what the file is, then one table per case in name order.
    fn render(&self, suite: &str, path: &Path) -> Result<String, GoldenError> {
        let body = toml::to_string(self)
            .map_err(|e| GoldenError::Manifest { path: path.to_owned(), reason: e.to_string() })?;
        Ok(format!(
            "# Golden manifest for suite `{suite}`: each case's reference image (BLAKE3 hash of the PNG) and the\n\
             # tolerance a render is compared with. The images are not in git: they are <OMUZAKI_GOLDENS>/objects/<hash>.png.\n\
             # `omuzaki-test --bless` rewrites this file (comments you add are not kept); a tolerance can be edited by hand.\n\n\
             {body}"
        ))
    }

    /// Changes the manifest at `path` while holding an exclusive lock, so tests blessing different cases of one suite
    /// at the same time (`OMUZAKI_BLESS=1 cargo test`) can't overwrite each other's entries.
    ///
    /// `change` runs on a fresh read of the file. The file is rewritten, atomically, only when `change` altered the
    /// manifest. The lock is on a sidecar `<suite>.toml.lock` (git-ignored) and is released when this returns, or if
    /// the process dies.
    ///
    /// # Errors
    /// [`GoldenError`] when the lock, the read or the write fails.
    pub fn update<R>(path: &Path, suite: &str, change: impl FnOnce(&mut Self) -> R) -> Result<R, GoldenError> {
        ensure_parent(path)?;
        let lock_path = path.with_extension("toml.lock");
        let lock =
            OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(io_err(&lock_path))?;
        lock.lock().map_err(io_err(&lock_path))?;

        let mut manifest = Self::load(path)?;
        let before = manifest.clone();
        let result = change(&mut manifest);
        if manifest != before {
            write_atomic(path, manifest.render(suite, path)?.as_bytes())?;
        }
        Ok(result)
    }
}

/// The suite a manifest file is named after: `render.toml` is suite `render`. `None` for any other file name (the
/// `.toml.lock` sidecars, notes, files whose stem is not a slug), none of which the tools ever read as a manifest.
pub(crate) fn suite_of(file_name: &str) -> Option<Slug> {
    file_name.strip_suffix(".toml").and_then(|stem| Slug::new(stem).ok())
}

/// The manifest files directly inside `dir`, with their suites, in name order. A missing folder has none.
///
/// # Errors
/// [`GoldenError::Io`] when the folder can't be read, or a file in it can't be inspected (a file that cannot be seen
/// is not a file that is not there).
pub(crate) fn suite_files(dir: &Path) -> Result<Vec<(Slug, PathBuf)>, GoldenError> {
    let mut files = Vec::new();
    for path in sorted_entries(dir)? {
        let Some(suite) = path.file_name().and_then(|name| name.to_str()).and_then(suite_of) else { continue };
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => files.push((suite, path)),
            // A folder named like a manifest, or a link whose target has gone.
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&path)(e)),
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(s: &str) -> Slug {
        Slug::new(s).unwrap()
    }

    fn entry(byte: u8, per_channel: u8) -> Entry {
        Entry::new(ContentHash([byte; 32]), Tolerance { per_channel, max_bad_fraction: 0.001 })
    }

    #[test]
    fn a_missing_manifest_is_an_empty_suite() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Manifest::load(&tmp.path().join("none.toml")).unwrap(), Manifest::default());
    }

    #[test]
    fn the_file_is_sorted_one_table_per_case_and_reads_back_identically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tests/goldens/render.toml");
        Manifest::update(&path, "render", |m| {
            m.insert(slug("side-view"), entry(2, 0));
            m.insert(slug("front-view"), entry(1, 2));
        })
        .unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let expected = format!(
            "[front-view]\nhash = \"{}\"\nper_channel = 2\nmax_bad_fraction = 0.001\n\n\
             [side-view]\nhash = \"{}\"\nper_channel = 0\nmax_bad_fraction = 0.001\n",
            "01".repeat(32),
            "02".repeat(32)
        );
        assert!(text.starts_with("# Golden manifest for suite `render`"), "{text}");
        assert!(text.ends_with(&expected), "{text}");

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.get(&slug("front-view")), Some(&entry(1, 2)));
        assert_eq!(loaded.get(&slug("nope")), None);
    }

    #[test]
    fn an_unchanged_manifest_is_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.toml");
        Manifest::update(&path, "s", |m| m.insert(slug("a"), entry(1, 0))).unwrap();
        fs::write(&path, format!("{}# a note\n", fs::read_to_string(&path).unwrap())).unwrap();
        let hand_edited = fs::read(&path).unwrap();

        Manifest::update(&path, "s", |m| m.insert(slug("a"), entry(1, 0))).unwrap();
        assert_eq!(fs::read(&path).unwrap(), hand_edited, "no change, so the hand-written note survives");
    }

    #[test]
    fn omitted_tolerance_means_exact_and_typos_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.toml");
        let hash = "ab".repeat(32);
        fs::write(&path, format!("[a]\nhash = \"{hash}\"\n")).unwrap();
        assert_eq!(Manifest::load(&path).unwrap().get(&slug("a")).unwrap().tolerance(), Tolerance::EXACT);

        fs::write(&path, format!("[a]\nhash = \"{hash}\"\nmax_bad_fracton = 0.1\n")).unwrap();
        let err = Manifest::load(&path).unwrap_err().to_string();
        assert!(err.contains("s.toml") && err.contains("max_bad_fracton"), "{err}");
    }

    #[test]
    fn bad_hashes_names_and_tolerances_are_refused_with_the_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.toml");
        let good = "ab".repeat(32);
        for (label, text) in [
            ("short hash", "[a]\nhash = \"abc\"\n".to_owned()),
            ("uppercase hash", format!("[a]\nhash = \"{}\"\n", "AB".repeat(32))),
            ("no hash", "[a]\nper_channel = 1\n".to_owned()),
            ("name not a slug", format!("[\"Bad Name\"]\nhash = \"{good}\"\n")),
            ("tolerance over 1", format!("[a]\nhash = \"{good}\"\nmax_bad_fraction = 2.0\n")),
            ("not toml", "[a\n".to_owned()),
        ] {
            fs::write(&path, text).unwrap();
            let err = Manifest::load(&path).unwrap_err();
            assert!(
                matches!(err, GoldenError::Manifest { .. }) && err.to_string().contains("s.toml"),
                "{label}: {err}"
            );
        }
    }

    #[test]
    fn only_slug_named_toml_files_are_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["render.toml", "face_2.toml", "render.toml.lock", "Notes.toml", "bad name.toml", "README.md"] {
            fs::write(tmp.path().join(name), "").unwrap();
        }
        fs::create_dir(tmp.path().join("folder.toml")).unwrap();
        let suites: Vec<_> = suite_files(tmp.path()).unwrap().into_iter().map(|(s, _)| s.as_str().to_owned()).collect();
        assert_eq!(suites, ["face_2", "render"]);
        let none = suite_files(&tmp.path().join("missing")).unwrap();
        assert!(none.is_empty(), "{none:?}");
    }

    #[test]
    fn parse_names_its_source_in_errors() {
        let err = Manifest::parse("[a\n", Path::new("abc123:tests/goldens/s.toml")).unwrap_err();
        assert!(err.to_string().contains("abc123:tests/goldens/s.toml"), "{err}");
    }

    #[test]
    fn concurrent_updates_all_land() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.toml");
        std::thread::scope(|scope| {
            for i in 0..16u8 {
                let path = &path;
                scope.spawn(move || {
                    Manifest::update(path, "s", |m| m.insert(slug(&format!("case-{i:02}")), entry(i, 0))).unwrap();
                });
            }
        });
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.cases.len(), 16, "no update may be lost to a race");
    }
}
