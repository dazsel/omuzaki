//! Repeated findings, reported once with a count (and the way hooks add findings of their own).
//!
//! A file with 400 matrix nodes or 30 `MToon` materials would otherwise bury the report in identical lines. A
//! [`Tally`] groups findings by rule and message, keeps the first location, and says how many places there were.
//! Groups appear in the order they were first seen, so reports stay deterministic.

use omuzaki_io_api::{Finding, Report, Severity};

struct Group {
    severity: Severity,
    rule: &'static str,
    message: String,
    hint: Option<&'static str>,
    first_at: String,
    count: usize,
}

/// Collects findings and merges the repeats.
#[derive(Default)]
pub struct Tally {
    groups: Vec<Group>,
}

impl Tally {
    /// Records one occurrence. Occurrences with the same `rule` and `message` merge; `at` is kept for the first.
    pub fn note(
        &mut self,
        severity: Severity,
        rule: &'static str,
        message: impl Into<String>,
        at: impl Into<String>,
        hint: Option<&'static str>,
    ) {
        let message = message.into();
        if let Some(g) = self.groups.iter_mut().find(|g| g.rule == rule && g.message == message) {
            g.count += 1;
            return;
        }
        self.groups.push(Group { severity, rule, message, hint, first_at: at.into(), count: 1 });
    }

    /// Records an info finding.
    pub fn info(&mut self, rule: &'static str, message: impl Into<String>, at: impl Into<String>) {
        self.note(Severity::Info, rule, message, at, None);
    }

    /// Records a warning.
    pub fn warn(
        &mut self,
        rule: &'static str,
        message: impl Into<String>,
        at: impl Into<String>,
        hint: Option<&'static str>,
    ) {
        self.note(Severity::Warning, rule, message, at, hint);
    }

    /// True if nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Appends one finding per group to `report`.
    pub fn finish(self, report: &mut Report) {
        for g in self.groups {
            let message = if g.count > 1 {
                format!("{} ({} places; the first is located)", g.message, g.count)
            } else {
                g.message
            };
            let mut finding = match g.severity {
                Severity::Error => Finding::error(g.rule, message),
                Severity::Warning => Finding::warning(g.rule, message),
                Severity::Info => Finding::info(g.rule, message),
            };
            finding = finding.at(g.first_at);
            if let Some(hint) = g.hint {
                finding = finding.hint(hint);
            }
            report.push(finding);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_merge_and_keep_the_first_location_and_the_order() {
        let mut t = Tally::default();
        t.info("gltf.a", "first kind", "/nodes/1");
        t.warn("gltf.b", "second kind", "/nodes/2", Some("do this"));
        t.info("gltf.a", "first kind", "/nodes/9");
        t.info("gltf.a", "another message", "/nodes/4");
        let mut r = Report::new();
        t.finish(&mut r);
        let got: Vec<_> = r.findings.iter().map(|f| (f.rule.as_str(), f.location.as_deref().unwrap_or(""))).collect();
        assert_eq!(got, [("gltf.a", "/nodes/1"), ("gltf.b", "/nodes/2"), ("gltf.a", "/nodes/4")]);
        assert!(r.findings[0].message.contains("2 places"));
        assert_eq!(r.findings[1].hint.as_deref(), Some("do this"));
        assert_eq!(r.findings[1].message, "second kind", "a single occurrence has no count");
    }
}
