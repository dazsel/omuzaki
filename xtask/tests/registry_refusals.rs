//! The link gate refuses the module forms it can't follow (ADR 0015), end to end through `xtask registry`. Each refused
//! form sits in a synthetic crate next to the allowed form it is easily confused with, so the test shows the refusal is
//! narrow: the allowed form in the same crate is followed and listed, and nothing else is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Section::Normal};

const OP_MACRO: &str = "#[macro_export]\nmacro_rules! register_op { ($ty:ty) => {}; }\n";
const OPS: &str = "use omuzaki_ops_api::register_op;\n\nregister_op!(Rename);\n";

/// An op contract and the crate `omuzaki-char`, whose `lib.rs` is `lib` and whose other source files are `files`
/// (relative to `src/`).
fn workspace(lib: &str, files: &[(&str, &str)]) -> Fixture {
    let fixture = Fixture::new().krate("omuzaki-ops-api", &[]).krate("omuzaki-char", &[("omuzaki-ops-api", Normal)]);
    fixture.write("crates/omuzaki-ops-api/src/lib.rs", OP_MACRO);
    fixture.write("crates/omuzaki-char/src/lib.rs", lib);
    for (path, source) in files {
        fixture.write(&format!("crates/omuzaki-char/src/{path}"), source);
    }
    fixture
}

/// Asserts that `xtask registry` refuses the workspace at `location` with `form` in its message, and lists nothing.
fn refused(fixture: &Fixture, location: &str, form: &str) {
    let run = fixture.xtask(&["registry"]);
    assert!(!run.success, "expected a refusal, got:\n{}", run.stdout);
    assert!(run.stdout.is_empty(), "nothing is listed on a refusal:\n{}", run.stdout);
    let message = format!("crates/omuzaki-char/src/{location}: unsupported module form: {form}");
    assert!(run.stderr.contains(&message), "{}", run.stderr);
    assert!(run.stderr.contains("see ADR 0015"), "{}", run.stderr);
}

/// Asserts that `xtask registry` accepts the workspace and lists its one registration, `ops.rs` at line 3, in `file`.
fn followed(fixture: &Fixture, file: &str) {
    let run = fixture.xtask(&["registry"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    let line = format!("omuzaki-char\top\tRename\tcrates/omuzaki-char/src/{file}:3");
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), [line.as_str()]);
}

#[test]
fn a_mod_inside_a_function_body_is_refused_and_a_file_level_mod_is_followed() {
    let refusal = workspace("fn load() {\n    mod ops;\n}\n", &[("ops.rs", OPS)]);
    refused(&refusal, "lib.rs:2", "a `mod` inside a function, block or macro body");

    followed(&workspace("mod ops;\n", &[("ops.rs", OPS)]), "ops.rs");
}

#[test]
fn include_is_refused_and_a_plain_path_mod_is_followed() {
    let refusal = workspace("include!(concat!(env!(\"OUT_DIR\"), \"/ops_mods.rs\"));\n", &[("ops.rs", OPS)]);
    refused(&refusal, "lib.rs:1", "`include!`");

    let allowed = workspace("#[path = \"elsewhere/ops.rs\"]\nmod ops;\n", &[("elsewhere/ops.rs", OPS)]);
    followed(&allowed, "elsewhere/ops.rs");
}

#[test]
fn a_cfg_attr_path_on_a_mod_is_refused_and_a_plain_path_mod_is_followed() {
    let refusal = workspace(
        "#[cfg_attr(unix, path = \"elsewhere/ops.rs\")]\nmod ops;\n",
        &[("elsewhere/ops.rs", OPS), ("ops.rs", OPS)],
    );
    refused(&refusal, "lib.rs:2", "`#[cfg_attr(..., path = ...)]` on a `mod`");

    let allowed = workspace("#[path = \"elsewhere/ops.rs\"]\nmod ops;\n", &[("elsewhere/ops.rs", OPS)]);
    followed(&allowed, "elsewhere/ops.rs");
}

#[test]
fn a_path_on_an_inline_mod_is_refused_and_an_inline_mod_without_one_is_followed() {
    let refusal = workspace("#[path = \"elsewhere\"]\nmod outer {\n    mod ops;\n}\n", &[("elsewhere/ops.rs", OPS)]);
    refused(&refusal, "lib.rs:2", "`#[path]` on an inline `mod { }` block");

    let allowed = workspace("mod outer {\n    mod ops;\n}\n", &[("outer/ops.rs", OPS)]);
    followed(&allowed, "outer/ops.rs");
}
