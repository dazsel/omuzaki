//! File access for ops: one place that answers "may this actor read or write this path?", and opens it.
//!
//! An op that touches a file takes the path as a string argument and asks [`OpCtx::fs_access`](crate::OpCtx::fs_access).
//! The answer comes from the request's [`Grants`] and the [`ProjectDir`], and it is the same for a dry-run and for
//! the real run. The rules, all of which fail closed:
//!
//! - **The decision comes first, and needs no disk.** The path is read in this machine's grammar, `..` is collapsed
//!   by name, and the result is compared with the project folder and the granted roots. A path that is not inside
//!   either is refused without a single question to the file system (or the network: on Windows a UNC path or a
//!   `WebDAV` name sent to the OS reaches the server, with the user's credentials, before any answer comes back).
//! - **Inside the project folder** (a root of its own, ADR 0013) reading needs only [`Scope::Read`] and writing needs
//!   [`Scope::Fs`]. A relative path is relative to the project folder (never to the process's current directory),
//!   and without a project folder it is refused. A path under the name the folder was opened by means the folder
//!   that was opened.
//! - **Everywhere else it needs [`Scope::Fs`] and a root that covers the path** ([`Grants::with_fs_root`]; never a
//!   whole disk, ADR 0013).
//! - **Then the path is walked on the disk, one name at a time, and compared again.** A symbolic link (or a Windows
//!   junction) is followed only while every place it leads through is inside, or on the way to, the project folder or
//!   a root this request may use; a link out of them is refused from its own text, and its target is never looked up
//!   (see [`resolve`]). The real place it ends at must lie inside the project folder or a root.
//! - **Ambiguous text is refused, not guessed**: Windows forms on a POSIX host and the other way round, drive- and
//!   root-relative paths, device paths and names (see [`form`]).
//!
//! The answer is a [`CanonicalPath`], which an op opens with its own methods and never by its text: they open the
//! allowed folder it lies in, then every folder below it, refusing a link at every step, so a file or folder swapped
//! for a link after the check cannot redirect the open (see [`open`] for what that does and does not cover).

mod form;
#[cfg(any(test, feature = "fuzzing"))]
pub mod fuzzing;
mod open;
mod project;
mod resolve;

use std::fmt;

use crate::{Grants, OpError, Scope};

use form::{Lexical, Parsed, Refusal};
pub(crate) use form::{PathKey, Style};
pub use open::{DirEntryInfo, EntryKind, Replacement, WriteMode};
pub use project::{ProjectDir, ProjectDirError};
use resolve::{Disk, OsDisk};

/// What an op wants to do with a path.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FsMode {
    /// Read the file, or list the folder.
    Read,
    /// Create, replace or delete the file.
    Write,
}

impl FsMode {
    const fn verb(self) -> &'static str {
        match self {
            Self::Read => "reading",
            Self::Write => "writing",
        }
    }
}

/// A path that [`OpCtx::fs_access`](crate::OpCtx::fs_access) approved for one [`FsMode`]: its real place, with no
/// link and no `..` in it, and the allowed folder (the project folder or a granted root) it lies in. Only `fs_access`
/// makes one, so holding one is proof that the check ran.
///
/// Open it with its methods ([`open_read`](Self::open_read), [`open_write`](Self::open_write),
/// [`replace`](Self::replace), [`create_dir_all`](Self::create_dir_all)), never by its text: they start from the
/// allowed folder and refuse a symbolic link or a junction at every step, so a file or folder swapped for a link
/// between the check and the open makes the open fail with `Denied` and can never redirect it. It shows as its path
/// ([`Display`](fmt::Display)), for messages; it deliberately has no `&Path` to hand to `std::fs`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalPath {
    /// The real place, as the check found it.
    place: Lexical,
    /// How many of `place`'s names are the allowed folder it lies in: opens start there.
    anchor: usize,
    /// What it was approved for.
    mode: FsMode,
}

impl fmt::Display for CanonicalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.place.path(usize::MAX).display().fmt(f)
    }
}

/// The check behind [`OpCtx::fs_access`](crate::OpCtx::fs_access), on this machine's file system and grammar.
pub(crate) fn check(
    grants: &Grants,
    project: Option<&ProjectDir>,
    raw: &str,
    mode: FsMode,
) -> Result<CanonicalPath, OpError> {
    check_in(Style::HOST, &OsDisk, grants, project, raw, mode)
}

