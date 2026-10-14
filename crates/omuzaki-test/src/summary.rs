//! Turns the evidence folder into a PR comment: which golden images changed, by how much, and where to look.
//!
//! Why paths and not embedded pictures: GitHub has no API for attaching images to a comment, and in a repo whose images are not public the
//! trick of linking images from a branch or release needs those images committed or published, which the repository's rules
//! forbid for heavy assets. The images already sit in the shared assets folder, so the comment names that folder (as a
//! Windows path, since a maintainer opens it from Explorer) and, in CI, links the run's artifacts page when the workflow
//! uploads the folder. See `crates/omuzaki-test/README.md`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use omuzaki_store_api::ContentHash;

use crate::dirs::ReportDirs;
use crate::fsio::sorted_entries;
use crate::goldens::{Finding, GoldenError, Status};

/// Every report starts with this, so a CI step can find and update its own comment instead of
/// posting a new one on each push.
pub const COMMENT_MARKER: &str = "<!-- omuzaki-golden-report";

/// The first line when something changed.
const MARKER_CHANGED: &str = "<!-- omuzaki-golden-report: changed -->";
/// The first line when nothing changed: the script then skips posting a comment that has nothing to say.
const MARKER_NONE: &str = "<!-- omuzaki-golden-report: none -->";

/// Reads every finding under `out`, ordered by suite then name so the output never depends on directory order.
/// A missing folder means nothing was recorded.
///
/// # Errors
/// [`GoldenError`] when the folder can't be listed or a `.json` file isn't a finding.
pub fn load_findings(out: &Path) -> Result<Vec<Finding>, GoldenError> {
    let mut findings = Vec::new();
    for suite_dir in sorted_entries(out)? {
        if !suite_dir.is_dir() {
            continue;
        }
        for file in sorted_entries(&suite_dir)? {
            if file.extension().is_some_and(|ext| ext == "json") {
                let bytes = fs::read(&file).map_err(|source| GoldenError::Io { path: file.clone(), source })?;
                findings.push(
                    serde_json::from_slice(&bytes)
                        .map_err(|source| GoldenError::Json { path: file.clone(), source })?,
                );
            }
        }
    }
    findings.sort_by(|a: &Finding, b| (&a.suite, &a.name).cmp(&(&b.suite, &b.name)));
    Ok(findings)
}

/// Most table rows in one comment. GitHub refuses comments over 65,536 characters; a row is at most ~300, so this stays
/// far below it. Failures are listed first, so what needs attention is never the part that gets cut.
const MAX_ROWS: usize = 100;

/// Renders the PR comment (Markdown) for `findings`.
///
/// `dirs` says where to look: the evidence folder (expected, actual and diff pictures) and, when known, the object
/// store that holds the golden images themselves. `artifact_url` is an optional link to where CI uploaded the evidence.
pub fn markdown(findings: &[Finding], dirs: &ReportDirs, artifact_url: Option<&str>) -> String {
    if findings.is_empty() {
        return format!("{MARKER_NONE}\n### Golden images\n\nNo golden image changed.\n");
    }
    let mut md = format!("{MARKER_CHANGED}\n### Golden images\n\n");
    let failing = findings.iter().filter(|f| f.is_failure()).count();
    let _ = writeln!(md, "{} changed, {} needing attention.\n", findings.len(), failing);
    md.push_str("| Image | Status | Difference | Size | Hash (golden → image) |\n|---|---|---|---|---|\n");
    let mut rows: Vec<&Finding> = findings.iter().collect();
    rows.sort_by_key(|f| !f.is_failure()); // stable: each group stays in suite/name order
    for f in rows.iter().take(MAX_ROWS) {
        let difference = f.stats.map_or_else(|| "n/a".to_owned(), |s| s.to_string());
        let _ = writeln!(
            md,
            "| `{}/{}` | {} | {} | {} | {} |",
            f.suite,
            f.name,
            f.status.label(),
            difference,
            size_text(f),
            hash_text(f)
        );
    }
    if rows.len() > MAX_ROWS {
        let _ = writeln!(md, "\n...and {} more, listed only in the folder below.", rows.len() - MAX_ROWS);
    }
    let _ = writeln!(
        md,
        "\nEach image has `<suite>/<name>.expected.png`, `.actual.png` and `.diff.png` (bad pixels in red) in:"
    );
    let _ = writeln!(md, "\n`{}`", display_path(&dirs.evidence));
    if let Some(url) = artifact_url {
        let _ = writeln!(md, "\nThe same folder as CI artifacts: {url}");
    }
    let location = dirs.objects.as_deref().map_or_else(
        || "the `objects` folder of the golden store".to_owned(),
        |objects| format!("`{}`", display_path(objects)),
    );
    let _ = writeln!(
        md,
        "\nThe goldens themselves are `<hash>.png` in {location}. Hashes are shortened here; `tests/goldens/<suite>.toml` \
         has them in full."
    );
    if findings.iter().any(|f| f.status != Status::Blessed) {
        md.push_str(
            "\nIf these changes are intended, re-run the tests with `OMUZAKI_BLESS=1`: it stores the new images and \
             updates `tests/goldens/<suite>.toml` in your working tree, so commit the manifest with the PR. The \
             previous images stay in the folders above.\n",
        );
    }
    md
}

