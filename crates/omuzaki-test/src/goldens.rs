//! The golden store and the check/bless flow around it.
//!
//! A golden is split in two so that it can be versioned per branch without putting images in git:
//!
//! - the **manifest** `tests/goldens/<suite>.toml` (in git, see [`crate::manifest`]) maps each case to the hash of its
//!   reference image and the tolerance it is compared with;
//! - the **image** lives in the content-addressed object store `<OMUZAKI_GOLDENS>/objects/<hash>.png` in the assets folder (see
//!   [`crate::objects`]), shared by all branches and never overwritten.
//!
//! `--bless` stores the new image and rewrites the manifest entry in the working tree, so the PR diff shows exactly
//! which goldens moved, and another branch keeps comparing against its own manifest's images.
//!
//! Whenever a check finds something to look at (a mismatch, a missing golden, or a blessed change), the evidence is
//! written under an *output directory* as `<suite>/<name>.{expected,actual,diff}.png` plus `<name>.json`. That folder is
//! what [`crate::summary`] turns into the PR comment. A check that passes deletes its own stale evidence, so the folder
//! always describes the latest run.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use image::{ImageError, RgbaImage};
use omuzaki_core::id::Slug;
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::compare::{CompareError, Stats, Tolerance, compare};
use crate::dirs::{Dirs, Located};
pub use crate::error::GoldenError;
use crate::error::io_err;
use crate::fsio::ensure_parent;
use crate::manifest::{Entry, Manifest};
use crate::objects::ObjectStore;

/// Environment variable that switches checks into [`Mode::Bless`] when set to `1`.
pub const BLESS_ENV: &str = "OMUZAKI_BLESS";

/// Names one golden image: `<suite>/<name>`, each part a [`Slug`] so it is safe as a file name on every OS and can
/// never climb out of the store with `..`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GoldenId {
    suite: Slug,
    name: Slug,
}

impl GoldenId {
    /// Builds an id from its two parts.
    ///
    /// # Errors
    /// [`GoldenError::BadId`] when either part is not a valid [`Slug`].
    pub fn new(suite: &str, name: &str) -> Result<Self, GoldenError> {
        let part = |s: &str| {
            Slug::new(s).map_err(|e| GoldenError::BadId { id: format!("{suite}/{name}"), reason: e.to_string() })
        };
        Ok(Self { suite: part(suite)?, name: part(name)? })
    }

    /// Parses `suite/name`.
    ///
    /// # Errors
    /// [`GoldenError::BadId`] when there is not exactly one `/` or a part is not a valid [`Slug`].
    pub fn parse(text: &str) -> Result<Self, GoldenError> {
        match text.split_once('/') {
            Some((suite, name)) if !name.contains('/') => Self::new(suite, name),
            _ => Err(GoldenError::BadId {
                id: text.to_owned(),
                reason: "expected exactly one '/', as in suite/name".into(),
            }),
        }
    }

    /// Builds an id from parts that are already valid slugs.
    pub(crate) fn from_slugs(suite: Slug, name: Slug) -> Self {
        Self { suite, name }
    }

    /// The suite part (one manifest file, one evidence folder).
    pub fn suite(&self) -> &str {
        self.suite.as_str()
    }

    /// The name part (the case within the manifest).
    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

impl fmt::Display for GoldenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.suite, self.name)
    }
}

/// What a check does when the image doesn't match its golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Report the difference and fail.
    Check,
    /// Accept the new image as the golden: store it and point the manifest at it (keeping the old image in the
    /// evidence folder).
    Bless,
}

impl Mode {
    /// [`Mode::Bless`] when `bless` is true (a `--bless` flag), otherwise whatever `OMUZAKI_BLESS` says.
    pub fn from_flag_or_env(bless: bool) -> Self {
        Self::parse(bless.then_some("1").or(std::env::var(BLESS_ENV).ok().as_deref()))
    }

    /// Reads a `OMUZAKI_BLESS` value: only `1` blesses, so a stray `OMUZAKI_BLESS=0` can't rewrite goldens by accident.
    pub fn parse(value: Option<&str>) -> Self {
        if value == Some("1") { Self::Bless } else { Self::Check }
    }
}

