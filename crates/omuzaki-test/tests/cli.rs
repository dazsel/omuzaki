//! End-to-end tests of the `omuzaki-test` binary: the acceptance checks of as a user would run them.
// Test helpers sit outside `#[test]` functions, where clippy.toml's unwrap exemption doesn't reach; a panic is the
// right failure here.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::{Rgba, RgbaImage};
use tempfile::TempDir;

use common::{arg, code, run, text};

const SIZE: u32 = 100; // 10 000 pixels, so 1 pixel is 0.01% and 500 pixels are exactly 5%

fn gradient() -> RgbaImage {
    RgbaImage::from_fn(SIZE, SIZE, |x, y| Rgba([u8::try_from(x * 2).unwrap(), u8::try_from(y * 2).unwrap(), 90, 255]))
}

fn write(dir: &TempDir, name: &str, image: &RgbaImage) -> PathBuf {
    let path = dir.path().join(name);
    image.save(&path).unwrap();
    path
}

fn red_pixels(path: &Path) -> usize {
    image::open(path).unwrap().into_rgba8().pixels().filter(|p| p.0 == [255, 0, 0, 255]).count()
}

#[test]
fn a_one_pixel_change_passes_under_the_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let expected = gradient();
    let mut changed = expected.clone();
    changed.put_pixel(50, 50, Rgba([255, 255, 255, 255]));
    let (a, e) = (write(&dir, "actual.png", &changed), write(&dir, "expected.png", &expected));

    let tolerant = run(&["compare", arg(&a), arg(&e), "--per-channel", "2", "--max-bad-fraction", "0.001"], &[]);
    assert_eq!(code(&tolerant), 0, "{}", text(&tolerant.stdout));
    assert!(text(&tolerant.stdout).contains("1 of 10000 px"));

    assert_eq!(code(&run(&["compare", arg(&a), arg(&e)], &[])), 1);
}

#[test]
fn a_five_percent_change_fails_and_writes_a_diff_image() {
    let dir = tempfile::tempdir().unwrap();
    let expected = gradient();
    let mut changed = expected.clone();
    for i in 0..500 {
        changed.put_pixel(i % SIZE, i / SIZE, Rgba([255, 255, 255, 255])); // blue 90 -> 255: always a big change
    }
    let (a, e) = (write(&dir, "actual.png", &changed), write(&dir, "expected.png", &expected));
    let diff = dir.path().join("diff.png");

    let args = ["compare", arg(&a), arg(&e), "--per-channel", "2", "--max-bad-fraction", "0.01", "--diff", arg(&diff)];
    let out = run(&args, &[]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stdout).starts_with("FAILED: 5.000% bad (500 of 10000 px)"), "{}", text(&out.stdout));
    assert_eq!(red_pixels(&diff), 500);
}

#[test]
fn output_is_identical_across_runs() {
    let dir = tempfile::tempdir().unwrap();
    let expected = gradient();
    let mut changed = expected.clone();
    changed.put_pixel(7, 9, Rgba([0, 0, 0, 255]));
    let (a, e) = (write(&dir, "actual.png", &changed), write(&dir, "expected.png", &expected));
    let (d1, d2) = (dir.path().join("d1.png"), dir.path().join("d2.png"));

    let first = run(&["compare", arg(&a), arg(&e), "--diff", arg(&d1)], &[]);
    let second = run(&["compare", arg(&a), arg(&e), "--diff", arg(&d2)], &[]);
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(fs::read(&d1).unwrap(), fs::read(&d2).unwrap());
}

/// The hash in `manifest`'s `hash = "…"` line for the one case it holds.
fn manifest_hash(manifest: &Path) -> String {
    let text = fs::read_to_string(manifest).unwrap();
    let line = text.lines().find(|l| l.starts_with("hash = ")).unwrap();
    line.trim_start_matches("hash = ").trim_matches('"').to_owned()
}

