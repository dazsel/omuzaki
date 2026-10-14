//! `about.toml` (the `cargo-about` config behind `tools/release/notices.sh`) against `deny.toml` (the licence policy
//! gate): the notices bundle must accept exactly the licences the gate allows, and cover exactly the targets it checks.
//! Reading both files from disk keeps the two in step without a second list to maintain.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Reads a file from the repository root, the parent of this crate.
fn repo_file(name: &str) -> toml::Table {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

/// The strings in a TOML list. A missing list is empty, so a removed setting shows up as a diff, not a panic.
fn strings(list: Option<&toml::Value>) -> BTreeSet<String> {
    let Some(items) = list.and_then(toml::Value::as_array) else {
        return BTreeSet::new();
    };
    items.iter().map(|v| v.as_str().expect("a string list item").to_owned()).collect()
}

/// The licences `deny.toml` allows: its `[licenses] allow` list plus the licences of its per-crate `exceptions`.
/// An exception is a licence we accept for one crate only, so `cargo-about` must accept it too, or the notices fail.
fn deny_gate_accepts(deny: &toml::Table) -> BTreeSet<String> {
    let licenses = deny.get("licenses");
    let mut accepted = strings(licenses.and_then(|l| l.get("allow")));
    for exception in licenses.and_then(|l| l.get("exceptions")).and_then(toml::Value::as_array).into_iter().flatten() {
        accepted.extend(strings(exception.get("allow")));
    }
    accepted
}

/// Whether a table key named `key` appears anywhere in the document, at any depth.
fn has_key(value: &toml::Value, key: &str) -> bool {
    match value {
        toml::Value::Table(table) => table.iter().any(|(k, v)| k == key || has_key(v, key)),
        toml::Value::Array(items) => items.iter().any(|v| has_key(v, key)),
        _ => false,
    }
}

/// One line naming what each side has and the other lacks, or `None` when the two sets are equal.
fn difference(what: &str, left: (&str, &BTreeSet<String>), right: (&str, &BTreeSet<String>)) -> Option<String> {
    let only_left: Vec<&String> = left.1.difference(right.1).collect();
    let only_right: Vec<&String> = right.1.difference(left.1).collect();
    if only_left.is_empty() && only_right.is_empty() {
        return None;
    }
    Some(format!("{what} differ: only in {}: {only_left:?}; only in {}: {only_right:?}", left.0, right.0))
}

#[test]
fn about_accepts_exactly_the_licences_the_deny_gate_allows() {
    let about = repo_file("about.toml");
    let deny = repo_file("deny.toml");
    let accepted = strings(about.get("accepted"));
    let allowed = deny_gate_accepts(&deny);
    let diff = difference("licences", ("about.toml accepted", &accepted), ("deny.toml allow + exceptions", &allowed));
    assert!(diff.is_none(), "{}", diff.unwrap_or_default());
}

/// `[[…clarify.git]]` entries make cargo-about fetch licence files over the network. The bundle must stay offline, so a
/// clarification added later fails here, and the author has to say why it is needed.
#[test]
fn about_has_no_clarify_entries_so_the_bundle_never_goes_online() {
    let about = toml::Value::Table(repo_file("about.toml"));
    assert!(!has_key(&about, "clarify"), "about.toml has a [clarify] entry, which fetches over the network");
}

#[test]
fn about_targets_are_the_targets_the_deny_gate_checks() {
    let about = repo_file("about.toml");
    let deny = repo_file("deny.toml");
    let about_targets = strings(about.get("targets"));
    let deny_targets = strings(deny.get("graph").and_then(|g| g.get("targets")));
    assert!(!deny_targets.is_empty(), "deny.toml [graph] targets is empty: nothing to compare");
    let diff =
        difference("targets", ("about.toml targets", &about_targets), ("deny.toml [graph] targets", &deny_targets));
    assert!(diff.is_none(), "{}", diff.unwrap_or_default());
}

/// `tools/release/notices.sh` end to end, in a throwaway repository with a stub `cargo` standing in for cargo-about and
/// cargo metadata. Unix only: the script is bash. The script is compiled in, so the test always runs the current one.
#[cfg(unix)]
mod script {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    use tempfile::TempDir;

    use crate::support::Fixture;

    const SCRIPT: &str = include_str!("../../tools/release/notices.sh");
    const ABOUT_HBS: &str = include_str!("../../about.hbs");
    const ABOUT_TOML: &str = include_str!("../../about.toml");
    const FONT_FILES: [&str; 4] = ["OFL.txt", "UFL.txt", "Hack-Regular.txt", "emoji-icon-font-mit-license.txt"];

    /// A repository with the script, its config and one tracked NOTICES.md, a font folder, and a stub `cargo`.
    struct Repo {
        fixture: Fixture,
        fonts: TempDir,
        bin: TempDir,
        out_dir: TempDir,
    }

    impl Repo {
        fn new() -> Self {
            let fixture = Fixture::new();
            fixture.write("tools/release/notices.sh", SCRIPT);
            fixture.write("about.toml", ABOUT_TOML);
            fixture.write("about.hbs", ABOUT_HBS);
            fixture.write("crates/demo/NOTICES.md", "demo notice\n");
            fixture.commit_all("base");

            let fonts = TempDir::new().unwrap();
            fs::create_dir_all(fonts.path().join("fonts")).unwrap();
            for name in FONT_FILES {
                fs::write(fonts.path().join("fonts").join(name), format!("{name} text\n")).unwrap();
            }

            // The metadata answer points the script at the scratch font folder: its dirname plus `fonts/`.
            let manifest = fonts.path().join("Cargo.toml");
            let stub = format!(
                r#"#!/bin/sh
case "$1 $2" in
  "about --version") echo "cargo-about 0.9.2" ;;
  "about generate") echo "GENERATED LICENCE TEXT" ;;
  "metadata "*) printf '{{"packages":[{{"name":"epaint_default_fonts","manifest_path":"{}"}}]}}\n' ;;
  *) echo "stub cargo: unexpected call: $*" >&2; exit 99 ;;
esac
"#,
                manifest.display()
            );
            let bin = TempDir::new().unwrap();
            let cargo = bin.path().join("cargo");
            fs::write(&cargo, stub).unwrap();
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();

            Self { fixture, fonts, bin, out_dir: TempDir::new().unwrap() }
        }

        fn out(&self) -> std::path::PathBuf {
            self.out_dir.path().join("THIRD-PARTY-NOTICES.txt")
        }

        /// Runs the script with `--out`. With `file_limit_kib`, the shell caps the file size first and ignores SIGXFSZ, so a
        /// write over the cap fails with EFBIG, as on a full disk, instead of killing the shell.
        fn run(&self, out: &Path, file_limit_kib: Option<u32>) -> Output {
            let path = format!("{}:{}", self.bin.path().display(), std::env::var("PATH").unwrap_or_default());
            let mut command = match file_limit_kib {
                None => {
                    let mut command = Command::new("bash");
                    command.args(["tools/release/notices.sh", "--out"]).arg(out);
                    command
                }
                Some(kib) => {
                    let script =
                        format!("trap '' XFSZ; ulimit -f {kib}; exec bash tools/release/notices.sh --out \"$1\"");
                    let mut command = Command::new("sh");
                    command.args(["-c", &script, "sh"]).arg(out);
                    command
                }
            };
            command.current_dir(self.fixture.root()).env("PATH", path).output().unwrap()
        }

        fn good_bundle(&self) -> Vec<u8> {
            let run = self.run(&self.out(), None);
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            fs::read(self.out()).unwrap()
        }

        /// How many entries the output folder holds: the bundle alone, or the bundle plus a leftover temp file.
        fn entries_in_out_dir(&self) -> usize {
            fs::read_dir(self.out_dir.path()).unwrap().count()
        }
    }

    #[test]
    fn a_failed_git_listing_exits_2_and_keeps_the_previous_bundle() {
        let repo = Repo::new();
        let good = repo.good_bundle();
        // Without `.git`, `git ls-files` fails, which discovery has to report rather than read as "no notices".
        fs::remove_dir_all(repo.fixture.root().join(".git")).unwrap();

        let run = repo.run(&repo.out(), None);
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("git ls-files failed"), "{}", stderr(&run));
        assert_eq!(fs::read(repo.out()).unwrap(), good, "the failed run changed the previous bundle");
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }

    #[test]
    fn no_tracked_notice_exits_2_and_keeps_the_previous_bundle() {
        let repo = Repo::new();
        let good = repo.good_bundle();
        // Untracked but still on disk: discovery must not find it, so the bundle would be missing a notice.
        repo.fixture.git(&["rm", "-q", "--cached", "crates/demo/NOTICES.md"]);

        let run = repo.run(&repo.out(), None);
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("no NOTICES.md is tracked"), "{}", stderr(&run));
        assert_eq!(fs::read(repo.out()).unwrap(), good, "the failed run changed the previous bundle");
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }

    fn code(run: &Output) -> Option<i32> {
        run.status.code()
    }

    fn stderr(run: &Output) -> String {
        String::from_utf8_lossy(&run.stderr).into_owned()
    }

    #[test]
    fn a_run_with_every_notice_readable_writes_the_bundle() {
        let repo = Repo::new();
        let text = String::from_utf8(repo.good_bundle()).unwrap();
        assert!(text.contains("GENERATED LICENCE TEXT"), "{text}");
        assert!(text.contains("==== Fonts bundled by egui ===="), "{text}");
        assert!(text.contains("--- crates/demo/NOTICES.md ---"), "{text}");
    }

    #[test]
    fn a_missing_tracked_notice_exits_2_and_keeps_the_previous_bundle() {
        let repo = Repo::new();
        let good = repo.good_bundle();
        fs::remove_file(repo.fixture.root().join("crates/demo/NOTICES.md")).unwrap();

        let run = repo.run(&repo.out(), None);
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("cannot read crates/demo/NOTICES.md"), "{}", stderr(&run));
        assert_eq!(fs::read(repo.out()).unwrap(), good, "the failed run changed the previous bundle");
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }

    #[test]
    fn a_failed_write_exits_2_and_keeps_the_previous_bundle() {
        let repo = Repo::new();
        let good = repo.good_bundle();
        // A font bigger than the 64 KiB cap: its append fails partway, after the earlier sections were written.
        fs::write(repo.fonts.path().join("fonts/OFL.txt"), vec![b'x'; 200 * 1024]).unwrap();

        let run = repo.run(&repo.out(), Some(64));
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("cannot write"), "{}", stderr(&run));
        assert_eq!(fs::read(repo.out()).unwrap(), good, "the failed write changed the previous bundle");
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed write left a temp file behind");
    }

    #[test]
    fn an_output_path_that_is_a_directory_exits_2_and_writes_nothing() {
        let repo = Repo::new();
        let run = repo.run(repo.out_dir.path(), None);
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("is a directory"), "{}", stderr(&run));
        assert_eq!(repo.entries_in_out_dir(), 0, "a hidden file was written into the directory");
    }

    /// Puts a `git` shim first on PATH that fails `ls-files` and hands every other call to the real git.
    fn shim_git_ls_files(repo: &Repo) {
        let path = std::env::var("PATH").unwrap_or_default();
        let real = path
            .split(':')
            .map(|dir| Path::new(dir).join("git"))
            .find(|candidate| candidate.is_file())
            .expect("a real git on PATH");
        let shim = format!(
            "#!/bin/sh\nif [ \"$1\" = ls-files ]; then echo 'shim: ls-files refused' >&2; exit 1; fi\nexec {} \"$@\"\n",
            real.display()
        );
        let git = repo.bin.path().join("git");
        fs::write(&git, shim).unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_git_listing_that_fails_in_a_healthy_repo_exits_2_and_keeps_the_previous_bundle() {
        let repo = Repo::new();
        let good = repo.good_bundle();
        shim_git_ls_files(&repo);

        let run = repo.run(&repo.out(), None);
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("git ls-files failed"), "{}", stderr(&run));
        assert_eq!(fs::read(repo.out()).unwrap(), good, "the failed run changed the previous bundle");
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }

    #[test]
    fn a_failed_write_of_the_output_path_to_stdout_exits_2() {
        let repo = Repo::new();
        let path = format!("{}:{}", repo.bin.path().display(), std::env::var("PATH").unwrap_or_default());
        // A read-only file as stdout fails every write with EBADF on any unix, unlike /dev/full, which macOS lacks.
        let read_only = fs::File::open(repo.fixture.root().join("about.toml")).unwrap();
        let run = Command::new("bash")
            .args(["tools/release/notices.sh", "--out"])
            .arg(repo.out())
            .current_dir(repo.fixture.root())
            .env("PATH", path)
            .stdout(read_only)
            .output()
            .unwrap();
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("cannot write the output path"), "{}", stderr(&run));
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }

    #[test]
    fn a_closed_stdout_pipe_exits_2_with_a_reason() {
        let repo = Repo::new();
        let path = format!("{}:{}", repo.bin.path().display(), std::env::var("PATH").unwrap_or_default());
        let mut child = Command::new("bash")
            .args(["tools/release/notices.sh", "--out"])
            .arg(repo.out())
            .current_dir(repo.fixture.root())
            .env("PATH", path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Drop the only read end before the script writes its answer: the write then meets a pipe with no reader.
        drop(child.stdout.take());
        let run = child.wait_with_output().unwrap();
        assert_eq!(code(&run), Some(2), "{}", stderr(&run));
        assert!(stderr(&run).contains("cannot write the output path"), "{}", stderr(&run));
        assert_eq!(repo.entries_in_out_dir(), 1, "the failed run left a temp file behind");
    }
}
