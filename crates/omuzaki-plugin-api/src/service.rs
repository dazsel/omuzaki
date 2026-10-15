//! The service the `plugin.*` ops and the plugin manager call.

use omuzaki_ops_api::{Actor, Scope};

use crate::{PluginError, PluginId, PluginInfo};

/// The installed plugins and their on/off switch. The host (`omuzaki-plugin`) implements it; the mock fakes it.
pub trait Plugins: Send + Sync {
    /// Every installed plugin, sorted by id.
    fn list(&self) -> Vec<PluginInfo>;

    /// One plugin.
    ///
    /// # Errors
    /// [`PluginError::NotFound`] when no plugin has this id.
    fn info(&self, id: &PluginId) -> Result<PluginInfo, PluginError>;

    /// Turns a plugin on. Person only: the default body refuses, so an implementation that forgets the check fails
    /// closed.
    ///
    /// # Errors
    /// [`PluginError::Denied`] when `by` is not a person, [`PluginError::NotFound`] for an unknown id, or the error
    /// that stopped it loading.
    fn enable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, by);
        Err(PluginError::denied(Scope::EditSettings))
    }

    /// Turns a plugin off. Person only, like [`enable`](Self::enable).
    ///
    /// # Errors
    /// The same as [`enable`](Self::enable).
    fn disable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let _ = (id, by);
        Err(PluginError::denied(Scope::EditSettings))
    }
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
    }

    #[test]
    fn the_default_bodies_refuse_and_the_trait_is_object_safe() {
        let plugins: &dyn Plugins = &Empty;
        let id = PluginId::new("example").unwrap();
        let person = Actor::Person { name: "p".into() };
        assert_eq!(plugins.enable(&id, &person), Err(PluginError::denied(Scope::EditSettings)));
        assert_eq!(plugins.disable(&id, &person), Err(PluginError::denied(Scope::EditSettings)));
        assert_eq!(plugins.info(&id), Err(PluginError::NotFound));
        assert_eq!(plugins.list().len(), 0);
    }
}
