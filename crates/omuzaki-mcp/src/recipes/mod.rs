//! The built-in recipes (spec v3 §11), one file each. A recipe is added by adding a file here that calls
//! [`register_recipe!`]; the module list is generated from the folder (ADR 0054 §1), so no list in code names it.

use crate::prompts::PromptDef;

/// One registered recipe, made by `register_recipe!`.
pub(crate) struct Recipe(pub(crate) &'static PromptDef);

omuzaki_core::__inventory::collect!(Recipe);

/// Registers a recipe (a `const` [`PromptDef`]) next to its definition. The kind is recorded for the link gate.
macro_rules! register_recipe {
    ($def:ident) => {
        omuzaki_core::__inventory::submit! { $crate::recipes::Recipe(&$def) }
        omuzaki_core::__record_registration!("recipe", $def);
    };
}

include!(concat!(env!("OUT_DIR"), "/recipes_mods.rs"));
