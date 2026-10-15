//! The ops linked into this binary, as a [`ScriptHost`] sees them.
//!
//! The `script.*` ops run inside the executor, which hands them no registry, so they read the same self-registrations
//! the executor was built from (`omuzaki_ops_api::registrations`). A plugin's ops appear as soon as they register.

use omuzaki_ops_api::{OpInfo, registrations};

use crate::{Limits, ScriptHost};

/// Every registered op, sorted by name, as [`Runner::ops`](omuzaki_ops_api::Runner::ops) lists them.
pub fn registered_ops() -> Vec<OpInfo> {
    let mut ops: Vec<OpInfo> = registrations()
        .map(|registration| {
            OpInfo::new((registration.meta)(), (registration.schema)()).with_coalesces((registration.coalesces)())
        })
        .collect();
    ops.sort_by_key(|info| info.name);
    ops
}

/// A host over [`registered_ops`], held to `limits`.
pub fn registered_host(limits: Limits) -> ScriptHost {
    ScriptHost::new(&registered_ops(), limits)
}
