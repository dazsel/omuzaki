//! What the mock knows about a character, so `Query::compatible_with` can be answered. The `Library` trait is given
//! only a character id, so the facts are handed to the mock (see [`MockLibrary::set_characters`]) as the real library
//! reads them from the project.
//!
//! [`MockLibrary::set_characters`]: crate::MockLibrary::set_characters

use std::collections::BTreeSet;

use omuzaki_library_api::{BodyKind, Compat, CompatStatus};

/// The parts of a character an item's `compat` is checked against (design D1: templates and kinds are hard, styles
/// only warn).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterFacts {
    /// The body kind of the character's template.
    pub body_kind: BodyKind,
    /// The character's body template id.
    pub template: String,
    /// The look style the character wears, if it has one. A character with none draws no style warning.
    pub style: Option<String>,
    /// The kinds of the parts the character has.
    pub part_kinds: BTreeSet<String>,
}

/// Whether an item with `compat` fits a character: [`Incompatible`](CompatStatus::Incompatible) on a body kind, a
/// template or a missing required part, [`StyleMismatch`](CompatStatus::StyleMismatch) when only the style differs.
/// The first hard failure is the reason.
pub(crate) fn status(compat: &Compat, facts: &CharacterFacts) -> CompatStatus {
    let any = |list: &[String], value: &str| list.iter().any(|s| s == "*" || s == value);
    if !compat.kinds.is_empty() && !compat.kinds.contains(&facts.body_kind) {
        return CompatStatus::Incompatible {
            reason: format!("made for {:?} bodies, not {:?}", compat.kinds, facts.body_kind),
        };
    }
    if !compat.templates.is_empty() && !any(&compat.templates, &facts.template) {
        return CompatStatus::Incompatible {
            reason: format!("made for the templates {:?}, not `{}`", compat.templates, facts.template),
        };
    }
    let missing: Vec<&str> =
        compat.requires.iter().map(String::as_str).filter(|k| !facts.part_kinds.contains(*k)).collect();
    if !missing.is_empty() {
        return CompatStatus::Incompatible { reason: format!("needs parts of kind {}", missing.join(", ")) };
    }
    match &facts.style {
        Some(style) if !compat.styles.is_empty() && !any(&compat.styles, style) => CompatStatus::StyleMismatch {
            reason: format!("designed for the styles {:?}, not `{style}`", compat.styles),
        },
        _ => CompatStatus::Compatible,
    }
}
