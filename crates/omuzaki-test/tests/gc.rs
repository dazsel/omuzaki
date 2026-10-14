//! End-to-end tests of `omuzaki-test gc` against real git repositories: branches, remote-tracking branches, worktrees,
//! uncommitted blesses and tags, because the whole point is that no branch's golden is ever collected.
// Test helpers sit outside `#[test]` functions, where clippy.toml's unwrap exemption doesn't reach.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{arg, code, run, text};
use omuzaki_core::id::Slug;
use omuzaki_store_api::ContentHash;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::gc::{self, Staleness, TEST_CLOCK_ENV, TEST_CLOCK_MARKER, Warning};
use omuzaki_test::goldens::Goldens;
use omuzaki_test::manifest::{Entry, Manifest};
use omuzaki_test::objects::ObjectStore;
use tempfile::TempDir;

const DAY: Duration = Duration::from_hours(24);

/// How far the fixtures' "today" is ahead of the real clock. The kernel stamps every file with the real time of its last
/// change and nothing can set that back, so an image can only look old to `gc` if `gc` is told it is later than it is:
/// the library gets `Fixture::today`, the command line `OMUZAKI_GC_TEST_NOW`. Longer than any age a test uses.
const ADVANCE: Duration = Duration::from_hours(40 * 24);

/// Runs git in `cwd`, isolated from the developer's own configuration, and returns its output.
fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", text(&output.stderr));
    text(&output.stdout)
}

/// Writes `<checkout>/tests/goldens/<suite>.toml` naming `cases` (replacing the file).
fn write_manifest(checkout: &Path, suite: &str, cases: &[(&str, ContentHash)]) {
    let path = checkout.join("tests/goldens").join(format!("{suite}.toml"));
    let _ = fs::remove_file(&path);
    Manifest::update(&path, suite, |manifest| {
        for (name, hash) in cases {
            manifest.insert(Slug::new(*name).unwrap(), Entry::new(*hash, Tolerance::EXACT));
        }
    })
    .unwrap();
    let _ = fs::remove_file(path.with_extension("toml.lock"));
}

/// Commits everything in `checkout`, new files included.
fn commit(checkout: &Path, message: &str) {
    git(checkout, &["add", "-A"]);
    git(checkout, &["commit", "-q", "-m", message, "--allow-empty"]);
}

/// A git repository with a golden store next to it, and helpers to put history and images into both.
struct Fixture {
    dir: TempDir,
    /// The day `gc` runs on, in whole seconds so that the command line is given exactly this time.
    today: SystemTime,
}

impl Fixture {
    fn new() -> Self {
        let real = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let today = UNIX_EPOCH + Duration::from_secs(real) + ADVANCE;
        let fixture = Self { dir: tempfile::tempdir().unwrap(), today };
        // The mark of a store of the test-suite: without it `gc` refuses to take its clock from `OMUZAKI_GC_TEST_NOW`.
        fs::create_dir_all(fixture.store()).unwrap();
        fs::write(fixture.store().join(TEST_CLOCK_MARKER), b"a store made by tests/gc.rs\n").unwrap();
        fs::create_dir_all(fixture.repo().join("tests/goldens")).unwrap();
        fs::write(fixture.repo().join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&fixture.repo(), &["init", "-q", "-b", "main"]);
        fixture
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn store(&self) -> PathBuf {
        self.dir.path().join("goldens")
    }

    fn manifests(&self) -> PathBuf {
        self.repo().join("tests/goldens")
    }

    /// Stores an image called `label` as it was stored `age` before today, and returns its hash. Its change time is the
    /// real moment of this call, [`ADVANCE`] before today: old, but not as old as its modification time may say.
    fn image(&self, label: &str, age: Duration) -> ContentHash {
        self.image_modified(label, self.today - age)
    }

    /// Stores an image called `label` whose modification time is `modified`, whenever that is.
    fn image_modified(&self, label: &str, modified: SystemTime) -> ContentHash {
        let objects = ObjectStore::new(&self.store());
        let hash = objects.put(format!("image {label}").as_bytes()).unwrap();
        let file = fs::File::options().write(true).open(objects.path(&hash)).unwrap();
        file.set_modified(modified).unwrap();
        hash
    }

    fn goldens(&self) -> Goldens {
        self.goldens_for(&self.manifests())
    }

    /// The store with the manifest folder `manifests` (of the main checkout, a worktree or a clone).
    fn goldens_for(&self, manifests: &Path) -> Goldens {
        Goldens::new(self.store(), manifests.to_owned(), self.dir.path().join("out"))
    }

    /// `omuzaki-test gc` on the main checkout, with `extra` arguments.
    fn gc(&self, extra: &[&str]) -> std::process::Output {
        self.gc_at(&self.manifests(), extra)
    }

    /// `omuzaki-test gc` pointed at the manifest folder `manifests`, with `extra` arguments, run on [`Fixture::today`].
    fn gc_at(&self, manifests: &Path, extra: &[&str]) -> std::process::Output {
        let today = self.today.duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();
        self.gc_with(manifests, extra, &[(TEST_CLOCK_ENV, &today)])
    }

    /// `omuzaki-test gc` pointed at `manifests`, with the environment `envs` and so, without `OMUZAKI_GC_TEST_NOW`, by the
    /// real clock.
    fn gc_with(&self, manifests: &Path, extra: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
        let store = self.store();
        run(&[&["gc", "--goldens", arg(&store), "--manifests", arg(manifests)], extra].concat(), envs)
    }

    fn stored(&self) -> BTreeSet<String> {
        let names = fs::read_dir(self.store().join("objects")).unwrap().map(|e| e.unwrap().file_name());
        names.map(|name| name.to_str().unwrap().trim_end_matches(".png").to_owned()).collect()
    }
}

/// Dates the record of the last `git fetch` of the main checkout of the clone at `clone` as `when`.
fn record_fetch(clone: &Path, when: SystemTime) {
    date(&clone.join(".git/FETCH_HEAD"), when);
}

/// Sets the modification time of the existing file `path`.
fn date(path: &Path, when: SystemTime) {
    fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
}

/// The hashes a gc output lists after `prefix` (`would delete: ` or `deleted: `), whatever the folder.
fn listed(output: &str, prefix: &str) -> BTreeSet<String> {
    let name = |line: &str| line.rsplit('/').next().unwrap().split(".png").next().unwrap().to_owned();
    output.lines().filter_map(|line| line.strip_prefix(prefix)).map(name).collect()
}

fn names(hashes: &[ContentHash]) -> BTreeSet<String> {
    hashes.iter().map(ToString::to_string).collect()
}

/// A repository where every kind of reference holds one image no other reference holds, plus two images nobody wants.
struct Scenario {
    fixture: Fixture,
    /// Images that must survive any gc.
    wanted: Vec<ContentHash>,
    /// Old images nobody names: the only ones gc may delete.
    garbage: Vec<ContentHash>,
    /// An image nobody names, but stored two days ago.
    young: ContentHash,
}

fn scenario() -> Scenario {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let old = 30 * DAY;
    let (replaced, orphan) = (fixture.image("replaced", old), fixture.image("orphan", old));
    let (front, side, remote, uncommitted, detached, wip) = (
        fixture.image("main front", old),
        fixture.image("only on local branch feature", old),
        fixture.image("only on remote-tracking branch pr-7", old),
        fixture.image("only in an uncommitted manifest of the main checkout", old),
        fixture.image("only on a detached HEAD", old),
        fixture.image("only in an uncommitted manifest of a worktree", old),
    );
    let young = fixture.image("young", 2 * DAY);

    // main: `replaced` was blessed first, then moved on to `front`: only the older commit names it.
    write_manifest(&repo, "render", &[("front", replaced)]);
    commit(&repo, "bless replaced");
    write_manifest(&repo, "render", &[("front", front)]);
    commit(&repo, "bless front");

    // A local branch that is not checked out anywhere.
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    write_manifest(&repo, "render", &[("front", front), ("side", side)]);
    commit(&repo, "bless side");
    git(&repo, &["checkout", "-q", "main"]);

    // A remote-tracking branch (an open PR) with no local branch of the same name.
    git(&repo, &["checkout", "-q", "-b", "scratch"]);
    write_manifest(&repo, "render", &[("front", front), ("remote", remote)]);
    commit(&repo, "bless remote");
    git(&repo, &["update-ref", "refs/remotes/origin/pr-7", "HEAD"]);
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "scratch"]);

    // A worktree on its own branch with a bless that is not committed yet.
    let wip_tree = fixture.dir.path().join("wt-wip");
    git(&repo, &["worktree", "add", "-q", arg(&wip_tree), "-b", "wip"]);
    write_manifest(&wip_tree, "face", &[("neutral", wip)]);

    // A worktree on a detached HEAD whose only commit is on no branch.
    let detached_tree = fixture.dir.path().join("wt-detached");
    git(&repo, &["worktree", "add", "-q", "--detach", arg(&detached_tree)]);
    write_manifest(&detached_tree, "render", &[("front", detached)]);
    commit(&detached_tree, "bless on a detached head");
    // The files on disk go back to the previous commit's, so only the checked-out commit still names the image.
    git(&detached_tree, &["checkout", "-q", "HEAD~1", "--", "tests/goldens/render.toml"]);

    // The main checkout itself, with a bless not committed yet in a manifest git has never seen.
    write_manifest(&repo, "extra", &[("case", uncommitted)]);

    Scenario {
        fixture,
        wanted: vec![front, side, remote, uncommitted, detached, wip],
        garbage: vec![replaced, orphan],
        young,
    }
}