#[test]
fn bless_then_check_then_change_then_report() {
    let dir = tempfile::tempdir().unwrap();
    let (goldens, manifests, out) = (dir.path().join("goldens"), dir.path().join("manifests"), dir.path().join("out"));
    let (g, m, o) = (arg(&goldens), arg(&manifests), arg(&out));
    let check = |image: &Path, extra: &[&str], envs: &[(&str, &str)]| {
        let mut args = vec!["check", "render/front-view", arg(image), "--goldens", g, "--manifests", m, "--out", o];
        args.extend_from_slice(extra);
        run(&args, envs)
    };
    let report = || run(&["report", "--goldens", g, "--out", o], &[]);
    let manifest = manifests.join("render.toml");

    let first = write(&dir, "first.png", &gradient());
    let mut edited_image = gradient();
    edited_image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    let edited = write(&dir, "edited.png", &edited_image);

    // No golden yet: fails, says so, and writes neither a manifest nor an image.
    let missing = check(&first, &[], &[]);
    assert_eq!(code(&missing), 1);
    assert!(text(&missing.stdout).contains("new, no golden"));
    assert!(!manifest.exists() && !goldens.join("objects").exists());

    // --bless accepts it: the manifest names the image, which sits in the object store under that hash.
    let blessed = check(&first, &["--bless"], &[]);
    assert_eq!(code(&blessed), 0);
    let first_hash = manifest_hash(&manifest);
    assert!(goldens.join(format!("objects/{first_hash}.png")).exists());
    assert!(text(&blessed.stdout).contains(&format!("-> {first_hash}")), "{}", text(&blessed.stdout));
    assert_eq!(code(&check(&first, &[], &[])), 0);

    // A change fails and leaves expected/actual/diff behind, with the manifest untouched.
    assert_eq!(code(&check(&edited, &[], &[])), 1);
    for file in ["expected.png", "actual.png", "diff.png", "json"] {
        assert!(out.join(format!("render/front-view.{file}")).exists(), "{file}");
    }
    assert_eq!(manifest_hash(&manifest), first_hash);

    // The report names the image, the folder and both hashes.
    let md = text(&report().stdout);
    assert!(md.starts_with("<!-- omuzaki-golden-report: changed -->"), "{md}");
    assert!(md.contains("| `render/front-view` | differs |"), "{md}");
    assert!(md.contains(&format!("`{}` → `", &first_hash[..12])), "{md}");

    // OMUZAKI_BLESS=1 works like --bless; afterwards the manifest names the new image and the report is clean.
    assert_eq!(code(&check(&edited, &[], &[("OMUZAKI_BLESS", "1")])), 0);
    let edited_hash = manifest_hash(&manifest);
    assert_ne!(edited_hash, first_hash);
    assert!(goldens.join(format!("objects/{first_hash}.png")).exists(), "the old image stays for other branches");
    assert_eq!(code(&check(&edited, &[], &[])), 0);
    let clean = text(&report().stdout);
    assert!(
        clean.starts_with("<!-- omuzaki-golden-report: none -->") && clean.contains("No golden image changed."),
        "{clean}"
    );

    // The report needs only the evidence folder, e.g. from OMUZAKI_GOLDEN_OUT.
    assert_eq!(text(&run(&["report"], &[("OMUZAKI_GOLDEN_OUT", o)]).stdout), clean);
}

