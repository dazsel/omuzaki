//! The verified open on a real disk: what it opens, what it refuses, and a race harness that swaps folders and files
//! for links while it opens them. On Linux every test runs twice: with `openat2` and `O_TMPFILE`, and with the
//! portable fallbacks that replace them where the kernel or the file system has neither (the component walk, named
//! temporary files).
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::super::{CanonicalPath, FsMode, ProjectDir, check};
use super::{DirEntryInfo, EntryKind, TempNames, WriteMode};
use crate::{ErrorCode, Grants, OpError, Scope};

/// A project folder next to a folder it does not own, laid out alike, so an open that strayed would find a file:
///
/// ```text
/// <base>/project/sub/file.txt   "inside"        <base>/outside/sub/file.txt   "SECRET"
/// <base>/project/other.txt      "other"         <base>/outside/sub/w.txt      "SECRET-W"
/// ```
struct Site {
    _dir: TempDir,
    base: PathBuf,
    project: PathBuf,
    outside: PathBuf,
    dir: ProjectDir,
}

fn site() -> Site {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (project, outside) = (base.join("project"), base.join("outside"));
    fs::create_dir_all(project.join("sub")).unwrap();
    fs::create_dir_all(outside.join("sub")).unwrap();
    for (file, text) in [
        (project.join("sub/file.txt"), "inside"),
        (project.join("other.txt"), "other"),
        (outside.join("sub/file.txt"), "SECRET"),
        (outside.join("sub/w.txt"), "SECRET-W"),
    ] {
        fs::write(file, text).unwrap();
    }
    let project_dir = ProjectDir::open(&project).unwrap();
    Site { _dir: dir, base, project, outside, dir: project_dir }
}

/// `fs_access` as an agent with `fs` sees it.
fn checked(site: &Site, raw: &str, mode: FsMode) -> CanonicalPath {
    check(&Grants::agent_default().with(Scope::Fs), Some(&site.dir), raw, mode).unwrap()
}

fn code<T>(result: Result<T, OpError>) -> ErrorCode {
    result.err().map(|e| e.code).unwrap()
}

fn read(path: &CanonicalPath) -> Result<String, OpError> {
    let mut text = String::new();
    path.open_read()?.read_to_string(&mut text).unwrap();
    Ok(text)
}

/// The names in a folder, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    names
}

/// Runs `test` with `openat2` and `O_TMPFILE` (where this kernel has them) and again with the portable fallbacks.
fn both_ways(test: impl Fn()) {
    test();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    super::unix::with_fallbacks(&test);
}

#[test]
fn reads_and_writes_inside_the_allowed_folder() {
    both_ways(|| {
        let site = site();
        assert_eq!(read(&checked(&site, "sub/file.txt", FsMode::Read)).unwrap(), "inside");

        let out = checked(&site, "new/deeper/out.txt", FsMode::Write);
        assert_eq!(code(out.open_write(WriteMode::CreateNew)), ErrorCode::NotFound, "its folder is not there yet");
        out.parent().unwrap().create_dir_all().unwrap();
        out.parent().unwrap().create_dir_all().unwrap();
        out.open_write(WriteMode::CreateNew).unwrap().write_all(b"one").unwrap();
        assert_eq!(code(out.open_write(WriteMode::CreateNew)), ErrorCode::Conflict);
        out.open_write(WriteMode::CreateOrTruncate).unwrap().write_all(b"two").unwrap();
        let written = site.project.join("new/deeper/out.txt");
        assert_eq!(fs::read_to_string(&written).unwrap(), "two");

        let mut replacement = out.replace().unwrap();
        replacement.write_all(b"three").unwrap();
        assert_eq!(fs::read_to_string(&written).unwrap(), "two", "the old file stays until the commit");
        replacement.commit().unwrap();
        assert_eq!(fs::read_to_string(&written).unwrap(), "three");
        // Dropped without a commit: nothing changes, and nothing is left behind.
        out.replace().unwrap().write_all(b"four").unwrap();
        assert_eq!(fs::read_to_string(&written).unwrap(), "three");
        assert_eq!(listing(&site.project.join("new/deeper")), ["out.txt"]);
    });
}

/// Temporary names handed out first, the ones a test plants, then fresh random ones.
fn planted_first(planted: &[String]) -> TempNames {
    let mut names = TempNames::random();
    names.fixed.extend(planted.iter().cloned());
    names
}