#[test]
fn the_default_is_a_dry_run_that_lists_exactly_the_unreferenced_old_images_and_deletes_nothing() {
    let s = scenario();
    let before = s.fixture.stored();

    let output = s.fixture.gc(&[]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", text(&output.stderr));
    assert_eq!(listed(&out, "would delete: "), names(&s.garbage), "{out}");
    assert!(out.contains(&format!("kept (younger than 7 days): {}", s.fixture.store().display())), "{out}");
    assert!(out.contains(&s.young.to_string()), "{out}");
    assert!(out.contains("dry run: 2 images") && out.contains("nothing was changed"), "{out}");
    assert!(out.contains("6 images named by a manifest, 1 unreferenced but too young"), "{out}");
    assert_eq!(s.fixture.stored(), before, "a dry run changes nothing");
    assert_eq!(text(&output.stderr), "", "no remote to be behind, no clock out of line: nothing to warn about");

    let spelled_out = s.fixture.gc(&["--dry-run"]);
    assert_eq!(text(&spelled_out.stdout), out, "--dry-run is the default, spelled out");
    assert_eq!(code(&s.fixture.gc(&["--dry-run", "--apply"])), 2, "the two flags contradict each other");
}

#[test]
fn apply_deletes_only_the_garbage_and_never_what_any_branch_or_worktree_names() {
    let s = scenario();
    let output = s.fixture.gc(&["--apply"]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", text(&output.stderr));
    assert_eq!(listed(&out, "deleted: "), names(&s.garbage), "{out}");
    assert!(out.contains("deleted 2 images"), "{out}");

    let mut survivors = names(&s.wanted);
    survivors.insert(s.young.to_string());
    assert_eq!(s.fixture.stored(), survivors, "every wanted image and the young one survive");

    let again = text(&s.fixture.gc(&["--apply"]).stdout);
    assert!(again.contains("deleted 0 images"), "a second run has nothing left to do: {again}");
}

#[test]
fn the_same_images_survive_when_gc_runs_from_another_worktree() {
    let s = scenario();
    let wip_manifests = s.fixture.dir.path().join("wt-wip/tests/goldens");
    let output = s.fixture.gc_at(&wip_manifests, &["--apply"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert_eq!(listed(&text(&output.stdout), "deleted: "), names(&s.garbage));
}

#[test]
fn nothing_younger_than_seven_days_is_deleted_and_the_edge_is_sharp() {
    let fixture = Fixture::new();
    let (repo, manifests) = (fixture.repo(), fixture.manifests());
    let kept = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", kept)]);
    commit(&repo, "bless");
    let just_under = fixture.image("under", Duration::from_hours(7 * 24 - 1));
    let just_over = fixture.image("over", Duration::from_hours(7 * 24 + 1));
    let fresh = fixture.image("fresh", Duration::ZERO);
    let plan = gc::plan(&fixture.goldens(), &[], fixture.today).unwrap();

    let young: Vec<_> = plan.young.iter().map(|o| o.hash).collect();
    let garbage: Vec<_> = plan.garbage.iter().map(|o| o.hash).collect();
    assert_eq!((names(&young), names(&garbage)), (names(&[just_under, fresh]), names(&[just_over])));
    assert_eq!(plan.wanted, 1);
    assert_eq!(manifests, fixture.manifests());

    // The same store a week later: now the young images are old enough.
    let later = gc::plan(&fixture.goldens(), &[], fixture.today + 8 * DAY).unwrap();
    assert_eq!(
        names(&later.garbage.iter().map(|o| o.hash).collect::<Vec<_>>()),
        names(&[just_under, just_over, fresh])
    );
    assert_eq!(later.young, Vec::new());
}

#[test]
fn an_image_chosen_again_after_the_plan_was_made_is_not_deleted() {
    let fixture = Fixture::new();
    let (repo, goldens) = (fixture.repo(), fixture.goldens());
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let stale = fixture.image("stale", 30 * DAY);
    let rechosen = fixture.image("rechosen", 30 * DAY);

    let plan = gc::plan(&goldens, &[], fixture.today).unwrap();
    assert_eq!(plan.garbage.len(), 2);
    // A bless picks the old image again between the plan and the deletion; its manifest isn't written yet.
    goldens.objects().put(b"image rechosen").unwrap();

    let removal = plan.apply().unwrap();
    assert_eq!(names(&removal.removed.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[stale]));
    assert_eq!(names(&removal.touched.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[rechosen]));
    assert!(goldens.objects().path(&rechosen).exists() && !goldens.objects().path(&stale).exists());
}

#[test]
fn extra_revisions_only_add_to_what_is_kept() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (on_main, only_orphaned) = (fixture.image("main", 30 * DAY), fixture.image("orphaned", 30 * DAY));
    write_manifest(&repo, "render", &[("front", on_main)]);
    commit(&repo, "bless main");
    // A commit nothing points at any more: not a branch, a tag, a stash or a worktree's HEAD.
    git(&repo, &["checkout", "-q", "-b", "release"]);
    write_manifest(&repo, "render", &[("front", only_orphaned)]);
    commit(&repo, "bless release");
    let orphan = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "release"]);

    let without = text(&fixture.gc(&[]).stdout);
    assert_eq!(listed(&without, "would delete: "), names(&[only_orphaned]), "nothing names the commit: {without}");

    let with = fixture.gc(&["--keep-manifests-of", &orphan]);
    assert_eq!(code(&with), 0, "{}", text(&with.stderr));
    assert!(listed(&text(&with.stdout), "would delete: ").is_empty());

    let unknown = fixture.gc(&["--apply", "--keep-manifests-of", "no-such-tag"]);
    assert_eq!(code(&unknown), 2);
    assert!(text(&unknown.stderr).contains("no-such-tag"), "{}", text(&unknown.stderr));
    assert!(
        fixture.stored().contains(&only_orphaned.to_string()),
        "an unknown revision stops the run before any deletion"
    );
}

#[test]
fn a_stash_whose_log_is_gone_still_keeps_what_it_and_its_untracked_files_hold() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (base, untracked, orphan) = (
        fixture.image("main", 30 * DAY),
        fixture.image("untracked in the stash", 30 * DAY),
        fixture.image("orphan", 30 * DAY),
    );
    write_manifest(&repo, "render", &[("front", base)]);
    commit(&repo, "bless main");
    write_manifest(&repo, "fresh", &[("case", untracked)]);
    git(&repo, &["stash", "-q", "--include-untracked"]);
    git(&repo, &["reflog", "expire", "--expire=now", "--expire-unreachable=now", "refs/stash"]);
    assert!(git(&repo, &["stash", "list"]).trim().is_empty(), "the log is gone, the ref is not");
    git(&repo, &["rev-parse", "--verify", "refs/stash"]);

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "{}", error_of(&output));
    assert_eq!(listed(&text(&output.stdout), "deleted: "), names(&[orphan]));
    assert!(fixture.stored().contains(&untracked.to_string()));
}

