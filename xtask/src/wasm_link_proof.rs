//! `cargo xtask wasm-link-proof`: proves that a `registers` crate keeps its self-registrations in a WebAssembly App
//! (ADR 0015, ADR 0054 §6).
//!
//! The native link gates (each App's `tests/registrations.rs`) can't catch a missing `link()` call: natively rustc
//! keeps every registration of a linked crate. On wasm it doesn't, so this command builds the test-only App
//! `omuzaki-link-probe` (which links `omuzaki-char` through its generated `link()` list) for `wasm32-unknown-unknown`,
//! runs the module in node (`crates/omuzaki-link-probe/run.mjs`), and compares what the module registered with what the
//! source declares, by name, as the native gates do.
//!
//! `--unlinked` is the negative case: the probe's build script generates an empty `link()`, as it would if the crate
//! lost its `registers` flag, and the command passes only if the module then holds none of those registrations. It
//! shows that the positive run can fail.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use clap::Args;
use xtask::registry::{self, Registration};

use crate::error::{Error, Result};
use crate::process::{capture, cargo, describe};
use crate::workspace::Workspace;

/// The test-only App the proof builds.
const PROBE: &str = "omuzaki-link-probe";
/// The node script that instantiates the module and prints its registrations, relative to the workspace root.
const RUNNER: &str = "crates/omuzaki-link-probe/run.mjs";
/// The target the web player ships on, with one codegen unit per crate from `.cargo/config.toml`.
const TARGET: &str = "wasm32-unknown-unknown";

/// Arguments of `cargo xtask wasm-link-proof`.
#[derive(Debug, Args)]
pub struct WasmLinkProofArgs {
    /// Build the negative case (an empty generated `link()`) and pass only if its registrations are gone.
    #[arg(long)]
    unlinked: bool,
}

/// Builds and runs the probe, then prints the verdict; `FAILURE` when the module lost a registration (or, with
/// `--unlinked`, kept one).
///
/// # Errors
/// When cargo or node can't run, the build fails, or the module prints something that isn't a registration.
pub fn run(workspace: &Workspace, args: &WasmLinkProofArgs) -> Result<ExitCode> {
    let module = build(workspace, args.unlinked)?;
    let mut node = Command::new("node");
    node.current_dir(&workspace.root).arg(RUNNER).arg(&module);
    let printed = String::from_utf8_lossy(&capture(&mut node)?).into_owned();
    let linked = printed
        .lines()
        .map(|line| {
            Registration::parse(line).ok_or_else(|| Error::Failed {
                command: describe(&node),
                stderr: format!("printed a line that isn't a registration: {line:?}"),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected = registry::expected(&workspace.root, PROBE, None)?;

    if args.unlinked {
        return Ok(negative_verdict(&expected, &linked));
    }
    Ok(match registry::verify(&format!("{PROBE} ({TARGET})"), &expected, &linked) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(report) => {
            eprint!("{report}");
            ExitCode::FAILURE
        }
    })
}

/// The negative case passes when the module holds no registration of a package the probe should have linked, and
/// there were some to lose: an empty expected set would prove nothing.
fn negative_verdict(expected: &[Registration], linked: &[Registration]) -> ExitCode {
    let packages: BTreeSet<&str> = expected.iter().map(|r| r.package.as_str()).collect();
    let kept = linked.iter().filter(|r| packages.contains(r.package.as_str())).count();
    if !expected.is_empty() && kept == 0 {
        println!(
            "wasm-link-proof: without the generated `link()` call the {TARGET} module lost all {} registrations, so \
             the positive run can fail",
            expected.len()
        );
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "wasm-link-proof: the unlinked {TARGET} module kept {kept} of {} registrations; the positive run proves \
         nothing until it can lose them",
        expected.len()
    );
    ExitCode::FAILURE
}

/// Builds the probe for wasm and returns the module's path, read from cargo's own artifact messages so a custom
/// target directory works. Cargo's diagnostics and progress go to stderr as usual.
fn build(workspace: &Workspace, unlinked: bool) -> Result<String> {
    let mut command = Command::new(cargo());
    command.current_dir(&workspace.root).args([
        "build",
        "--locked",
        "--message-format=json-render-diagnostics",
        "--target",
        TARGET,
        "-p",
        PROBE,
    ]);
    if unlinked {
        command.args(["--features", &format!("{PROBE}/unlinked")]);
    }
    command.stdout(Stdio::piped());
    let line = describe(&command);
    let failed = |stderr: String| Error::Failed { command: line.clone(), stderr };
    let mut child = command.spawn().map_err(|source| Error::Spawn { command: line.clone(), source })?;
    let mut module = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|e| failed(format!("cannot read its output: {e}")))?;
            module = module.or_else(|| wasm_artifact(&line));
        }
    }
    let status = child.wait().map_err(|e| failed(format!("cannot wait for it: {e}")))?;
    if !status.success() {
        return Err(failed(format!("exited with {status}; its errors are above")));
    }
    module.ok_or_else(|| failed(format!("built no `.wasm` for {PROBE}")))
}

/// The `.wasm` file named by one line of cargo's JSON output, if the line is the probe's artifact message.
fn wasm_artifact(line: &str) -> Option<String> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    if message["reason"] != "compiler-artifact" || message["target"]["name"] != PROBE.replace('-', "_") {
        return None;
    }
    message["filenames"]
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|f| Path::new(f).extension().is_some_and(|e| e == "wasm"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(package: &str, name: &str) -> Registration {
        Registration {
            package: package.into(),
            kind: "op".into(),
            name: name.into(),
            file: "crates/x/src/lib.rs".into(),
            line: 1,
        }
    }

    #[test]
    fn the_negative_case_passes_only_when_every_expected_registration_is_gone() {
        let expected = [reg("omuzaki-char", "A"), reg("omuzaki-char", "B")];
        assert_eq!(negative_verdict(&expected, &[]), ExitCode::SUCCESS);
        assert_eq!(negative_verdict(&expected, &[reg("omuzaki-char", "B")]), ExitCode::FAILURE);
        // Nothing to lose proves nothing.
        assert_eq!(negative_verdict(&[], &[]), ExitCode::FAILURE);
    }

    #[test]
    fn the_module_is_read_from_the_probes_artifact_message_only() {
        let probe = r#"{"reason":"compiler-artifact","target":{"name":"omuzaki_link_probe"},"filenames":["/t/omuzaki_link_probe.rlib","/t/omuzaki_link_probe.wasm"]}"#;
        assert_eq!(wasm_artifact(probe).as_deref(), Some("/t/omuzaki_link_probe.wasm"));
        let other =
            r#"{"reason":"compiler-artifact","target":{"name":"omuzaki_char"},"filenames":["/t/omuzaki_char.wasm"]}"#;
        assert_eq!(wasm_artifact(other), None);
        assert_eq!(wasm_artifact("Compiling omuzaki-char"), None);
        assert_eq!(wasm_artifact(r#"{"reason":"build-finished","success":true}"#), None);
    }
}
