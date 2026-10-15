//! End-to-end tests of `omuzaki-test eval lint` and `eval list`: the real workspace lints clean, and each
//! rule fails a throwaway workspace with exit 1 and a finding naming the file.
// Helpers sit outside `#[test]` functions, where clippy.toml's unwrap exemption doesn't reach.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{arg, code, run, text};
use tempfile::TempDir;

const SPEC_ROW: &str = "| QG-03 | Agent evals | P0 | notes |\n";
const TASK: &str = "id = 'a.one'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make her hair shorter'\nfixture = 'f'\nscopes = ['read']\n\
                    [[check]]\nid = 'c'\nkind = 'doc.exists'\npath = 'a'\n";
const FIXTURE: &str = "id = 'f'\nsummary = 's'\nsteps = []\n";

fn real_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A minimal workspace that lints clean: config files, a spec with one feature row, one task with both scripts.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "docs/product/02-studio-spec-v3.md", SPEC_ROW);
    write(root, "crates/omuzaki-test/evals/defaults.toml", "[caps]\nturns=1\ntool_calls=1\nwall_s=1\ntokens=1\n");
    write(root, "docs/licensing/private-material.toml", "prefixes = ['characters/example-private/']\nhashes = []\n");
    write(root, "crates/c/evals/a.one.toml", TASK);
    write(root, "crates/c/evals/a.one.x.pass.jsonl", "{}\n");
    write(root, "crates/c/evals/a.one.x.fail.jsonl", "{}\n");
    write(root, "crates/c/evals/fixtures/f.toml", FIXTURE);
    dir
}

fn lint(root: &Path, extra: &[&str]) -> (i32, String) {
    let mut args = vec!["eval", "lint", "--root", arg(root)];
    args.extend_from_slice(extra);
    let out = run(&args, &[]);
    (code(&out), text(&out.stdout))
}

#[test]
fn the_real_workspace_lints_clean() {
    let (status, out) = lint(&real_root(), &[]);
    assert_eq!(status, 0, "{out}");
}

#[test]
fn a_clean_workspace_passes_and_a_missing_script_fails_by_file() {
    let dir = workspace();
    assert_eq!(lint(dir.path(), &[]).0, 0);
    fs::remove_file(dir.path().join("crates/c/evals/a.one.x.fail.jsonl")).unwrap();
    let (status, out) = lint(dir.path(), &[]);
    assert_eq!(status, 1);
    assert!(out.contains("crates/c/evals/a.one.toml") && out.contains("fail.jsonl"), "{out}");
}

#[test]
fn every_finding_is_listed_not_just_the_first() {
    let dir = workspace();
    write(
        dir.path(),
        "crates/c/evals/a.two.toml",
        &TASK.replace("QG-03", "ZZ-99").replace("fixture = 'f'", "fixture = 'ghost'"),
    );
    let (status, out) = lint(dir.path(), &[]);
    assert_eq!(status, 1);
    assert!(
        out.contains("unknown fixture `ghost`") && out.contains("ZZ-99") && out.contains("duplicate task id"),
        "{out}"
    );
}

#[test]
fn an_unknown_check_kind_is_named() {
    let dir = workspace();
    write(dir.path(), "crates/c/evals/a.one.toml", &TASK.replace("doc.exists", "doc.sparkle"));
    let (status, out) = lint(dir.path(), &[]);
    assert_eq!(status, 1);
    assert!(out.contains("a.one.toml: unknown check kind `doc.sparkle`"), "{out}");
}

#[test]
fn a_prompt_quoting_a_tool_description_fails_with_a_tools_dump() {
    let dir = workspace();
    let tools = dir.path().join("tools.json");
    fs::write(
        &tools,
        r#"[{"name":"t","description":"Use when the user says make her hair shorter than it is now, please."}]"#,
    )
    .unwrap();
    assert_eq!(lint(dir.path(), &["--tools", arg(&tools)]).0, 0, "four words is not a quote");
    write(
        dir.path(),
        "crates/c/evals/a.one.toml",
        &TASK.replace("Make her hair shorter", "make her hair shorter than it is now"),
    );
    let (status, out) = lint(dir.path(), &["--tools", arg(&tools)]);
    assert_eq!(status, 1);
    assert!(out.contains("prompt shares 8+ words with tool t"), "{out}");
}

#[test]
fn a_fixture_item_from_example_private_is_refused_naming_the_private_list_even_with_a_yes_record() {
    let dir = workspace();
    write(
        dir.path(),
        "crates/c/evals/fixtures/f.toml",
        "id = 'f'\nsummary = 's'\nsteps = [{ op = 'a.b', args = { sheet = { set = 's1', item = 'b3:01' } } }]\n",
    );
    write(
        dir.path(),
        "crates/c/evalsets/s1.toml",
        "id='s1'\nkind='synthetic'\nsplit='dev'\nsummary='s'\n[[item]]\nhash='b3:01'\npath='D:/assets/characters/example-private/concept/a.png'\n\
         [item.provenance]\nsource='own'\ncommercial='yes'\n",
    );
    let (status, out) = lint(dir.path(), &[]);
    assert_eq!(status, 1);
    assert!(out.contains("private list") && out.contains("private-material.toml"), "{out}");
}

#[test]
fn a_missing_private_list_is_harness_trouble_not_a_pass() {
    let dir = workspace();
    fs::remove_file(dir.path().join("docs/licensing/private-material.toml")).unwrap();
    assert_eq!(lint(dir.path(), &[]).0, 2);
}

fn list(root: &Path) -> serde_json::Value {
    let out = run(&["eval", "list", "--json", "--root", arg(root)], &[]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn list_json_matches_the_golden_for_the_sample_task() {
    let rows = list(&real_root());
    let sample = rows.as_array().unwrap().iter().find(|r| r["id"] == "sample.rename-character").unwrap().clone();
    let hash = sample["hash"].as_str().unwrap().to_owned();
    assert!(hash.starts_with("b3:") && hash.len() == 3 + 64, "{hash}");
    let golden = serde_json::json!({
        "id": "sample.rename-character", "feature": "QG-03", "crate": "omuzaki-test",
        "tags": ["starter"], "version": 1, "hash": hash,
    });
    assert_eq!(sample, golden);
}

#[test]
fn the_hash_changes_with_the_task_a_script_or_the_fixture() {
    let dir = workspace();
    let hash = || list(dir.path())[0]["hash"].as_str().unwrap().to_owned();
    let before = hash();
    assert_eq!(before, hash(), "deterministic");
    for (file, content) in [
        ("crates/c/evals/a.one.x.pass.jsonl", "{\"changed\":1}\n"),
        ("crates/c/evals/fixtures/f.toml", "id = 'f'\nsummary = 'other'\nsteps = []\n"),
        ("crates/c/evals/a.one.toml", &format!("{TASK}# note\n")),
    ] {
        let original = fs::read_to_string(dir.path().join(file)).unwrap();
        write(dir.path(), file, content);
        assert_ne!(before, hash(), "{file}");
        write(dir.path(), file, &original);
    }
    assert_eq!(before, hash());
}

#[test]
fn list_fails_and_prints_no_catalogue_when_a_task_file_is_malformed() {
    let dir = workspace();
    write(dir.path(), "crates/c/evals/broken.toml", "id = 'broken'\nbogus = 1\n");
    let out = run(&["eval", "list", "--json", "--root", arg(dir.path())], &[]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stdout).is_empty(), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("broken.toml: does not parse"), "{}", text(&out.stderr));
}
