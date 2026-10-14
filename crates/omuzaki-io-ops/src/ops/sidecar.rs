//! The sidecar resolver: the files a glTF refers to by a relative URI (`a.bin`, `tex/a.png`), read from the folder of
//! the importing file and nowhere else.
//!
//! **Threat model (ADR 0013).** File grants confine ops; they are not a sandbox. The importer is an op, so every read
//! here goes through [`OpCtx::fs_access`] and the [`CanonicalPath`] it returns, which refuse a link at every step. On
//! top of that this resolver adds the rule the importer needs: a file a model names is read only if it really lies in
//! the folder of the file the person chose. A `.gltf` can name any path it likes, so the URI is refused on its form
//! first, the join is made on the text the person typed, and the result is confined by where it really is.
//!
//! The steps for each read: refuse a URI that is not a plain relative path ([`clean_relative`]); join it onto the typed
//! folder; ask `fs_access` to read it (the grants and roots apply, and a missing file is `NotFound`); compare its real
//! place with the importing file's real folder by path components and refuse unless it lies inside (a link in the
//! folder to a file elsewhere in the project fails here, sub-folders pass, and the order of the grants changes nothing);
//! then open it through the checked path and read at most `max_bytes`.
//!
//! Every error is an [`IoError::Resource`] for the URI. Refusals of the form or of the place say `refused: `; a
//! `Denied` from `fs_access` keeps its own text, which names the root to grant.

use std::io::Read;
use std::path::{Path, PathBuf};

use omuzaki_io_api::{IoError, Resolver};
use omuzaki_ops_api::{CanonicalPath, FsMode, OpCtx};

/// Reads a glTF's external files from the folder of the importing file, within the request's file access.
///
/// `main` may have been checked for either mode: confinement compares the real place of each folder, which does not
/// depend on the mode the path was approved for.
pub struct DirResolver<'c, 'o> {
    ctx: &'c OpCtx<'o>,
    /// The folder as the person typed it (relative to the project folder, or empty for the project folder itself).
    /// Sidecar paths are joined onto this text, never onto the canonical folder, so the check runs on the same path
    /// the person named.
    folder_text: String,
    /// The importing file's real folder, as its real place's text. Every sidecar's real place must start with it.
    folder: PathBuf,
    /// The most bytes one sidecar may hold.
    max_bytes: u64,
}

impl<'c, 'o> DirResolver<'c, 'o> {
    /// A resolver for the sidecars of `main`, which the person typed as `typed_path`.
    pub fn new(ctx: &'c OpCtx<'o>, main: &CanonicalPath, typed_path: &str, max_bytes: u64) -> Self {
        let folder_text = Path::new(typed_path).parent().and_then(Path::to_str).unwrap_or("").to_owned();
        // The folder comes from the real place's text, not from `CanonicalPath::parent`: that stops at the allowed folder
        // `fs_access` chose, which with overlapping grants can be the file itself, and the answer would then depend on
        // grant order. A file always has a parent; if one ever did not, the file itself is the fallback, so no
        // sidecar lies inside it and every read is refused rather than widened.
        let real = PathBuf::from(main.to_string());
        let folder = real.parent().map_or_else(|| real.clone(), Path::to_path_buf);
        Self { ctx, folder_text, folder, max_bytes }
    }

    /// Whether `sidecar` really lies in the importing file's folder, by path components of the two real places.
    ///
    /// Both are real places from `fs_access` (no link, no `..`), so a prefix of components is a containment: a link in
    /// the folder to a file elsewhere already resolves to where that file really is, and that place is outside.
    ///
    /// This deliberately does not walk up with [`CanonicalPath::parent`]. That walk stops at the allowed folder
    /// `fs_access` chose, which is the first covering grant in grant order; with overlapping grants it can be an inner
    /// root, and the walk would miss the importing folder. Nothing here depends on the order of the grants.
    fn is_confined(&self, sidecar: &CanonicalPath) -> bool {
        Path::new(&sidecar.to_string()).starts_with(&self.folder)
    }
}

