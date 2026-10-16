//! The ops linked into this binary, as a [`ScriptHost`] sees them.
//!
//! The `script.*` ops run inside the executor, which hands them no registry, so they read the same self-registrations
//! the executor was built from (`omuzaki_ops_api::registrations`). A plugin's ops appear as soon as they register.
//!
//! Building the list generates the JSON Schema of every op, and a host builds its bindings from them, so both are kept
//! and reused for as long as the registry is the same size (registrations only ever get added).

use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_ops_api::{OpInfo, registrations};

use crate::bindings::Bindings;
use crate::{Limits, ScriptHost};

/// What was built for one registry size.
struct Built {
    /// How many ops were registered when this was built: the registry's version.
    registered: usize,
    ops: Arc<Vec<OpInfo>>,
    /// The bindings for each set of limits asked for (in practice the console's and the job's).
    bindings: Vec<(Limits, Arc<Bindings>)>,
}

static BUILT: Mutex<Option<Built>> = Mutex::new(None);

/// The built list for the current registry, rebuilt when an op has registered since.
fn with_built<R>(use_it: impl FnOnce(&mut Built) -> R) -> R {
    let registered = registrations().count();
    let mut slot = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    let current = slot.as_ref().is_some_and(|built| built.registered == registered);
    if !current {
        let mut ops: Vec<OpInfo> = registrations()
            .map(|registration| {
                OpInfo::new((registration.meta)(), (registration.schema)()).with_coalesces((registration.coalesces)())
            })
            .collect();
        ops.sort_by_key(|info| info.name);
        *slot = Some(Built { registered, ops: Arc::new(ops), bindings: Vec::new() });
    }
    match slot.as_mut() {
        Some(built) => use_it(built),
        None => unreachable!("filled above"),
    }
}

/// Every registered op, sorted by name, as [`Runner::ops`](omuzaki_ops_api::Runner::ops) lists them.
pub fn registered_ops() -> Vec<OpInfo> {
    shared_ops().as_ref().clone()
}

/// [`registered_ops`] without copying the list.
pub(crate) fn shared_ops() -> Arc<Vec<OpInfo>> {
    with_built(|built| built.ops.clone())
}

/// A host over [`registered_ops`], held to `limits`.
pub fn registered_host(limits: Limits) -> ScriptHost {
    let bindings = with_built(|built| {
        if let Some((_, bindings)) = built.bindings.iter().find(|(known, _)| *known == limits) {
            return bindings.clone();
        }
        let bindings = Arc::new(Bindings::new(&built.ops, &limits));
        built.bindings.push((limits.clone(), bindings.clone()));
        bindings
    });
    ScriptHost::from_bindings(bindings, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_and_the_bindings_are_built_once_per_registry_version() {
        let (first, second) = (shared_ops(), shared_ops());
        assert!(Arc::ptr_eq(&first, &second), "the second call reuses the first's list");
        let (console, job) = (registered_host(Limits::console()), registered_host(Limits::job(None)));
        let again = registered_host(Limits::console());
        assert!(Arc::ptr_eq(&console.bindings, &again.bindings), "same limits, same bindings");
        assert!(!Arc::ptr_eq(&console.bindings, &job.bindings), "other limits, other bindings");
        assert_eq!(registered_ops(), first.as_ref().clone());
    }
}
