//! The body template registry: templates register themselves at link time and are found by id.
//!
//! No central list exists. A template file ends with `register_template!(MyTemplate);`, and [`templates`] and [`find`]
//! see it. The macro takes a unit struct (the template is a table, not state), so a registration is a `const` and costs
//! nothing at startup.

use super::Template;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

/// One registered template. Created by [`register_template!`](crate::register_template), not by hand.
pub struct TemplateRegistration {
    /// The template. Registered by reference to a unit struct, so it lives for the whole program.
    pub template: &'static dyn Template,
}

inventory::collect!(TemplateRegistration);

/// Registers a body template so [`templates`] and [`find`] (and with them the body part, the ops and the docs) see it.
/// Put it next to the template; `$template` is a unit struct that implements [`Template`].
///
/// ```ignore
/// omuzaki_char::register_template!(SoftSd);
/// ```
#[macro_export]
macro_rules! register_template {
    ($template:ident) => {
        $crate::template::registry::__inventory::submit! {
            $crate::template::registry::TemplateRegistration { template: &$template }
        }
        $crate::template::registry::__record_registration!("template", $template);
    };
}

/// Every registered template, sorted by id so listings and tests do not depend on link order.
pub fn templates() -> Vec<&'static dyn Template> {
    let mut all: Vec<&'static dyn Template> =
        inventory::iter::<TemplateRegistration>.into_iter().map(|r| r.template).collect();
    all.sort_by_key(|t| t.id());
    all
}

/// The template called `id`, if one is registered.
pub fn find(id: &str) -> Option<&'static dyn Template> {
    inventory::iter::<TemplateRegistration>.into_iter().map(|r| r.template).find(|t| t.id() == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_char_api::BodyTemplateId;

    #[test]
    fn the_soft_sd_template_is_registered_and_found_by_id() {
        let found = find("soft-sd").expect("soft-sd registers itself");
        assert_eq!(found.id(), "soft-sd");
        assert!(find("no-such-template").is_none());
    }

    #[test]
    fn ids_are_valid_slugs_and_unique_and_listings_are_sorted() {
        let ids: Vec<_> = templates().iter().map(|t| t.id()).collect();
        assert!(ids.contains(&"soft-sd"));
        for id in &ids {
            BodyTemplateId::new(*id).unwrap();
        }
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "sorted and without duplicates: {ids:?}");
    }
}
