//! The file fence around ops: every crate that registers ops forbids reaching files by path, in its own
//! `clippy.toml`.
//!
//! `OpCtx::fs_access` returns a `CanonicalPath`, whose own methods open it without following a link at any step. It
//! has no `&Path`, so the way around it is to turn it into text and open that with `std::fs`, which a link swapped in
//! after the check would redirect. Clippy's `disallowed-methods` refuses those calls in op crates (it sees through
//! `use std::fs as f` and the like), and a legitimate use is allowed where it is, with its reason. What clippy cannot
//! notice is its own configuration missing: this test fails a crate that registers ops without the fence, a fence
//! that lost a method, and a crate file that dropped the workspace's settings (a crate's `clippy.toml` replaces the
//! root one).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Every way of reaching a file through its path that an op crate must not call. The `std::os` ones exist on one
/// platform each, so the fences mark them `allow-invalid` (clippy skips a path it cannot resolve on this target).
const FENCED: [&str; 37] = [
    "std::fs::File::open",
    "std::fs::File::create",
    "std::fs::File::create_new",
    "std::fs::OpenOptions::open",
    "std::fs::DirBuilder::create",
    "std::fs::read",
    "std::fs::read_to_string",
    "std::fs::write",
    "std::fs::exists",
    "std::fs::create_dir",
    "std::fs::create_dir_all",
    "std::fs::remove_file",
    "std::fs::remove_dir",
    "std::fs::remove_dir_all",
    "std::fs::rename",
    "std::fs::copy",
    "std::fs::hard_link",
    "std::fs::soft_link",
    "std::fs::set_permissions",
    "std::fs::read_link",
    "std::fs::read_dir",
    "std::fs::metadata",
    "std::fs::symlink_metadata",
    "std::fs::canonicalize",
    "std::os::unix::fs::symlink",
    "std::os::windows::fs::symlink_file",
    "std::os::windows::fs::symlink_dir",
    "std::path::Path::exists",
    "std::path::Path::try_exists",
    "std::path::Path::is_file",
    "std::path::Path::is_dir",
    "std::path::Path::is_symlink",
    "std::path::Path::metadata",
    "std::path::Path::symlink_metadata",
    "std::path::Path::read_link",
    "std::path::Path::read_dir",
    "std::path::Path::canonicalize",
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Whether a crate's library or binaries register an op: `register_op!` invoked anywhere in code (a wrapper macro's
/// body included), however it is spaced (`register_op! (X)`, `register_op ! { X }`). Comment lines are skipped, so a
/// doc example does not count; anything else that looks like a call counts, which can only demand a fence too many.
fn registers_ops(krate: &Path) -> bool {
    let mut sources = Vec::new();
    rust_sources(&krate.join("src"), &mut sources);
    sources.iter().any(|source| invokes_register_op(&fs::read_to_string(source).unwrap()))
}

fn invokes_register_op(source: &str) -> bool {
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    ["register_op!(", "register_op!{", "register_op!["].iter().any(|call| code.contains(call))
}

/// The setting lines of a `clippy.toml`: no comments, no blank lines.
fn settings(text: &str) -> Vec<&str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect()
}

/// What is wrong with an op crate's fence, if anything.
fn faults(fence: Option<&str>, workspace: &str) -> Vec<String> {
    let Some(fence) = fence else { return vec!["no clippy.toml".to_owned()] };
    let lines = settings(fence);
    let missing_setting = settings(workspace)
        .into_iter()
        .filter(|setting| !lines.contains(setting))
        .map(|setting| format!("drops the workspace setting `{setting}`"));
    let missing_method = FENCED
        .iter()
        .filter(|method| !lines.iter().any(|line| line.starts_with(&format!("{{ path = \"{method}\""))))
        .map(|method| format!("does not forbid `{method}`"));
    missing_setting.chain(missing_method).collect()
}

#[test]
fn every_crate_that_registers_ops_forbids_reaching_files_by_path() {
    let workspace = fs::read_to_string(crates_dir().parent().unwrap().join("clippy.toml")).unwrap();
    let mut op_crates = Vec::new();
    let mut problems = Vec::new();
    for entry in fs::read_dir(crates_dir()).unwrap() {
        let krate = entry.unwrap().path();
        if !krate.join("src").is_dir() || !registers_ops(&krate) {
            continue;
        }
        let fence = fs::read_to_string(krate.join("clippy.toml")).ok();
        let name = krate.file_name().unwrap().to_string_lossy().into_owned();
        problems.extend(faults(fence.as_deref(), &workspace).into_iter().map(|fault| format!("{name}: {fault}")));
        op_crates.push(name);
    }
    assert!(op_crates.len() >= 3, "the scan should find the op crates, found {op_crates:?}");
    assert!(
        problems.is_empty(),
        "crates that register ops must carry the file fence (copy `crates/omuzaki-face/clippy.toml`):\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_registration_is_found_however_it_is_written() {
    for source in [
        "register_op!(Inspect);",
        "omuzaki_ops_api::register_op!(Inspect);",
        "register_op! (Inspect);",
        "register_op ! {\n    Inspect\n}",
        "register_op!\n(Inspect);",
        "macro_rules! ops { ($($op:ty),*) => { $(register_op!($op);)* } }",
    ] {
        assert!(invokes_register_op(source), "{source}");
    }
    for source in ["/// omuzaki_ops_api::register_op!(Example);", "// register_op!(Old);", "macro_rules! register_op {"]
    {
        assert!(!invokes_register_op(source), "{source}");
    }
}

#[test]
fn a_fence_that_is_missing_or_incomplete_is_caught() {
    let workspace = "# why\nallow-unwrap-in-tests = true\n";
    assert_eq!(faults(None, workspace), ["no clippy.toml"]);
    let mut fence = String::from("allow-unwrap-in-tests = true\ndisallowed-methods = [\n");
    for method in FENCED {
        writeln!(fence, "    {{ path = \"{method}\", reason = \"r\" }},").unwrap();
    }
    fence.push_str("]\n");
    assert_eq!(faults(Some(&fence), workspace), Vec::<String>::new());
    let without_read = fence.replace("{ path = \"std::fs::read\",", "{ path = \"std::fs::nothing\",");
    assert_eq!(faults(Some(&without_read), workspace), ["does not forbid `std::fs::read`"]);
    let without_setting = fence.replace("allow-unwrap-in-tests = true\n", "");
    assert_eq!(
        faults(Some(&without_setting), workspace),
        ["drops the workspace setting `allow-unwrap-in-tests = true`"]
    );
}
