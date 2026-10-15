//! `xtask registry` and `xtask lint-links` against fixture workspaces: the expected side of the link gate and the
//! `link()` wiring rules (ADR 0015), end to end.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use xtask::registry::Target;

use support::{
    Fixture,
    Section::{Dev, Normal},
};

const OP_MACRO: &str = "#[macro_export]\nmacro_rules! register_op { ($ty:ty) => {}; }\n";
const LINK: &str = "omuzaki_core::define_link!();\n";

/// An op contract, an op crate with two ops, a mock with one, and an app that links the op crate.
fn workspace() -> Fixture {
    let fixture = Fixture::new()
        .krate("omuzaki-ops-api", &[])
        .krate("omuzaki-char", &[("omuzaki-ops-api", Normal)])
        .krate("omuzaki-char-mock", &[("omuzaki-ops-api", Normal)])
        .krate("omuzaki-stage", &[("omuzaki-char", Normal), ("omuzaki-char-mock", Dev)]);
    fixture.write("crates/omuzaki-ops-api/src/lib.rs", OP_MACRO);
    fixture.write("crates/omuzaki-char/src/lib.rs", &format!("mod ops;\n{LINK}"));
    fixture.write(
        "crates/omuzaki-char/src/ops.rs",
        "use omuzaki_ops_api::register_op;\n\nregister_op!(Rename);\n// register_op!(Commented);\nregister_op!(Create);\n",
    );
    fixture.write("crates/omuzaki-char/tests/ops.rs", "omuzaki_ops_api::register_op!(TestOnly);\n");
    fixture.write("crates/omuzaki-char-mock/src/lib.rs", &format!("omuzaki_ops_api::register_op!(MockOp);\n{LINK}"));
    fixture.write("crates/omuzaki-stage/src/lib.rs", "pub fn link() {\n    omuzaki_char::link();\n}\n");
    fixture
}