/// Commits a manifest naming `image` on a branch that is deleted again right away, running each of `while_on_it` (git
/// arguments) first: whatever they create is then the only thing that still names that commit.
fn commit_on_a_branch_that_is_then_deleted(repo: &Path, image: ContentHash, while_on_it: &[&[&str]]) {
    git(repo, &["checkout", "-q", "-b", "gone"]);
    write_manifest(repo, "render", &[("front", image)]);
    commit(repo, "bless");
    for args in while_on_it {
        git(repo, args);
    }
    git(repo, &["checkout", "-q", "main"]);
    git(repo, &["branch", "-q", "-D", "gone"]);
}

#[test]
fn tags_and_every_stash_entry_keep_their_images_but_a_dropped_stash_does_not() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let [base, light, annotated, nested, treed, blobbed, stash_tracked, stash_untracked, dropped, orphan] = [
        "main",
        "light",
        "annotated",
        "nested",
        "treed",
        "blobbed",
        "stash tracked",
        "stash untracked",
        "dropped",
        "orphan",
    ]
    .map(|label| fixture.image(label, 30 * DAY));
    write_manifest(&repo, "render", &[("front", base)]);
    commit(&repo, "bless main");

    // Each tag is the only thing naming a commit whose branch is gone.
    commit_on_a_branch_that_is_then_deleted(&repo, light, &[&["tag", "light"]]);
    commit_on_a_branch_that_is_then_deleted(&repo, annotated, &[&["tag", "-a", "-m", "note", "annotated"]]);
    let tag_of_a_tag: &[&[&str]] = &[
        &["tag", "-a", "-m", "inner", "inner"],
        &["tag", "-a", "-m", "outer", "nested", "inner"],
        &["tag", "-d", "inner"],
    ];
    commit_on_a_branch_that_is_then_deleted(&repo, nested, tag_of_a_tag);
    // A tag straight on a tree is read; one on a file holds no folder, so that commit's image is garbage.
    commit_on_a_branch_that_is_then_deleted(&repo, treed, &[&["tag", "tree-tag", "HEAD^{tree}"]]);
    commit_on_a_branch_that_is_then_deleted(&repo, blobbed, &[&["tag", "blob-tag", "HEAD:tests/goldens/render.toml"]]);

    // Stash entries: one dropped, an older one with an untracked manifest, a newer one with a tracked change.
    write_manifest(&repo, "dropped", &[("case", dropped)]);
    git(&repo, &["stash", "-q", "--include-untracked"]);
    git(&repo, &["stash", "drop", "-q"]);
    write_manifest(&repo, "fresh", &[("case", stash_untracked)]);
    git(&repo, &["stash", "-q", "--include-untracked"]);
    write_manifest(&repo, "render", &[("front", stash_tracked)]);
    git(&repo, &["stash", "-q"]);
    assert_eq!(git(&repo, &["stash", "list"]).lines().count(), 2);

    let output = fixture.gc(&["--apply"]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", text(&output.stderr));
    assert_eq!(listed(&out, "deleted: "), names(&[dropped, orphan, blobbed]), "{out}");
    let kept = names(&[base, light, annotated, nested, treed, stash_tracked, stash_untracked]);
    assert!(kept.is_subset(&fixture.stored()), "every tag and stash entry keeps its image: {out}");
}

#[test]
fn gc_stops_without_deleting_when_git_cannot_be_read_completely() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let named = fixture.image("named", 30 * DAY);
    let garbage = fixture.image("garbage", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");

    // Another branch has a manifest this tool cannot parse: it might name anything, so nothing may be deleted.
    git(&repo, &["checkout", "-q", "-b", "broken"]);
    fs::write(repo.join("tests/goldens/render.toml"), "[front\n").unwrap();
    commit(&repo, "break the manifest");
    git(&repo, &["checkout", "-q", "main"]);
    let broken = fixture.gc(&["--apply"]);
    assert_eq!(code(&broken), 2);
    assert!(text(&broken.stderr).contains("render.toml"), "{}", text(&broken.stderr));
    assert!(fixture.stored().contains(&garbage.to_string()));
}

#[test]
fn a_manifest_folder_outside_any_git_checkout_or_without_any_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, manifests) = (dir.path().join("goldens"), dir.path().join("loose-manifests"));
    let object = ObjectStore::new(&store).put(b"image").unwrap();
    fs::File::options()
        .write(true)
        .open(ObjectStore::new(&store).path(&object))
        .unwrap()
        .set_modified(SystemTime::now() - 30 * DAY)
        .unwrap();
    fs::create_dir_all(&manifests).unwrap();

    let outside = run(&["gc", "--apply", "--goldens", arg(&store), "--manifests", arg(&manifests)], &[]);
    assert_eq!(code(&outside), 2);
    assert!(text(&outside.stderr).contains("git"), "{}", text(&outside.stderr));

    // Inside a repository, but pointing at a folder no branch has manifests in: a typo, not a store full of garbage.
    let fixture = Fixture::new();
    let wrong = fixture.repo().join("tests/wrong-folder");
    let stored = fixture.image("old", 30 * DAY);
    commit(&fixture.repo(), "empty");
    let store = fixture.store();
    let typo = run(&["gc", "--apply", "--goldens", arg(&store), "--manifests", arg(&wrong)], &[]);
    assert_eq!(code(&typo), 2);
    assert!(text(&typo.stderr).contains("no golden manifest found") && text(&typo.stderr).contains("--manifests"));
    assert!(fixture.stored().contains(&stored.to_string()));
}

#[test]
fn files_that_are_not_images_are_left_alone_and_an_empty_store_is_fine() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let objects = fixture.store().join("objects");
    let strays = [
        objects.join("notes.txt"),
        objects.join(format!("{}.png.1-0.tmp", fixture.image("x", DAY))),
        objects.join("UPPER.png"),
    ];
    for stray in &strays {
        fs::write(stray, b"not an object").unwrap();
        fs::File::options().write(true).open(stray).unwrap().set_modified(fixture.today - 30 * DAY).unwrap();
    }
    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(strays.iter().all(|stray| stray.exists()));

    let empty = Fixture::new();
    let output = empty.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("deleted 0 images"));
}

/// What gc printed on stderr.
fn error_of(output: &std::process::Output) -> String {
    text(&output.stderr)
}

/// A repository with `main` and a second branch `pr` that names an image of its own, plus a store holding those two
/// images and one nobody names: the shape of a teammate's open PR.
struct PullRequest {
    fixture: Fixture,
    pr_image: ContentHash,
    orphan: ContentHash,
}

fn pull_request() -> PullRequest {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (front, pr_image, orphan) = (
        fixture.image("main", 30 * DAY),
        fixture.image("only on the pr branch", 30 * DAY),
        fixture.image("orphan", 30 * DAY),
    );
    write_manifest(&repo, "render", &[("front", front)]);
    commit(&repo, "bless main");
    git(&repo, &["checkout", "-q", "-b", "pr"]);
    write_manifest(&repo, "render", &[("front", front), ("side", pr_image)]);
    commit(&repo, "bless on the pr branch");
    git(&repo, &["checkout", "-q", "main"]);
    PullRequest { fixture, pr_image, orphan }
}

