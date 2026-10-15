//! Injected services: how ops reach things that live outside the document (the asset store, the renderer, the
//! character builder, the IO registry) without depending on the crates that implement them.
//!
//! Ops are registered by implementation crates that may not depend on each other (`omuzaki-io-ops`, `omuzaki-char`,
//! `omuzaki-face`, ...), yet `io.import`, `render.*` or `head.trace` need exactly such things at run time. So the **app**
//! builds a [`Services`] once, with one concrete implementation per API trait, and hands it to the executor. Ops ask
//! for a trait with [`OpCtx::service`](crate::OpCtx::service) and never see the implementation:
//!
//! ```
//! use std::sync::Arc;
//! use omuzaki_core::time::{FakeClock, Seconds};
//! use omuzaki_doc_mock::MemDoc;
//! use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, ServicesBuilder};
//!
//! // Defined in an API crate (`omuzaki-render-api`, say). A service must be `Send + Sync`.
//! trait RenderService: Send + Sync {
//!     fn name(&self) -> &'static str;
//! }
//! struct Cpu;
//! impl RenderService for Cpu {
//!     fn name(&self) -> &'static str {
//!         "cpu"
//!     }
//! }
//!
//! let doc = MemDoc::default();
//! let clock = FakeClock::new(Seconds(0.0));
//! let actor = Actor::Person { name: "test".into() };
//!
//! // The app wires the concrete service once, before the executor starts.
//! let services = ServicesBuilder::new().insert::<dyn RenderService>(Arc::new(Cpu)).build();
//! let ctx = OpCtx::new(&doc, &actor, &clock).with_services(&services);
//! assert_eq!(ctx.service::<dyn RenderService>().map(|r| r.name()).ok(), Some("cpu"));
//!
//! // Without it, the op fails with a structured `Unavailable` error instead of panicking.
//! let bare = OpCtx::new(&doc, &actor, &clock);
//! assert_eq!(bare.service::<dyn RenderService>().err().map(|e| e.code), Some(ErrorCode::Unavailable));
//! ```
//!
//! # Rules
//!
//! - **Only traits from API crates** (`omuzaki-core`, `omuzaki-*-api`) are services. The layering rule already enforces
//!   it (`cargo xtask lint-deps`): an implementation crate may depend on API crates only, so the only traits it can
//!   name are theirs (or its own, which no other crate can use). The app crates, which may depend on anything, are
//!   where the concrete implementations are chosen. A service trait must be `Send + Sync`, because [`Services`] is
//!   shared by every thread that runs ops.
//! - **Keyed by the trait object's type.** `service::<dyn RenderService>()` and `service::<dyn Store>()` are
//!   different entries; one trait has one implementation per [`Services`].
//! - **Frozen.** [`ServicesBuilder::build`] makes the set immutable; the executor holds it for its whole life, and
//!   two executors with different sets never see each other's.
//! - **`describe` and `list` need no services.** Only running an op that asks for one can fail with
//!   [`ErrorCode::Unavailable`](crate::ErrorCode::Unavailable).
//! - **A dry-run sees the same services, and must not change anything through them.** Ops check
//!   [`OpCtx::dry_run`](crate::OpCtx::dry_run) before any call that has an effect.
//! - **Long work goes through the job path, not through `run`.** A render, an import or a trace is an
//!   [`OpKind::Job`](crate::OpKind::Job): until the job manager lands, a service must be called from the op's job
//!   and never from `plan`, which the executor runs under its lock.

use std::any::{Any, TypeId, type_name};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// A trait object that can be injected into ops: anything `Send + Sync + 'static`, which is every `dyn Trait` whose
/// trait has `Send + Sync` supertraits. The bound exists so that [`Services`] is `Send + Sync` and so that the type
/// can be a key; *which* traits may be services is a layering rule, see the [module docs](self).
pub trait Service: Send + Sync + 'static {}

impl<T: ?Sized + Send + Sync + 'static> Service for T {}

/// What `agent.connection` reads from the host: the connection state of the agent server it runs under. The app
/// inserts an implementation into its [`Services`]; `omuzaki-mcp` provides one.
pub trait ConnectionService: Send + Sync + 'static {
    /// The `{connected, client_name?, sessions}` answer for the sessions live now. It never carries a credential.
    fn connection(&self) -> serde_json::Value;
}

/// One injected implementation. The name is kept for error messages and `Debug`.
#[derive(Clone)]
struct Entry {
    name: &'static str,
    /// An `Arc<S>` (S may be a trait object, so the `Arc` is fat) behind `Any`, recovered by `downcast_ref`.
    handle: Arc<dyn Any + Send + Sync>,
}