#[test]
fn two_branches_bless_different_images_for_the_same_case_independently() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("goldens"); // shared, like the assets folder
    let (a, b) = (dir.path().join("branch-a/tests/goldens"), dir.path().join("branch-b/tests/goldens"));
    let (red, blue) =
        (RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255])), RgbaImage::from_pixel(8, 8, Rgba([0, 0, 255, 255])));
    let (red, blue) = (write(&dir, "red.png", &red), write(&dir, "blue.png", &blue));
    let out = dir.path().join("out");
    let check = |manifests: &Path, image: &Path, bless: bool| {
        let mut args = vec!["check", "s/x", arg(image), "--goldens", arg(&store), "--manifests", arg(manifests)];
        args.extend(["--out", arg(&out)]);
        if bless {
            args.push("--bless");
        }
        code(&run(&args, &[]))
    };

    assert_eq!(check(&a, &red, true), 0);
    assert_eq!(check(&b, &blue, true), 0);

    assert_eq!((check(&a, &red, false), check(&a, &blue, false)), (0, 1), "branch A still has red");
    assert_eq!((check(&b, &blue, false), check(&b, &red, false)), (0, 1), "branch B has blue");
    assert_ne!(manifest_hash(&a.join("s.toml")), manifest_hash(&b.join("s.toml")));
}

#[test]
fn migrate_moves_old_goldens_into_a_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let (goldens, manifests, out) = (dir.path().join("goldens"), dir.path().join("manifests"), dir.path().join("out"));
    let old = write(&dir, "old.png", &gradient());
    fs::create_dir_all(goldens.join("render")).unwrap();
    fs::copy(&old, goldens.join("render/front-view.png")).unwrap();
    let migrate = || {
        let args = ["migrate", "--goldens", arg(&goldens), "--manifests", arg(&manifests), "--per-channel", "2"];
        run(&args, &[])
    };

    let first = migrate();
    assert_eq!(code(&first), 0, "{}", text(&first.stderr));
    assert!(text(&first.stdout).contains("migrated: render/front-view"), "{}", text(&first.stdout));
    let hash = manifest_hash(&manifests.join("render.toml"));
    assert!(fs::read_to_string(manifests.join("render.toml")).unwrap().contains("per_channel = 2"));
    assert!(goldens.join(format!("objects/{hash}.png")).exists());
    assert!(goldens.join("render/front-view.png").exists(), "the old file is left alone");

    // The migrated golden checks out, and a second migration has nothing to do.
    let args = ["check", "render/front-view", arg(&old), "--goldens", arg(&goldens), "--manifests", arg(&manifests)];
    assert_eq!(code(&run(&[&args[..], &["--out", arg(&out)]].concat(), &[])), 0);
    let again = migrate();
    assert_eq!(code(&again), 0);
    assert!(text(&again.stdout).contains("0 migrated, 1 already there"), "{}", text(&again.stdout));
}

#[test]
fn problems_exit_with_2_and_a_readable_message() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let nope = dir.path().join("nope.png");

    let missing_file = run(&["compare", arg(&img), arg(&nope)], &[]);
    assert_eq!(code(&missing_file), 2);
    assert!(text(&missing_file.stderr).contains("nope.png"));

    let no_store = run(&["check", "render/x", arg(&img)], &[]);
    assert_eq!(code(&no_store), 2);
    assert!(text(&no_store.stderr).contains("OMUZAKI_GOLDENS"));

    // Outside any workspace there is no tests/goldens to default to.
    let no_manifests = run(&["check", "render/x", arg(&img), "--goldens", arg(dir.path())], &[]);
    assert_eq!(code(&no_manifests), 2);
    assert!(text(&no_manifests.stderr).contains("--manifests"), "{}", text(&no_manifests.stderr));

    let manifests = dir.path().join("m");
    let bad_id = ["check", "../x", arg(&img), "--goldens", arg(dir.path()), "--manifests", arg(&manifests)];
    assert_eq!(code(&run(&bad_id, &[])), 2);
    assert_eq!(code(&run(&["compare", arg(&img), arg(&img), "--max-bad-fraction", "3"], &[])), 2);
}