/// Why an image needs a human's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Differs from the golden by more than the tolerance.
    Failed,
    /// Has different dimensions than the golden, so no pixel comparison is possible.
    SizeMismatch,
    /// There is no golden yet, and the run was not blessing.
    New,
    /// Was accepted as the new golden. Recorded so the PR can show exactly which goldens changed.
    Blessed,
}

impl Status {
    /// Short label for logs and PR comments.
    pub fn label(self) -> &'static str {
        match self {
            Self::Failed => "differs",
            Self::SizeMismatch => "size mismatch",
            Self::New => "new, no golden",
            Self::Blessed => "blessed",
        }
    }
}

/// The evidence for one image that needs a look; also the `<name>.json` file stored beside its pictures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The golden's suite. A [`Slug`], so reading a hand-edited or hostile `.json` back fails instead of letting
    /// arbitrary text into the PR comment's Markdown.
    pub suite: Slug,
    /// The golden's name (a [`Slug`], for the same reason).
    pub name: Slug,
    /// Why it was recorded.
    pub status: Status,
    /// Comparison numbers. `None` when there was nothing to compare (new golden, size mismatch). Also means there is
    /// no diff image.
    pub stats: Option<Stats>,
    /// Width and height of the image under test (`<name>.actual.png`).
    pub actual_size: [u32; 2],
    /// Width and height of the previous golden (`<name>.expected.png`). `None` when there was none.
    pub expected_size: Option<[u32; 2]>,
    /// Hash of the image under test: the "after" of the manifest entry when the run blessed. Like the names, it can't
    /// carry text into the PR comment: a hash is exactly 64 hex characters or the `.json` is refused.
    pub actual_hash: ContentHash,
    /// Hash of the golden the manifest had before (the "before"). `None` when the case had no entry.
    pub expected_hash: Option<ContentHash>,
}

impl Finding {
    /// Whether this finding should fail the run: everything except an explicit bless.
    pub fn is_failure(&self) -> bool {
        self.status != Status::Blessed
    }
}

/// What [`Goldens::check`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Within tolerance. Nothing was written (and any evidence left by an earlier run was removed).
    Passed(Stats),
    /// Something needs a look. The evidence is in the output directory.
    Changed(Finding),
}

impl Outcome {
    /// True for a pass or a bless; false when the test should fail.
    pub fn is_ok(&self) -> bool {
        match self {
            Self::Passed(_) => true,
            Self::Changed(finding) => !finding.is_failure(),
        }
    }
}

/// Reads a PNG as 8-bit RGBA.
///
/// # Errors
/// [`GoldenError::Image`] when the file is missing, unreadable or not a decodable image.
pub fn load_png(path: &Path) -> Result<RgbaImage, GoldenError> {
    image::open(path)
        .map(image::DynamicImage::into_rgba8)
        .map_err(|source| GoldenError::Image { path: path.to_owned(), source })
}

/// Writes `image` as a PNG, creating parent folders. The encoder is deterministic, so equal pixels give equal bytes.
///
/// # Errors
/// [`GoldenError::Io`] or [`GoldenError::Image`] when the file can't be written.
pub fn save_png(image: &RgbaImage, path: &Path) -> Result<(), GoldenError> {
    ensure_parent(path)?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|source| GoldenError::Image { path: path.to_owned(), source })
}

/// The PNG bytes of `image`: what gets hashed, stored as the golden and shown as `<name>.actual.png`.
///
/// # Errors
/// [`GoldenError::Encode`] when the encoder fails.
pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, GoldenError> {
    let mut bytes = Vec::new();
    image.write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png).map_err(GoldenError::Encode)?;
    Ok(bytes)
}

/// Decodes PNG `bytes` read from `path` (named only for the error message).
pub(crate) fn decode_png(bytes: &[u8], path: &Path) -> Result<RgbaImage, GoldenError> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(image::DynamicImage::into_rgba8)
        .map_err(|source: ImageError| GoldenError::Image { path: path.to_owned(), source })
}

/// The reference image a manifest entry points at, with the exact bytes it was stored as.
struct Reference {
    bytes: Vec<u8>,
    image: RgbaImage,
}

/// The golden store: manifests (in git), reference images (in the assets folder) and the folder evidence goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goldens {
    store: PathBuf,
    objects: ObjectStore,
    manifests: PathBuf,
    out: PathBuf,
}