impl PullRequest {
    /// Clones the repository into `<dir>/<name>` with the extra `clone` arguments, and returns its manifest folder. A
    /// clone records no fetch, so it is fetched once, on the fixture's today, as a person who just cloned would.
    fn clone(&self, name: &str, clone_args: &[&str]) -> PathBuf {
        let manifests = self.clone_unfetched(name, clone_args);
        self.fetch(manifests.parent().and_then(Path::parent).unwrap(), &[]);
        manifests
    }

    /// Like [`PullRequest::clone`], but leaves it as `git clone` does: with no record that anything was fetched.
    fn clone_unfetched(&self, name: &str, clone_args: &[&str]) -> PathBuf {
        let (origin, dest) = (format!("file://{}", self.fixture.repo().display()), self.fixture.dir.path().join(name));
        git(self.fixture.dir.path(), &[&["clone", "-q"], clone_args, &[&origin, arg(&dest)]].concat());
        dest.join("tests/goldens")
    }

    /// `git fetch -q <args>` in `clone_dir`, dated on the fixture's today (the real clock is [`ADVANCE`] behind it).
    fn fetch(&self, clone_dir: &Path, args: &[&str]) {
        git(clone_dir, &[&["fetch", "-q"], args].concat());
        record_fetch(clone_dir, self.fixture.today);
    }

    /// The images gc would delete from the clone whose manifest folder is `manifests`, and its exit code.
    fn verdict(&self, manifests: &Path) -> (i32, BTreeSet<String>, String) {
        let output = self.fixture.gc_at(manifests, &[]);
        (code(&output), listed(&text(&output.stdout), "would delete: "), error_of(&output))
    }
}

#[test]
fn a_full_clone_sees_every_pr_branch_and_a_shallow_or_single_branch_clone_is_refused_until_fixed() {
    let pr = pull_request();

    let full = pr.clone("full", &[]);
    assert_eq!(pr.verdict(&full), (0, names(&[pr.orphan]), String::new()), "the pr branch's image is kept");

    // `--depth` implies `--single-branch`: the pr branch was never fetched, so its image looks like garbage.
    let shallow = pr.clone("shallow", &["--depth", "1"]);
    let (exit, would_delete, why) = pr.verdict(&shallow);
    assert_eq!(exit, 2, "{why}");
    assert!(would_delete.is_empty(), "nothing is listed when gc cannot be sure: {would_delete:?}");
    assert!(why.contains("shallow") && why.contains("git fetch --unshallow"), "{why}");

    // `git fetch --all` does not help: the remote's refspec still names only main.
    let clone_dir = shallow.parent().and_then(Path::parent).unwrap().to_owned();
    pr.fetch(&clone_dir, &["--all"]);
    let apply = pr.fixture.gc_at(&shallow, &["--apply"]);
    assert_eq!(code(&apply), 2, "{}", error_of(&apply));
    assert!(pr.fixture.stored().contains(&pr.pr_image.to_string()), "--apply deleted nothing");

    pr.fetch(&clone_dir, &["--unshallow"]);
    let (exit, _, why) = pr.verdict(&shallow);
    assert_eq!(exit, 2, "no longer shallow, still single-branch");
    assert!(why.contains("fetches only +refs/heads/main:refs/remotes/origin/main"), "{why}");
    assert!(why.contains("git remote set-branches origin '*'"), "{why}");

    // Doing what the message says makes the pr branch visible, and its image is kept.
    git(&clone_dir, &["remote", "set-branches", "origin", "*"]);
    pr.fetch(&clone_dir, &["origin"]);
    assert_eq!(pr.verdict(&shallow), (0, names(&[pr.orphan]), String::new()));

    let single = pr.clone("single", &["--single-branch"]);
    assert_eq!(pr.verdict(&single).0, 2, "a single-branch clone of any depth is refused");
}

#[test]
fn a_remote_with_no_fetch_refspec_and_a_negative_refspec_are_both_refused() {
    let pr = pull_request();
    let manifests = pr.clone("full", &[]);
    let clone_dir = manifests.parent().and_then(Path::parent).unwrap().to_owned();
    assert_eq!(pr.verdict(&manifests).0, 0);

    git(&clone_dir, &["config", "--add", "remote.origin.fetch", "^refs/heads/pr"]);
    let (exit, _, why) = pr.verdict(&manifests);
    assert_eq!(exit, 2, "a negative refspec hides branches: {why}");
    git(&clone_dir, &["config", "--unset-all", "remote.origin.fetch"]);
    let (exit, _, why) = pr.verdict(&manifests);
    assert_eq!(exit, 2, "{why}");
    assert!(why.contains("no fetch refspec"), "{why}");
}

#[test]
fn a_manifest_folder_in_another_letter_case_is_refused_even_when_the_files_are_reachable() {
    let s = scenario();
    let repo = s.fixture.repo();
    let before = s.fixture.stored();

    // Not the file system's spelling: NTFS would open it, git would find no manifests under it.
    for wrong in ["Tests/goldens", "tests/Goldens", "TESTS/GOLDENS"] {
        let output = s.fixture.gc_at(&repo.join(wrong), &["--apply"]);
        assert_eq!(code(&output), 2, "{wrong}: {}", text(&output.stdout));
        let why = error_of(&output);
        assert!(why.contains("exact spelling") && why.contains("the file system calls it"), "{wrong}: {why}");
    }
    assert_eq!(s.fixture.stored(), before, "nothing was deleted");

    // The reviewer's repro in a form a case-sensitive file system can hold: a folder spelled in the other case really
    // exists on disk (with an uncommitted manifest), and the repository's committed manifests are under `tests/goldens`.
    let lookalike = repo.join("Tests/Goldens");
    fs::create_dir_all(&lookalike).unwrap();
    fs::copy(repo.join("tests/goldens/extra.toml"), lookalike.join("extra.toml")).unwrap();
    let output = s.fixture.gc_at(&lookalike, &["--apply"]);
    assert_eq!(code(&output), 2, "{}", text(&output.stdout));
    assert!(error_of(&output).contains("'tests/goldens'"), "{}", error_of(&output));
    assert_eq!(s.fixture.stored(), before, "nothing was deleted");
}

#[test]
fn a_commit_that_spells_the_manifest_folder_in_another_case_stops_gc() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (named, other) = (fixture.image("named", 30 * DAY), fixture.image("on the odd branch", 30 * DAY));
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    // A branch from a machine whose file system folds case: its tree has the folder as `Tests/Goldens`.
    git(&repo, &["checkout", "-q", "-b", "odd"]);
    git(&repo, &["mv", "tests", "Tests-tmp"]);
    git(&repo, &["mv", "Tests-tmp", "Tests"]);
    git(&repo, &["mv", "Tests/goldens", "Tests/Goldens"]);
    write_manifest(&repo, "render", &[("front", other)]);
    commit(&repo, "case rename");
    git(&repo, &["checkout", "-q", "main"]);
    assert!(repo.join("tests/goldens").is_dir());

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 2, "{}", text(&output.stdout));
    assert!(error_of(&output).contains("'Tests/Goldens'"), "{}", error_of(&output));
    assert!(fixture.stored().contains(&other.to_string()));
}

#[test]
fn an_unparseable_uncommitted_manifest_stops_gc_wherever_it_is() {
    let s = scenario();
    let before = s.fixture.stored();
    let in_main = s.fixture.manifests().join("extra.toml");
    let in_worktree = s.fixture.dir.path().join("wt-wip/tests/goldens/face.toml");
    for broken in [in_main, in_worktree] {
        let good = fs::read_to_string(&broken).unwrap();
        fs::write(&broken, "<<<<<<< ours\n[front]\n=======\n[front\n>>>>>>> theirs\n").unwrap();
        let output = s.fixture.gc(&["--apply"]);
        assert_eq!(code(&output), 2, "{}: {}", broken.display(), text(&output.stdout));
        assert!(error_of(&output).contains(broken.file_name().unwrap().to_str().unwrap()), "{}", error_of(&output));
        assert_eq!(s.fixture.stored(), before, "nothing was deleted");
        fs::write(&broken, good).unwrap();
    }
    assert_eq!(code(&s.fixture.gc(&["--apply"])), 0, "with both manifests restored gc runs again");
}

