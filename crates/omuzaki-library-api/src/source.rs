//! First-party sources of items (design D14): one registration per crate, from data, on every target.

use crate::ItemManifest;

/// A crate's first-party items. The crate that owns the ops an item uses owns the item: its seed files are
/// embedded with `omuzaki-embed`, and an adapter may turn its existing data files (body shapes, poses...) into
/// items in code.
pub trait ItemSource: Send + Sync {
    /// A stable name for the source, normally the crate (`omuzaki-char`). Sources are listed in this order.
    fn id(&self) -> &'static str;

    /// Every item the source offers. Called once at startup, so it may allocate and parse.
    fn items(&self) -> Vec<ItemManifest>;
}

/// One registered source, made by [`register_library_source!`](crate::register_library_source).
pub struct SourceRegistration {
    /// Builds the source.
    pub make: fn() -> Box<dyn ItemSource>,
}

fn make<T: ItemSource + Default + 'static>() -> Box<dyn ItemSource> {
    Box::new(T::default())
}

impl SourceRegistration {
    /// The registration for source type `T`. A `const fn` so the macro works at link time.
    pub const fn of<T: ItemSource + Default + 'static>() -> Self {
        Self { make: make::<T> }
    }
}

inventory::collect!(SourceRegistration);

/// Every source linked into this binary, sorted by [`ItemSource::id`] so the order never depends on link order.
pub fn sources() -> Vec<Box<dyn ItemSource>> {
    let mut all: Vec<Box<dyn ItemSource>> =
        inventory::iter::<SourceRegistration>.into_iter().map(|r| (r.make)()).collect();
    all.sort_by_key(|s| s.id());
    all
}

/// Registers an [`ItemSource`] (which must also be `Default`) so `sources()` finds it. One call per crate, at item
/// level in `src/library_items.rs`; the crate already has `link()` for its ops, so no new wiring is needed. It
/// records the registration for the link gate as ADR 0015 rule 3 requires, so the gate counts one source per crate.
///
/// ```ignore
/// omuzaki_library_api::register_library_source!(Seed);
/// ```
#[macro_export]
macro_rules! register_library_source {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::SourceRegistration::of::<$ty>() }
        $crate::__record_registration!("library_source", $ty);
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixtures::TWINTAILS;

    #[derive(Default)]
    struct Probe;

    impl ItemSource for Probe {
        fn id(&self) -> &'static str {
            "probe"
        }

        fn items(&self) -> Vec<ItemManifest> {
            ItemManifest::parse(TWINTAILS).into_iter().collect()
        }
    }

    crate::register_library_source!(Probe);

    #[test]
    fn a_registered_source_is_found_and_recorded_for_the_link_gate() {
        let found: Vec<_> = sources().into_iter().filter(|s| s.id() == "probe").collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].items()[0].id, "omuzaki/hair/twintails");
        let recorded = omuzaki_core::link::registered().into_iter().filter(|r| r.kind == "library_source").count();
        assert_eq!(recorded, 1, "one record per registration");
    }
}