/// Where one image's evidence files live.
struct EvidencePaths {
    expected: PathBuf,
    actual: PathBuf,
    diff: PathBuf,
    json: PathBuf,
}

impl EvidencePaths {
    /// Every file, the json first: removing in this order never leaves a json describing missing pictures.
    fn all(&self) -> [&Path; 4] {
        [&self.json, &self.expected, &self.actual, &self.diff]
    }
}

impl Goldens {
    /// A store whose images are under `store` (in `store/objects`), whose manifests are in `manifests`, writing
    /// evidence to `out`.
    pub fn new(store: impl AsRef<Path>, manifests: impl Into<PathBuf>, out: impl Into<PathBuf>) -> Self {
        let store = store.as_ref();
        Self { store: store.to_owned(), objects: ObjectStore::new(store), manifests: manifests.into(), out: out.into() }
    }

    /// The store named by the environment (`OMUZAKI_GOLDENS`, `OMUZAKI_GOLDEN_MANIFESTS`, `OMUZAKI_GOLDEN_OUT`) and the
    /// workspace around the current directory: what a render test normally wants.
    ///
    /// # Errors
    /// [`GoldenError::NoGoldensDir`] when `OMUZAKI_GOLDENS` is unset, [`GoldenError::NoManifestsDir`] when the
    /// manifest folder can't be found.
    pub fn from_env() -> Result<Self, GoldenError> {
        Self::resolve(Dirs::default())
    }

    /// Resolves the folders from explicit paths, falling back to the environment and then to defaults
    /// (see [`crate::dirs`]).
    ///
    /// # Errors
    /// As [`Dirs::locate`].
    pub fn resolve(dirs: Dirs) -> Result<Self, GoldenError> {
        dirs.locate().map(Self::from_located)
    }

    /// Builds the store from folders that are already decided.
    pub fn from_located(located: Located) -> Self {
        Self::new(located.store, located.manifests, located.out)
    }

    /// The store root (images are in its `objects` folder).
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// The content-addressed image store.
    pub fn objects(&self) -> &ObjectStore {
        &self.objects
    }

    /// The manifest folder.
    pub fn manifests(&self) -> &Path {
        &self.manifests
    }

    /// The evidence directory.
    pub fn out(&self) -> &Path {
        &self.out
    }

    /// Where the manifest of `suite` is (or would be).
    pub fn manifest_path(&self, suite: &str) -> PathBuf {
        self.manifests.join(format!("{suite}.toml"))
    }

    fn evidence_paths(&self, id: &GoldenId) -> EvidencePaths {
        let dir = self.out.join(id.suite());
        let file = |ext: &str| dir.join(format!("{}.{ext}", id.name()));
        EvidencePaths {
            expected: file("expected.png"),
            actual: file("actual.png"),
            diff: file("diff.png"),
            json: file("json"),
        }
    }

    /// The tolerance the manifest has for `id`, or `None` when the case has no entry yet. This is what a check will
    /// compare under, whatever tolerance the caller passes.
    ///
    /// # Errors
    /// [`GoldenError::Manifest`] or [`GoldenError::Io`] when the manifest can't be read.
    pub fn recorded_tolerance(&self, id: &GoldenId) -> Result<Option<Tolerance>, GoldenError> {
        Ok(Manifest::load(&self.manifest_path(id.suite()))?.get(&id.name).map(Entry::tolerance))
    }

    fn load_reference(&self, entry: &Entry) -> Result<Reference, GoldenError> {
        let bytes = self.objects.get(&entry.hash)?;
        let image = decode_png(&bytes, &self.objects.path(&entry.hash))?;
        Ok(Reference { bytes, image })
    }