#[test]
fn a_git_failure_stops_gc_instead_of_shrinking_what_is_wanted() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (named, garbage, on_damaged) = (
        fixture.image("named", 30 * DAY),
        fixture.image("garbage", 30 * DAY),
        fixture.image("on a damaged branch", 30 * DAY),
    );
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    git(&repo, &["checkout", "-q", "-b", "damaged"]);
    write_manifest(&repo, "render", &[("front", on_damaged)]);
    commit(&repo, "bless on the damaged branch");
    git(&repo, &["checkout", "-q", "main"]);
    assert_eq!(code(&fixture.gc(&[])), 0);

    // The branch's tree object is lost: git can list the branch but not read its manifests.
    let tree = git(&repo, &["rev-parse", "damaged^{tree}"]);
    let tree = tree.trim();
    fs::remove_file(repo.join(".git/objects").join(&tree[..2]).join(&tree[2..])).unwrap();

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 2, "{}", text(&output.stdout));
    assert!(error_of(&output).contains("git:"), "{}", error_of(&output));
    assert!(fixture.stored().contains(&garbage.to_string()) && fixture.stored().contains(&on_damaged.to_string()));
}

#[test]
fn a_deleted_worktree_still_counts_by_its_head_and_a_locked_one_stops_gc() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (named, on_head, orphan) = (
        fixture.image("named", 30 * DAY),
        fixture.image("only on a deleted worktree's head", 30 * DAY),
        fixture.image("orphan", 30 * DAY),
    );
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let tree = fixture.dir.path().join("wt-gone");
    git(&repo, &["worktree", "add", "-q", "--detach", arg(&tree)]);
    write_manifest(&tree, "render", &[("front", on_head)]);
    commit(&tree, "bless on a detached head");
    fs::remove_dir_all(&tree).unwrap();

    let output = fixture.gc(&["--apply"]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", error_of(&output));
    assert!(out.contains("(1 missing on disk)"), "{out}");
    assert_eq!(listed(&out, "deleted: "), names(&[orphan]));
    assert!(fixture.stored().contains(&on_head.to_string()), "the deleted worktree's HEAD still names its image");

    // The same worktree, locked: its folder may only be on a drive that is not plugged in.
    git(&repo, &["worktree", "lock", "--reason", "usb stick", arg(&tree)]);
    let locked = fixture.gc(&["--apply"]);
    assert_eq!(code(&locked), 2, "{}", text(&locked.stdout));
    assert!(error_of(&locked).contains("locked worktree"), "{}", error_of(&locked));
}

/// Sets the permissions of `path` to `mode` for as long as it lives (and back to 0o755 after), to lock this process
/// out of it. Root ignores permissions, so a test checks [`is_blocked`] and skips itself when it is root.
#[cfg(unix)]
struct NoAccess(PathBuf);

#[cfg(unix)]
impl NoAccess {
    fn new(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path.to_owned())
    }
}

/// Whether permissions really keep this process from inspecting `probe`.
#[cfg(unix)]
fn is_blocked(probe: &Path) -> bool {
    fs::metadata(probe).is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}

#[cfg(unix)]
impl Drop for NoAccess {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn a_worktree_that_cannot_be_read_is_an_error_and_not_a_missing_worktree() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let (named, hidden, orphan) = (
        fixture.image("named", 30 * DAY),
        fixture.image("uncommitted in an unreadable worktree", 30 * DAY),
        fixture.image("orphan", 30 * DAY),
    );
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let (parent, tree) = (fixture.dir.path().join("trees"), fixture.dir.path().join("trees/wt"));
    fs::create_dir(&parent).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "away", arg(&tree)]);
    write_manifest(&tree, "face", &[("neutral", hidden)]);
    let face = tree.join("tests/goldens/face.toml");
    assert_eq!(listed(&text(&fixture.gc(&[]).stdout), "would delete: "), names(&[orphan]));

    // Each case hides the uncommitted `face.toml` in a different way, and must stop gc rather than look like "missing".
    let cases: [(&str, &Path, u32, &Path); 3] = [
        ("the folder holding the worktree can't be searched", &parent, 0o000, &tree),
        ("the worktree can't be entered", &tree, 0o000, &tree.join("tests")),
        ("the manifest folder can be listed but its files not inspected", &tree.join("tests/goldens"), 0o444, &face),
    ];
    for (what, locked, mode, probe) in cases {
        let _denied = NoAccess::new(locked, mode);
        if !is_blocked(probe) {
            return; // running as root: permissions are not enforced, so there is nothing to test
        }
        let output = fixture.gc(&["--apply"]);
        assert_eq!(code(&output), 2, "{what}: {}", text(&output.stdout));
        assert!(error_of(&output).contains("Permission denied"), "{what}: {}", error_of(&output));
        assert!(fixture.stored().contains(&hidden.to_string()), "{what}");
        assert!(fixture.stored().contains(&orphan.to_string()), "{what}: nothing was deleted");
    }

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "readable again: {}", error_of(&output));
    assert_eq!(listed(&text(&output.stdout), "deleted: "), names(&[orphan]));
}

#[test]
fn a_worktree_path_that_is_not_a_folder_stops_gc() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let named = fixture.image("named", 30 * DAY);
    let orphan = fixture.image("orphan", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let tree = fixture.dir.path().join("wt-file");
    git(&repo, &["worktree", "add", "-q", "--detach", arg(&tree)]);
    fs::remove_dir_all(&tree).unwrap();
    fs::write(&tree, b"not a worktree any more").unwrap();

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 2, "{}", text(&output.stdout));
    assert!(error_of(&output).contains("wt-file"), "{}", error_of(&output));
    assert!(fixture.stored().contains(&orphan.to_string()));
}

#[test]
fn apply_repeats_the_scan_and_deletes_nothing_if_what_is_wanted_changed_since_the_plan() {
    type Change = fn(&Fixture, ContentHash);
    let changes: [(&str, Change); 5] = [
        ("a branch appears (a fetch)", |f, late| {
            git(&f.repo(), &["checkout", "-q", "-b", "fetched"]);
            write_manifest(&f.repo(), "render", &[("front", late)]);
            commit(&f.repo(), "bless late");
            git(&f.repo(), &["checkout", "-q", "main"]);
        }),
        ("a bless that is not committed yet", |f, late| write_manifest(&f.repo(), "extra", &[("case", late)])),
        ("a worktree is checked out", |f, late| {
            git(&f.repo(), &["checkout", "-q", "-b", "temp"]);
            write_manifest(&f.repo(), "render", &[("front", late)]);
            commit(&f.repo(), "bless late");
            git(&f.repo(), &["checkout", "-q", "main"]);
            git(&f.repo(), &["worktree", "add", "-q", "--detach", arg(&f.dir.path().join("wt")), "temp"]);
            git(&f.repo(), &["branch", "-q", "-D", "temp"]);
        }),
        ("a tag is created", |f, late| {
            git(&f.repo(), &["checkout", "-q", "--detach"]);
            write_manifest(&f.repo(), "render", &[("front", late)]);
            commit(&f.repo(), "bless late");
            git(&f.repo(), &["tag", "late"]);
            git(&f.repo(), &["checkout", "-q", "main"]);
        }),
        ("a branch disappears", |f, _| {
            git(&f.repo(), &["branch", "-q", "-D", "other"]);
        }),
    ];
    for (what, change) in changes {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        let (named, on_other, late, stale) = (
            fixture.image("named", 30 * DAY),
            fixture.image("on another branch", 30 * DAY),
            fixture.image("late", 30 * DAY),
            fixture.image("stale", 30 * DAY),
        );
        write_manifest(&repo, "render", &[("front", named)]);
        commit(&repo, "bless");
        git(&repo, &["branch", "other"]);
        git(&repo, &["checkout", "-q", "other"]);
        write_manifest(&repo, "render", &[("front", on_other)]);
        commit(&repo, "bless other");
        git(&repo, &["checkout", "-q", "main"]);

        let plan = gc::plan(&fixture.goldens(), &[], fixture.today).unwrap();
        assert_eq!(names(&plan.garbage.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[late, stale]), "{what}");
        let before = fixture.stored();

        change(&fixture, late);
        let err = plan.apply().unwrap_err().to_string();
        assert!(err.contains("changed while it ran") && err.contains("run gc again"), "{what}: {err}");
        assert_eq!(fixture.stored(), before, "{what}: nothing was deleted");
    }
}