impl Resolver for DirResolver<'_, '_> {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        let relative = clean_relative(uri).map_err(|why| refused(uri, &why))?;
        let joined = Path::new(&self.folder_text).join(relative);
        let joined = joined.to_str().ok_or_else(|| refused(uri, "the path is not text this machine can name"))?;

        let sidecar = self.ctx.fs_access(joined, FsMode::Read).map_err(|err| resource(uri, err.message))?;
        if !self.is_confined(&sidecar) {
            return Err(refused(uri, "the file is not in the folder of the importing file"));
        }

        let file = sidecar.open_read().map_err(|err| resource(uri, err.message))?;
        // One byte past the limit is enough to tell an over-long file from one that fits, and `take` stops the read
        // there, so an over-long file never fills memory.
        let mut bytes = Vec::new();
        file.take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|err| resource(uri, err.to_string()))?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(refused(uri, &format!("the file is over the {} byte limit", self.max_bytes)));
        }
        Ok(bytes)
    }
}

/// Checks that `uri` is a plain relative path: `name` or `folder/name`, with no way to climb, no drive, no backslash
/// (which some systems read as a separator), no NUL, and no empty segment. Returns the same text when it passes.
///
/// The importer has already percent-decoded the URI and refused schemes and `//` (`omuzaki-gltf`'s `uri.rs`). The
/// messages carry no prefix; [`DirResolver::read`] adds `refused: `.
///
/// A colon is refused anywhere, not only as a drive prefix. In a glTF URI a colon means a scheme (`data:`, `http:`) or
/// a Windows drive, and a sidecar is always a plain relative path, so the stricter rule is the safer contract. (On
/// Windows `name:stream` is also a stream of `name`, not a file.)
pub(crate) fn clean_relative(uri: &str) -> Result<&str, String> {
    if uri.is_empty() {
        return Err("the path is empty".to_owned());
    }
    if uri.contains('\0') {
        return Err("the path has a NUL byte".to_owned());
    }
    if uri.contains('\\') {
        return Err("the path has a backslash; use `/`".to_owned());
    }
    if uri.starts_with('/') {
        return Err("the path is absolute".to_owned());
    }
    if uri.contains(':') {
        return Err("the path has a colon (a drive or a stream name)".to_owned());
    }
    if uri.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err("the path has an empty, `.` or `..` part".to_owned());
    }
    Ok(uri)
}

/// A refusal of the form or the place of `uri`.
fn refused(uri: &str, why: &str) -> IoError {
    resource(uri, format!("refused: {why}"))
}

