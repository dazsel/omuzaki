//! Helpers shared by the end-to-end tests of the `omuzaki-test` binary.
// Test helpers sit outside `#[test]` functions, where clippy.toml's unwrap exemption doesn't reach; a panic is the
// right failure here.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

/// A path as a command-line argument (temp paths are always UTF-8).
pub fn arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Runs the CLI with a clean environment, so the developer's own `OMUZAKI_*` variables can't change the result, and from
/// outside the repo, so a test that forgets `--manifests` can never find (and write to) the real `tests/goldens`.
pub fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_omuzaki-test"));
    cmd.env_remove("OMUZAKI_GOLDENS").env_remove("OMUZAKI_GOLDEN_MANIFESTS").env_remove("OMUZAKI_GOLDEN_OUT");
    cmd.env_remove("OMUZAKI_BLESS").env_remove("OMUZAKI_GC_TEST_NOW").current_dir(std::env::temp_dir());
    cmd.args(args).envs(envs.iter().copied());
    cmd.output().unwrap()
}

/// The exit code of a finished command.
pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

/// Output bytes as text.
pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