/// [`check`] over an explicit grammar and disk, so tests can run the Windows rules on Linux and watch every question
/// the check asks the disk.
fn check_in(
    style: Style,
    disk: &dyn Disk,
    grants: &Grants,
    project: Option<&ProjectDir>,
    raw: &str,
    mode: FsMode,
) -> Result<CanonicalPath, OpError> {
    let malformed = |refusal: Refusal| {
        OpError::invalid("path", format!("`{raw}` is not accepted: {}", refusal.reason())).hint(refusal.hint())
    };
    let refuse = || refused(grants, raw, mode);
    // The project folder is a root for the actor's own modes (ADR 0013): `read` opens files in it; writing there is
    // the `fs` scope with the project as the root. Relative paths start there either way.
    let in_project = grants.allows(match mode {
        FsMode::Read => Scope::Read,
        FsMode::Write => Scope::Fs,
    });
    let reach = Reach::new(grants, project.filter(|_| in_project), style);

    // 1. Decide by name. Nothing below this block runs unless the path is inside the project folder or a root, so a
    //    refused path costs no file-system or network access at all.
    let place = Parsed::parse(raw, style).and_then(|parsed| parsed.lexical(project.map(ProjectDir::base)));
    let place = place.map_err(malformed)?;
    if !reach.covers(&place.key(style)) {
        return Err(refuse());
    }
    let place = match reach.project {
        Some(dir) => dir.rebase(&place, style),
        None => place,
    };

    // 2. Walk it, never asking about a place outside what this request may reach, then decide again on where it
    //    really is. Every failure looks like a plain refusal.
    let resolved = resolve::resolve(disk, &|key| reach.may_visit(key), &place, style).map_err(|_| refuse())?;
    let anchor = reach.anchor(&resolved.place.key(style)).ok_or_else(refuse)?;
    if mode == FsMode::Read && !resolved.exists {
        return Err(OpError::not_found(format!("`{raw}` does not exist")));
    }
    Ok(CanonicalPath { place: resolved.place, anchor, mode })
}

/// The places one request may reach: the project folder when the mode lets the actor in there, and the granted roots
/// that count.
struct Reach<'a> {
    project: Option<&'a ProjectDir>,
    roots: Vec<PathKey>,
}

impl<'a> Reach<'a> {
    fn new(grants: &Grants, project: Option<&'a ProjectDir>, style: Style) -> Self {
        Self { project, roots: grants.roots(style).collect() }
    }

    /// Whether `place` is inside the project folder (by either of its names) or a root.
    fn covers(&self, place: &PathKey) -> bool {
        self.project.is_some_and(|dir| dir.covers_request(place)) || self.roots.iter().any(|root| root.covers(place))
    }

    /// Whether the walk may ask the disk about `place`: it is inside a place this request may reach, or a folder on
    /// the way to one.
    fn may_visit(&self, place: &PathKey) -> bool {
        self.covers(place)
            || self.project.is_some_and(|dir| dir.lies_in(place))
            || self.roots.iter().any(|root| place.covers(root))
    }

    /// The allowed folder a real place lies in, as its number of names: the project's real folder, else the first
    /// root that covers it.
    fn anchor(&self, real: &PathKey) -> Option<usize> {
        match self.project {
            Some(dir) if dir.covers_resolved(real) => Some(dir.base().depth()),
            _ => self.roots.iter().find(|root| root.covers(real)).map(PathKey::depth),
        }
    }
}

