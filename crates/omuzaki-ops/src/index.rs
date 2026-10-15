//! The op index: registrations by name, built once at startup (spec §39 rule 2: nothing keeps a central list).

use std::collections::BTreeMap;

use omuzaki_ops_api::{OpError, OpInfo, OpRegistration, checks};

use crate::ExecutorError;

struct Entry {
    registration: &'static OpRegistration,
    info: OpInfo,
}

/// Every op the executor can run, sorted by name.
pub(crate) struct OpIndex {
    by_name: BTreeMap<&'static str, Entry>,
}

impl OpIndex {
    /// Indexes `registrations`, generating each op's schema once.
    ///
    /// # Errors
    /// [`ExecutorError::DuplicateOp`] if two registrations share a name.
    pub(crate) fn build(
        registrations: impl IntoIterator<Item = &'static OpRegistration>,
    ) -> Result<Self, ExecutorError> {
        let mut by_name = BTreeMap::new();
        for registration in registrations {
            let meta = (registration.meta)();
            let info = checks::op_info(registration);
            if let Some(first) = by_name.insert(meta.name, Entry { registration, info }) {
                return Err(ExecutorError::DuplicateOp {
                    name: meta.name,
                    first: first.info.summary,
                    second: meta.summary,
                });
            }
        }
        Ok(Self { by_name })
    }

    /// The registration called `name`.
    ///
    /// # Errors
    /// `Invalid` on the `op` field when there's no such op; the hint lists the ops in the same area.
    pub(crate) fn find(&self, name: &str) -> Result<&'static OpRegistration, OpError> {
        self.by_name.get(name).map(|entry| entry.registration).ok_or_else(|| self.unknown(name))
    }

    /// What surfaces show for each op, sorted by name.
    pub(crate) fn infos(&self) -> Vec<OpInfo> {
        self.by_name.values().map(|entry| entry.info.clone()).collect()
    }

    fn unknown(&self, name: &str) -> OpError {
        let area = area_of(name).unwrap_or(name);
        let siblings: Vec<&str> =
            self.by_name.keys().copied().filter(|candidate| area_of(candidate) == Some(area)).take(5).collect();
        let hint = if siblings.is_empty() {
            "list the available ops first (MCP tools/list, or `omuzaki help`)".to_owned()
        } else {
            format!("ops in the `{area}` area: {}", siblings.join(", "))
        };
        OpError::invalid("op", format!("no op is named {name:?}")).hint(hint)
    }
}

/// The part of `<area>.<verb>` before the dot.
fn area_of(name: &str) -> Option<&str> {
    name.split_once('.').map(|(area, _)| area)
}
