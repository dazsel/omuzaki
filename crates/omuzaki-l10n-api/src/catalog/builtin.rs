//! The catalog's own message: how a list is joined (`l10n-api-list`), declared once per shipped language.
//!
//! It is the one message the catalog itself needs (a `List` argument has no caller to join it), so it lives here and
//! is added to every bundle, instead of in a crate's `locales/` files. Attributes follow CLDR's list patterns:
//! `two` for exactly two items, then `start`, `middle` and `end` for longer lists, folded from the left.

/// The id of the list message.
pub(super) const LIST_ID: &str = "l10n-api-list";

/// The list message for a shipped language tag; `None` for a locale that ships none (an override inherits its
/// parent's).
pub(super) fn list_message(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "en" => EN,
        "ja" => JA,
        "ko" => KO,
        "zh-Hans" => ZH_HANS,
        "es-419" => ES_419,
        "pt-BR" => PT_BR,
        _ => return None,
    })
}

const EN: &str = "l10n-api-list =
    .two = { $a } and { $b }
    .start = { $a }, { $b }
    .middle = { $a }, { $b }
    .end = { $a } and { $b }
";

const JA: &str = "l10n-api-list =
    .two = { $a }と{ $b }
    .start = { $a }、{ $b }
    .middle = { $a }、{ $b }
    .end = { $a }、{ $b }
";

const KO: &str = "l10n-api-list =
    .two = { $a } 및 { $b }
    .start = { $a }, { $b }
    .middle = { $a }, { $b }
    .end = { $a } 및 { $b }
";

const ZH_HANS: &str = "l10n-api-list =
    .two = { $a }和{ $b }
    .start = { $a }、{ $b }
    .middle = { $a }、{ $b }
    .end = { $a }和{ $b }
";

const ES_419: &str = "l10n-api-list =
    .two = { $a } y { $b }
    .start = { $a }, { $b }
    .middle = { $a }, { $b }
    .end = { $a } y { $b }
";

const PT_BR: &str = "l10n-api-list =
    .two = { $a } e { $b }
    .start = { $a }, { $b }
    .middle = { $a }, { $b }
    .end = { $a } e { $b }
";
