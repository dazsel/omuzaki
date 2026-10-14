//! Validation reports: what is wrong with a file or a scene, and how to fix it.
//!
//! Importing and exporting both produce a [`Report`]. Like an op error (`omuzaki_ops_api::OpError`), every finding
//! says what to fix: a stable rule id agents and scripts can branch on, a location, and a hint. Reports are
//! plain data (serialisable, schema'd) because they go straight back to people and agents as op output.
//!
//! **Rule ids** are `<area>.<rule>` in snake case, for example `scene.index_out_of_range` or
//! `vrm.humanoid_required_bone`. The areas are `scene` (the intermediate scene's own invariants), `rights`,
//! `vrm`, `gltf` and `perfect_sync`; an implementation crate may add rules under its own area (`vrm0.…`, `fbx.…`).
//! Ids are part of the contract: renaming one is a breaking change. Because rules live next to the code that emits
//! them, there is no central list to edit (spec §39 rule 2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How serious a finding is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing, nothing to fix: a lossy but expected conversion ("spring joint parameters averaged").
    Info,
    /// The result works but isn't what the author may have meant ("thumbnail isn't square").
    Warning,
    /// The file or scene can't be used as is: import produced nothing usable, or export would write a broken file.
    Error,
}

/// One thing a check found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// How serious it is.
    pub severity: Severity,
    /// Stable rule id, `<area>.<rule>` (see the module docs).
    pub rule: String,
    /// What is wrong, in a sentence a person can read.
    pub message: String,
    /// Where: a path into the scene (`nodes/3/mesh`) or, for file problems, a JSON pointer into the glTF
    /// (`/materials/2/extensions/VRMC_materials_mtoon`).
    pub location: Option<String>,
    /// How to fix it.
    pub hint: Option<String>,
}

impl Finding {
    fn new(severity: Severity, rule: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity, rule: rule.into(), message: message.into(), location: None, hint: None }
    }

    /// An [`Severity::Error`] finding.
    pub fn error(rule: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, rule, message)
    }

    /// A [`Severity::Warning`] finding.
    pub fn warning(rule: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, rule, message)
    }

    /// An [`Severity::Info`] finding.
    pub fn info(rule: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, rule, message)
    }

    /// Sets where the finding applies.
    #[must_use]
    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Adds a fix-it hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The findings of one check, in the order they were found (checks run in a fixed order, so reports are
/// deterministic).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// Everything found.
    pub findings: Vec<Finding>,
}

impl Report {
    /// An empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a finding.
    pub fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// Appends another report's findings.
    pub fn extend(&mut self, other: Report) {
        self.findings.extend(other.findings);
    }

    /// How many findings have exactly this severity.
    pub fn count(&self, severity: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == severity).count()
    }

    /// True if anything is an [`Severity::Error`].
    pub fn has_errors(&self) -> bool {
        self.count(Severity::Error) > 0
    }

    /// The errors only.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Error)
    }

    /// True if any finding carries this rule id (for tests and for callers that special-case a rule).
    pub fn has_rule(&self, rule: &str) -> bool {
        self.findings.iter().any(|f| f.rule == rule)
    }

    /// Raises every warning to an error: the `strict` mode used by CI and by exports that must be clean.
    #[must_use]
    pub fn strict(mut self) -> Self {
        for f in &mut self.findings {
            if f.severity == Severity::Warning {
                f.severity = Severity::Error;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_set_fields_and_counts_are_per_severity() {
        let mut r = Report::new();
        r.push(Finding::error("scene.x", "bad").at("nodes/1").hint("fix it"));
        r.push(Finding::warning("vrm.y", "meh"));
        r.push(Finding::info("gltf.z", "fyi"));
        assert_eq!((r.count(Severity::Error), r.count(Severity::Warning), r.count(Severity::Info)), (1, 1, 1));
        assert!(r.has_errors() && r.has_rule("vrm.y") && !r.has_rule("nope"));
        let e = r.errors().next().unwrap();
        assert_eq!((e.location.as_deref(), e.hint.as_deref()), (Some("nodes/1"), Some("fix it")));
    }

    #[test]
    fn strict_promotes_warnings_only() {
        let mut r = Report::new();
        r.push(Finding::warning("a.b", "w"));
        r.push(Finding::info("a.c", "i"));
        let s = r.strict();
        assert_eq!((s.count(Severity::Error), s.count(Severity::Info)), (1, 1));
    }

    #[test]
    fn severity_orders_info_below_error() {
        assert!(Severity::Info < Severity::Warning && Severity::Warning < Severity::Error);
    }
}
