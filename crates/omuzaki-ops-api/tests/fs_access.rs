//! `OpCtx::fs_access` as an op sees it: what the request's grants and the project folder allow, with paths resolved on
//! a real disk (a temporary folder). The path grammar itself (Windows forms, device names) is tested in the crate.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CanonicalPath, ErrorCode, FsMode, Grants, OpCtx, OpError, ProjectDir, ProjectDirError, Scope,
};
use tempfile::TempDir;

/// A scratch disk laid out like a project next to a folder the project does not own:
///
/// ```text
/// <base>/project/assets/model.vrm        <base>/outside/secret.txt
/// <base>/project/sub/x.txt               <base>/outside/art/pic.png
/// ```
struct Site {
    _dir: TempDir,
    base: PathBuf,
    project: PathBuf,
    outside: PathBuf,
    /// The project folder, opened the way an app opens it.
    dir: ProjectDir,
}

impl Site {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // The grants hold real paths, so start from the real path of the scratch folder (it may be behind a symlink).
        let base = plain(fs::canonicalize(dir.path()).unwrap());
        let (project, outside) = (base.join("project"), base.join("outside"));
        for folder in ["project/assets", "project/sub", "outside/art"] {
            fs::create_dir_all(base.join(folder)).unwrap();
        }
        for file in ["project/assets/model.vrm", "project/sub/x.txt", "outside/secret.txt", "outside/art/pic.png"] {
            fs::write(base.join(file), file).unwrap();
        }
        let project_dir = ProjectDir::open(&project).unwrap();
        Self { _dir: dir, base, project, outside, dir: project_dir }
    }
}

/// `canonicalize` returns `\\?\C:\...` on Windows, where `/` and `..` are literal names. Tests spell paths by hand, so
/// they use the plain drive form, which names the same place.
fn plain(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    path
}