    /// Compares `actual` with the golden the manifest has for `id`.
    ///
    /// The manifest decides the tolerance: `new_case_tolerance` is only what gets recorded when this run creates the
    /// case (so it is ignored for a case that already has an entry; to change an existing tolerance, edit the
    /// manifest, where the change is reviewed).
    ///
    /// - Within tolerance: [`Outcome::Passed`]; stale evidence from earlier runs is deleted and the manifest is left
    ///   untouched even when blessing (tolerated noise is not a change).
    /// - Otherwise the evidence is written, and in [`Mode::Bless`] the image is also stored in the object store and
    ///   the manifest entry rewritten in the working tree. Blessing also repairs a case whose image is missing from or
    ///   damaged in the store (a plain check reports that as an error). The old image stays in the object store (other branches may
    ///   still use it) and in the evidence folder as `<name>.expected.png`.
    ///
    /// # Errors
    /// [`GoldenError`] for an unreadable manifest, a golden missing from or damaged in the store, a bad tolerance or a
    /// failed write. An image that simply doesn't match is [`Outcome::Changed`], not an error.
    pub fn check(
        &self,
        id: &GoldenId,
        actual: &RgbaImage,
        new_case_tolerance: Tolerance,
        mode: Mode,
    ) -> Result<Outcome, GoldenError> {
        // Up front, so blessing a brand-new golden (which never reaches `compare`) can't smuggle in a bad tolerance.
        new_case_tolerance.validate()?;
        let manifest_path = self.manifest_path(id.suite());
        let entry = Manifest::load(&manifest_path)?.get(&id.name).copied();
        let reference = match entry.as_ref().map(|entry| self.load_reference(entry)).transpose() {
            Ok(reference) => reference,
            // The manifest names an image the store has lost or damaged. Checking can't go on, but blessing can: the
            // new image is stored (replacing the damaged object) and the entry rewritten, as the error advises.
            Err(GoldenError::ObjectMissing { .. } | GoldenError::ObjectCorrupt { .. }) if mode == Mode::Bless => None,
            Err(e) => return Err(e),
        };
        let tolerance = entry.map_or(new_case_tolerance, |entry| entry.tolerance());
        let paths = self.evidence_paths(id);

        let (problem, stats, diff) = match &reference {
            None => (Status::New, None, None),
            Some(old) => match compare(actual, &old.image, tolerance) {
                Ok(report) if report.stats.passed => {
                    clear_evidence(&paths)?;
                    return Ok(Outcome::Passed(report.stats));
                }
                Ok(report) => (Status::Failed, Some(report.stats), Some(report.diff)),
                Err(CompareError::SizeMismatch { .. }) => (Status::SizeMismatch, None, None),
                Err(e) => return Err(e.into()),
            },
        };

        let actual_png = encode_png(actual)?;
        let actual_hash = ContentHash::of(&actual_png);
        let blessing = mode == Mode::Bless;
        let finding = Finding {
            suite: id.suite.clone(),
            name: id.name.clone(),
            status: if blessing { Status::Blessed } else { problem },
            stats,
            actual_size: [actual.width(), actual.height()],
            expected_size: reference.as_ref().map(|old| [old.image.width(), old.image.height()]),
            actual_hash,
            expected_hash: entry.map(|entry| entry.hash),
        };
        let expected_png = reference.as_ref().map(|old| old.bytes.as_slice());
        write_evidence(&paths, &actual_png, expected_png, diff.as_ref(), &finding)?;
        if blessing {
            // The image first: a manifest must never point at an image that isn't in the store yet.
            self.objects.put(&actual_png)?;
            Manifest::update(&manifest_path, id.suite(), |manifest| {
                manifest.insert(id.name.clone(), Entry::new(actual_hash, tolerance));
            })?;
        }
        Ok(Outcome::Changed(finding))
    }
}

fn write_evidence(
    paths: &EvidencePaths,
    actual_png: &[u8],
    expected_png: Option<&[u8]>,
    diff: Option<&RgbaImage>,
    finding: &Finding,
) -> Result<(), GoldenError> {
    clear_evidence(paths)?;
    write_file(&paths.actual, actual_png)?;
    if let Some(expected) = expected_png {
        // The stored bytes, not a re-encode, so the saved "before" is exactly the golden that is being replaced.
        write_file(&paths.expected, expected)?;
    }
    if let Some(diff) = diff {
        save_png(diff, &paths.diff)?;
    }
    // Written last: a half-written evidence set has no json, so the summary ignores it.
    let json =
        serde_json::to_vec_pretty(finding).map_err(|source| GoldenError::Json { path: paths.json.clone(), source })?;
    write_file(&paths.json, &json)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), GoldenError> {
    ensure_parent(path)?;
    fs::write(path, bytes).map_err(io_err(path))
}