#[test]
fn apply_deletes_when_nothing_changed_and_refuses_a_plan_it_cannot_repeat() {
    let fixture = Fixture::new();
    let (named, stale) = (fixture.image("named", 30 * DAY), fixture.image("stale", 30 * DAY));
    write_manifest(&fixture.repo(), "render", &[("front", named)]);
    commit(&fixture.repo(), "bless");
    let plan = gc::plan(&fixture.goldens(), &[], fixture.today).unwrap();
    let removal = plan.apply().unwrap();
    assert_eq!(names(&removal.removed.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[stale]));
    assert_eq!(fixture.stored(), names(&[named]));

    // The scan is repeated with the same extra revisions, so a revision that stops resolving is a change too.
    let kept = fixture.image("kept by a revision", 30 * DAY);
    git(&fixture.repo(), &["checkout", "-q", "--detach"]);
    write_manifest(&fixture.repo(), "render", &[("front", kept)]);
    commit(&fixture.repo(), "bless kept");
    let tip = git(&fixture.repo(), &["rev-parse", "HEAD"]).trim().to_owned();
    git(&fixture.repo(), &["checkout", "-q", "main"]);
    fixture.image("another stale", 30 * DAY);
    let plan = gc::plan(&fixture.goldens(), std::slice::from_ref(&tip), fixture.today).unwrap();
    assert_eq!(plan.garbage.len(), 1);
    git(&fixture.repo(), &["reflog", "expire", "--expire=now", "--all"]);
    git(&fixture.repo(), &["gc", "-q", "--prune=now"]);
    assert!(plan.apply().is_err(), "the revision no longer resolves");
    assert_eq!(fixture.stored().len(), 3);
}

/// The change time of `path` in whole nanoseconds, to tell whether a change was noticed by the file system.
#[cfg(unix)]
fn change_time_of(path: &Path) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).unwrap();
    (meta.ctime(), meta.ctime_nsec())
}

#[test]
fn an_image_copied_with_its_old_modification_time_is_as_new_as_the_copy() {
    let fixture = Fixture::new();
    let (repo, goldens) = (fixture.repo(), fixture.goldens());
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");

    // `rsync -a` or `cp -p` from a backup, just now: a new file that carries the source's month-old modification time.
    // Only the copy's own clocks (the kernel's change time) say when it really arrived, so this one runs by the real clock.
    let copied_at = SystemTime::now();
    let restored = fixture.image_modified("restored from backup", copied_at - 30 * DAY);

    let minute_later = copied_at + Duration::from_mins(1);
    let plan = gc::plan(&goldens, &[], minute_later).unwrap();
    assert_eq!(
        plan.garbage,
        Vec::new(),
        "a month-old modification time is no reason to delete a file copied a minute ago"
    );
    let [kept] = plan.young.as_slice() else { panic!("{:?}", plan.young) };
    assert_eq!(kept.hash, restored);
    assert!(kept.age < Duration::from_mins(2), "as old as the copy: {:?}", kept.age);
    assert!(kept.modified_age > 30 * DAY && kept.changed_since_stored());
    assert_eq!(plan.warnings, Vec::new());

    // The command line says why such an image counts as young, and --apply leaves it be.
    let manifests = fixture.manifests();
    let output = fixture.gc_with(&manifests, &["--apply"], &[]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", error_of(&output));
    assert!(out.contains("kept (younger than 7 days)") && out.contains(&restored.to_string()), "{out}");
    assert!(out.contains("0 days old; last stored 30 days ago, but the file itself changed since"), "{out}");
    assert!(fixture.stored().contains(&restored.to_string()), "{out}");

    // A week on, the copy has aged like any other file.
    let later = gc::plan(&goldens, &[], copied_at + 8 * DAY).unwrap();
    assert_eq!(names(&later.garbage.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[restored]));
}

#[test]
fn a_clock_reading_in_the_future_keeps_the_image_and_warns_with_its_path_and_field() {
    let fixture = Fixture::new();
    let (repo, goldens) = (fixture.repo(), fixture.goldens());
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let fine = fixture.image("old and fine", 30 * DAY);
    // Restored from a machine whose clock ran three days fast.
    let skewed = fixture.image_modified("from a machine with a fast clock", fixture.today + 3 * DAY);
    let path = goldens.objects().path(&skewed);

    let plan = gc::plan(&goldens, &[], fixture.today).unwrap();
    assert_eq!(names(&plan.garbage.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[fine]));
    assert_eq!(names(&plan.young.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[skewed]));
    assert_eq!(plan.young[0].age, Duration::ZERO);
    assert_eq!(plan.warnings, vec![Warning::FutureTime { path: path.clone(), field: "mtime", ahead: 3 * DAY }]);

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "{}", error_of(&output));
    let (out, warned) = (text(&output.stdout), error_of(&output));
    assert_eq!(listed(&out, "deleted: "), names(&[fine]), "{out}");
    assert!(fixture.stored().contains(&skewed.to_string()), "kept");
    let expected = format!("{skewed}.png: its mtime is 3 days in the future");
    assert!(warned.starts_with("warning: ") && warned.contains(&expected), "{warned}");

    // Once the clock gets past the reading and a week more, the image is old like any other.
    let much_later = gc::plan(&goldens, &[], fixture.today + 10 * DAY + Duration::from_secs(1)).unwrap();
    assert!(much_later.garbage.iter().any(|o| o.hash == skewed));
}

#[test]
fn a_clock_far_behind_the_files_keeps_everything_and_prints_a_bounded_number_of_warnings() {
    let fixture = Fixture::new();
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&fixture.repo(), "render", &[("front", named)]);
    commit(&fixture.repo(), "bless");
    let unnamed: Vec<_> = (0..12).map(|n| fixture.image(&format!("unnamed {n}"), 30 * DAY)).collect();

    // `gc` is told it is a year before the files were written: every one of their clocks is in the future.
    let behind = (fixture.today - 400 * DAY).duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();
    let output = fixture.gc_with(&fixture.manifests(), &["--apply"], &[(TEST_CLOCK_ENV, &behind)]);
    let (out, warned) = (text(&output.stdout), error_of(&output));
    assert_eq!(code(&output), 0, "{out}{warned}");
    assert_eq!(fixture.stored().len(), unnamed.len() + 1, "nothing was deleted: {out}");
    assert!(out.contains("deleted 0 images"), "{out}");

    let lines: Vec<_> = warned.lines().collect();
    let more = lines.iter().find_map(|line| line.strip_prefix("warning: and ")?.strip_suffix(" more like these"));
    let shown = lines.iter().filter(|line| line.starts_with("warning: ") && line.contains("in the future")).count();
    assert_eq!(shown, 10, "{warned}");
    assert!(more.and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| n >= 14), "{warned}");
}

#[test]
fn a_clock_given_in_the_environment_must_be_whole_seconds() {
    let fixture = Fixture::new();
    for bad in ["soon", "", "-5", "1.5"] {
        let output = fixture.gc_with(&fixture.manifests(), &[], &[(TEST_CLOCK_ENV, bad)]);
        assert_eq!(code(&output), 2, "'{bad}'");
        assert!(
            error_of(&output).contains("OMUZAKI_GC_TEST_NOW must be whole seconds"),
            "'{bad}': {}",
            error_of(&output)
        );
    }
}

