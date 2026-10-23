//! What a library needs to know about a character to answer `Query::compatible_with` (design D1, D13): its body kind,
//! template, look style and part kinds. The [`Library`](crate::Library) trait is given only a character id, so the
//! caller, which holds the document, reads the facts ([`CharacterFacts::read`]) and passes them to
//! [`Library::search_for`](crate::Library::search_for).

use std::collections::BTreeSet;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::DocRead;

use crate::{BodyKind, Compat, CompatStatus};

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

impl CharacterFacts {
    /// The facts of character `id` in `doc`, or `None` when there is no such character or it has no body template
    /// (an imported model, which no recipe item fits). A character is a creature when one of its parts is a creature
    /// body (a part kind starting with `creature`), and humanoid otherwise; its style is its look preset.
    pub fn read(doc: &dyn DocRead, id: &CharId) -> Option<Self> {
        let node = doc.get(&paths::character(id))?;
        let template = node.get("template")?.as_str()?.to_owned();
        let part_kinds: BTreeSet<String> = node
            .get("parts")
            .and_then(|parts| parts.as_object())
            .into_iter()
            .flat_map(|parts| parts.values())
            .filter_map(|part| part.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
            .collect();
        let body_kind =
            if part_kinds.iter().any(|k| k.starts_with("creature")) { BodyKind::Creature } else { BodyKind::Humanoid };
        let style = node.get("look").and_then(|look| look.get("preset")).and_then(|p| p.as_str()).map(str::to_owned);
        Some(Self { body_kind, template, style, part_kinds })
    }

    /// Whether an item with `compat` fits this character: [`Incompatible`](CompatStatus::Incompatible) on a body kind,
    /// a template or a missing required part, [`StyleMismatch`](CompatStatus::StyleMismatch) when only the style
    /// differs. The first hard failure is the reason.
    pub fn status(&self, compat: &Compat) -> CompatStatus {
        let any = |list: &[String], value: &str| list.iter().any(|s| s == "*" || s == value);
        if !compat.kinds.is_empty() && !compat.kinds.contains(&self.body_kind) {
            return CompatStatus::Incompatible {
                reason: format!("made for {:?} bodies, not {:?}", compat.kinds, self.body_kind),
            };
        }
        if !compat.templates.is_empty() && !any(&compat.templates, &self.template) {
            return CompatStatus::Incompatible {
                reason: format!("made for the templates {:?}, not `{}`", compat.templates, self.template),
            };
        }
        let missing: Vec<&str> =
            compat.requires.iter().map(String::as_str).filter(|k| !self.part_kinds.contains(*k)).collect();
        if !missing.is_empty() {
            return CompatStatus::Incompatible { reason: format!("needs parts of kind {}", missing.join(", ")) };
        }
        match &self.style {
            Some(style) if !compat.styles.is_empty() && !any(&compat.styles, style) => CompatStatus::StyleMismatch {
                reason: format!("designed for the styles {:?}, not `{style}`", compat.styles),
            },
            _ => CompatStatus::Compatible,
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_doc_mock::MemDoc;
    use serde_json::json;

    use super::*;

    #[test]
    fn facts_come_from_the_character_node() {
        let doc = MemDoc::from_value(json!({"characters": {"mia": {
            "template": "soft-sd",
            "parts": {"body": {"kind": "body"}, "hair": {"kind": "hair"}},
            "look": {"preset": "matte-cel"},
        }}}));
        let facts = CharacterFacts::read(&doc, &CharId::new("mia").unwrap()).unwrap();
        assert_eq!(facts.body_kind, BodyKind::Humanoid);
        assert_eq!(facts.template, "soft-sd");
        assert_eq!(facts.style.as_deref(), Some("matte-cel"));
        assert_eq!(facts.part_kinds, ["body".to_owned(), "hair".to_owned()].into());
        assert!(CharacterFacts::read(&doc, &CharId::new("kai").unwrap()).is_none());
        let compat = Compat { requires: vec!["head".into()], ..Compat::default() };
        assert!(matches!(facts.status(&compat), CompatStatus::Incompatible { reason } if reason.contains("head")));
        let compat = Compat { styles: vec!["glossy".into()], ..Compat::default() };
        assert!(matches!(facts.status(&compat), CompatStatus::StyleMismatch { .. }));
    }
}
