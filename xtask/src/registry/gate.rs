//! The comparison at the heart of the link gate: what a binary must contain against what it recorded.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::Registration;

/// Registrations are matched on package, kind and name; the place is only for messages, because `file!()` and the
/// scan may spell a path differently (a Windows build writes `\`).
type Key = (String, String, String);

fn key(r: &Registration) -> Key {
    let name: String = r.name.split_whitespace().collect();
    (r.package.clone(), r.kind.clone(), name)
}

/// Checks that `linked` (what the binary `subject` recorded at runtime) holds exactly the `expected` registrations.
///
/// Returns a one-line summary when it does. Otherwise returns the report: every registration the binary lost (the
/// linker dropped it), and every one it has that the scan did not find (a registration the scan can't see, which
/// therefore isn't protected), with where each one is.
///
/// # Errors
/// The report, when the two differ.
pub fn verify(subject: &str, expected: &[Registration], linked: &[Registration]) -> Result<String, String> {
    let missing = surplus(expected, linked);
    let unexpected = surplus(linked, expected);
    if missing.is_empty() && unexpected.is_empty() {
        return Ok(format!("link gate: {subject} has all {} registrations it links", expected.len()));
    }
    let mut report = format!(
        "link gate: {subject} is missing {} of the {} registrations it links and has {} the scan did not find \
         (ADR 0015)\n",
        missing.len(),
        expected.len(),
        unexpected.len()
    );
    for (label, list) in [("missing", &missing), ("unexpected", &unexpected)] {
        for r in list {
            // Writing to a String cannot fail.
            let _ = writeln!(report, "  {label:<10} {} {} {} ({}:{})", r.package, r.kind, r.name, r.file, r.line);
        }
    }
    if !missing.is_empty() {
        report.push_str(
            "missing: the binary doesn't link the crate or the object file that registers it. Call the crate's \
             `link()` from the App (`cargo xtask lint-links` shows where).\n",
        );
    }
    if !unexpected.is_empty() {
        report.push_str(
            "unexpected: the scan can't see this registration (it is nested in a block or made by another macro), so \
             the gate can't protect it. Move the `register_*!` call to the top level of its file.\n",
        );
    }
    Err(report)
}

/// The entries of `from` that `against` lacks, counting duplicates (one registration per call).
fn surplus<'a>(from: &'a [Registration], against: &[Registration]) -> Vec<&'a Registration> {
    let mut available: BTreeMap<Key, usize> = BTreeMap::new();
    for r in against {
        *available.entry(key(r)).or_default() += 1;
    }
    let mut out = Vec::new();
    for r in from {
        match available.get_mut(&key(r)) {
            Some(count) if *count > 0 => *count -= 1,
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(package: &str, kind: &str, name: &str) -> Registration {
        Registration {
            package: package.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            file: format!("crates/{package}/src/lib.rs"),
            line: 7,
        }
    }

    #[test]
    fn equal_sets_pass_whatever_the_order_and_spacing() {
        let expected = [reg("omuzaki-char", "op", "Foo<u8>"), reg("omuzaki-anim", "grammar_rule", "Arc")];
        let linked = [reg("omuzaki-anim", "grammar_rule", "Arc"), reg("omuzaki-char", "op", "Foo < u8 >")];
        assert_eq!(verify("app", &expected, &linked), Ok("link gate: app has all 2 registrations it links".into()));
    }

    #[test]
    fn a_dropped_registration_is_named_with_its_place() {
        let expected = [reg("omuzaki-char", "op", "PaletteSet"), reg("omuzaki-char", "op", "PaletteAddSlot")];
        let linked = [reg("omuzaki-char", "op", "PaletteAddSlot")];
        let report = verify("omuzaki-stage", &expected, &linked).unwrap_err();
        assert!(report.starts_with("link gate: omuzaki-stage is missing 1 of the 2 registrations"), "{report}");
        assert!(
            report.contains("  missing    omuzaki-char op PaletteSet (crates/omuzaki-char/src/lib.rs:7)"),
            "{report}"
        );
        assert!(!report.contains("unexpected:"), "{report}");
    }

    #[test]
    fn duplicates_count_and_an_unseen_registration_is_reported() {
        let expected = [reg("a", "op", "Set"), reg("a", "op", "Set")];
        let linked = [reg("a", "op", "Set"), reg("b", "op", "Hidden")];
        let report = verify("app", &expected, &linked).unwrap_err();
        assert!(report.contains("missing 1 of the 2"), "{report}");
        assert!(report.contains("  unexpected b op Hidden"), "{report}");
    }
}