#[cfg(unix)]
#[test]
fn an_image_whose_file_changed_after_the_plan_is_kept_even_if_only_its_change_time_moved() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let (repo, goldens) = (fixture.repo(), fixture.goldens());
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&repo, "render", &[("front", named)]);
    commit(&repo, "bless");
    let (in_use, stale) = (fixture.image("in use", 30 * DAY), fixture.image("stale", 30 * DAY));
    let plan = gc::plan(&goldens, &[], fixture.today).unwrap();
    assert_eq!(plan.garbage.len(), 2);

    // Something touches the file between the plan and the deletion without writing to it: a test that opened it and a
    // backup agent, `chmod -R`, a virus scan. Content and modification time stay as they were.
    let path = goldens.objects().path(&in_use);
    let (before, mode) = (change_time_of(&path), fs::metadata(&path).unwrap().permissions().mode());
    fs::set_permissions(&path, fs::Permissions::from_mode(mode ^ 0o040)).unwrap();
    if change_time_of(&path) == before {
        return; // a file system whose change time is too coarse to show this: nothing to test here
    }

    let removal = plan.apply().unwrap();
    assert_eq!(names(&removal.removed.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[stale]));
    assert_eq!(names(&removal.touched.iter().map(|o| o.hash).collect::<Vec<_>>()), names(&[in_use]));
    assert!(path.exists() && !goldens.objects().path(&stale).exists());
}

#[cfg(unix)]
#[test]
fn a_link_named_like_an_image_is_neither_followed_nor_removed() {
    let fixture = Fixture::new();
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&fixture.repo(), "render", &[("front", named)]);
    commit(&fixture.repo(), "bless");
    let garbage = fixture.image("garbage", 30 * DAY);

    // One link to an old file outside the store, one to nothing: deleting either would be deleting someone else's file.
    let outside = fixture.dir.path().join("elsewhere.png");
    fs::write(&outside, b"not the store's to delete").unwrap();
    fs::File::options().write(true).open(&outside).unwrap().set_modified(fixture.today - 30 * DAY).unwrap();
    let objects = ObjectStore::new(&fixture.store());
    let (to_file, to_nothing) =
        (objects.path(&ContentHash::of(b"linked")), objects.path(&ContentHash::of(b"dangling")));
    std::os::unix::fs::symlink(&outside, &to_file).unwrap();
    std::os::unix::fs::symlink(fixture.dir.path().join("missing.png"), &to_nothing).unwrap();

    let output = fixture.gc(&["--apply"]);
    assert_eq!(code(&output), 0, "{}", error_of(&output));
    assert_eq!(listed(&text(&output.stdout), "deleted: "), names(&[garbage]));
    for link in [&to_file, &to_nothing] {
        assert!(fs::symlink_metadata(link).is_ok_and(|meta| meta.is_symlink()), "{}", link.display());
    }
    assert_eq!(fs::read(&outside).unwrap(), b"not the store's to delete");
}

#[test]
fn a_manifest_naming_an_image_that_is_not_stored_is_harmless_and_not_counted() {
    let fixture = Fixture::new();
    let (present, garbage) = (fixture.image("present", 30 * DAY), fixture.image("garbage", 30 * DAY));
    let missing = ContentHash::of(b"named on some branch, but never stored here");
    write_manifest(&fixture.repo(), "render", &[("front", present), ("back", missing)]);
    commit(&fixture.repo(), "bless");

    let output = fixture.gc(&["--apply"]);
    let out = text(&output.stdout);
    assert_eq!(code(&output), 0, "{out}{}", error_of(&output));
    assert_eq!(listed(&out, "deleted: "), names(&[garbage]), "{out}");
    assert!(out.contains("1 image named by a manifest"), "only images that are stored are counted: {out}");
    assert_eq!(fixture.stored(), names(&[present]));
}

#[test]
fn a_last_fetch_older_than_three_days_warns_in_a_dry_run_and_stops_apply_until_the_next_fetch() {
    let pr = pull_request();
    let fixture = &pr.fixture;
    let manifests = pr.clone("full", &[]);
    let clone_dir = manifests.parent().and_then(Path::parent).unwrap().to_owned();
    let only_the_orphan = (0, names(&[pr.orphan]), String::new());

    // Fetched today, two days ago and exactly three days ago: current enough, no warning.
    for ago in [Duration::ZERO, 2 * DAY, 3 * DAY] {
        record_fetch(&clone_dir, fixture.today - ago);
        assert_eq!(pr.verdict(&manifests), only_the_orphan, "fetched {ago:?} ago");
    }

    // Four days ago: the dry run still lists what it would delete, but says that the list may be short of a branch.
    record_fetch(&clone_dir, fixture.today - 4 * DAY);
    let (exit, would_delete, warned) = pr.verdict(&manifests);
    assert_eq!((exit, &would_delete), (0, &only_the_orphan.1), "{warned}");
    assert!(warned.starts_with("warning: the last `git fetch` was 4 days ago ("), "{warned}");
    assert!(warned.contains("full/.git/FETCH_HEAD was written then"), "names the file: {warned}");
    assert!(warned.contains("git fetch --all") && warned.contains("--apply refuses"), "{warned}");

    // --apply deletes nothing and says how to go on.
    let apply = fixture.gc_at(&manifests, &["--apply"]);
    let refusal = error_of(&apply);
    assert_eq!(code(&apply), 2, "{refusal}");
    assert!(refusal.contains("gc stopped without deleting anything: the last `git fetch` was 4 days ago"), "{refusal}");
    assert!(refusal.contains("run `git fetch --all`, then run gc again"), "{refusal}");
    assert!(fixture.stored().contains(&pr.orphan.to_string()));

    // Run from a linked worktree it reads the same clone, and the same records.
    let linked = fixture.dir.path().join("full-wt");
    git(&clone_dir, &["worktree", "add", "-q", "--detach", arg(&linked)]);
    let from_linked = fixture.gc_at(&linked.join("tests/goldens"), &["--apply"]);
    assert_eq!(code(&from_linked), 2, "{}", error_of(&from_linked));
    assert!(error_of(&from_linked).contains("full/.git/FETCH_HEAD"), "{}", error_of(&from_linked));

    // With nothing to delete there is nothing to refuse.
    let orphan_file = fixture.store().join("objects").join(format!("{}.png", pr.orphan));
    let held_back = fs::read(&orphan_file).unwrap();
    fs::remove_file(&orphan_file).unwrap();
    let nothing = fixture.gc_at(&manifests, &["--apply"]);
    assert_eq!(code(&nothing), 0, "{}", error_of(&nothing));
    assert!(error_of(&nothing).contains("warning: the last `git fetch` was 4 days ago"), "still warned");
    fs::write(&orphan_file, held_back).unwrap();
    fs::File::options().write(true).open(&orphan_file).unwrap().set_modified(fixture.today - 30 * DAY).unwrap();

    // A fetch settles it.
    pr.fetch(&clone_dir, &[]);
    let apply = fixture.gc_at(&manifests, &["--apply"]);
    assert_eq!(code(&apply), 0, "{}", error_of(&apply));
    assert_eq!(listed(&text(&apply.stdout), "deleted: "), names(&[pr.orphan]));
    assert_eq!(error_of(&apply), "");
}