/// The services an app injected, immutable once built. Cloning is cheap: it shares the implementations.
///
/// Lookups are by [`TypeId`] in a small ordered map: a handful of entries, no hashing, no allocation. See the
/// [module docs](self) for the rules.
#[derive(Clone, Default)]
pub struct Services {
    entries: BTreeMap<TypeId, Entry>,
}

/// The empty set that [`OpCtx::new`](crate::OpCtx::new) points at, so building a context costs nothing.
pub(crate) static NO_SERVICES: Services = Services::none();

impl Services {
    /// A set with no services. `const` so a `static` can hold one.
    pub const fn none() -> Self {
        Self { entries: BTreeMap::new() }
    }

    /// The implementation of `S`, if the app registered one.
    pub fn get<S: ?Sized + Service>(&self) -> Option<&S> {
        let entry = self.entries.get(&TypeId::of::<S>())?;
        entry.handle.downcast_ref::<Arc<S>>().map(|service| &**service)
    }

    /// How many services are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no service is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.values().map(|entry| entry.name)).finish()
    }
}

/// Collects services before the executor starts. Build it in the app's wiring code, then [`build`](Self::build).
#[derive(Default)]
#[must_use = "a builder does nothing until `build` is called"]
pub struct ServicesBuilder {
    services: Services,
}

impl ServicesBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `service` as the implementation of the trait `S` (`insert::<dyn RenderService>(renderer)`).
    /// Registering the same trait twice keeps the last one, so a test can override a default wiring.
    pub fn insert<S: ?Sized + Service>(mut self, service: Arc<S>) -> Self {
        let entry = Entry { name: type_name::<S>(), handle: Arc::new(service) };
        self.services.entries.insert(TypeId::of::<S>(), entry);
        self
    }

    /// Freezes the set. There is no way to add to a [`Services`] afterwards.
    pub fn build(self) -> Services {
        self.services
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }
    trait Counter: Send + Sync {
        fn count(&self) -> u32;
    }
    struct English;
    impl Greeter for English {
        fn greet(&self) -> String {
            "hello".into()
        }
    }
    struct French;
    impl Greeter for French {
        fn greet(&self) -> String {
            "bonjour".into()
        }
    }
    struct Three;
    impl Counter for Three {
        fn count(&self) -> u32 {
            3
        }
    }

    #[test]
    fn services_are_keyed_by_trait() {
        let services = ServicesBuilder::new()
            .insert::<dyn Greeter>(Arc::new(English))
            .insert::<dyn Counter>(Arc::new(Three))
            .build();
        assert_eq!(services.len(), 2);
        assert_eq!(services.get::<dyn Greeter>().map(Greeter::greet), Some("hello".into()));
        assert_eq!(services.get::<dyn Counter>().map(Counter::count), Some(3));
    }

    #[test]
    fn a_missing_trait_is_none_and_the_empty_set_is_empty() {
        let only_counter = ServicesBuilder::new().insert::<dyn Counter>(Arc::new(Three)).build();
        assert!(only_counter.get::<dyn Greeter>().is_none());
        assert!(Services::none().is_empty());
        assert!(Services::default().get::<dyn Counter>().is_none());
    }

    #[test]
    fn the_last_insert_of_a_trait_wins() {
        let services = ServicesBuilder::new()
            .insert::<dyn Greeter>(Arc::new(English))
            .insert::<dyn Greeter>(Arc::new(French))
            .build();
        assert_eq!(services.len(), 1);
        assert_eq!(services.get::<dyn Greeter>().map(Greeter::greet), Some("bonjour".into()));
    }

    #[test]
    fn a_concrete_type_is_a_different_key_from_its_trait() {
        let services = ServicesBuilder::new().insert::<dyn Greeter>(Arc::new(English)).build();
        assert!(services.get::<English>().is_none());
    }

    #[test]
    fn a_clone_shares_the_implementations() {
        let services = ServicesBuilder::new().insert::<dyn Greeter>(Arc::new(English)).build();
        let copy = services.clone();
        let (a, b) = (services.get::<dyn Greeter>(), copy.get::<dyn Greeter>());
        assert!(matches!((a, b), (Some(a), Some(b)) if std::ptr::addr_eq(a, b)));
    }

    #[test]
    fn services_cross_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Services>();
    }

    #[test]
    fn debug_lists_the_trait_names() {
        let services = ServicesBuilder::new().insert::<dyn Greeter>(Arc::new(English)).build();
        assert!(format!("{services:?}").contains("Greeter"));
    }
}