#[test]
fn a_size_mismatch_exits_1_under_compare_as_under_check() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let small = write(&dir, "small.png", &RgbaImage::new(2, 2));

    let compared = run(&["compare", arg(&img), arg(&small)], &[]);
    assert_eq!(code(&compared), 1, "{}", text(&compared.stderr));
    assert!(text(&compared.stdout).contains("size mismatch"), "{}", text(&compared.stdout));

    let (goldens, manifests) = (dir.path().join("g"), dir.path().join("m"));
    let at = ["--goldens", arg(&goldens), "--manifests", arg(&manifests)];
    assert_eq!(code(&run(&[&["check", "s/x", arg(&img), "--bless"], &at[..]].concat(), &[])), 0);
    let checked = run(&[&["check", "s/x", arg(&small)], &at[..]].concat(), &[]);
    assert_eq!(code(&checked), 1);
    assert!(text(&checked.stdout).contains("size mismatch"), "{}", text(&checked.stdout));
}

#[test]
fn a_bad_tolerance_is_refused_even_when_blessing_a_new_golden() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let (goldens, manifests) = (dir.path().join("g"), dir.path().join("m"));

    let blessed = run(
        &[
            "check",
            "s/x",
            arg(&img),
            "--bless",
            "--max-bad-fraction",
            "3",
            "--goldens",
            arg(&goldens),
            "--manifests",
            arg(&manifests),
        ],
        &[],
    );
    assert_eq!(code(&blessed), 2);
    assert!(text(&blessed.stderr).contains("max_bad_fraction"), "{}", text(&blessed.stderr));
    assert!(!manifests.join("s.toml").exists() && !goldens.join("objects").exists());
}

#[test]
fn the_folders_come_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let (goldens, manifests) = (dir.path().join("g"), dir.path().join("m"));
    let envs = [("OMUZAKI_GOLDENS", arg(&goldens)), ("OMUZAKI_GOLDEN_MANIFESTS", arg(&manifests))];

    assert_eq!(code(&run(&["check", "s/x", arg(&img), "--bless"], &envs)), 0);
    assert_eq!(code(&run(&["check", "s/x", arg(&img)], &envs)), 0);
    assert!(manifests.join("s.toml").exists() && goldens.join("objects").exists());
}

#[test]
fn the_default_manifest_folder_is_tests_goldens_of_the_workspace_around_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let member = dir.path().join("repo/crates/render");
    fs::create_dir_all(&member).unwrap();
    fs::write(dir.path().join("repo/Cargo.toml"), "[workspace]\nmembers = [\"crates/*\"]\n").unwrap();
    let goldens = dir.path().join("g");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_omuzaki-test"));
    cmd.env_remove("OMUZAKI_GOLDEN_MANIFESTS").env_remove("OMUZAKI_BLESS").env("OMUZAKI_GOLDENS", &goldens);
    let blessed = cmd.args(["check", "s/x", arg(&img), "--bless"]).current_dir(&member).output().unwrap();
    assert_eq!(code(&blessed), 0, "{}", text(&blessed.stderr));
    assert!(dir.path().join("repo/tests/goldens/s.toml").exists());
}

#[test]
fn blessing_repairs_a_lost_golden_image_as_the_error_advises() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let (goldens, manifests, out) = (dir.path().join("g"), dir.path().join("m"), dir.path().join("o"));
    let at = ["--goldens", arg(&goldens), "--manifests", arg(&manifests), "--out", arg(&out)];
    let check = |extra: &[&str]| run(&[&["check", "s/x", arg(&img)], extra, &at[..]].concat(), &[]);

    assert_eq!(code(&check(&["--bless"])), 0);
    let manifest = fs::read(manifests.join("s.toml")).unwrap();
    let object = goldens.join(format!("objects/{}.png", manifest_hash(&manifests.join("s.toml"))));
    fs::remove_file(&object).unwrap();

    let broken = check(&[]);
    assert_eq!(code(&broken), 2);
    assert!(text(&broken.stderr).contains("bless it again"), "{}", text(&broken.stderr));
    assert_eq!(code(&check(&["--bless"])), 0);
    assert!(object.exists());
    assert_eq!(fs::read(manifests.join("s.toml")).unwrap(), manifest, "the manifest already named this image");
    assert_eq!(code(&check(&[])), 0);
}