#[test]
fn temporary_names_are_random() {
    let names: Vec<String> = TempNames::random().map(Result::unwrap).collect();
    assert_eq!(names.len(), 16, "a bounded number of tries");
    for name in &names {
        let hex = name.strip_prefix(".omuzaki-").and_then(|rest| rest.strip_suffix(".tmp")).unwrap();
        assert!(hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
    }
    let distinct: std::collections::BTreeSet<&String> = names.iter().collect();
    assert_eq!(distinct.len(), names.len());
}

#[test]
fn planted_temporary_names_cannot_block_or_redirect_a_replacement() {
    both_ways(|| {
        let site = site();
        let sub = site.project.join("sub");
        // Every name the replacement will try first is taken: by links out of the project and by plain files.
        let planted: Vec<String> = (0..10).map(|n| format!(".planted-{n}")).collect();
        for (n, name) in planted.iter().enumerate() {
            if n % 2 == 0 {
                symlink(site.outside.join("sub/file.txt"), sub.join(name)).unwrap();
            } else {
                fs::write(sub.join(name), "planted").unwrap();
            }
        }
        // And the names an attacker could have guessed before temporary names were random.
        for n in 0..2_000 {
            fs::write(sub.join(format!(".omuzaki-{}-{n}.tmp", std::process::id())), "planted").unwrap();
        }
        let path = checked(&site, "sub/file.txt", FsMode::Write);
        for mut replacement in [path.replace().unwrap(), path.replace_naming(planted_first(&planted)).unwrap()] {
            replacement.write_all(b"replaced").unwrap();
            replacement.commit().unwrap();
            assert_eq!(fs::read_to_string(sub.join("file.txt")).unwrap(), "replaced");
        }
        assert_eq!(fs::read_to_string(site.outside.join("sub/file.txt")).unwrap(), "SECRET");
        assert_eq!(listing(&site.outside.join("sub")), ["file.txt", "w.txt"]);
        assert_eq!(listing(&sub).len(), 1 + 10 + 2_000, "the plants are untouched and nothing is left behind");

        // Only when every name it may try is taken does it give up, with `Conflict` and nothing left behind.
        let all: Vec<String> = (0..16).map(|n| format!(".planted-{}", n % 10)).collect();
        let result = path.replace_naming(planted_first(&all)).and_then(|mut replacement| {
            replacement.write_all(b"never").unwrap();
            replacement.commit()
        });
        assert_eq!(code(result), ErrorCode::Conflict);
        assert_eq!(fs::read_to_string(sub.join("file.txt")).unwrap(), "replaced");
        assert_eq!(listing(&sub).len(), 1 + 10 + 2_000);
    });
}

#[test]
fn a_replacement_keeps_the_permissions_of_the_file_it_replaces() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    both_ways(|| {
        let site = site();
        let sub = site.project.join("sub");
        // Set-user-ID is not carried over: new content does not inherit the old file's privileges.
        for (old, new) in [(0o600, 0o600), (0o640, 0o640), (0o755, 0o755), (0o4755, 0o755)] {
            let name = format!("mode-{old:o}.txt");
            fs::write(sub.join(&name), "old").unwrap();
            fs::set_permissions(sub.join(&name), fs::Permissions::from_mode(old)).unwrap();
            let mut replacement = checked(&site, &format!("sub/{name}"), FsMode::Write).replace().unwrap();
            // While it is written, nothing in the folder is wider than the old file (a named temporary file included).
            for entry in fs::read_dir(&sub).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().unwrap().to_string_lossy().starts_with(".omuzaki-") {
                    assert_eq!(mode(&path) & !new, 0, "{name}: the temporary file is {:o}", mode(&path));
                }
            }
            replacement.write_all(b"new").unwrap();
            replacement.commit().unwrap();
            assert_eq!(mode(&sub.join(&name)), new, "{name}");
            assert_eq!(fs::read_to_string(sub.join(&name)).unwrap(), "new");
        }
        // With no old file, the new one gets what any new file gets here (0666 narrowed by the umask).
        fs::write(sub.join("reference.txt"), "").unwrap();
        let mut fresh = checked(&site, "sub/fresh.txt", FsMode::Write).replace().unwrap();
        fresh.write_all(b"fresh").unwrap();
        fresh.commit().unwrap();
        assert_eq!(mode(&sub.join("fresh.txt")), mode(&sub.join("reference.txt")));
    });
}