fn size_text(f: &Finding) -> String {
    let [w, h] = f.actual_size;
    match f.expected_size {
        Some([ew, eh]) if (ew, eh) != (w, h) => format!("{w}x{h} (was {ew}x{eh})"),
        _ => format!("{w}x{h}"),
    }
}

/// How many hex characters of a hash the table shows: enough to tell goldens apart and to find the file.
const SHORT_HASH_LEN: usize = 12;

fn short(hash: &ContentHash) -> String {
    hash.to_string()[..SHORT_HASH_LEN].to_owned()
}

/// `before → after`, with `none` for a case that had no golden.
fn hash_text(f: &Finding) -> String {
    let before = f.expected_hash.as_ref().map_or_else(|| "none".to_owned(), |hash| format!("`{}`", short(hash)));
    format!("{before} → `{}`", short(&f.actual_hash))
}

/// A path as a Windows user types it: a WSL mount path shown as a Windows drive path; anything else is left alone.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let drive_path = text.strip_prefix("/mnt/").and_then(|rest| {
        let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
        (drive.len() == 1 && drive.chars().all(|c| c.is_ascii_lowercase())).then_some((drive, tail))
    });
    match drive_path {
        Some((drive, tail)) => format!("{}:\\{}", drive.to_ascii_uppercase(), tail.replace('/', "\\")),
        None => text.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;
    use crate::compare::Tolerance;
    use omuzaki_core::id::Slug;

    use crate::goldens::{GoldenId, Goldens, Mode};

    fn flat(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(16, 16, Rgba(color))
    }

    /// A store in a temp folder: images in `goldens/`, manifests in `manifests/`, evidence in `out/`.
    fn store(dir: &tempfile::TempDir) -> Goldens {
        Goldens::new(dir.path().join("goldens"), dir.path().join("manifests"), dir.path().join("out"))
    }

    fn dirs(evidence: &str, objects: Option<&str>) -> ReportDirs {
        ReportDirs { evidence: evidence.into(), objects: objects.map(Into::into) }
    }

    #[test]
    fn windows_paths_for_wsl_drives() {
        assert_eq!(display_path(Path::new("/mnt/e/assets/goldens-out")), "E:\\assets\\goldens-out");
        assert_eq!(display_path(Path::new("/mnt/c")), "C:\\");
        assert_eq!(display_path(Path::new("/home/user/out")), "/home/user/out");
        assert_eq!(display_path(Path::new("/mnt/data/x")), "/mnt/data/x");
    }

    #[test]
    fn an_empty_folder_says_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_findings(&dir.path().join("does-not-exist")).unwrap().len(), 0);
        let md = markdown(&[], &dirs("/some/out", None), None);
        assert!(md.starts_with(MARKER_NONE) && md.contains("No golden image changed."));
    }

    #[test]
    fn findings_round_trip_through_the_folder_in_a_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let goldens = store(&dir);
        let old = flat([0, 0, 0, 255]);
        for id in ["b/two", "a/one", "b/one"] {
            let id = GoldenId::parse(id).unwrap();
            goldens.check(&id, &old, Tolerance::EXACT, Mode::Bless).unwrap();
        }
        let id = GoldenId::parse("b/two").unwrap();
        goldens.check(&id, &flat([255, 0, 0, 255]), Tolerance::EXACT, Mode::Check).unwrap();

        let findings = load_findings(goldens.out()).unwrap();
        let names: Vec<_> = findings.iter().map(|f| format!("{}/{}", f.suite, f.name)).collect();
        assert_eq!(names, ["a/one", "b/one", "b/two"]);
        assert_eq!(findings[2].status, Status::Failed);
    }

    #[test]
    fn the_comment_lists_each_image_with_its_numbers_hashes_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let goldens = store(&dir);
        let id = GoldenId::parse("render/front-view").unwrap();
        goldens.check(&id, &flat([0, 0, 0, 255]), Tolerance::EXACT, Mode::Bless).unwrap();
        goldens.check(&id, &flat([255, 0, 0, 255]), Tolerance::EXACT, Mode::Check).unwrap();

        let findings = load_findings(goldens.out()).unwrap();
        let (before, after) = (findings[0].expected_hash.unwrap().to_string(), findings[0].actual_hash.to_string());
        assert_eq!(
            before,
            ContentHash::of(&fs::read(goldens.out().join("render/front-view.expected.png")).unwrap()).to_string()
        );
        let report = dirs("/mnt/e/assets/goldens-out/pr-9", Some("/mnt/e/assets/goldens/objects"));
        let md = markdown(&findings, &report, Some("https://example.test/run/1"));
        assert!(md.starts_with(MARKER_CHANGED) && MARKER_CHANGED.starts_with(COMMENT_MARKER), "{md}");
        let row = format!(
            "| `render/front-view` | differs | 100.000% bad (256 of 256 px), max delta 255, PSNR 6.02 dB | 16x16 | `{}` → `{}` |",
            &before[..12],
            &after[..12]
        );
        assert!(md.contains(&row), "{md}");
        assert!(md.contains("`E:\\assets\\goldens-out\\pr-9`"), "{md}");
        assert!(md.contains("`<hash>.png` in `E:\\assets\\goldens\\objects`"), "{md}");
        assert!(md.contains("https://example.test/run/1"), "{md}");
        assert!(md.contains("OMUZAKI_BLESS=1") && md.contains("tests/goldens/<suite>.toml"), "{md}");
        assert_eq!(md, markdown(&findings, &report, Some("https://example.test/run/1")));
    }

    #[test]
    fn a_new_case_shows_no_before_hash_and_an_unknown_store_is_named_generically() {
        let dir = tempfile::tempdir().unwrap();
        let goldens = store(&dir);
        let id = GoldenId::parse("render/brand-new").unwrap();
        goldens.check(&id, &flat([1; 4]), Tolerance::EXACT, Mode::Check).unwrap();
        let findings = load_findings(goldens.out()).unwrap();
        let md = markdown(&findings, &dirs("/some/out", None), None);
        assert!(md.contains(&format!("| none → `{}` |", &findings[0].actual_hash.to_string()[..12])), "{md}");
        assert!(md.contains("in the `objects` folder of the golden store"), "{md}");
    }

    #[test]
    fn blessed_only_runs_do_not_tell_people_to_bless() {
        let dir = tempfile::tempdir().unwrap();
        let goldens = store(&dir);
        let id = GoldenId::parse("render/new-one").unwrap();
        goldens.check(&id, &flat([1; 4]), Tolerance::EXACT, Mode::Bless).unwrap();
        let md = markdown(&load_findings(goldens.out()).unwrap(), &dirs("/some/out", None), None);
        assert!(md.contains("1 changed, 0 needing attention."), "{md}");
        assert!(!md.contains("OMUZAKI_BLESS"), "{md}");
    }

    #[test]
    fn a_finding_with_markdown_in_its_names_is_refused_not_rendered() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s")).unwrap();
        let evil = r#"{"suite":"s|x\n![](https://evil.test/a.png)","name":"n","status":"failed","stats":null,
                       "actual_size":[1,1],"expected_size":null}"#;
        fs::write(dir.path().join("s/evil.json"), evil).unwrap();
        let err = load_findings(dir.path()).unwrap_err();
        assert!(err.to_string().contains("evil.json"), "{err}");
    }

    #[test]
    fn a_huge_report_stays_under_githubs_comment_limit_with_failures_first() {
        // The longest rows possible: 64-character names, every number at its widest, both hashes present.
        let finding = |name: &str, status| Finding {
            suite: Slug::new("s".repeat(64)).unwrap(),
            name: Slug::new(name).unwrap(),
            status,
            stats: Some(crate::compare::Stats {
                width: u32::MAX,
                height: u32::MAX,
                bad_pixels: u64::MAX,
                bad_fraction: 1.0,
                max_delta: 255,
                psnr_db: Some(100.0),
                passed: false,
            }),
            actual_size: [u32::MAX; 2],
            expected_size: Some([u32::MAX - 1; 2]),
            actual_hash: ContentHash([0xab; 32]),
            expected_hash: Some(ContentHash([0xcd; 32])),
        };
        let mut findings: Vec<_> = (0..500).map(|i| finding(&format!("{i:0>64}"), Status::Blessed)).collect();
        findings.push(finding(&format!("{:z>64}", ""), Status::Failed));
        let md = markdown(&findings, &dirs("/some/out", Some("/some/objects")), None);
        assert!(md.len() < 65_536, "{} chars", md.len());
        assert!(md.contains(&"z".repeat(64)), "the failing image sorts last by name but is listed");
        assert!(md.contains("...and 401 more"), "{md}");
    }

    #[test]
    fn a_broken_json_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s")).unwrap();
        fs::write(dir.path().join("s/x.json"), "{").unwrap();
        let err = load_findings(dir.path()).unwrap_err();
        assert!(err.to_string().contains("x.json"), "{err}");
    }
}