#[test]
fn tolerance_flags_for_an_existing_case_are_reported_as_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let (goldens, manifests, out) = (dir.path().join("g"), dir.path().join("m"), dir.path().join("o"));
    let at = ["--goldens", arg(&goldens), "--manifests", arg(&manifests), "--out", arg(&out)];
    let check = |extra: &[&str]| run(&[&["check", "s/x", arg(&img)], extra, &at[..]].concat(), &[]);

    let first = check(&["--bless", "--per-channel", "2"]);
    assert!(!text(&first.stderr).contains("note:"), "recording a new case's tolerance is not ignoring it");

    let differing = check(&["--per-channel", "5", "--max-bad-fraction", "0.1"]);
    let note = text(&differing.stderr);
    assert_eq!(code(&differing), 0);
    assert!(note.contains("note: s/x already has per_channel = 2, max_bad_fraction = 0"), "{note}");
    assert!(note.contains("--per-channel 5 and --max-bad-fraction 0.1 are ignored"), "{note}");

    let same = check(&["--per-channel", "2"]);
    assert!(!text(&same.stderr).contains("note:"), "a flag that agrees with the manifest needs no note");
    assert!(!text(&check(&[]).stderr).contains("note:"));
}

#[test]
fn an_empty_golden_store_variable_is_an_error_not_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let img = write(&dir, "x.png", &gradient());
    let manifests = dir.path().join("m");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_omuzaki-test"));
    cmd.env("OMUZAKI_GOLDENS", "").env_remove("OMUZAKI_GOLDEN_OUT").env_remove("OMUZAKI_BLESS");
    let args = ["check", "s/x", arg(&img), "--bless", "--manifests", arg(&manifests)];
    let output = cmd.args(args).current_dir(dir.path()).output().unwrap();
    assert_eq!(code(&output), 2);
    assert!(text(&output.stderr).contains("OMUZAKI_GOLDENS"), "{}", text(&output.stderr));
    assert!(!dir.path().join("objects").exists() && !manifests.exists(), "nothing may be written");
}

#[test]
fn each_checkout_reports_its_own_evidence_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (pass, fail) = (write(&dir, "pass.png", &gradient()), write(&dir, "fail.png", &RgbaImage::new(100, 100)));
    let goldens = dir.path().join("goldens");
    // Two checkouts of the repo (worktrees), each with its own manifests, sharing one store and no OMUZAKI_GOLDEN_OUT.
    let checkout = |name: &str| {
        let root = dir.path().join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        root
    };
    let (a, b) = (checkout("192-branch-a"), checkout("201-branch-b"));
    let run_in = |root: &Path, args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_omuzaki-test"));
        cmd.env_remove("OMUZAKI_GOLDEN_OUT").env_remove("OMUZAKI_GOLDEN_MANIFESTS").env_remove("OMUZAKI_BLESS");
        cmd.env("OMUZAKI_GOLDENS", &goldens).args(args).current_dir(root).output().unwrap()
    };

    // Both bless the same image for the same case; branch A then fails it, and branch B passes it.
    assert_eq!(code(&run_in(&a, &["check", "s/x", arg(&pass), "--bless"])), 0);
    assert_eq!(code(&run_in(&b, &["check", "s/x", arg(&pass), "--bless"])), 0);
    assert_eq!(code(&run_in(&a, &["check", "s/x", arg(&fail)])), 1);
    assert_eq!(code(&run_in(&b, &["check", "s/x", arg(&pass)])), 0);

    let report_a = text(&run_in(&a, &["report"]).stdout);
    assert!(report_a.contains("| `s/x` | differs |"), "B's pass must not erase A's failure: {report_a}");
    assert!(text(&run_in(&b, &["report"]).stdout).contains("No golden image changed."));
    assert!(dir.path().join("goldens-out/192-branch-a/s/x.json").exists());
}

