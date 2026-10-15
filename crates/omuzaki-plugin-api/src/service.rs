//! The service the `plugin.*` ops and the plugin manager call.

use omuzaki_ops_api::{Actor, Scope};

use crate::{Approval, LogLine, PluginError, PluginId, PluginInfo};

/// The installed plugins, their switches and their logs. The host (`omuzaki-plugin`) implements it; the mock fakes it.
///
/// Every method that changes a plugin is person only: its default body refuses, so an implementation that forgets the
/// check fails closed, and an agent that turns a plugin on would be handing its work to an actor whose grants it chose
/// (design §9).
pub trait Plugins: Send + Sync {
    /// Every installed plugin, sorted by id.
    fn list(&self) -> Vec<PluginInfo>;

    /// One plugin.
    ///
    /// # Errors
    /// [`PluginError::NotFound`] when no plugin has this id.
    fn info(&self, id: &PluginId) -> Result<PluginInfo, PluginError>;

    /// Turns a plugin on: it starts, with a new session, and `init` runs. Person only.
    ///
    /// # Errors
    /// [`PluginError::Denied`] when `by` is not a person, [`PluginError::NotFound`] for an unknown id, or the error
    /// that stopped it loading.
    fn enable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, by);
        Err(PluginError::denied(Scope::EditSettings))
    }

    /// Turns a plugin off: its session ends `Closed`, `shutdown` runs. Person only, like [`enable`](Self::enable).
    ///
    /// # Errors
    /// The same as [`enable`](Self::enable).
    fn disable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, by);
        Err(PluginError::denied(Scope::EditSettings))
    }

    /// Replaces what the person approved, never beyond the manifest's requests; a running plugin's session gets the
    /// new ceiling at once. Person only.
    ///
    /// # Errors
    /// The same as [`enable`](Self::enable).
    fn set_approval(&self, id: &PluginId, approval: Approval, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, approval, by);
        Err(PluginError::denied(Scope::EditSettings))
    }

    /// Restarts a running plugin: new entry calls wait, the running one finishes or meets its deadline, the old
    /// session ends `Closed`, `shutdown` runs and a new instance starts with a new session. No state is handed over.
    /// Person only.
    ///
    /// # Errors
    /// The same as [`enable`](Self::enable).
    fn reload(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, by);
        Err(PluginError::denied(Scope::EditSettings))
    }

    /// Restarts every faulted plugin whose backoff has passed on the runner's clock, and returns their ids in id order.
    /// Hosts call it from their supervisor; tests call it after advancing a `FakeClock`.
    fn run_due(&self) -> Vec<PluginId> {
        Vec::new()
    }

    /// Up to `limit` lines of a plugin's log with a sequence number above `after`, oldest first.
    ///
    /// # Errors
    /// [`PluginError::NotFound`] when no plugin has this id.
    fn log(&self, id: &PluginId, after: u64, limit: usize) -> Result<Vec<LogLine>, PluginError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Empty;

    impl Plugins for Empty {
        fn list(&self) -> Vec<PluginInfo> {
            Vec::new()
        }
        fn info(&self, _: &PluginId) -> Result<PluginInfo, PluginError> {
            Err(PluginError::NotFound)
        }
        fn log(&self, _: &PluginId, _: u64, _: usize) -> Result<Vec<LogLine>, PluginError> {
            Err(PluginError::NotFound)
        }
    }

    #[test]
    fn the_default_bodies_refuse_and_the_trait_is_object_safe() {
        let plugins: &dyn Plugins = &Empty;
        let id = PluginId::new("example").unwrap();
        let person = Actor::Person { name: "p".into() };
        let refused = Err(PluginError::denied(Scope::EditSettings));
        assert_eq!(plugins.enable(&id, &person), refused);
        assert_eq!(plugins.disable(&id, &person), refused);
        assert_eq!(plugins.reload(&id, &person), refused);
        assert_eq!(plugins.set_approval(&id, Approval::default(), &person), refused);
        assert_eq!(plugins.run_due(), Vec::<PluginId>::new());
        assert_eq!(plugins.info(&id), Err(PluginError::NotFound));
        assert_eq!(plugins.log(&id, 0, 10), Err(PluginError::NotFound));
        assert_eq!(plugins.list().len(), 0);
    }
}