fn clear_evidence(paths: &EvidencePaths) -> Result<(), GoldenError> {
    for path in paths.all() {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_err(path)(e)),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use image::Rgba;
    use tempfile::TempDir;

    use super::*;

    fn flat(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(16, 16, Rgba(color))
    }

    /// A store in a temp folder: images in `store/`, manifests in `manifests/` (standing in for `tests/goldens`).
    fn store() -> (TempDir, Goldens) {
        let dir = tempfile::tempdir().unwrap();
        let goldens = Goldens::new(dir.path().join("store"), dir.path().join("manifests"), dir.path().join("out"));
        (dir, goldens)
    }

    fn id() -> GoldenId {
        GoldenId::new("render", "front-view").unwrap()
    }

    fn finding(outcome: Outcome) -> Finding {
        match outcome {
            Outcome::Changed(f) => f,
            Outcome::Passed(stats) => panic!("expected a finding, got a pass: {stats}"),
        }
    }

    fn entry_of(goldens: &Goldens, id: &GoldenId) -> Option<Entry> {
        Manifest::load(&goldens.manifest_path(id.suite())).unwrap().get(&id.name).copied()
    }

    /// The golden image a manifest currently names, read back through the object store.
    fn golden_image(goldens: &Goldens, id: &GoldenId) -> RgbaImage {
        let hash = entry_of(goldens, id).unwrap().hash;
        load_png(&goldens.objects().path(&hash)).unwrap()
    }

    #[test]
    fn ids_are_validated_slugs() {
        assert_eq!(GoldenId::parse("render/front-view").unwrap(), id());
        assert_eq!(id().to_string(), "render/front-view");
        for bad in ["", "render", "render/", "/x", "a/b/c", "../x", "render/../x", "Render/x", "a b/c"] {
            assert!(GoldenId::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_missing_golden_fails_and_leaves_the_actual_image_as_evidence() {
        let (_dir, goldens) = store();
        let img = flat([1, 2, 3, 255]);
        let outcome = goldens.check(&id(), &img, Tolerance::EXACT, Mode::Check).unwrap();
        assert!(!outcome.is_ok());
        let f = finding(outcome);
        assert_eq!((f.status, f.stats, f.expected_size, f.expected_hash), (Status::New, None, None, None));
        assert_eq!(f.actual_hash, ContentHash::of(&fs::read(goldens.evidence_paths(&id()).actual).unwrap()));
        assert!(!goldens.manifest_path("render").exists(), "a plain check never writes a manifest");
        assert!(!goldens.objects().dir().exists(), "nor an image");
    }

    #[test]
    fn blessing_a_new_image_creates_the_golden_and_the_next_check_passes() {
        let (_dir, goldens) = store();
        let img = flat([1, 2, 3, 255]);
        let outcome = goldens.check(&id(), &img, Tolerance::EXACT, Mode::Bless).unwrap();
        assert!(outcome.is_ok());
        let f = finding(outcome);
        assert_eq!((f.status, f.expected_hash), (Status::Blessed, None));
        assert_eq!(entry_of(&goldens, &id()).unwrap().hash, f.actual_hash, "the manifest names the new image");
        assert_eq!(golden_image(&goldens, &id()), img);

        let again = goldens.check(&id(), &img, Tolerance::EXACT, Mode::Check).unwrap();
        assert!(matches!(again, Outcome::Passed(_)));
        assert!(!goldens.evidence_paths(&id()).json.exists(), "a pass clears the old evidence");
    }

    #[test]
    fn a_mismatch_writes_expected_actual_diff_and_json_without_touching_the_golden() {
        let (_dir, goldens) = store();
        goldens.check(&id(), &flat([0, 0, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        let manifest_before = fs::read(goldens.manifest_path("render")).unwrap();
        let old_hash = entry_of(&goldens, &id()).unwrap().hash;
        let golden_bytes = fs::read(goldens.objects().path(&old_hash)).unwrap();

        let outcome = goldens.check(&id(), &flat([200, 0, 0, 255]), Tolerance::EXACT, Mode::Check).unwrap();
        assert!(!outcome.is_ok());
        let f = finding(outcome);
        assert_eq!((f.status, f.expected_hash), (Status::Failed, Some(old_hash)));
        assert_ne!(f.actual_hash, old_hash);
        assert_eq!(f.stats.unwrap().bad_pixels, 256);
        let paths = goldens.evidence_paths(&id());
        assert!(paths.all().iter().all(|p| p.exists()));
        assert_eq!(fs::read(goldens.manifest_path("render")).unwrap(), manifest_before);
        assert_eq!(fs::read(&paths.expected).unwrap(), golden_bytes);
        assert!(load_png(&paths.diff).unwrap().pixels().all(|p| p.0 == [255, 0, 0, 255]));
    }

    #[test]
    fn blessing_a_change_repoints_the_manifest_and_keeps_the_old_image() {
        let (_dir, goldens) = store();
        let (old, new) = (flat([0, 0, 0, 255]), flat([200, 0, 0, 255]));
        goldens.check(&id(), &old, Tolerance::EXACT, Mode::Bless).unwrap();
        let old_hash = entry_of(&goldens, &id()).unwrap().hash;

        let f = finding(goldens.check(&id(), &new, Tolerance::EXACT, Mode::Bless).unwrap());
        assert_eq!((f.status, f.expected_hash), (Status::Blessed, Some(old_hash)));
        assert!(f.stats.is_some());
        assert_eq!(golden_image(&goldens, &id()), new);
        assert_eq!(load_png(&goldens.evidence_paths(&id()).expected).unwrap(), old);
        assert_eq!(load_png(&goldens.objects().path(&old_hash)).unwrap(), old, "an older branch still finds its image");
        assert_eq!(fs::read_dir(goldens.objects().dir()).unwrap().count(), 2, "and no staging file is left behind");
    }

    #[test]
    fn a_bless_changes_exactly_one_entry_of_the_manifest() {
        let (_dir, goldens) = store();
        let other = GoldenId::new("render", "side-view").unwrap();
        goldens.check(&id(), &flat([0, 0, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        goldens.check(&other, &flat([9, 9, 9, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        let before = fs::read_to_string(goldens.manifest_path("render")).unwrap();

        goldens.check(&id(), &flat([200, 0, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        let after = fs::read_to_string(goldens.manifest_path("render")).unwrap();
        let changed: Vec<_> = before.lines().zip(after.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(before.lines().count(), after.lines().count());
        assert_eq!(changed.len(), 1, "only the hash line moves: {changed:?}");
        assert!(changed[0].0.starts_with("hash = ") && changed[0].1.starts_with("hash = "));
    }

    #[test]
    fn two_branches_bless_different_images_for_one_case_without_touching_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let (store, out) = (dir.path().join("store"), dir.path().join("out"));
        // Two checkouts of the repo: separate manifests, one shared image store.
        let branch_a = Goldens::new(&store, dir.path().join("branch-a/tests/goldens"), &out);
        let branch_b = Goldens::new(&store, dir.path().join("branch-b/tests/goldens"), &out);
        let (red, blue) = (flat([255, 0, 0, 255]), flat([0, 0, 255, 255]));

        branch_a.check(&id(), &red, Tolerance::EXACT, Mode::Bless).unwrap();
        branch_b.check(&id(), &blue, Tolerance::EXACT, Mode::Bless).unwrap();
        // B moves its golden again; A must not notice.
        branch_b.check(&id(), &flat([0, 255, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();

        assert!(branch_a.check(&id(), &red, Tolerance::EXACT, Mode::Check).unwrap().is_ok());
        assert!(!branch_a.check(&id(), &blue, Tolerance::EXACT, Mode::Check).unwrap().is_ok());
        assert!(branch_b.check(&id(), &flat([0, 255, 0, 255]), Tolerance::EXACT, Mode::Check).unwrap().is_ok());
        assert!(!branch_b.check(&id(), &red, Tolerance::EXACT, Mode::Check).unwrap().is_ok());
        assert_ne!(entry_of(&branch_a, &id()), entry_of(&branch_b, &id()));
    }

    #[test]
    fn noise_within_tolerance_passes_and_blessing_does_not_rewrite_the_manifest() {
        let (_dir, goldens) = store();
        let tolerance = Tolerance { per_channel: 3, max_bad_fraction: 0.0 };
        goldens.check(&id(), &flat([100, 100, 100, 255]), tolerance, Mode::Bless).unwrap();
        let before = fs::read(goldens.manifest_path("render")).unwrap();

        let outcome = goldens.check(&id(), &flat([102, 100, 99, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        assert!(matches!(outcome, Outcome::Passed(s) if s.max_delta == 2), "the manifest's tolerance applies");
        assert_eq!(fs::read(goldens.manifest_path("render")).unwrap(), before);
    }

    #[test]
    fn the_manifest_decides_the_tolerance_and_the_argument_only_names_it_for_a_new_case() {
        let (_dir, goldens) = store();
        let loose = Tolerance { per_channel: 5, max_bad_fraction: 0.25 };
        goldens.check(&id(), &flat([10, 10, 10, 255]), loose, Mode::Bless).unwrap();
        assert_eq!(entry_of(&goldens, &id()).unwrap().tolerance(), loose, "recorded for the reviewer to see");

        // A later caller asking for something stricter or looser changes nothing: the manifest wins.
        let strict = goldens.check(&id(), &flat([14, 10, 10, 255]), Tolerance::EXACT, Mode::Check).unwrap();
        assert!(matches!(strict, Outcome::Passed(_)));
        let looser = Tolerance { per_channel: 100, max_bad_fraction: 1.0 };
        let off = goldens.check(&id(), &flat([40, 10, 10, 255]), looser, Mode::Check).unwrap();
        assert_eq!(finding(off).status, Status::Failed);

        // Blessing a change keeps the tolerance the manifest already has.
        goldens.check(&id(), &flat([40, 10, 10, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        assert_eq!(entry_of(&goldens, &id()).unwrap().tolerance(), loose);
    }

    #[test]
    fn the_recorded_tolerance_is_what_the_manifest_says_or_none_for_a_new_case() {
        let (_dir, goldens) = store();
        assert_eq!(goldens.recorded_tolerance(&id()).unwrap(), None);
        let tolerance = Tolerance { per_channel: 4, max_bad_fraction: 0.5 };
        goldens.check(&id(), &flat([1; 4]), tolerance, Mode::Bless).unwrap();
        assert_eq!(goldens.recorded_tolerance(&id()).unwrap(), Some(tolerance));
    }

    #[test]
    fn a_size_change_is_a_finding_with_no_diff() {
        let (_dir, goldens) = store();
        goldens.check(&id(), &flat([0; 4]), Tolerance::EXACT, Mode::Bless).unwrap();
        let f = finding(goldens.check(&id(), &RgbaImage::new(8, 8), Tolerance::EXACT, Mode::Check).unwrap());
        assert_eq!(f.status, Status::SizeMismatch);
        assert_eq!((f.actual_size, f.expected_size), ([8, 8], Some([16, 16])));
        assert!(!goldens.evidence_paths(&id()).diff.exists());
    }

    #[test]
    fn a_bad_tolerance_is_an_error_even_when_blessing_a_new_golden() {
        let (_dir, goldens) = store();
        let bad = Tolerance { per_channel: 0, max_bad_fraction: f64::NAN };
        for mode in [Mode::Check, Mode::Bless] {
            let result = goldens.check(&id(), &flat([0; 4]), bad, mode);
            assert!(matches!(result, Err(GoldenError::Compare(CompareError::InvalidTolerance { .. }))), "{mode:?}");
        }
        assert!(!goldens.manifest_path("render").exists(), "nothing may be written for an invalid tolerance");
        assert!(!goldens.evidence_paths(&id()).json.exists());
    }

    #[test]
    fn a_golden_missing_from_the_store_or_damaged_is_an_error_not_a_new_case() {
        let (_dir, goldens) = store();
        goldens.check(&id(), &flat([0; 4]), Tolerance::EXACT, Mode::Bless).unwrap();
        let object = goldens.objects().path(&entry_of(&goldens, &id()).unwrap().hash);

        fs::write(&object, b"not a png").unwrap();
        let damaged = goldens.check(&id(), &flat([0; 4]), Tolerance::EXACT, Mode::Check);
        assert!(matches!(damaged, Err(GoldenError::ObjectCorrupt { .. })));

        fs::remove_file(&object).unwrap();
        let missing = goldens.check(&id(), &flat([0; 4]), Tolerance::EXACT, Mode::Check);
        assert!(matches!(missing, Err(GoldenError::ObjectMissing { .. })));
    }

    #[test]
    fn blessing_repairs_an_object_that_is_missing_or_damaged_as_the_error_advises() {
        let (_dir, goldens) = store();
        let tolerance = Tolerance { per_channel: 2, max_bad_fraction: 0.0 };
        let img = flat([10, 20, 30, 255]);
        goldens.check(&id(), &img, tolerance, Mode::Bless).unwrap();
        let manifest = fs::read(goldens.manifest_path("render")).unwrap();
        let object = goldens.objects().path(&entry_of(&goldens, &id()).unwrap().hash);

        // Missing, then a zero-length file (what a crash between rename and flush used to leave), then garbage:
        // each time a plain check refuses, and blessing the same image restores the object with the manifest untouched.
        for damage in [None, Some(&b""[..]), Some(&b"garbage"[..])] {
            match damage {
                None => fs::remove_file(&object).unwrap(),
                Some(bytes) => fs::write(&object, bytes).unwrap(),
            }
            assert!(goldens.check(&id(), &img, Tolerance::EXACT, Mode::Check).is_err());

            let f = finding(goldens.check(&id(), &img, Tolerance::EXACT, Mode::Bless).unwrap());
            assert_eq!(f.status, Status::Blessed);
            assert_eq!(f.expected_hash, Some(f.actual_hash), "the manifest already named this image");
            assert_eq!((f.stats, f.expected_size), (None, None), "there was nothing usable to compare with");
            assert!(!goldens.evidence_paths(&id()).expected.exists());
            assert_eq!(fs::read(goldens.manifest_path("render")).unwrap(), manifest, "same image, same entry");
            assert!(matches!(goldens.check(&id(), &img, Tolerance::EXACT, Mode::Check).unwrap(), Outcome::Passed(_)));
        }
    }

    #[test]
    fn blessing_a_different_image_over_a_lost_object_repoints_the_entry_and_keeps_its_tolerance() {
        let (_dir, goldens) = store();
        let tolerance = Tolerance { per_channel: 2, max_bad_fraction: 0.01 };
        goldens.check(&id(), &flat([0, 0, 0, 255]), tolerance, Mode::Bless).unwrap();
        let old = entry_of(&goldens, &id()).unwrap();
        fs::remove_file(goldens.objects().path(&old.hash)).unwrap();

        let new = flat([200, 0, 0, 255]);
        let f = finding(goldens.check(&id(), &new, Tolerance::EXACT, Mode::Bless).unwrap());
        assert_eq!((f.status, f.expected_hash), (Status::Blessed, Some(old.hash)));
        let entry = entry_of(&goldens, &id()).unwrap();
        assert_eq!((entry.hash, entry.tolerance()), (f.actual_hash, tolerance));
        assert_eq!(golden_image(&goldens, &id()), new);
    }

    #[test]
    fn a_stored_image_that_is_not_a_png_is_an_image_error() {
        let (_dir, goldens) = store();
        let hash = goldens.objects().put(b"not a png").unwrap();
        Manifest::update(&goldens.manifest_path("render"), "render", |m| {
            m.insert(id().name.clone(), Entry::new(hash, Tolerance::EXACT));
        })
        .unwrap();
        let result = goldens.check(&id(), &flat([0; 4]), Tolerance::EXACT, Mode::Check);
        assert!(matches!(result, Err(GoldenError::Image { .. })));
    }

    #[test]
    fn many_cases_blessed_at_once_all_reach_the_manifest() {
        let (_dir, goldens) = store();
        std::thread::scope(|scope| {
            for i in 0..12u8 {
                let goldens = &goldens;
                scope.spawn(move || {
                    let id = GoldenId::new("render", &format!("case-{i:02}")).unwrap();
                    // Half the cases share one image, so concurrent writers also meet on the same object.
                    let shade = if i % 2 == 0 { 0 } else { i };
                    goldens.check(&id, &flat([shade, 0, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
                });
            }
        });
        for i in 0..12u8 {
            let id = GoldenId::new("render", &format!("case-{i:02}")).unwrap();
            assert!(entry_of(&goldens, &id).is_some(), "case {i} lost to a race");
        }
        assert_eq!(fs::read_dir(goldens.objects().dir()).unwrap().count(), 7, "shared images are stored once");
    }

    #[test]
    fn mode_blesses_only_on_exactly_one() {
        assert_eq!(Mode::parse(Some("1")), Mode::Bless);
        for other in [None, Some("0"), Some(""), Some("true")] {
            assert_eq!(Mode::parse(other), Mode::Check, "{other:?}");
        }
    }
}