/// On Linux the new file has no name at all while it is written (`O_TMPFILE`), so there is nothing to plant or swap.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn a_replacement_has_no_name_until_it_is_committed() {
    let site = site();
    let mut replacement = checked(&site, "sub/file.txt", FsMode::Write).replace().unwrap();
    replacement.write_all(b"new").unwrap();
    assert_eq!(listing(&site.project.join("sub")), ["file.txt"], "this needs a file system with `O_TMPFILE`");
    replacement.commit().unwrap();
    assert_eq!(listing(&site.project.join("sub")), ["file.txt"]);
    assert_eq!(fs::read_to_string(site.project.join("sub/file.txt")).unwrap(), "new");
}

#[test]
fn a_path_checked_for_one_mode_does_not_open_in_the_other() {
    let site = site();
    let read_only = checked(&site, "sub/file.txt", FsMode::Read);
    assert_eq!(code(read_only.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Internal);
    assert_eq!(code(read_only.replace()), ErrorCode::Internal);
    assert_eq!(code(read_only.create_dir_all()), ErrorCode::Internal);
    assert_eq!(code(checked(&site, "sub/file.txt", FsMode::Write).open_read()), ErrorCode::Internal);
    assert_eq!(fs::read_to_string(site.project.join("sub/file.txt")).unwrap(), "inside");
}

#[test]
fn a_link_at_the_last_name_is_refused_wherever_it_points() {
    both_ways(|| {
        let site = site();
        let (read_path, write_path) =
            (checked(&site, "sub/file.txt", FsMode::Read), checked(&site, "sub/file.txt", FsMode::Write));
        let name = site.project.join("sub/file.txt");
        // Swapped after the check: for a link out of the project, then for a link to another file in it.
        for target in [site.outside.join("sub/file.txt"), site.project.join("other.txt")] {
            fs::remove_file(&name).unwrap();
            symlink(&target, &name).unwrap();
            assert_eq!(code(read(&read_path)), ErrorCode::Denied, "{target:?}");
            assert_eq!(code(write_path.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Denied, "{target:?}");
            assert_eq!(code(write_path.open_write(WriteMode::CreateNew)), ErrorCode::Conflict, "something is there");
        }
        assert_eq!(fs::read_to_string(site.outside.join("sub/file.txt")).unwrap(), "SECRET");
        assert_eq!(fs::read_to_string(site.project.join("other.txt")).unwrap(), "other");
        // A replacement takes the name, not the link's target: the link itself is replaced.
        let mut replacement = write_path.replace().unwrap();
        replacement.write_all(b"new").unwrap();
        replacement.commit().unwrap();
        assert!(!fs::symlink_metadata(&name).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&name).unwrap(), "new");
        assert_eq!(fs::read_to_string(site.project.join("other.txt")).unwrap(), "other");
    });
}

#[test]
fn a_link_in_the_middle_is_refused_for_every_open() {
    both_ways(|| {
        let site = site();
        let read_path = checked(&site, "sub/file.txt", FsMode::Read);
        let write_path = checked(&site, "sub/new.txt", FsMode::Write);
        let folder = checked(&site, "sub/made/deeper", FsMode::Write);
        fs::rename(site.project.join("sub"), site.project.join("sub-real")).unwrap();
        symlink(site.outside.join("sub"), site.project.join("sub")).unwrap();

        assert_eq!(code(read(&read_path)), ErrorCode::Denied);
        assert_eq!(code(write_path.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Denied);
        assert_eq!(code(write_path.open_write(WriteMode::CreateNew)), ErrorCode::Denied);
        assert_eq!(code(write_path.replace()), ErrorCode::Denied);
        assert_eq!(code(folder.create_dir_all()), ErrorCode::Denied);
        assert_eq!(listing(&site.outside.join("sub")), ["file.txt", "w.txt"], "nothing was made outside");
        assert_eq!(fs::read_to_string(site.outside.join("sub/file.txt")).unwrap(), "SECRET");
    });
}

#[test]
fn the_allowed_folder_itself_swapped_for_a_link_is_refused() {
    both_ways(|| {
        let site = site();
        let read_path = checked(&site, "sub/file.txt", FsMode::Read);
        let write_path = checked(&site, "sub/new.txt", FsMode::Write);
        fs::rename(&site.project, site.base.join("project-real")).unwrap();
        symlink(&site.outside, &site.project).unwrap();
        assert_eq!(code(read(&read_path)), ErrorCode::Denied);
        assert_eq!(code(write_path.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Denied);
        assert_eq!(listing(&site.outside.join("sub")), ["file.txt", "w.txt"]);
    });
}

#[test]
fn dot_dot_is_gone_before_the_open() {
    // `evil/..` through the link would be `outside/sub`, where a `file.txt` waits; by name it is the project folder,
    // and the open only ever walks names.
    both_ways(|| {
        let site = site();
        fs::create_dir(site.outside.join("sub/deeper")).unwrap();
        symlink(site.outside.join("sub/deeper"), site.project.join("evil")).unwrap();
        let path = checked(&site, "evil/../sub/file.txt", FsMode::Read);
        assert_eq!(path.to_string(), site.project.join("sub/file.txt").to_str().unwrap());
        assert_eq!(read(&path).unwrap(), "inside");
    });
}

#[test]
fn a_hard_link_is_the_file_itself() {
    both_ways(|| {
        let site = site();
        fs::hard_link(site.outside.join("sub/file.txt"), site.project.join("sub/hard.txt")).unwrap();
        // A hard link is not a link to a file but a second name of it: reading it reads that file.
        assert_eq!(read(&checked(&site, "sub/hard.txt", FsMode::Read)).unwrap(), "SECRET");
        // Writing in place would change the file under its other name too, so it is refused...
        let write_path = checked(&site, "sub/hard.txt", FsMode::Write);
        assert_eq!(code(write_path.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Denied);
        // ...and a replacement takes this name only.
        let mut replacement = write_path.replace().unwrap();
        replacement.write_all(b"mine").unwrap();
        replacement.commit().unwrap();
        assert_eq!(fs::read_to_string(site.project.join("sub/hard.txt")).unwrap(), "mine");
        assert_eq!(fs::read_to_string(site.outside.join("sub/file.txt")).unwrap(), "SECRET");
    });
}

#[test]
fn only_a_regular_file_opens_as_a_file() {
    both_ways(|| {
        let site = site();
        for raw in ["sub", "."] {
            assert_eq!(code(checked(&site, raw, FsMode::Read).open_read()), ErrorCode::Invalid, "{raw}");
            let folder = checked(&site, raw, FsMode::Write);
            assert_eq!(code(folder.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Invalid, "{raw}");
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // A FIFO nobody writes to would block a plain open forever.
            use rustix::fs::{CWD, FileType, Mode, mknodat};
            mknodat(CWD, site.project.join("sub/pipe"), FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
            assert_eq!(code(checked(&site, "sub/pipe", FsMode::Read).open_read()), ErrorCode::Invalid);
            let pipe = checked(&site, "sub/pipe", FsMode::Write);
            assert_eq!(code(pipe.open_write(WriteMode::CreateOrTruncate)), ErrorCode::Invalid);
        }
    });
}

/// The race harness: one thread swaps a real folder or file for a link and back (atomically, `RENAME_EXCHANGE`, so
/// at every moment one name is the real entry and the other the link) while this thread opens the checked paths as
/// fast as it can. The verified opens must never reach the outside folder; a plain `File::open` of the same path
/// text, the control, must reach it, or the race was never on.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod race {
    use std::fs::File;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use rustix::fs::{CWD, Mode, OFlags, RenameFlags, openat, renameat_with};

    use super::*;

    /// How long one race runs, and the most rounds it plays: long enough for the control to escape many times, short
    /// enough for every CI run.
    const RACE_TIME: Duration = Duration::from_millis(1000);
    const MAX_ROUNDS: u32 = 200_000;

    /// What one race saw.
    #[derive(Debug, Default)]
    struct Tally {
        rounds: u32,
        swaps: u64,
        /// Verified reads that opened (and read the inside file).
        opened: u32,
        /// Verified opens refused because a link was in the way.
        refused: u32,
        /// Plain opens of the same text that read the outside file.
        control_escapes: u32,
    }

    /// Swaps the names `a` and `b` in `folder` until told to stop. Returns how many swaps it made.
    fn swapper(folder: &Path, a: &'static str, b: &'static str, stop: Arc<AtomicBool>) -> thread::JoinHandle<u64> {
        let fd = openat(CWD, folder, OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap();
        thread::spawn(move || {
            let mut swaps = 0;
            while !stop.load(Ordering::Relaxed) {
                renameat_with(&fd, a, &fd, b, RenameFlags::EXCHANGE).unwrap();
                swaps += 1;
            }
            swaps
        })
    }

    /// Plays the race: `swaps` run while each round reads `read_path` the verified way and the plain way, and runs
    /// `writes`, the verified writes of the scenario.
    fn play(read_path: &CanonicalPath, swaps: &[(PathBuf, &'static str, &'static str)], writes: impl Fn()) -> Tally {
        let stop = Arc::new(AtomicBool::new(false));
        let threads: Vec<_> = swaps.iter().map(|(dir, a, b)| swapper(dir, a, b, Arc::clone(&stop))).collect();
        let mut tally = Tally::default();
        let started = Instant::now();
        while started.elapsed() < RACE_TIME && tally.rounds < MAX_ROUNDS {
            tally.rounds += 1;
            match read(read_path) {
                Ok(text) => {
                    assert_eq!(text, "inside", "the verified open escaped");
                    tally.opened += 1;
                }
                Err(e) => {
                    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
                    tally.refused += 1;
                }
            }
            let mut text = String::new();
            let control = File::open(read_path.to_string()).and_then(|mut file| file.read_to_string(&mut text));
            if control.is_ok() && text == "SECRET" {
                tally.control_escapes += 1;
            }
            writes();
        }
        stop.store(true, Ordering::Relaxed);
        tally.swaps = threads.into_iter().map(|thread| thread.join().unwrap()).sum();
        tally
    }

    /// The verdict on a race: the control escaped (the race was real), the verified open did not (asserted in each
    /// round), both outcomes of the verified open happened, and nothing was written outside.
    fn judge(name: &str, site: &Site, tally: &Tally) {
        eprintln!("race {name}: {tally:?}");
        assert!(tally.control_escapes > 0, "{name}: the control never escaped, so the race never ran: {tally:?}");
        assert!(tally.opened > 0 && tally.refused > 0, "{name}: {tally:?}");
        assert_eq!(listing(&site.outside.join("sub")), ["file.txt", "w.txt"], "{name}: something was made outside");
        assert_eq!(fs::read_to_string(site.outside.join("sub/file.txt")).unwrap(), "SECRET", "{name}");
        assert_eq!(fs::read_to_string(site.outside.join("sub/w.txt")).unwrap(), "SECRET-W", "{name}");
    }

    /// Ignores the outcome of a write in the race: refused or done, it may only ever land inside, which `judge`
    /// checks afterwards.
    fn either<T>(_: Result<T, OpError>) {}

    #[test]
    fn a_folder_swapped_for_a_link_mid_open_never_lets_an_open_out() {
        both_ways(|| {
            let site = site();
            symlink(site.outside.join("sub"), site.project.join("sub-link")).unwrap();
            let read_path = checked(&site, "sub/file.txt", FsMode::Read);
            let (new, rep, made) = (
                checked(&site, "sub/new.txt", FsMode::Write),
                checked(&site, "sub/rep.txt", FsMode::Write),
                checked(&site, "sub/made/deeper", FsMode::Write),
            );
            let tally = play(&read_path, &[(site.project.clone(), "sub", "sub-link")], || {
                either(new.open_write(WriteMode::CreateOrTruncate).map(|mut file| file.write_all(b"PWNED")));
                either(rep.replace().map(|mut replacement| {
                    replacement.write_all(b"PWNED").unwrap();
                    replacement.commit()
                }));
                either(made.create_dir_all());
            });
            judge("folder", &site, &tally);
        });
    }

    #[test]
    fn a_file_swapped_for_a_link_mid_open_never_lets_an_open_out() {
        both_ways(|| {
            let site = site();
            let sub = site.project.join("sub");
            symlink(site.outside.join("sub/file.txt"), sub.join("file-link")).unwrap();
            fs::write(sub.join("w.txt"), "inside-w").unwrap();
            symlink(site.outside.join("sub/w.txt"), sub.join("w-link")).unwrap();
            let read_path = checked(&site, "sub/file.txt", FsMode::Read);
            let write_path = checked(&site, "sub/w.txt", FsMode::Write);
            let swaps = [(sub.clone(), "file.txt", "file-link"), (sub.clone(), "w.txt", "w-link")];
            let tally = play(&read_path, &swaps, || {
                either(write_path.open_write(WriteMode::CreateOrTruncate).map(|mut file| file.write_all(b"PWNED")));
            });
            judge("file", &site, &tally);
        });
    }

    #[test]
    fn the_allowed_folder_swapped_for_a_link_mid_open_never_lets_an_open_out() {
        both_ways(|| {
            let site = site();
            symlink(&site.outside, site.base.join("project-link")).unwrap();
            let read_path = checked(&site, "sub/file.txt", FsMode::Read);
            let write_path = checked(&site, "sub/new.txt", FsMode::Write);
            let tally = play(&read_path, &[(site.base.clone(), "project", "project-link")], || {
                either(write_path.open_write(WriteMode::CreateNew).map(drop));
            });
            judge("anchor", &site, &tally);
        });
    }
}

fn entry(name: &str, kind: EntryKind) -> DirEntryInfo {
    DirEntryInfo { name: name.to_owned(), kind }
}

#[test]
fn a_folder_is_listed_with_what_each_entry_is_and_a_link_is_listed_as_a_link() {
    both_ways(|| {
        let site = site();
        let sub = site.project.join("sub");
        fs::create_dir(sub.join("child")).unwrap();
        symlink(site.outside.join("sub"), sub.join("to-folder")).unwrap();
        symlink(site.outside.join("sub/file.txt"), sub.join("to-file")).unwrap();
        symlink(site.outside.join("missing"), sub.join("dangling")).unwrap();

        let entries = checked(&site, "sub", FsMode::Read).read_dir().unwrap();
        assert_eq!(
            entries,
            [
                entry("child", EntryKind::Folder),
                entry("dangling", EntryKind::Link),
                entry("file.txt", EntryKind::File),
                entry("to-file", EntryKind::Link),
                entry("to-folder", EntryKind::Link),
            ]
        );
        let top = checked(&site, ".", FsMode::Read).read_dir().unwrap();
        assert_eq!(top, [entry("other.txt", EntryKind::File), entry("sub", EntryKind::Folder)]);
        assert_eq!(checked(&site, "sub/child", FsMode::Read).read_dir().unwrap(), []);
    });
}

#[test]
fn a_folder_swapped_for_a_link_after_the_check_is_denied() {
    both_ways(|| {
        let site = site();
        let folder = checked(&site, "sub", FsMode::Read);
        let nested = checked(&site, "sub/file.txt", FsMode::Read);
        fs::rename(site.project.join("sub"), site.project.join("sub-real")).unwrap();
        symlink(site.outside.join("sub"), site.project.join("sub")).unwrap();
        assert_eq!(code(folder.read_dir()), ErrorCode::Denied);
        assert_eq!(code(nested.read_dir()), ErrorCode::Denied);

        // The allowed folder itself swapped for a link.
        let site = site_with_top_link();
        assert_eq!(code(site.1.read_dir()), ErrorCode::Denied);
        drop(site.0);
    });
}

/// A project folder that has been swapped for a link to the outside folder, and a path checked before the swap.
fn site_with_top_link() -> (Site, CanonicalPath) {
    let site = site();
    let top = checked(&site, ".", FsMode::Read);
    fs::rename(&site.project, site.base.join("project-real")).unwrap();
    symlink(&site.outside, &site.project).unwrap();
    (site, top)
}

#[test]
fn a_listing_needs_a_folder_that_is_there_and_a_path_checked_for_reading() {
    both_ways(|| {
        let site = site();
        assert_eq!(code(checked(&site, "sub/file.txt", FsMode::Read).read_dir()), ErrorCode::Denied, "a file");
        let gone = checked(&site, "sub", FsMode::Read);
        fs::remove_file(site.project.join("sub/file.txt")).unwrap();
        fs::remove_dir(site.project.join("sub")).unwrap();
        assert_eq!(code(gone.read_dir()), ErrorCode::NotFound);
        assert_eq!(code(checked(&site, "other", FsMode::Write).read_dir()), ErrorCode::Internal);
    });
}