#[test]
fn the_registry_lists_every_library_registration_with_its_place() {
    let run = workspace().xtask(&["registry"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout.lines().collect::<Vec<_>>(),
        [
            "omuzaki-char\top\tCreate\tcrates/omuzaki-char/src/ops.rs:5",
            "omuzaki-char\top\tRename\tcrates/omuzaki-char/src/ops.rs:3",
            "omuzaki-char-mock\top\tMockOp\tcrates/omuzaki-char-mock/src/lib.rs:1",
        ]
    );
}

#[test]
fn an_app_expects_its_dependencies_registrations_but_not_its_dev_dependencies() {
    let run = workspace().xtask(&["registry", "--app", "omuzaki-stage"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    let names: Vec<&str> = run.stdout.lines().map(|line| line.split('\t').nth(2).unwrap()).collect();
    assert_eq!(names, ["Create", "Rename"]);
}

#[test]
fn a_binary_expects_its_own_registrations_too() {
    let fixture = workspace();
    fixture.write("crates/omuzaki-stage/src/main.rs", "omuzaki_ops_api::register_op!(InMain);\nfn main() {}\n");
    let run = fixture.xtask(&["registry", "--app", "omuzaki-stage", "--bin", "omuzaki-stage"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("omuzaki-stage\top\tInMain\tcrates/omuzaki-stage/src/main.rs:1"), "{}", run.stdout);
    let library_only = fixture.xtask(&["registry", "--app", "omuzaki-stage"]);
    assert!(!library_only.stdout.contains("InMain"), "{}", library_only.stdout);
}

#[test]
fn complete_wiring_passes() {
    let run = workspace().xtask(&["lint-links"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("lint-links: ok (4 crates, 3 registrations)"), "{}", run.stdout);
}

#[test]
fn a_registering_crate_without_link_and_a_dependant_that_never_calls_it_are_caught() {
    let fixture = workspace();
    fixture.write("crates/omuzaki-char/src/lib.rs", "mod ops;\n");
    fixture.write("crates/omuzaki-stage/src/lib.rs", "use omuzaki_char as _;\n");
    let run = fixture.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines[..3],
        [
            "omuzaki-char: defines no `link()` (`omuzaki_core::define_link!`) in src/lib.rs, but its library makes 2 registration(s)",
            "omuzaki-stage → omuzaki-char: never calls `omuzaki_char::link()`, so its registrations may not be linked",
            "omuzaki-stage: defines no `link()` (`omuzaki_core::define_link!`) in src/lib.rs, but it links crates that register (omuzaki-char), \
             and the crates that link it reach them through its `link()`",
        ]
    );
    assert!(run.stdout.contains("lint-links: 3 violation(s)"), "{}", run.stdout);
}

#[test]
fn test_only_code_is_not_expected_whether_gated_on_the_call_or_on_its_module() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-char/src/lib.rs",
        &format!("mod ops;\n#[cfg(test)]\nmod tests;\n#[cfg(all(test, feature = \"x\"))]\nmod both;\n{LINK}"),
    );
    fixture.write("crates/omuzaki-char/src/tests.rs", "omuzaki_ops_api::register_op!(InTestModule);\n");
    fixture.write("crates/omuzaki-char/src/both.rs", "omuzaki_ops_api::register_op!(InTestAndFeature);\n");
    fixture.write(
        "crates/omuzaki-char/src/ops.rs",
        "use omuzaki_ops_api::register_op;\n#[cfg(test)]\nregister_op!(TestOnly);\nregister_op!(Create);\n\
         #[cfg(any(test, not(test)))]\nregister_op!(Always);\n#[cfg(test)]\nmod probes { register_op!(Probe); }\n",
    );
    let run = fixture.xtask(&["registry", "--app", "omuzaki-stage"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    let names: Vec<&str> = run.stdout.lines().map(|line| line.split('\t').nth(2).unwrap()).collect();
    assert_eq!(names, ["Always", "Create"]);
}

#[test]
fn a_registration_under_a_feature_is_an_error_naming_its_place() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-char/src/ops.rs",
        "use omuzaki_ops_api::register_op;\nregister_op!(Create);\n#[cfg(feature = \"wgpu\")]\nregister_op!(Gpu);\n",
    );
    let run = fixture.xtask(&["registry"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stderr.contains(
            "crates/omuzaki-char/src/ops.rs:4 register_op!(Gpu) is compiled only under `cfg(feature = \"wgpu\")` \
             (crates/omuzaki-char/src/ops.rs:3)"
        ),
        "{}",
        run.stderr
    );
}

#[test]
fn a_feature_gated_module_is_an_error_naming_the_declaration() {
    let fixture = workspace();
    fixture
        .write("crates/omuzaki-char/src/lib.rs", &format!("mod ops;\n#[cfg(feature = \"extra\")]\nmod extra;\n{LINK}"));
    fixture.write("crates/omuzaki-char/src/extra.rs", "omuzaki_ops_api::register_op!(Extra);\n");
    let run = fixture.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stderr.contains(
            "crates/omuzaki-char/src/extra.rs:1 register_op!(Extra) is compiled only under \
             `cfg(feature = \"extra\")` (crates/omuzaki-char/src/lib.rs:2)"
        ),
        "{}",
        run.stderr
    );
}

#[test]
fn a_register_macro_the_workspace_does_not_define_is_not_a_registration() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-char/src/ops.rs",
        "use omuzaki_ops_api::register_op;\nregister_op!(Create);\nregister_handler!(NotARegistry);\n",
    );
    let run = fixture.xtask(&["registry"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(!run.stdout.contains("NotARegistry"), "{}", run.stdout);
}

/// The op crate with one registration per platform branch: always, wasm only, native (unix) only.
fn platform_workspace() -> Fixture {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-char/src/ops.rs",
        "use omuzaki_ops_api::register_op;\n\nregister_op!(Always);\n\
         #[cfg(target_arch = \"wasm32\")]\nregister_op!(WasmOnly);\n\
         #[cfg(unix)]\nregister_op!(NativeOnly);\n\
         #[cfg(all(target_family = \"wasm\", target_os = \"unknown\", target_pointer_width = \"32\"))]\n\
         register_op!(WasmAll);\n\
         #[cfg(all(debug_assertions, target_endian = \"little\"))]\nregister_op!(DevLittle);\n",
    );
    fixture
}

fn names(registrations: Vec<xtask::registry::Registration>) -> Vec<String> {
    registrations.into_iter().map(|r| r.name).collect()
}

#[test]
fn a_wasm_only_registration_is_expected_for_wasm32_and_a_native_only_one_is_not() {
    let fixture = platform_workspace();
    let wasm = Target::wasm32_unknown_unknown(true);
    let expected = xtask::registry::expected_for(fixture.root(), &wasm, "omuzaki-stage", None).unwrap();
    assert_eq!(names(expected), ["Always", "DevLittle", "WasmAll", "WasmOnly"]);
}

#[test]
fn the_host_scan_expects_the_native_registration_and_not_the_wasm_only_ones() {
    let fixture = platform_workspace();
    let expected = xtask::registry::expected(fixture.root(), "omuzaki-stage", None).unwrap();
    let names = names(expected);
    assert_eq!(names.contains(&"NativeOnly".to_owned()), cfg!(unix), "{names:?}");
    assert!(names.contains(&"Always".to_owned()), "{names:?}");
    assert!(!names.contains(&"WasmOnly".to_owned()) && !names.contains(&"WasmAll".to_owned()), "{names:?}");
}

#[test]
fn the_profile_decides_debug_assertions_for_the_target_not_the_host() {
    let fixture = platform_workspace();
    let release = Target::wasm32_unknown_unknown(false);
    let expected = xtask::registry::expected_for(fixture.root(), &release, "omuzaki-stage", None).unwrap();
    assert_eq!(names(expected), ["Always", "WasmAll", "WasmOnly"]);
}