/// The image the manifest at `manifest` names for `case`, as a path in the object store `goldens`.
fn object_of(goldens: &Path, manifest: &Path, case: &str) -> PathBuf {
    let manifest = omuzaki_test::manifest::Manifest::load(manifest).unwrap();
    let entry = manifest.get(&omuzaki_core::id::Slug::new(case).unwrap()).unwrap();
    goldens.join(format!("objects/{}.png", entry.hash))
}

#[test]
fn verify_names_the_cases_whose_image_was_deleted_or_tampered_with() {
    let dir = tempfile::tempdir().unwrap();
    let (goldens, manifests) = (dir.path().join("goldens"), dir.path().join("manifests"));
    let at = ["--goldens", arg(&goldens), "--manifests", arg(&manifests)];
    let verify = || run(&[&["verify"], &at[..]].concat(), &[]);
    for (case, shade) in [("gone", 10u8), ("bent", 20), ("fine", 30)] {
        let image = write(&dir, &format!("{case}.png"), &RgbaImage::from_pixel(8, 8, Rgba([shade, 0, 0, 255])));
        let blessed = run(&[&["check", &format!("s/{case}"), arg(&image), "--bless"], &at[..]].concat(), &[]);
        assert_eq!(code(&blessed), 0, "{}", text(&blessed.stderr));
    }

    let clean = verify();
    assert_eq!(code(&clean), 0, "{}", text(&clean.stderr));
    assert!(
        text(&clean.stdout).contains("0 of 3 cases have a missing or damaged image, in 1 manifest."),
        "{}",
        text(&clean.stdout)
    );

    let manifest = manifests.join("s.toml");
    fs::remove_file(object_of(&goldens, &manifest, "gone")).unwrap();
    fs::write(object_of(&goldens, &manifest, "bent"), b"tampered").unwrap();
    let broken = verify();
    let out = text(&broken.stdout);
    assert_eq!(code(&broken), 1, "{out}{}", text(&broken.stderr));
    assert!(out.contains("MISSING: s/gone: "), "{out}");
    assert!(out.contains("CORRUPT: s/bent: "), "{out}");
    assert!(!out.contains("s/fine"), "the intact case is not listed: {out}");
    assert!(out.contains("2 of 3 cases have"), "{out}");

    // Blessing the lost case again repairs it; the tampered one still fails until it is repaired too.
    let again = write(&dir, "again.png", &RgbaImage::from_pixel(8, 8, Rgba([10, 0, 0, 255])));
    assert_eq!(code(&run(&[&["check", "s/gone", arg(&again), "--bless"], &at[..]].concat(), &[])), 0);
    let partly = text(&verify().stdout);
    assert!(!partly.contains("s/gone") && partly.contains("s/bent"), "{partly}");
}

#[test]
fn verify_without_a_store_is_a_harness_error() {
    let dir = tempfile::tempdir().unwrap();
    let no_store = run(&["verify", "--manifests", arg(dir.path())], &[]);
    assert_eq!(code(&no_store), 2);
    assert!(text(&no_store.stderr).contains("OMUZAKI_GOLDENS"), "{}", text(&no_store.stderr));
}

#[test]
fn verify_refuses_a_missing_or_empty_manifest_folder_with_exit_2_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let goldens = dir.path().join("goldens");
    let missing = dir.path().join("typo-manifests");
    let empty = dir.path().join("empty-manifests");
    fs::create_dir_all(&empty).unwrap();

    for folder in [&missing, &empty] {
        let out = run(&["verify", "--goldens", arg(&goldens), "--manifests", arg(folder)], &[]);
        assert_eq!(code(&out), 2, "{}{}", text(&out.stdout), text(&out.stderr));
        assert!(text(&out.stdout).is_empty(), "nothing is reported as verified: {}", text(&out.stdout));
        assert!(text(&out.stderr).contains(arg(folder)), "the message names the folder: {}", text(&out.stderr));
    }
}
