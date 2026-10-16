//! Every rule the importers and preparation steps can raise has a report section, a message and a hint.
//!
//! The rule ids come from the sources themselves, read at test time, so a new rule that nobody wrote text for fails
//! here. The English Fluent file and the pseudo-locale check wait for the bless tooling and `check_crate!`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_io_ops::report_sections::{Section, section};
use omuzaki_l10n_api::MessageTable;

/// What opens a call whose first argument (after `skip` identifiers and commas) is a rule id.
const OPENERS: &[(&str, usize)] = &[
    ("Finding::error(", 0),
    ("Finding::warning(", 0),
    ("Finding::info(", 0),
    // The io-ops material notes (`notes.info(rule, field, msg)`) and the glTF tally.
    (".info(", 0),
    (".warn(", 0),
    (".warning(", 0),
    (".error(", 0),
    // The VRM avatar helper `note(report, rule, count, what)`.
    ("note(", 1),
];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sibling(name: &str) -> PathBuf {
    crate_dir().join("..").join(name)
}

/// Every `.rs` file under `path`, or `path` itself when it is a file.
fn rust_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        out.push(path.to_path_buf());
        return;
    }
    let entries = fs::read_dir(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("cannot list a file: {e}")).path();
        if path.is_dir() || path.extension().is_some_and(|x| x == "rs") {
            rust_files(&path, out);
        }
    }
}

/// The sources the design names: this crate's preparation and import code, the metadata findings and both importers.
fn scanned_files() -> Vec<PathBuf> {
    let ops = crate_dir().join("src");
    let roots = [
        ops.join("prepare"),
        ops.join("ops/avatar"),
        ops.join("ops/import.rs"),
        ops.join("ops/inspect.rs"),
        ops.join("convert/material.rs"),
        ops.join("convert/texture.rs"),
        sibling("omuzaki-io-api/src/meta"),
        sibling("omuzaki-io-vrm/src"),
        sibling("omuzaki-io-gltf/src"),
    ];
    let mut files = Vec::new();
    for root in &roots {
        rust_files(root, &mut files);
    }
    files.sort();
    files
}

/// The text of a rule id (`area.rule_name`) at the start of `s`, if a string literal with one starts there.
fn rule_literal(s: &str) -> Option<&str> {
    let body = s.strip_prefix('"')?;
    let end = body.find('"')?;
    let id = &body[..end];
    let (area, name) = id.split_once('.')?;
    let ok = |part: &str, digits: bool| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c == '_' || (digits && c.is_ascii_digit()))
    };
    (ok(area, false) && ok(name, true)).then_some(id)
}

/// Skips `skip` leading arguments (an identifier and a comma each) and whitespace.
fn after_args(mut s: &str, skip: usize) -> Option<&str> {
    s = s.trim_start();
    for _ in 0..skip {
        let (_, rest) = s.split_once(',')?;
        s = rest.trim_start();
    }
    Some(s)
}

fn rules_in(source: &str, out: &mut BTreeSet<String>) {
    for (opener, skip) in OPENERS {
        for (at, _) in source.match_indices(opener) {
            let rest = &source[at + opener.len()..];
            if let Some(id) = after_args(rest, *skip).and_then(rule_literal) {
                out.insert(id.to_owned());
            }
        }
    }
}

fn scanned_rules() -> BTreeSet<String> {
    let mut rules = BTreeSet::new();
    for file in scanned_files() {
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        rules_in(&source, &mut rules);
    }
    rules
}

fn kebab(rule: &str) -> String {
    rule.replace(['.', '_'], "-")
}

/// Every message id this crate registered, read back from the registry.
fn registered_ids() -> BTreeSet<&'static str> {
    inventory::iter::<MessageTable>()
        .filter(|table| table.package == env!("CARGO_PKG_NAME"))
        .flat_map(|table| table.messages.iter().map(|m| m.id))
        .collect()
}

/// The rule ids among `rules` that lack a message or a hint.
fn missing_text(rules: impl IntoIterator<Item = String>) -> Vec<String> {
    // `section` is total; calling it here is the "every rule has a section" check.
    let registered = registered_ids();
    let mut missing = Vec::new();
    for rule in rules {
        let _ = section(&rule);
        let id = format!("io-ops-{}", kebab(&rule));
        for wanted in [id.clone(), format!("{id}-hint")] {
            if !registered.contains(wanted.as_str()) {
                missing.push(wanted);
            }
        }
    }
    missing
}