/// The refusal: names the path and what to grant. Its text depends only on the grants and the mode, never on the
/// disk, so it is the same for every reason a path is refused (see [`check_in`]).
fn refused(grants: &Grants, raw: &str, mode: FsMode) -> OpError {
    let missing = if grants.allows(Scope::Fs) { "no granted file root covers it" } else { "it needs the `fs` scope" };
    let rule = match mode {
        FsMode::Read => "a file inside the project folder needs only `read`; any other path needs `fs` and a root",
        FsMode::Write => "writing needs `fs`, and the file inside the project folder or under a granted root",
    };
    OpError::denied(format!("{} `{raw}` is refused: {missing}", mode.verb()))
        .hint(format!("ask the user to grant the `fs` scope with a root that covers `{raw}` ({rule})"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::resolve::Entry;
    use super::*;
    use crate::ErrorCode;

    /// A disk that answers from a table and writes down every question it is asked.
    #[derive(Default)]
    struct FakeDisk {
        /// What is at each path; anything not listed is missing.
        entries: BTreeMap<String, Entry>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeDisk {
        fn folders<'a>(mut self, paths: impl IntoIterator<Item = &'a str>) -> Self {
            for path in paths {
                self.entries.insert(path.to_owned(), Entry::Folder);
            }
            self
        }

        fn file(mut self, path: &str) -> Self {
            self.entries.insert(path.to_owned(), Entry::File);
            self
        }

        fn link(mut self, path: &str, target: &str) -> Self {
            self.entries.insert(path.to_owned(), Entry::Link(target.to_owned()));
            self
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    impl Disk for FakeDisk {
        fn entry(&self, path: &Path) -> io::Result<Entry> {
            let text = path.to_str().unwrap().to_owned();
            self.asked.borrow_mut().push(text.clone());
            Ok(self.entries.get(&text).cloned().unwrap_or(Entry::Missing))
        }
    }

    fn posix_project() -> ProjectDir {
        ProjectDir::from_paths(PathBuf::from("/data/proj"), None, Style::Posix).unwrap()
    }

    fn windows_project() -> ProjectDir {
        ProjectDir::from_paths(PathBuf::from(r"\\?\C:\Proj"), Some(r"C:\Proj"), Style::Windows).unwrap()
    }

    /// The disk under [`posix_project`]: its folders, one file, and the links each test adds.
    fn posix_disk() -> FakeDisk {
        FakeDisk::default().folders(["/data", "/data/proj", "/data/proj/sub"]).file("/data/proj/ok.txt")
    }

    fn run(
        style: Style,
        disk: &FakeDisk,
        grants: &Grants,
        project: &ProjectDir,
        raw: &str,
        mode: FsMode,
    ) -> Result<CanonicalPath, OpError> {
        check_in(style, disk, grants, Some(project), raw, mode)
    }

    fn shown(path: &CanonicalPath) -> String {
        path.to_string()
    }

    #[test]
    fn a_path_outside_the_project_and_the_roots_asks_the_disk_nothing() {
        let (project, disk) = (posix_project(), FakeDisk::default());
        let agent = Grants::agent_default().with_fs_root("/srv/art");
        for raw in [
            "/etc/passwd",
            "/data/proj2/x",
            "/data/pro",
            "/data",
            "/srv/art2/x",
            "/srv",
            "../x",
            "../../etc/passwd",
            "sub/../../x",
            "/data/proj/../secret",
            "/data/proj/sub/../../secret",
            "/proc/self/root/etc/passwd",
            "/data/proj/../../etc/shadow",
        ] {
            let e = run(Style::Posix, &disk, &agent, &project, raw, FsMode::Read).unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied, "{raw}");
        }
        // Without `read` the project folder is just another place; with `read` but no `fs`, writes are refused first.
        let no_read = Grants::of([Scope::EditScene]);
        assert_eq!(
            run(Style::Posix, &disk, &no_read, &project, "a.vrm", FsMode::Read).unwrap_err().code,
            ErrorCode::Denied
        );
        let no_fs = Grants::agent_default();
        assert_eq!(
            run(Style::Posix, &disk, &no_fs, &project, "out.glb", FsMode::Write).unwrap_err().code,
            ErrorCode::Denied
        );
        assert_eq!(disk.asked(), Vec::<String>::new(), "a refused path must not reach the disk");
    }

    #[test]
    fn a_refused_path_on_windows_never_reaches_the_resolver_or_the_network() {
        let (project, disk) = (windows_project(), FakeDisk::default());
        let agent = Grants::agent_default().with_fs_root(r"D:\Art");
        for raw in [
            r"\\attacker\share\x",
            "//attacker/share/x",
            r"\\host@80\DavWWWRoot\x",
            r"\\host@SSL@443\DavWWWRoot\x",
            r"\\?\UNC\attacker\share\x",
            r"\\localhost\c$\Windows\win.ini",
            r"Z:\mapped\x",
            r"\\?\Z:\x",
            r"A:\floppy",
            r"C:\Proj2\x",
            r"C:\Proj\..\Other\x",
            r"c:\proj\..\..\x",
            r"..\Other\x",
            r"sub\..\..\x",
            r"D:\Art\..\Secret\x",
            r"D:\Artwork\x",
        ] {
            for mode in [FsMode::Read, FsMode::Write] {
                let e = run(Style::Windows, &disk, &agent, &project, raw, mode).unwrap_err();
                assert!(matches!(e.code, ErrorCode::Denied | ErrorCode::Invalid), "{raw}: {e:?}");
            }
        }
        assert_eq!(disk.asked(), Vec::<String>::new(), "a refused path must not reach the disk");
    }

    #[test]
    fn forms_that_cannot_be_compared_are_invalid_before_any_gate() {
        let (project, disk) = (windows_project(), FakeDisk::default());
        let everything = Grants::person().with_fs_root(r"C:\").with_fs_root(r"\\NAS\Models");
        for raw in [
            r"\\.\COM1",
            r"\\.\C:\x",
            r"\\.\pipe\x",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"C:x",
            r"\x",
            "/x",
            r"CON",
            r"sub\NUL.txt",
            r"x.txt::$DATA",
            r"x.txt:stream",
            "x.txt.",
            r"\\server",
            r"\\server\",
            r"\\?\C:\a\..\b",
            "",
        ] {
            let e = run(Style::Windows, &disk, &everything, &project, raw, FsMode::Read).unwrap_err();
            assert_eq!(e.code, ErrorCode::Invalid, "{raw}");
        }
        assert_eq!(disk.asked(), Vec::<String>::new());
    }

    #[test]
    fn only_a_path_a_root_covers_reaches_the_disk_and_only_as_that_path() {
        let project = windows_project();
        let grants = Grants::agent_default().with_fs_root(r"\\NAS\Models").with_fs_root(r"\\host@80\DavWWWRoot");
        let disk = FakeDisk::default().file(r"\\nas\models\a.glb");
        // The user granted these shares by name, so the OS may be asked about them (and nothing else).
        let got = run(Style::Windows, &disk, &grants, &project, r"\\NAS\Models\a.glb", FsMode::Read).unwrap();
        assert_eq!(shown(&got), r"\\nas\models\a.glb");
        assert!(run(Style::Windows, &disk, &grants, &project, r"//host@80/DavWWWRoot/x", FsMode::Write).is_ok());
        assert_eq!(disk.asked(), [r"\\nas\models\a.glb", r"\\host@80\davwwwroot\x"]);

        // A neighbouring share is not covered, so the disk hears nothing about it.
        let before = disk.asked().len();
        assert!(run(Style::Windows, &disk, &grants, &project, r"\\NAS\Other\a.glb", FsMode::Read).is_err());
        assert_eq!(disk.asked().len(), before);
    }

    #[test]
    fn a_link_out_of_every_allowed_place_is_refused_without_looking_at_its_target() {
        let project = posix_project();
        let disk = posix_disk()
            .link("/data/proj/link", "/etc")
            .link("/data/proj/evil.txt", "../../etc/passwd")
            .link("/data/proj/sub/up", "../../other");
        let agent = Grants::agent_default();
        for raw in ["link/secret.txt", "link", "evil.txt", "sub/up/x"] {
            for mode in [FsMode::Read, FsMode::Write] {
                let e = run(Style::Posix, &disk, &agent.clone().with(Scope::Fs), &project, raw, mode).unwrap_err();
                assert_eq!(e.code, ErrorCode::Denied, "{raw}");
            }
        }
        assert!(run(Style::Posix, &disk, &agent, &project, "ok.txt", FsMode::Read).is_ok());
        let questions = disk.asked();
        assert!(questions.iter().all(|q| q.starts_with("/data/proj") || q == "/data"), "{questions:?}");
        assert!(!questions.iter().any(|q| q.starts_with("/etc") || q.starts_with("/data/other")), "{questions:?}");

        // On Windows, a junction in the project to a share nobody granted never reaches the network.
        let project = windows_project();
        let disk = FakeDisk::default().folders([r"C:\Proj"]).link(r"C:\Proj\share", r"\\attacker\share\loot");
        let e = run(Style::Windows, &disk, &Grants::person(), &project, r"share\x.glb", FsMode::Read).unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied);
        assert_eq!(disk.asked(), [r"C:\Proj", r"C:\Proj\share"]);
    }

    #[test]
    fn links_inside_the_allowed_places_are_followed_one_name_at_a_time() {
        let project = posix_project();
        let disk = posix_disk()
            .folders(["/data/proj/assets", "/srv", "/srv/art"])
            .file("/data/proj/assets/m.vrm")
            .file("/srv/art/pic.png")
            .link("/data/proj/alias", "assets")
            .link("/data/proj/sub/up", "..")
            .link("/data/proj/art", "/srv/art");
        let read = Grants::of([Scope::Read]);
        let got = run(Style::Posix, &disk, &read, &project, "alias/m.vrm", FsMode::Read).unwrap();
        assert_eq!((shown(&got), got.anchor), ("/data/proj/assets/m.vrm".to_owned(), 2));
        let got = run(Style::Posix, &disk, &read, &project, "sub/up/alias/m.vrm", FsMode::Read).unwrap();
        assert_eq!(shown(&got), "/data/proj/assets/m.vrm", "`..` in a link is the real parent");
        // A link to a root is followed only for a request that holds the root, and then lies in the root.
        let e = run(Style::Posix, &disk, &read, &project, "art/pic.png", FsMode::Read).unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied);
        assert!(!disk.asked().iter().any(|q| q.starts_with("/srv")), "without the root, its folders are never asked");
        let rooted = read.clone().with_fs_root("/srv/art");
        let got = run(Style::Posix, &disk, &rooted, &project, "art/pic.png", FsMode::Read).unwrap();
        assert_eq!((shown(&got), got.anchor), ("/srv/art/pic.png".to_owned(), 2));
    }

    #[test]
    fn the_name_the_project_was_opened_by_means_the_folder_that_was_opened() {
        let project = ProjectDir::from_paths(PathBuf::from("/data/proj"), Some("/home/u/link"), Style::Posix).unwrap();
        let agent = Grants::agent_default();
        // The opened-by name has since been swapped for a real folder with a file of the same name in it.
        let disk = posix_disk().file("/data/proj/a.vrm").folders(["/home", "/home/u", "/home/u/link"]);
        let disk = disk.file("/home/u/link/a.vrm").file("/home/u/link/swapped.vrm");
        let got = run(Style::Posix, &disk, &agent, &project, "/home/u/link/a.vrm", FsMode::Read).unwrap();
        assert_eq!(shown(&got), "/data/proj/a.vrm");
        let e = run(Style::Posix, &disk, &agent, &project, "/home/u/link/swapped.vrm", FsMode::Read).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "looked up in the opened folder, not the swapped one");
        assert!(!disk.asked().iter().any(|q| q.starts_with("/home")), "{:?}", disk.asked());
    }

    #[test]
    fn dangling_links_and_other_failures_are_refused_alike() {
        let project = posix_project();
        let agent = Grants::agent_default().with_fs_root("/data/proj");
        let disk = posix_disk()
            .link("/data/proj/new.txt", "/data/proj/gone.txt")
            .link("/data/proj/gone-dir", "/data/proj/nothing")
            .link("/data/proj/a", "b")
            .link("/data/proj/b", "a")
            .link("/data/proj/odd", "C:\u{0}x");
        for raw in ["new.txt", "gone-dir/x.glb", "a", "a/x", "ok.txt/x", "odd"] {
            let e = run(Style::Posix, &disk, &agent, &project, raw, FsMode::Write).unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied, "{raw}");
        }
        // A missing file below an existing folder is fine to write, and is `NotFound` to read.
        let ok = run(Style::Posix, &disk, &agent, &project, "fresh/out.glb", FsMode::Write).unwrap();
        assert_eq!(shown(&ok), "/data/proj/fresh/out.glb");
        let e = run(Style::Posix, &disk, &agent, &project, "fresh/out.glb", FsMode::Read).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
    }

    #[test]
    fn the_answer_remembers_its_mode_and_the_allowed_folder_it_lies_in() {
        let project = posix_project();
        let disk = posix_disk().folders(["/data/proj/sub/deeper"]);
        let rooted = Grants::person().with_fs_root("/data/proj/sub");
        let read = run(Style::Posix, &disk, &rooted, &project, "ok.txt", FsMode::Read).unwrap();
        assert_eq!((read.mode, read.anchor, read.file_name()), (FsMode::Read, 2, Some("ok.txt")));
        // The project folder comes first when both it and a root hold the path.
        let write = run(Style::Posix, &disk, &rooted, &project, "sub/deeper/x.glb", FsMode::Write).unwrap();
        assert_eq!((write.mode, write.anchor), (FsMode::Write, 2));
        // Its parents stop at the allowed folder.
        let parents: Vec<String> =
            std::iter::successors(Some(write), CanonicalPath::parent).map(|p| shown(&p)).collect();
        assert_eq!(parents, ["/data/proj/sub/deeper/x.glb", "/data/proj/sub/deeper", "/data/proj/sub", "/data/proj"]);
        // Without the project, the root is the allowed folder.
        let no_project = Grants::default().with_fs_root("/data/proj/sub");
        let got = run(Style::Posix, &disk, &no_project, &project, "/data/proj/sub/deeper", FsMode::Read).unwrap();
        assert_eq!(got.anchor, 3);
        assert_eq!(got.parent().map(|p| shown(&p)).as_deref(), Some("/data/proj/sub"));
        assert_eq!(got.parent().and_then(|p| p.parent()), None);
    }

    #[test]
    fn a_context_with_no_project_resolves_relative_paths_nowhere() {
        let disk = FakeDisk::default();
        let e = check_in(Style::Posix, &disk, &Grants::person(), None, "a.vrm", FsMode::Read).unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid);
        assert_eq!(disk.asked(), Vec::<String>::new());
    }
}