/// An error for `uri` with `reason`.
fn resource(uri: &str, reason: impl Into<String>) -> IoError {
    IoError::Resource { uri: uri.to_owned(), reason: reason.into() }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use omuzaki_core::time::{FakeClock, Seconds};
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{Actor, FsMode, Grants, OpCtx, ProjectDir, Scope};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    /// A project folder holding a glTF in `sub/`, a file one level up from it, and a folder outside the project.
    struct Fixture {
        _dir: TempDir,
        project: PathBuf,
        outside: PathBuf,
        doc: MemDoc,
        actor: Actor,
        clock: FakeClock,
        grants: Grants,
        project_dir: ProjectDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            // Canonical, so the paths the checks return are the paths the tests name.
            let base = fs::canonicalize(dir.path()).unwrap();
            let (project, outside) = (base.join("project"), base.join("outside"));
            fs::create_dir_all(project.join("sub/tex")).unwrap();
            fs::create_dir_all(&outside).unwrap();
            fs::write(project.join("sub/main.gltf"), "{}").unwrap();
            fs::write(project.join("sub/a.bin"), "A").unwrap();
            fs::write(project.join("sub/tex/a.png"), "P").unwrap();
            fs::write(project.join("up.bin"), "U").unwrap();
            fs::write(outside.join("secret.bin"), "S").unwrap();
            let project_dir = ProjectDir::open(&project).unwrap();
            Self {
                _dir: dir,
                project,
                outside,
                doc: MemDoc::from_value(json!({})),
                actor: Actor::Person { name: "tester".into() },
                clock: FakeClock::new(Seconds(0.0)),
                grants: Grants::of([Scope::Read]),
                project_dir,
            }
        }

        /// A context that reads inside the project folder, with the request's read grant.
        fn ctx(&self) -> OpCtx<'_> {
            OpCtx::new(&self.doc, &self.actor, &self.clock)
                .with_project_dir(&self.project_dir)
                .with_grants(&self.grants)
        }

        /// Reads `uri` from a sidecar of the glTF at `typed`, with a `max_bytes` limit.
        fn read(&self, typed: &str, uri: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
            let ctx = self.ctx();
            let main = ctx.fs_access(typed, FsMode::Read).unwrap();
            DirResolver::new(&ctx, &main, typed, max_bytes).read(uri)
        }
    }

    /// The reason of a [`IoError::Resource`], for asserting on.
    fn reason(err: IoError) -> String {
        match err {
            IoError::Resource { reason, .. } => reason,
            other => panic!("expected a resource error, got {other:?}"),
        }
    }

    const LIMIT: u64 = 1 << 20;

    #[test]
    fn clean_relative_accepts_plain_relative_paths() {
        assert_eq!(clean_relative("a.bin"), Ok("a.bin"));
        assert_eq!(clean_relative("tex/a.png"), Ok("tex/a.png"));
    }

    #[test]
    fn clean_relative_refuses_every_other_form() {
        for uri in ["", "..", "a/../b", "./a", "a//b", "/etc/x", "C:x", "a\\b", "a\0b", "a/"] {
            assert!(clean_relative(uri).is_err(), "{uri:?} should be refused");
        }
    }

    #[test]
    fn clean_relative_refuses_a_colon_in_any_position() {
        // A colon is a scheme or a drive in a glTF URI; a sidecar is never either.
        for uri in ["a:b.bin", "C:x.bin", "sub/a:b.bin"] {
            assert!(clean_relative(uri).is_err(), "{uri:?} should be refused");
        }
    }

    #[test]
    fn reads_when_the_importing_file_was_checked_for_writing() {
        let fx = Fixture::new();
        // Writing needs the `fs` scope and a root that covers the path; the project folder is that root here.
        let grants = Grants::of([Scope::Read]).with_fs_root(fx.project.to_str().unwrap());
        let ctx = OpCtx::new(&fx.doc, &fx.actor, &fx.clock).with_project_dir(&fx.project_dir).with_grants(&grants);
        let main = ctx.fs_access("sub/main.gltf", FsMode::Write).unwrap();
        let bytes = DirResolver::new(&ctx, &main, "sub/main.gltf", LIMIT).read("a.bin").unwrap();
        assert_eq!(bytes, b"A");
    }

    /// Reads `uri` beside `main` (an absolute path), with `roots` granted in the order given, as an import would.
    fn read_with_roots(main: &Path, roots: &[&Path], uri: &str) -> Result<Vec<u8>, IoError> {
        let mut grants = Grants::of([Scope::Read]);
        for root in roots {
            grants = grants.with_fs_root(root.to_str().unwrap());
        }
        let doc = MemDoc::from_value(json!({}));
        let actor = Actor::Person { name: "tester".into() };
        let clock = FakeClock::new(Seconds(0.0));
        let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
        let typed = main.to_str().unwrap();
        let main = ctx.fs_access(typed, FsMode::Read).unwrap();
        DirResolver::new(&ctx, &main, typed, LIMIT).read(uri)
    }

    #[test]
    fn overlapping_roots_give_the_same_result_in_either_order() {
        // The importing file is in `sub`, and `sub/tex` is a granted root inside the granted root `sub`.
        let fx = Fixture::new();
        let (assets, tex) = (fx.project.join("sub"), fx.project.join("sub/tex"));
        let main = fx.project.join("sub/main.gltf");
        assert_eq!(read_with_roots(&main, &[&tex, &assets], "tex/a.png").unwrap(), b"P");
        assert_eq!(read_with_roots(&main, &[&assets, &tex], "tex/a.png").unwrap(), b"P");
    }

    #[test]
    fn a_sidecar_under_the_deeper_of_two_nested_roots_reads_in_either_order() {
        let fx = Fixture::new();
        fs::create_dir_all(fx.project.join("sub/tex/deep")).unwrap();
        fs::write(fx.project.join("sub/tex/deep/c.bin"), "C").unwrap();
        fs::write(fx.project.join("sub/tex/main.gltf"), "{}").unwrap();
        let (assets, tex) = (fx.project.join("sub"), fx.project.join("sub/tex"));
        let main = fx.project.join("sub/tex/main.gltf");
        assert_eq!(read_with_roots(&main, &[&assets, &tex], "deep/c.bin").unwrap(), b"C");
        assert_eq!(read_with_roots(&main, &[&tex, &assets], "deep/c.bin").unwrap(), b"C");
    }

    #[test]
    fn a_grant_on_the_importing_file_alone_refuses_its_sidecars() {
        // A file-only grant covers the glTF and nothing beside it, so no sidecar can be read.
        let fx = Fixture::new();
        let main = fx.project.join("sub/main.gltf");
        assert!(read_with_roots(&main, &[&main], "a.bin").is_err());
    }

    #[test]
    fn a_root_on_the_importing_file_does_not_change_the_result() {
        // The glTF is itself a granted root, so its own real place can be its allowed folder: the folder must still be
        // the glTF's folder, whichever order the grants come in.
        let fx = Fixture::new();
        let (main, assets) = (fx.project.join("sub/main.gltf"), fx.project.join("sub"));
        assert_eq!(read_with_roots(&main, &[&main, &assets], "a.bin").unwrap(), b"A");
        assert_eq!(read_with_roots(&main, &[&assets, &main], "a.bin").unwrap(), b"A");
    }

    #[test]
    fn reads_a_sibling_and_a_nested_file() {
        let fx = Fixture::new();
        assert_eq!(fx.read("sub/main.gltf", "a.bin", LIMIT).unwrap(), b"A");
        assert_eq!(fx.read("sub/main.gltf", "tex/a.png", LIMIT).unwrap(), b"P");
    }

    #[test]
    fn refuses_a_file_one_level_up() {
        let fx = Fixture::new();
        let err = fx.read("sub/main.gltf", "../up.bin", LIMIT).unwrap_err();
        assert!(reason(err).starts_with("refused: "));
    }

    #[test]
    fn refuses_a_missing_file_without_the_refused_prefix() {
        let fx = Fixture::new();
        let why = reason(fx.read("sub/main.gltf", "missing.bin", LIMIT).unwrap_err());
        assert!(!why.starts_with("refused: "), "a missing file is not a refusal: {why}");
    }

    #[test]
    fn refuses_a_file_over_the_byte_limit() {
        let fx = Fixture::new();
        fs::write(fx.project.join("sub/big.bin"), [0_u8; 100]).unwrap();
        let why = reason(fx.read("sub/main.gltf", "big.bin", 99).unwrap_err());
        assert!(why.starts_with("refused: ") && why.contains("99 byte limit"), "{why}");
        assert_eq!(fx.read("sub/main.gltf", "big.bin", 100).unwrap().len(), 100);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_link_in_the_folder_to_another_file_in_the_project() {
        let fx = Fixture::new();
        std::os::unix::fs::symlink(fx.project.join("up.bin"), fx.project.join("sub/link.bin")).unwrap();
        let why = reason(fx.read("sub/main.gltf", "link.bin", LIMIT).unwrap_err());
        assert!(why.starts_with("refused: "), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_every_root_is_denied_with_fs_access_own_text() {
        let fx = Fixture::new();
        std::os::unix::fs::symlink(fx.outside.join("secret.bin"), fx.project.join("sub/out.bin")).unwrap();
        let expected = fx.ctx().fs_access("sub/out.bin", FsMode::Read).unwrap_err().message;
        let why = reason(fx.read("sub/main.gltf", "out.bin", LIMIT).unwrap_err());
        assert_eq!(why, expected);
        assert!(!why.starts_with("refused: "));
    }
}