#[test]
fn every_scanned_rule_has_a_section_a_message_and_a_hint() {
    let rules = scanned_rules();
    // A scan that finds almost nothing means a moved directory or a broken pattern, not a clean tree.
    assert!(rules.len() >= 60, "the scan found only {} rule ids: {rules:?}", rules.len());
    let missing = missing_text(rules);
    assert!(missing.is_empty(), "messages missing from the io-ops table: {missing:?}");
}

#[test]
fn the_scan_finds_the_rules_the_design_names() {
    let rules = scanned_rules();
    let named = [
        "avatar.rest_normalised",
        "avatar.thumb_off_axis",
        "avatar.expression_custom",
        "avatar.expression_undriven",
        "avatar.too_many_bones",
        "avatar.bind_dropped",
        "look.mtoon_not_drawn",
        "texture.unreadable",
        "texture.depth_reduced",
        "cleanup.welded",
        "cleanup.merged",
        "cleanup.dropped_unused",
        "arkit.missing",
        "arkit.gaze_inert",
        "arkit.gaze_partial",
        "arkit.synthesised",
        "humanoid.inferred",
        "humanoid.ambiguous",
        "humanoid.required_missing",
        "humanoid.override_invalid",
        "rights.avatar_permission",
        "rights.perform_non_profit",
        "rights.credit_required",
        "rights.content_restricted",
        "rights.unrecognised_license_url",
        "rights.import_restricted",
        "material.not_drawn",
        "material.approximated",
        "material.texture_missing",
        "vrm.tpose",
        "vrm.collider_dropped",
        "vrm.spring_chain_dropped",
        "gltf.toon_dropped",
    ];
    let absent: Vec<_> = named.iter().filter(|id| !rules.contains(**id)).collect();
    assert!(absent.is_empty(), "the scan does not see these rules: {absent:?}");
}

#[test]
fn rules_the_scan_cannot_see_still_have_text() {
    // `arkit.generated` is raised by a later step, so no source in the scan names it yet.
    let missing = missing_text(["arkit.generated".to_owned()]);
    assert!(missing.is_empty(), "messages missing from the io-ops table: {missing:?}");
}

#[test]
fn the_design_sections_hold() {
    let cases = [
        ("humanoid.inferred", Section::Mapped),
        ("avatar.thumb_off_axis", Section::Mapped),
        ("avatar.expression_custom", Section::Mapped),
        ("avatar.rest_normalised", Section::MadeForYou),
        ("avatar.weights_normalised", Section::MadeForYou),
        ("cleanup.welded", Section::MadeForYou),
        ("arkit.synthesised", Section::MadeForYou),
        ("arkit.generated", Section::MadeForYou),
        ("arkit.missing", Section::NotUsed),
        ("avatar.bind_dropped", Section::NotUsed),
        ("avatar.too_many_bones", Section::NotUsed),
        ("texture.unreadable", Section::NotUsed),
        ("look.mtoon_not_drawn", Section::NotUsed),
        ("vrm.tpose", Section::NotUsed),
        ("rights.credit_required", Section::Rights),
    ];
    for (rule, expected) in cases {
        assert_eq!(section(rule), expected, "{rule}");
    }
}

#[test]
fn an_unknown_rule_falls_back_by_area() {
    assert_eq!(section("no.such_rule"), Section::NotUsed);
    assert_eq!(section("humanoid.no_such_rule"), Section::Mapped);
    assert_eq!(section("rights.no_such_rule"), Section::Rights);
    assert_eq!(section("cleanup.no_such_rule"), Section::MadeForYou);
    assert_eq!(section("arkit.no_such_rule"), Section::NotUsed);
    assert_eq!(section("vrm.no_such_rule"), Section::NotUsed);
    assert_eq!(section("gltf.no_such_rule"), Section::NotUsed);
    assert_eq!(section("scene.no_such_rule"), Section::NotUsed);
    assert_eq!(section("nodots"), Section::NotUsed);
    assert_eq!(section(""), Section::NotUsed);
}

#[test]
fn a_rule_without_text_is_reported() {
    let missing = missing_text(["no.such_rule".to_owned()]);
    assert_eq!(missing, ["io-ops-no-such-rule", "io-ops-no-such-rule-hint"]);
}