/// Where a checked path is, as the test compares it (a `CanonicalPath` has no `&Path` on purpose: ops open it with its
/// own methods).
fn real(path: &CanonicalPath) -> PathBuf {
    plain(PathBuf::from(path.to_string()))
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Asks `fs_access` through a context built the way the executor builds one.
fn ask(grants: &Grants, project: Option<&ProjectDir>, path: &str, mode: FsMode) -> Result<PathBuf, OpError> {
    let (doc, actor, clock) =
        (MemDoc::default(), Actor::Agent { id: "a".into(), model: None }, FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(grants);
    match project {
        Some(dir) => ctx.with_project_dir(dir).fs_access(path, mode).map(|path| real(&path)),
        None => ctx.fs_access(path, mode).map(|path| real(&path)),
    }
}

fn read_only() -> Grants {
    Grants::of([Scope::Read])
}

fn code<T: std::fmt::Debug>(result: Result<T, OpError>) -> ErrorCode {
    result.unwrap_err().code
}

#[test]
fn reading_inside_the_project_needs_only_read() {
    let site = Site::new();
    let file = site.project.join("assets/model.vrm");
    for path in [text(&file), "assets/model.vrm", "./assets/model.vrm", "assets/../sub/../assets/model.vrm"] {
        let got = ask(&read_only(), Some(&site.dir), path, FsMode::Read).unwrap();
        assert_eq!(got, file, "{path}");
    }
    // The folder itself is inside it.
    assert!(ask(&read_only(), Some(&site.dir), ".", FsMode::Read).is_ok());
    assert!(ask(&read_only(), Some(&site.dir), text(&site.project), FsMode::Read).is_ok());
}

#[test]
fn reading_inside_the_project_without_read_is_denied() {
    let site = Site::new();
    let no_read = Grants::of([Scope::EditScene, Scope::Render]);
    assert_eq!(code(ask(&no_read, Some(&site.dir), "assets/model.vrm", FsMode::Read)), ErrorCode::Denied);
}

#[test]
fn a_context_nobody_granted_anything_refuses_everything() {
    let site = Site::new();
    let (doc, actor, clock) = (MemDoc::default(), Actor::System, FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, &actor, &clock).with_project_dir(&site.dir);
    assert_eq!(code(ctx.fs_access("assets/model.vrm", FsMode::Read)), ErrorCode::Denied);
    assert_eq!(code(ctx.fs_access(text(&site.outside.join("secret.txt")), FsMode::Read)), ErrorCode::Denied);
    assert_eq!(code(ctx.fs_access(text(&site.outside.join("new.txt")), FsMode::Write)), ErrorCode::Denied);
}

#[test]
fn a_relative_path_needs_a_project_folder() {
    let e = ask(&Grants::person(), None, "assets/model.vrm", FsMode::Read).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")));
    assert!(e.hint.unwrap().contains("absolute"));
    // Absolute paths still work without one, but only through a grant.
    let site = Site::new();
    let secret = site.outside.join("secret.txt");
    assert_eq!(code(ask(&read_only(), None, text(&secret), FsMode::Read)), ErrorCode::Denied);
    let granted = Grants::default().with_fs_root(text(&site.outside));
    assert!(ask(&granted, None, text(&secret), FsMode::Read).is_ok());
}

#[test]
fn a_project_folder_must_exist_be_a_folder_and_not_be_a_root() {
    let site = Site::new();
    assert!(matches!(ProjectDir::open(site.base.join("gone")), Err(ProjectDirError::NotFound(_))));
    let file = site.project.join("assets/model.vrm");
    assert!(matches!(ProjectDir::open(&file), Err(ProjectDirError::NotAFolder(_))));
    // A relative folder is pinned to where it points now, and read back as the real path.
    let relative = ProjectDir::open(".").unwrap();
    assert_eq!(relative.as_path(), fs::canonicalize(".").unwrap());
}

#[test]
fn writing_needs_fs_and_the_project_folder_counts_as_a_root() {
    let site = Site::new();
    let target = site.project.join("sub/out.glb");
    let project = Some(&site.dir);
    // Without `fs` nothing is written, inside the project or out of it.
    for path in [text(&target), "sub/out.glb"] {
        assert_eq!(code(ask(&Grants::agent_default(), project, path, FsMode::Write)), ErrorCode::Denied, "{path}");
    }
    // With `fs`, the project folder is a root of its own (ADR 0013), so no further root is needed there...
    let with_fs = Grants::agent_default().with(Scope::Fs);
    for path in [text(&target), "sub/out.glb"] {
        assert_eq!(ask(&with_fs, project, path, FsMode::Write).unwrap(), target, "{path}");
    }
    assert_eq!(ask(&Grants::person(), project, "sub/out.glb", FsMode::Write).unwrap(), target);
    // ...but only there: outside it still needs a root that covers the file.
    let outside = site.outside.join("out.glb");
    assert_eq!(code(ask(&with_fs, project, text(&outside), FsMode::Write)), ErrorCode::Denied);
    // And a context with no project folder has no project root to write into.
    assert_eq!(code(ask(&with_fs, None, text(&target), FsMode::Write)), ErrorCode::Denied);
}

#[test]
fn a_root_on_a_whole_disk_grants_nothing() {
    let site = Site::new();
    let secret = text(&site.outside.join("secret.txt")).to_owned();
    let whole = Grants::person().with_fs_root("/");
    #[cfg(windows)]
    let whole = whole.with_fs_root(r"C:\");
    for mode in [FsMode::Read, FsMode::Write] {
        assert_eq!(code(ask(&whole, Some(&site.dir), &secret, mode)), ErrorCode::Denied, "{mode:?}");
    }
}

#[test]
fn outside_the_project_needs_fs_and_a_covering_root() {
    let site = Site::new();
    let pic = site.outside.join("art/pic.png");
    let project = Some(&site.dir);
    assert_eq!(code(ask(&read_only(), project, text(&pic), FsMode::Read)), ErrorCode::Denied, "read only");
    assert_eq!(code(ask(&Grants::person(), project, text(&pic), FsMode::Read)), ErrorCode::Denied, "fs but no root");
    for root in [text(&site.outside), text(&site.base), &format!("{}/art/", text(&site.outside))] {
        let granted = Grants::agent_default().with_fs_root(root);
        assert_eq!(ask(&granted, project, text(&pic), FsMode::Read).unwrap(), pic, "root {root}");
    }
    // A root covers whole names only: `art` does not cover `art_extra` (nor a sibling that starts the same way).
    let prefix_only = Grants::default().with_fs_root(format!("{}/ar", text(&site.outside)));
    assert_eq!(code(ask(&prefix_only, project, text(&pic), FsMode::Read)), ErrorCode::Denied);
    let other = Grants::default().with_fs_root(text(&site.project));
    assert_eq!(code(ask(&other, project, text(&pic), FsMode::Read)), ErrorCode::Denied, "a root elsewhere");
}

#[test]
fn dot_dot_traversal_is_refused_unless_the_target_is_granted() {
    let site = Site::new();
    let secret = site.outside.join("secret.txt");
    let project = Some(&site.dir);
    let through_project = format!("{}/../outside/secret.txt", text(&site.project));
    let deeper = format!("{}/assets/../../outside/secret.txt", text(&site.project));
    let escapes = ["../outside/secret.txt", "assets/../../outside/secret.txt", &through_project, &deeper];
    for path in escapes {
        assert_eq!(code(ask(&read_only(), project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
        assert_eq!(code(ask(&Grants::person(), project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
        let granted = Grants::default().with_fs_root(text(&site.outside));
        assert_eq!(ask(&granted, project, path, FsMode::Read).unwrap(), secret, "{path}");
        // A root on the project says nothing about the folder the path really ends up in.
        let wrong = Grants::default().with_fs_root(text(&site.project));
        assert_eq!(code(ask(&wrong, project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
    }
}

#[test]
fn a_file_to_be_written_resolves_to_where_it_will_be_created() {
    let site = Site::new();
    let granted = Grants::default().with_fs_root(text(&site.outside));
    let project = Some(&site.dir);
    let wanted = format!("{}/new/dir/./file.glb", text(&site.outside));
    let got = ask(&granted, project, &wanted, FsMode::Write).unwrap();
    assert_eq!(got, site.outside.join("new/dir/file.glb"));
    assert!(!site.outside.join("new").exists(), "checking creates nothing");

    // `..` below a folder that does not exist is collapsed by name: this ends in a third folder, which no root covers...
    let sneaky = format!("{}/new/../../elsewhere/x.txt", text(&site.outside));
    assert_eq!(code(ask(&granted, project, &sneaky, FsMode::Write)), ErrorCode::Denied);
    // ...and this one ends in the project folder, which is a root of its own for a holder of `fs`.
    let home = format!("{}/new/../../project/sub/x.txt", text(&site.outside));
    assert_eq!(ask(&granted, project, &home, FsMode::Write).unwrap(), site.project.join("sub/x.txt"));
    // A file in the way: `secret.txt` is not a folder.
    let blocked = format!("{}/secret.txt/x", text(&site.outside));
    assert_eq!(code(ask(&granted, project, &blocked, FsMode::Write)), ErrorCode::Denied);
}

#[test]
fn a_missing_file_is_not_found_only_where_the_actor_may_look() {
    let site = Site::new();
    let project = Some(&site.dir);
    let e = ask(&read_only(), project, "assets/nope.vrm", FsMode::Read).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");

    let (there, missing) = (site.outside.join("secret.txt"), site.outside.join("missing/deeper/file.txt"));
    let granted = Grants::default().with_fs_root(text(&site.outside));
    assert_eq!(code(ask(&granted, project, text(&missing), FsMode::Read)), ErrorCode::NotFound);

    // Outside what the actor may touch, an existing and a missing file are refused in exactly the same words.
    let existing = ask(&read_only(), project, text(&there), FsMode::Read).unwrap_err();
    let absent = ask(&read_only(), project, text(&missing), FsMode::Read).unwrap_err();
    assert_eq!(existing.code, ErrorCode::Denied);
    assert_eq!(
        existing.hint.as_ref().unwrap().replace(text(&there), "P"),
        absent.hint.as_ref().unwrap().replace(text(&missing), "P")
    );
    assert_eq!(existing.message.replace(text(&there), "P"), absent.message.replace(text(&missing), "P"));
}

#[test]
fn a_denial_names_the_path_and_what_to_grant() {
    let site = Site::new();
    let secret = site.outside.join("secret.txt");
    let path = text(&secret);
    let no_fs = ask(&read_only(), Some(&site.dir), path, FsMode::Read).unwrap_err();
    assert_eq!(no_fs.code, ErrorCode::Denied);
    assert!(no_fs.message.contains(path) && no_fs.message.contains("`fs` scope"), "{no_fs:?}");
    assert!(no_fs.hint.as_deref().unwrap().contains("`fs`"), "{no_fs:?}");

    let no_root = ask(&Grants::person(), Some(&site.dir), path, FsMode::Read).unwrap_err();
    assert!(no_root.message.contains(path) && no_root.message.contains("root"), "{no_root:?}");
    assert!(no_root.hint.as_deref().unwrap().contains("root that covers"), "{no_root:?}");

    let write = ask(&Grants::agent_default(), Some(&site.dir), "sub/o.glb", FsMode::Write).unwrap_err();
    assert!(write.message.contains("writing `sub/o.glb`"), "{write:?}");
    assert!(write.hint.as_deref().unwrap().contains("inside the project folder or under a granted root"), "{write:?}");
}

#[test]
fn the_answer_does_not_depend_on_the_dry_run_flag() {
    let site = Site::new();
    let (doc, actor, clock) = (MemDoc::default(), Actor::System, FakeClock::new(Seconds(0.0)));
    let grants = Grants::default().with_fs_root(text(&site.outside));
    let secret = site.outside.join("secret.txt");
    for dry_run in [false, true] {
        let ctx =
            OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_project_dir(&site.dir).with_dry_run(dry_run);
        assert_eq!(real(&ctx.fs_access(text(&secret), FsMode::Read).unwrap()), secret);
        assert_eq!(
            real(&ctx.fs_access("../outside/art/pic.png", FsMode::Read).unwrap()),
            site.outside.join("art/pic.png")
        );
        assert_eq!(
            real(&ctx.fs_access("assets/missing.vrm", FsMode::Write).unwrap()),
            site.project.join("assets/missing.vrm")
        );
        assert_eq!(code(ctx.fs_access("assets/missing.vrm", FsMode::Read)), ErrorCode::Denied);
    }
}

#[test]
fn text_that_is_not_an_unambiguous_path_is_invalid_with_a_hint() {
    let site = Site::new();
    for path in ["", "assets/model\0.vrm"] {
        let e = ask(&Grants::person(), Some(&site.dir), path, FsMode::Read).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")), "{path:?}");
        assert!(e.hint.is_some());
    }
}

/// An absolute path in this host's own grammar that nothing covers: a POSIX root on Windows is not one (it is
/// root-relative, so `Invalid`, see `docs/design/ops-api.md` section 5).
const NOWHERE: &str = if cfg!(windows) { r"C:\nowhere\x" } else { "/nowhere/x" };

#[test]
fn an_absolute_path_nobody_covers_is_denied_on_every_os() {
    let site = Site::new();
    for mode in [FsMode::Read, FsMode::Write] {
        assert_eq!(code(ask(&Grants::person(), Some(&site.dir), NOWHERE, mode)), ErrorCode::Denied, "{mode:?}");
        assert_eq!(code(ask(&Grants::person(), None, NOWHERE, mode)), ErrorCode::Denied, "{mode:?}, no project");
    }
}

#[test]
fn a_folder_opened_as_a_file_is_invalid_and_not_denied_on_every_os() {
    let site = Site::new();
    let (doc, actor, clock) = (MemDoc::default(), Actor::System, FakeClock::new(Seconds(0.0)));
    let grants = Grants::person();
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_project_dir(&site.dir);
    let folder = ctx.fs_access("assets", FsMode::Read).unwrap();
    let e = folder.open_read().unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")));
}

/// Windows forms on a POSIX host: a file called `C:` is not what anyone means, so they are refused with a pointer to
/// the form that works. (The mirror image, POSIX forms on Windows, is covered by the grammar tests in the crate.)
#[cfg(unix)]
#[test]
fn windows_forms_are_invalid_on_a_posix_host() {
    let site = Site::new();
    let everything = Grants::person().with_fs_root("/");
    for path in
        [r"C:\Users\u\f.vrm", "C:/Users/u/f.vrm", "C:f.vrm", r"\\server\share\f.vrm", r"\\?\C:\f.vrm", r"\f.vrm"]
    {
        for mode in [FsMode::Read, FsMode::Write] {
            let e = ask(&everything, Some(&site.dir), path, mode).unwrap_err();
            assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")), "{path}");
            assert!(e.hint.as_deref().unwrap().contains("/mnt/c"), "{path}: {e:?}");
        }
    }
    // The same text is an ordinary name when it is not at the start.
    assert_eq!(code(ask(&everything, Some(&site.dir), "sub/C:f.vrm", FsMode::Read)), ErrorCode::NotFound);
}

/// The grants of the attack tests below, with the places each one may touch in a given mode.
struct Config {
    name: &'static str,
    grants: Grants,
    roots: Vec<PathBuf>,
}

impl Config {
    /// Where an `Ok` answer may point: the project folder for reads with `read`, and every root.
    fn allowed(&self, mode: FsMode, project: &Path) -> Vec<PathBuf> {
        let mut places = self.roots.clone();
        let scope = if mode == FsMode::Read { Scope::Read } else { Scope::Fs };
        if self.grants.allows(scope) {
            places.push(project.to_path_buf());
        }
        places
    }
}

fn configs(site: &Site) -> Vec<Config> {
    let art = site.outside.join("art");
    vec![
        Config { name: "read only", grants: read_only(), roots: vec![] },
        Config {
            name: "agent + art",
            grants: Grants::agent_default().with_fs_root(text(&art)),
            roots: vec![art.clone()],
        },
        Config {
            name: "person + project",
            grants: Grants::person().with_fs_root(text(&site.project)),
            roots: vec![site.project.clone()],
        },
        Config {
            name: "fs outside only",
            grants: Grants::default().with_fs_root(text(&site.outside)),
            roots: vec![site.outside.clone()],
        },
    ]
}

/// Name pieces an attacker would try, singly and in every order: traversal in its spellings, lookalikes, empty names,
/// and the names of the links and folders of the site.
const PIECES: [&str; 15] = [
    "..",
    ".",
    "",
    "link",
    "alias",
    "assets",
    "sub",
    "outside",
    "project",
    "secret.txt",
    "x.txt",
    "%2e%2e",
    "\u{ff0e}\u{ff0e}",
    "...",
    ".. ",
];

/// The attack invariant: whatever the text, an `Ok` answer points inside what the grants allow, and a read answer
/// points at something that exists.
fn assert_contained(site: &Site, config: &Config, project: Option<&ProjectDir>, raw: &str, mode: FsMode) {
    let Ok(got) = ask(&config.grants, project, raw, mode) else { return };
    let allowed = config.allowed(mode, &site.project);
    assert!(
        allowed.iter().any(|place| got.starts_with(place)),
        "{}: {mode:?} {raw:?} escaped to {}",
        config.name,
        got.display()
    );
    if mode == FsMode::Read {
        assert!(
            got.exists(),
            "{}: read {raw:?} answered with something that is not there: {}",
            config.name,
            got.display()
        );
    }
}

#[cfg(unix)]
fn plant_links(site: &Site) {
    use std::os::unix::fs::symlink;
    symlink(&site.outside, site.project.join("link")).unwrap();
    symlink(site.project.join("assets"), site.project.join("alias")).unwrap();
    symlink(&site.project, site.outside.join("back")).unwrap();
    symlink("..", site.outside.join("art/up")).unwrap();
}

#[test]
fn no_spelling_of_a_path_gets_outside_what_the_grants_allow() {
    let site = Site::new();
    #[cfg(unix)]
    plant_links(&site);
    let configs = configs(&site);
    let prefixes = [
        String::new(),
        format!("{}/", text(&site.project)),
        format!("{}/", text(&site.outside)),
        format!("{}/", text(&site.base)),
        "/".to_owned(),
        "/proc/self/root/".to_owned(),
    ];
    let mut tried = 0_u32;
    // Every sequence of up to three pieces, behind every prefix.
    let mut sequences: Vec<Vec<&str>> = PIECES.iter().map(|piece| vec![*piece]).collect();
    for length in 2..=3 {
        let shorter: Vec<Vec<&str>> = sequences.iter().filter(|seq| seq.len() == length - 1).cloned().collect();
        for seq in shorter {
            for piece in PIECES {
                sequences.push(seq.iter().copied().chain([piece]).collect());
            }
        }
    }
    // And a deterministic spread of long ones (a fixed xorshift, so a failure replays).
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % 1_000_003).unwrap()
    };
    for _ in 0..3_000 {
        let length = 4 + next() % 8;
        sequences.push((0..length).map(|_| PIECES[next() % PIECES.len()]).collect());
    }
    for config in &configs {
        for prefix in &prefixes {
            for sequence in &sequences {
                let raw = format!("{prefix}{}", sequence.join("/"));
                for mode in [FsMode::Read, FsMode::Write] {
                    assert_contained(&site, config, Some(&site.dir), &raw, mode);
                    tried += 1;
                }
            }
        }
    }
    assert!(tried > 100_000, "the table is meant to be big: {tried}");
}

#[test]
fn separators_and_odd_whitespace_do_not_change_where_a_path_ends_up() {
    let site = Site::new();
    #[cfg(unix)]
    plant_links(&site);
    let secret = text(&site.outside).to_owned();
    let project = text(&site.project).to_owned();
    for config in configs(&site) {
        for raw in [
            format!("{project}//..//outside//secret.txt"),
            format!("{project}/./../outside/./secret.txt"),
            format!("{project}/assets/..//..//outside/secret.txt"),
            format!("{project}\\..\\outside\\secret.txt"),
            format!("{project}/..\\outside/secret.txt"),
            format!("{project}/%2e%2e/outside/secret.txt"),
            format!("{project}/\u{ff0e}\u{ff0e}/outside/secret.txt"),
            format!(" {project}/../outside/secret.txt"),
            format!("{project}/../outside/secret.txt "),
            format!("{project}/../outside/secret.txt/"),
            format!("{project}/../outside/secret.txt/."),
            format!("{project}/../outside/secret.txt/.."),
            format!("{secret}/../project/sub/x.txt"),
            "\u{202e}txt.terces/../outside".to_owned(),
            "file:///etc/passwd".to_owned(),
            "~/../../etc/passwd".to_owned(),
            "$HOME/.ssh/id_rsa".to_owned(),
            "-".to_owned(),
        ] {
            for mode in [FsMode::Read, FsMode::Write] {
                assert_contained(&site, &config, Some(&site.dir), &raw, mode);
            }
        }
    }
}

#[test]
fn a_path_of_a_hundred_thousand_names_is_refused_quickly_inside_and_outside() {
    let site = Site::new();
    let deep = vec!["a"; 100_000].join("/");
    let started = std::time::Instant::now();
    let outside = format!("{}/{deep}", text(&site.outside));
    for raw in [outside.as_str(), deep.as_str(), &vec![".."; 100_000].join("/"), &"a/".repeat(500_000)] {
        for mode in [FsMode::Read, FsMode::Write] {
            let e = ask(&Grants::person().with_fs_root("/"), Some(&site.dir), raw, mode).unwrap_err();
            assert_eq!(e.code, ErrorCode::Invalid, "{} bytes", raw.len());
        }
    }
    // Just under the limit on names, inside the project and not there: a normal answer, still quick.
    let nearly = vec!["a"; 900].join("/");
    assert_eq!(code(ask(&read_only(), Some(&site.dir), &nearly, FsMode::Read)), ErrorCode::NotFound);
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "took {:?}", started.elapsed());
}

#[test]
fn dot_dot_is_collapsed_by_name_before_anything_is_resolved() {
    // `a/b/../c` is `a/c`, whatever `b` is on the disk (a symlink, even): the answer never depends on where a link
    // points, so a link cannot make `..` mean something the gate did not see.
    let site = Site::new();
    let by_name = ask(&read_only(), Some(&site.dir), "assets/../sub/x.txt", FsMode::Read).unwrap();
    assert_eq!(by_name, site.project.join("sub/x.txt"));
    #[cfg(unix)]
    {
        plant_links(&site);
        let granted = Grants::default().with_fs_root(text(&site.outside));
        let through_link = ask(&read_only(), Some(&site.dir), "link/../sub/x.txt", FsMode::Read).unwrap();
        assert_eq!(through_link, site.project.join("sub/x.txt"), "collapsed by name, not through the link");
        let above = ask(&granted, Some(&site.dir), "link/../../outside/secret.txt", FsMode::Read).unwrap();
        assert_eq!(above, site.outside.join("secret.txt"));
    }
    // Climbing above the root of the path is refused outright.
    for raw in ["/..", "/../..", "/a/../..", &format!("{}/../../../../../../../..", text(&site.project))] {
        let e = ask(&Grants::person().with_fs_root("/"), Some(&site.dir), raw, FsMode::Read);
        if raw.starts_with("/..") || raw == "/a/../.." {
            assert_eq!(e.unwrap_err().code, ErrorCode::Invalid, "{raw}");
        }
    }
}

/// A symbolic link whose access time shows whether anything looked it up: reading a link updates its access time, so
/// a spy set to the year 2000 that is still at 2000 afterwards was never followed.
#[cfg(unix)]
struct Spy(PathBuf);

#[cfg(unix)]
impl Spy {
    /// A spy at `path` pointing at `target`, or `None` (the test is skipped) where access times cannot show a
    /// lookup: no `touch -h`, or a file system that does not update them.
    fn plant(path: PathBuf, target: &Path) -> Option<Self> {
        std::os::unix::fs::symlink(target, &path).unwrap();
        let spy = Self(path);
        if !spy.arm() {
            eprintln!("skipped: `touch -h` is not available");
            return None;
        }
        // The control: this is what a lookup does, and it must be visible, or the test proves nothing here.
        fs::read_link(&spy.0).unwrap();
        if !spy.looked_up() {
            eprintln!("skipped: this file system does not update access times");
            return None;
        }
        assert!(spy.atime() <= std::time::SystemTime::now());
        Some(spy)
    }

    /// Sets the access time to the year 2000.
    fn arm(&self) -> bool {
        std::process::Command::new("touch")
            .args(["-h", "-a", "-d", "2000-01-01 00:00:00"])
            .arg(&self.0)
            .status()
            .is_ok_and(|status| status.success())
    }

    fn atime(&self) -> std::time::SystemTime {
        fs::symlink_metadata(&self.0).unwrap().accessed().unwrap()
    }

    /// Whether the link was read since it was armed.
    fn looked_up(&self) -> bool {
        self.atime() > std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)
    }
}

/// A refused path must not touch the file system at all: asking for a spy outside the project as a person without a
/// root, or through a lexically outside path, must leave it unread.
#[cfg(unix)]
#[test]
fn a_refused_path_leaves_the_access_time_of_a_symlink_alone() {
    let site = Site::new();
    let Some(spy) = Spy::plant(site.outside.join("spy"), &site.outside.join("secret.txt")) else { return };
    for raw in [
        text(&spy.0).to_owned(),
        format!("{}/../outside/spy", text(&site.project)),
        "../outside/spy".to_owned(),
        format!("{}/spy/../spy", text(&site.outside)),
    ] {
        for grants in [
            read_only(),
            Grants::agent_default(),
            Grants::person(),
            Grants::default().with_fs_root(text(&site.project)),
        ] {
            assert!(spy.arm());
            let refused = ask(&grants, Some(&site.dir), &raw, FsMode::Read).unwrap_err();
            assert_eq!(refused.code, ErrorCode::Denied, "{raw}");
            assert!(!spy.looked_up(), "the refused path {raw:?} was looked up");
        }
    }
}

/// A link inside the project that leads out of every place the request may reach is refused from its own text: the
/// OS is never asked to follow it, so the spy at its far end stays unread (before it, the path was resolved
/// first, wherever it led, and refused after).
#[cfg(unix)]
#[test]
fn a_link_out_of_the_project_is_refused_without_its_target_being_looked_up() {
    let site = Site::new();
    let Some(spy) = Spy::plant(site.outside.join("spy"), &site.outside.join("secret.txt")) else { return };
    std::os::unix::fs::symlink(&spy.0, site.project.join("door")).unwrap();
    std::os::unix::fs::symlink(&site.outside, site.project.join("hall")).unwrap();
    for raw in ["door", "hall/spy", "sub/../door"] {
        for grants in [read_only(), Grants::person(), Grants::person().with_fs_root(text(&site.project))] {
            for mode in [FsMode::Read, FsMode::Write] {
                assert!(spy.arm());
                let refused = ask(&grants, Some(&site.dir), raw, mode).unwrap_err();
                assert_eq!(refused.code, ErrorCode::Denied, "{raw}");
                assert!(!spy.looked_up(), "{raw:?} followed the link out before refusing it");
            }
        }
    }
    // With a root on the far side the links are followed, and the spy shows it.
    let granted = read_only().with_fs_root(text(&site.outside));
    assert!(spy.arm());
    assert_eq!(ask(&granted, Some(&site.dir), "door", FsMode::Read).unwrap(), site.outside.join("secret.txt"));
    assert!(spy.looked_up());
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_filesystem_root_is_not_a_project_folder() {
    let site = Site::new();
    let to_root = site.base.join("to-root");
    std::os::unix::fs::symlink("/", &to_root).unwrap();
    for root in [Path::new("/"), Path::new("/."), Path::new("//"), &to_root, &site.project.join("../../../..")] {
        // The last one climbs out of the temporary folder; wherever it ends up must not be `/` unless refused.
        match ProjectDir::open(root) {
            Err(e) => assert!(
                matches!(e, ProjectDirError::FilesystemRoot(_) | ProjectDirError::NotFound(_)),
                "{root:?}: {e:?}"
            ),
            Ok(dir) => assert_ne!(dir.as_path(), Path::new("/"), "{root:?}"),
        }
    }
    assert!(matches!(ProjectDir::open("/"), Err(ProjectDirError::FilesystemRoot(_))));
    assert!(matches!(ProjectDir::open(&to_root), Err(ProjectDirError::FilesystemRoot(_))));
}

/// The mirror image on Windows: POSIX roots and drive-relative paths are ambiguous there, and device and stream names
/// are not files. Built, but not run, by the Windows cross-build (`cargo build --target x86_64-pc-windows-gnu`); the
/// grammar behind it runs on every host in `file_access::form`.
#[cfg(windows)]
#[test]
fn posix_and_ambiguous_forms_are_invalid_on_a_windows_host() {
    let site = Site::new();
    let everything = Grants::person().with_fs_root(r"C:\");
    for path in
        ["/etc/passwd", r"\x", "C:x", r"\\.\C:\x", r"\\.\pipe\x", "sub\\CON", "sub\\x.txt::$DATA", "sub\\x.txt."]
    {
        let e = ask(&everything, Some(&site.dir), path, FsMode::Read).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")), "{path}");
    }
}

#[cfg(unix)]
mod symlinks {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_link_out_of_the_project_is_checked_by_where_it_leads() {
        let site = Site::new();
        symlink(&site.outside, site.project.join("link")).unwrap();
        symlink(site.outside.join("secret.txt"), site.project.join("evil.txt")).unwrap();
        let project = Some(&site.dir);
        let secret = site.outside.join("secret.txt");
        for path in ["link/secret.txt", "evil.txt", "sub/../link/secret.txt"] {
            assert_eq!(code(ask(&read_only(), project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
            assert_eq!(code(ask(&Grants::person(), project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
            // A root on the link's own folder is not a root on its target.
            let on_project = Grants::default().with_fs_root(text(&site.project));
            assert_eq!(code(ask(&on_project, project, path, FsMode::Read)), ErrorCode::Denied, "{path}");
            // A link inside the project is gated twice: by its name (so the actor needs `read`) and by where it
            // lands (so a root must cover the target). Either alone is not enough.
            let on_target = Grants::of([Scope::Read]).with_fs_root(text(&site.outside));
            assert_eq!(ask(&on_target, project, path, FsMode::Read).unwrap(), secret, "{path}");
            let root_only = Grants::default().with_fs_root(text(&site.outside));
            assert_eq!(code(ask(&root_only, project, path, FsMode::Read)), ErrorCode::Denied, "{path}: no `read`");
        }
    }

    #[test]
    fn a_link_to_a_missing_target_cannot_be_written_through() {
        let site = Site::new();
        let target = site.outside.join("not_yet.txt");
        symlink(&target, site.project.join("new.txt")).unwrap();
        let project = Some(&site.dir);
        let on_project = Grants::default().with_fs_root(text(&site.project));
        assert_eq!(code(ask(&on_project, project, "new.txt", FsMode::Write)), ErrorCode::Denied);
        assert_eq!(code(ask(&on_project, project, "new.txt/more", FsMode::Write)), ErrorCode::Denied);
        assert_eq!(code(ask(&read_only(), project, "new.txt", FsMode::Read)), ErrorCode::Denied);
        assert!(!target.exists());
        // Even a root on the target does not allow it: the broken link is refused, not resolved by hand.
        let on_target = Grants::default().with_fs_root(text(&site.outside));
        assert_eq!(code(ask(&on_target, project, "new.txt", FsMode::Write)), ErrorCode::Denied);
    }

    #[test]
    fn a_link_inside_the_project_is_fine() {
        let site = Site::new();
        symlink(site.project.join("assets"), site.project.join("alias")).unwrap();
        let got = ask(&read_only(), Some(&site.dir), "alias/model.vrm", FsMode::Read).unwrap();
        assert_eq!(got, site.project.join("assets/model.vrm"));
    }

    #[test]
    fn the_project_may_be_opened_through_a_link() {
        let site = Site::new();
        let link = site.base.join("project-link");
        symlink(&site.project, &link).unwrap();
        let through_link = ProjectDir::open(&link).unwrap();
        let file = site.project.join("assets/model.vrm");
        for path in ["assets/model.vrm", text(&link.join("assets/model.vrm")), text(&file)] {
            let got = ask(&read_only(), Some(&through_link), path, FsMode::Read).unwrap();
            assert_eq!(got, file, "{path}");
        }
    }

    #[test]
    fn a_link_loop_is_refused() {
        let site = Site::new();
        symlink(site.project.join("b"), site.project.join("a")).unwrap();
        symlink(site.project.join("a"), site.project.join("b")).unwrap();
        assert_eq!(code(ask(&read_only(), Some(&site.dir), "a", FsMode::Read)), ErrorCode::Denied);
    }

    #[test]
    fn a_root_reached_through_a_link_covers_nothing() {
        let site = Site::new();
        let link = site.base.join("art-link");
        symlink(site.outside.join("art"), &link).unwrap();
        let pic = site.outside.join("art/pic.png");
        let granted = Grants::default().with_fs_root(text(&link));
        assert_eq!(code(ask(&granted, Some(&site.dir), text(&pic), FsMode::Read)), ErrorCode::Denied);
        assert_eq!(code(ask(&granted, Some(&site.dir), text(&link.join("pic.png")), FsMode::Read)), ErrorCode::Denied);
    }

    #[test]
    fn a_path_that_cannot_be_written_as_text_is_refused() {
        let site = Site::new();
        let odd = site.outside.join(OsStr::from_bytes(b"caf\xe9"));
        fs::create_dir(&odd).unwrap();
        symlink(&odd, site.project.join("odd")).unwrap();
        let granted = Grants::default().with_fs_root(text(&site.outside));
        assert_eq!(code(ask(&granted, Some(&site.dir), "odd", FsMode::Read)), ErrorCode::Denied);
    }
}
