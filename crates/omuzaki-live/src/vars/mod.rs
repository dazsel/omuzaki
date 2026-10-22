//! Variables that outlive a run (design D10, #954).
//!
//! - [`DiskVariables`] is the [`VariableStore`](omuzaki_live_api::services::VariableStore) on disk: a show's values in
//!   `<project>/.omuzaki/automation/<show>/values.toml`, this OS user's globals in `<data>/automation/globals.toml`,
//!   each written atomically (a temporary file in the same folder, then a rename).
//! - [`Files`] is the engine's side: which values files a step changed, and when each is due. A file is written one
//!   second after the first change since its last write, so a burst of changes is one write, and at most one write a
//!   second reaches each file; the final flush on stop writes every file the run changed. Values in a file that the
//!   show no longer declares are written back untouched.

pub(crate) mod disk;

use std::collections::BTreeMap;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::ids::{ShowId, VarName};
use omuzaki_live_api::services::{PersistScope, ShowKey, VariableStore};
use omuzaki_live_api::show::{Finding, VarScope};
use serde_json::Value;

pub use disk::DiskVariables;

use crate::core::Core;
use crate::l10n;

/// Loads the stored values of show `id` into `core` from `store` (the show's own under `project`; without one, they
/// last one run). Returns what was wrong (a file it couldn't read, a value of another type: those variables start
/// from their defaults, and a start never fails over them) and the file each persisted scope writes back to.
pub(crate) fn load(
    core: &mut Core,
    store: &dyn VariableStore,
    project: Option<&str>,
    id: &ShowId,
) -> (Vec<Finding>, Vec<(VarScope, PersistScope)>) {
    let mut findings = Vec::new();
    let mut scopes = vec![(VarScope::Global, PersistScope::Global, "global")];
    if let Some(project) = project {
        scopes.push((VarScope::Show, PersistScope::Show(ShowKey::new(project, id.clone())), "show"));
    }
    let mut files = Vec::new();
    for (scope, persist, which) in scopes {
        let values = store.load(&persist).unwrap_or_else(|e| {
            // A damaged file is left alone until something changes; then the new values replace it.
            findings.push(Finding::note(l10n::values_unreadable(which, e.to_string())));
            BTreeMap::new()
        });
        for name in core.load_values(scope, values) {
            findings.push(if scope == VarScope::Global {
                Finding::inert(l10n::global_type_conflict(name.as_str()))
            } else {
                Finding::note(l10n::value_wrong_type(name.as_str()))
            });
        }
        files.push((scope, persist));
    }
    (findings, files)
}

/// How long after a file's first unsaved change it is written (design D10: at most one write a second per file).
pub(crate) const WRITE_AFTER: Seconds = Seconds(1.0);

/// One values file's state in a run.
#[derive(Default)]
struct File {
    /// What the file held at the start: what is written back for names the show doesn't declare.
    base: BTreeMap<VarName, Value>,
    /// When the unsaved changes are due to be written.
    due: Option<Seconds>,
    /// Whether the run changed it at all (the final flush writes those).
    touched: bool,
}

/// The persisted scopes' files in one run.
#[derive(Default)]
pub(crate) struct Files {
    files: BTreeMap<VarScope, File>,
}

/// Whether values of `scope` persist.
pub(crate) fn persists(scope: VarScope) -> bool {
    !matches!(scope, VarScope::Session)
}

impl Files {
    /// Records what `scope`'s file held at the start.
    pub(crate) fn loaded(&mut self, scope: VarScope, values: BTreeMap<VarName, Value>) {
        if persists(scope) {
            self.files.entry(scope).or_default().base = values;
        }
    }

    /// A variable of `scope` changed at `now`.
    pub(crate) fn changed(&mut self, scope: VarScope, now: Seconds) {
        if !persists(scope) {
            return;
        }
        let file = self.files.entry(scope).or_default();
        file.touched = true;
        if file.due.is_none() {
            file.due = Some(Seconds(now.0 + WRITE_AFTER.0));
        }
    }

    /// The earliest time a file is due.
    pub(crate) fn next_due(&self) -> Option<Seconds> {
        self.files.values().filter_map(|f| f.due).min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// The files due at `now` (with `flush`, every file the run changed), each as its whole contents: the start's
    /// values with `current(scope)`, the declared variables' values now, laid over them.
    pub(crate) fn take(
        &mut self,
        now: Seconds,
        flush: bool,
        current: impl Fn(VarScope) -> BTreeMap<VarName, Value>,
    ) -> Vec<(VarScope, BTreeMap<VarName, Value>)> {
        let mut out = Vec::new();
        for (scope, file) in &mut self.files {
            let due = file.due.is_some_and(|d| d.0 <= now.0);
            if !(due || (flush && file.touched)) {
                continue;
            }
            file.due = None;
            let mut values = file.base.clone();
            values.extend(current(*scope));
            file.base.clone_from(&values);
            out.push((*scope, values));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn name(s: &str) -> VarName {
        VarName::new(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_burst_is_one_write_a_second_after_its_first_change() {
        let mut files = Files::default();
        files.loaded(VarScope::Show, BTreeMap::from([(name("old"), json!(1))]));
        files.changed(VarScope::Show, Seconds(2.0));
        files.changed(VarScope::Show, Seconds(2.5));
        files.changed(VarScope::Session, Seconds(2.5));
        assert_eq!(files.next_due(), Some(Seconds(3.0)));
        let current = |_: VarScope| BTreeMap::from([(name("n"), json!(5))]);
        assert_eq!(files.take(Seconds(2.9), false, current).len(), 0);
        let written = files.take(Seconds(3.0), false, current);
        assert_eq!(written, vec![(VarScope::Show, BTreeMap::from([(name("old"), json!(1)), (name("n"), json!(5))]))]);
        assert_eq!(files.next_due(), None);
        // The flush writes what the run touched, even with nothing pending.
        assert_eq!(files.take(Seconds(10.0), true, current).len(), 1);
    }

    #[test]
    fn an_untouched_file_is_never_written() {
        let mut files = Files::default();
        files.loaded(VarScope::Global, BTreeMap::new());
        assert_eq!(files.take(Seconds(10.0), true, |_| BTreeMap::new()).len(), 0);
    }
}