#[test]
fn a_clone_that_never_fetched_and_a_fetch_record_from_the_future_both_stop_apply() {
    let pr = pull_request();
    let fixture = &pr.fixture;
    let manifests = pr.clone_unfetched("fresh", &[]);
    let clone_dir = manifests.parent().and_then(Path::parent).unwrap().to_owned();
    let record = clone_dir.join(".git/FETCH_HEAD");
    assert!(!record.exists(), "git clone leaves no record of a fetch");
    let stale = |plan: &gc::Plan| {
        plan.warnings.iter().find_map(|w| if let Warning::StaleRemotes(s) = w { Some(s.clone()) } else { None })
    };

    // By the real clock: a genuine `git fetch` is what ends the warning, with no help from the test.
    let now = SystemTime::now();
    assert!(matches!(
        stale(&gc::plan(&fixture.goldens_for(&manifests), &[], now).unwrap()),
        Some(Staleness::Never { .. })
    ));
    let (exit, _, warned) = pr.verdict(&manifests);
    assert_eq!(exit, 0, "{warned}");
    assert!(
        warned.starts_with("warning: no `git fetch` is recorded (") && warned.contains("fresh/.git/FETCH_HEAD"),
        "{warned}"
    );
    let apply = fixture.gc_at(&manifests, &["--apply"]);
    assert_eq!(code(&apply), 2, "{}", error_of(&apply));
    assert!(error_of(&apply).contains("no `git fetch` is recorded"), "{}", error_of(&apply));
    assert!(fixture.stored().contains(&pr.orphan.to_string()));

    git(&clone_dir, &["fetch", "-q"]);
    assert_eq!(stale(&gc::plan(&fixture.goldens_for(&manifests), &[], SystemTime::now()).unwrap()), None);

    // A record dated after `now` means a clock is wrong somewhere: no telling how long ago the fetch was.
    pr.fetch(&clone_dir, &[]);
    record_fetch(&clone_dir, fixture.today + DAY);
    let apply = fixture.gc_at(&manifests, &["--apply"]);
    assert_eq!(code(&apply), 2, "{}", error_of(&apply));
    assert!(error_of(&apply).contains("is dated 1 day in the future"), "{}", error_of(&apply));
    assert!(fixture.stored().contains(&pr.orphan.to_string()));

    // A hair ahead is rounding.
    record_fetch(&clone_dir, fixture.today + Duration::from_mins(1));
    assert_eq!(pr.verdict(&manifests), (0, names(&[pr.orphan]), String::new()));
}

#[test]
fn the_test_clock_is_refused_on_a_store_that_is_not_marked_as_a_test_store() {
    // The hole this closes: a clock set a year ahead makes an image written seconds ago look 399 days old.
    let fixture = Fixture::new();
    let named = fixture.image("named", 30 * DAY);
    write_manifest(&fixture.repo(), "render", &[("front", named)]);
    commit(&fixture.repo(), "bless");
    let just_written = fixture.image_modified("written seconds ago", SystemTime::now());
    let year_ahead = (SystemTime::now() + 400 * DAY).duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();
    let before = fixture.stored();
    let manifests = fixture.manifests();

    // With the mark it works as it always did: every image not named is old by that clock.
    let marked = fixture.gc_with(&manifests, &[], &[(TEST_CLOCK_ENV, &year_ahead)]);
    assert_eq!(code(&marked), 0, "{}", error_of(&marked));
    assert_eq!(listed(&text(&marked.stdout), "would delete: "), names(&[just_written]));

    // Without it (the real store has none) dry run and --apply both stop, and nothing is listed or deleted.
    fs::remove_file(fixture.store().join(TEST_CLOCK_MARKER)).unwrap();
    for extra in [&[][..], &["--apply"], &["--dry-run"]] {
        let output = fixture.gc_with(&manifests, extra, &[(TEST_CLOCK_ENV, &year_ahead)]);
        let why = error_of(&output);
        assert_eq!(code(&output), 2, "{extra:?}: {}", text(&output.stdout));
        assert_eq!(text(&output.stdout), "", "{extra:?}: nothing is listed");
        assert!(why.contains("gc stopped without deleting anything"), "{extra:?}: {why}");
        assert!(why.contains(TEST_CLOCK_ENV) && why.contains(TEST_CLOCK_MARKER), "{extra:?}: {why}");
        assert!(why.contains(&fixture.store().display().to_string()) && why.contains("unset"), "{extra:?}: {why}");
        assert_eq!(fixture.stored(), before, "{extra:?}: nothing was deleted");
    }
    // Any value is refused, a bad one included, and so is an empty one.
    for value in ["soon", ""] {
        let output = fixture.gc_with(&manifests, &["--apply"], &[(TEST_CLOCK_ENV, value)]);
        assert_eq!(code(&output), 2, "'{value}': {}", error_of(&output));
        assert!(error_of(&output).contains(TEST_CLOCK_MARKER), "'{value}': {}", error_of(&output));
    }

    // Not set, the real clock rules, mark or no mark: the image written seconds ago is far too young to go.
    let real = fixture.gc_with(&manifests, &["--apply"], &[]);
    assert_eq!(code(&real), 0, "{}", error_of(&real));
    assert_eq!(fixture.stored(), before, "{}", text(&real.stdout));
    assert!(text(&real.stdout).contains("kept (younger than 7 days)"), "{}", text(&real.stdout));
}

#[test]
fn a_fetch_in_a_linked_worktree_counts_as_a_fetch() {
    let pr = pull_request();
    let fixture = &pr.fixture;
    let manifests = pr.clone_unfetched("fresh", &[]);
    let clone_dir = manifests.parent().and_then(Path::parent).unwrap().to_owned();
    let linked = fixture.dir.path().join("fresh-wt");
    git(&clone_dir, &["worktree", "add", "-q", "--detach", arg(&linked)]);
    let linked_manifests = linked.join("tests/goldens");
    let main_record = clone_dir.join(".git/FETCH_HEAD");
    let linked_record = PathBuf::from(git(&linked, &["rev-parse", "--absolute-git-dir"]).trim()).join("FETCH_HEAD");
    assert!(linked_record.starts_with(clone_dir.join(".git/worktrees")), "{}", linked_record.display());
    let only_the_orphan = (0, names(&[pr.orphan]), String::new());

    // Nothing fetched anywhere yet.
    assert!(pr.verdict(&manifests).2.contains("no `git fetch` is recorded"));

    // The person does what the message says, but in the linked worktree: git writes the record there, not in the clone.
    git(&linked, &["fetch", "-q", "--all"]);
    assert!(linked_record.exists() && !main_record.exists(), "the record is the worktree's own");
    date(&linked_record, fixture.today);
    assert_eq!(pr.verdict(&manifests), only_the_orphan, "seen from the main checkout");
    let from_linked = fixture.gc_at(&linked_manifests, &[]);
    assert_eq!((code(&from_linked), error_of(&from_linked)), (0, String::new()), "and from the linked worktree");

    // Stale there and nowhere else: the warning names that file.
    date(&linked_record, fixture.today - 4 * DAY);
    let (exit, would_delete, warned) = pr.verdict(&manifests);
    assert_eq!((exit, &would_delete), (0, &only_the_orphan.1), "{warned}");
    assert!(
        warned.contains("4 days ago") && warned.contains("worktrees/") && warned.contains("FETCH_HEAD"),
        "{warned}"
    );

    // The newest of the records counts: yesterday in the main checkout beats four days in the linked one.
    git(&clone_dir, &["fetch", "-q"]);
    date(&main_record, fixture.today - DAY);
    assert_eq!(pr.verdict(&manifests), only_the_orphan);
    // ... and the other way round.
    date(&main_record, fixture.today - 6 * DAY);
    date(&linked_record, fixture.today - DAY);
    assert_eq!(pr.verdict(&manifests), only_the_orphan);

    // A record from the future beside a fresh one is ignored; when every record is in the future, --apply refuses.
    date(&linked_record, fixture.today + 2 * DAY);
    date(&main_record, fixture.today - DAY);
    assert_eq!(pr.verdict(&manifests), only_the_orphan);
    date(&main_record, fixture.today + DAY);
    let apply = fixture.gc_at(&manifests, &["--apply"]);
    assert_eq!(code(&apply), 2, "{}", error_of(&apply));
    assert!(error_of(&apply).contains("is dated 1 day in the future"), "{}", error_of(&apply));
    assert!(fixture.stored().contains(&pr.orphan.to_string()));
}
