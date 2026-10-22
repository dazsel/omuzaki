//! Every message the studio says, by id (spec SE-03, design `l10n-ops.md` section 3.3, #206): the one index that turns
//! the UI's English into the ids its Fluent files are keyed by, and back.
//!
//! The UI's words live in tables beside the code that says them: the components' [`Msg`], each area's `Word` (the
//! shell, the Live workspace, the Settings window) and the panels' and the schema form's named constants. Each already
//! has an id, or a name an id is made from (`ui-panels-outliner` for `OUTLINER`). Registered English that the UI shows
//! but no table holds, such as a panel's title or a setting's summary, gets an id made from its words
//! (`ui-text-reset-the-layout-3f2a`), so it can be translated too.
//!
//! [`entries`] lists them all, in table order, for the English Fluent file (`locales/en/omuzaki-ui.ftl`), which
//! `tests/l10n.rs` generates and keeps current; [`id_of`] finds the id of a text the drawing code passes as English.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use super::strings::Msg;

/// Every table's words: each id beside its English, in table order.
pub fn entries() -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    out.extend(Msg::ALL.iter().map(|m| (m.id().to_owned(), m.english())));
    out.extend(crate::shell::text::Word::ALL.iter().map(|w| (w.id().to_owned(), w.english())));
    out.extend(crate::live::text::Word::ALL.iter().map(|w| (w.id().to_owned(), w.english())));
    out.extend(crate::settings::text::Word::ALL.iter().map(|w| (w.id().to_owned(), w.english())));
    out.extend(crate::broadcast::text::Word::ALL.iter().map(|w| (w.id().to_owned(), w.english())));
    let named = |prefix: &str, table: &[(&str, &'static str)], out: &mut Vec<(String, &'static str)>| {
        out.extend(table.iter().map(|(name, english)| (format!("{prefix}-{}", kebab(name)), *english)));
    };
    named(crate::panels::text::PREFIX, crate::panels::text::ALL, &mut out);
    named(crate::ds::schema_form::text::PREFIX, crate::ds::schema_form::text::ALL, &mut out);
    named(crate::theatre::text::PREFIX, crate::theatre::text::ALL, &mut out);
    named(crate::generate::text::PREFIX, crate::generate::text::ALL, &mut out);
    named(crate::script::text::PREFIX, crate::script::text::ALL, &mut out);
    named("ui-live-channel", crate::live::text::CHANNEL_NAMES, &mut out);
    out
}

/// Where a message is shown, for its translator (design 11), by the start of its id; the most specific start wins.
pub fn note_of(id: &str) -> &'static str {
    const NOTES: &[(&str, &str)] = &[
        ("ds-", "Said by a design-system component (a button, a chip, a form field) in any panel."),
        (
            "ui-shell-",
            "The studio's frame: menus, the status bar, History, the command palette and the shortcut sheet.",
        ),
        ("ui-live-channel-", "A face or body channel's name in the Channels panel: keep it short."),
        ("ui-live-", "The Live workspace: the camera, tracking, the stage, expressions and outputs."),
        ("ui-settings-", "The Settings window, with its Models page."),
        ("ui-panels-", "The edit panels: the outliner, the inspector, the agent panel, approvals and the setup flow."),
        ("ui-form-", "A generated form field's own words."),
        (
            "ui-theatre-",
            "The Build Theatre: watching and steering agents (the plan board, the stage, the compare strip, the steps).",
        ),
        (
            "ui-generate-",
            "Generation: the Generators settings page, the key field, the Generated tray and the cost card before a run.",
        ),
        (
            "ui-script-",
            "The Script workspace: the console's editor and output, the scripts list, macros and the ops reference.",
        ),
        ("ui-panel-", "A panel's title (a short tab label), or what its body says while it can't work."),
        ("ui-workspace-", "A workspace's name on its tab: keep it short."),
        ("ui-setting-", "The line under a setting in the Settings window, saying what it does."),
    ];
    NOTES
        .iter()
        .filter(|(start, _)| id.starts_with(start))
        .max_by_key(|(start, _)| start.len())
        .map_or("Registered English the studio shows.", |(_, note)| note)
}

/// Whether the message `id` is a convention rather than prose (a key legend, a format hint), which translators and
/// the pseudo-locale leave as written.
pub fn is_verbatim(id: &str) -> bool {
    Msg::ALL.iter().any(|m| m.id() == id && m.is_verbatim())
}

/// Registered English the UI shows and no table holds, by an id made from what registered it: each panel's title and
/// empty state (`ui-panel-<id>`, `-reason`, `-fix`), each workspace's title (`ui-workspace-<id>`), each setting's
/// summary (`ui-setting-<key>`), and the model registry's notice (`ui-settings-models-disclaimer`). A text a table
/// already holds keeps the table's id.
pub fn registered() -> Vec<(String, &'static str)> {
    let registry = omuzaki_ui_api::registry::Registry::collect();
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for panel in registry.panels() {
        out.push((format!("ui-panel-{}", kebab(panel.id)), panel.title));
        if let Some(empty) = &panel.empty {
            out.push((format!("ui-panel-{}-reason", kebab(panel.id)), empty.reason));
            out.push((format!("ui-panel-{}-fix", kebab(panel.id)), empty.fix));
        }
    }
    for workspace in registry.workspaces() {
        out.push((format!("ui-workspace-{}", kebab(workspace.id)), workspace.title));
    }
    // The notice a person accepts for a model they add (ADR 0016): one text for every surface, kept by the registry.
    out.push(("ui-settings-models-disclaimer".to_owned(), omuzaki_ai_api::DISCLAIMER));
    for setting in omuzaki_settings_api::registry::registrations() {
        out.push((format!("ui-setting-{}", kebab(&setting.meta.key.replace('.', "-"))), setting.meta.summary));
    }
    let tables: std::collections::HashSet<&str> = entries().into_iter().map(|(_, english)| english).collect();
    let mut seen = std::collections::HashSet::new();
    out.retain(|(id, english)| !tables.contains(english) && seen.insert(id.clone()));
    out
}

/// A constant's or a channel's name in kebab case: `AGENT_NONE` and `eyeBlinkLeft` read `agent-none` and
/// `eye-blink-left`.
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let camel = name.chars().any(|c| c.is_ascii_lowercase());
    for c in name.chars() {
        match c {
            '_' => out.push('-'),
            c if camel && c.is_ascii_uppercase() => {
                if !out.is_empty() {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
            }
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    out
}

/// The id of the message whose English is `english`: its table's id, or one made from its words when no table holds
/// it (registered English).
pub fn id_of(english: &str) -> Cow<'static, str> {
    static INDEX: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    let index = INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        for (id, english) in entries().into_iter().chain(registered()) {
            // Two tables may say the same thing; the first one's id stands, and both read its translation.
            index.entry(english).or_insert(id);
        }
        index
    });
    match index.get(english) {
        Some(id) => Cow::Borrowed(id.as_str()),
        None => Cow::Owned(derived_id(english)),
    }
}

/// An id for registered English: `ui-text-`, its first words in kebab case, and four hex digits of a hash of the whole,
/// so two texts that start alike still differ.
pub fn derived_id(english: &str) -> String {
    let words: Vec<String> = english
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(str::to_ascii_lowercase)
        .collect();
    // FNV-1a: stable across builds and platforms, unlike the standard library's hasher.
    let mut hasher = Fnv(0xcbf2_9ce4_8422_2325);
    english.hash(&mut hasher);
    let hash = hasher.finish() & 0xffff;
    if words.is_empty() { format!("ui-text-{hash:04x}") } else { format!("ui-text-{}-{hash:04x}", words.join("-")) }
}

/// The FNV-1a hash.
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// `english` (with `{name}` placeholders) as a Fluent pattern (`{ $name }`): other braces become string literals, and
/// a pattern that starts or ends with a space, or spans lines, is written so Fluent keeps it as it is.
pub fn to_fluent(english: &str) -> String {
    let mut out = String::with_capacity(english.len() + 8);
    let mut rest = english;
    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if rest[open..].starts_with('{')
            && let Some(close) = after.find('}')
            && super::strings::placeholders(&rest[open..=open + 1 + close]).next().is_some()
        {
            out.push_str("{ $");
            out.push_str(&after[..close]);
            out.push_str(" }");
            rest = &after[close + 1..];
        } else {
            out.push_str(if rest[open..].starts_with('{') { "{\"{\"}" } else { "{\"}\"}" });
            rest = after;
        }
    }
    out.push_str(rest);
    let edge = |c: Option<char>| c.is_some_and(char::is_whitespace);
    if edge(out.chars().next()) || edge(out.chars().last()) {
        // Fluent trims a pattern's ends; a literal keeps them. Only short separators (", ") need this.
        let quoted: String =
            out.chars().flat_map(|c| if c == '"' || c == '\\' { vec!['\\', c] } else { vec![c] }).collect();
        return format!("{{\"{quoted}\"}}");
    }
    if out.contains('\n') {
        return format!("\n    {}", out.replace('\n', "\n    "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_is_unique_and_reads_as_fluent() {
        let mut seen = std::collections::HashSet::new();
        for (id, english) in entries() {
            assert!(seen.insert(id.clone()), "{id} is used twice");
            assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{id}");
            assert!(!english.is_empty(), "{id} has no English");
        }
        assert!(seen.len() > 400, "every table is listed: {}", seen.len());
    }

    #[test]
    fn a_tables_english_finds_its_id_and_registered_english_gets_one() {
        assert_eq!(id_of(Msg::StatusOk.english()), "ds-status-ok");
        assert_eq!(id_of(crate::panels::text::OUTLINER), "ui-panels-outliner");
        let made = id_of("Reset the layout to its default");
        assert!(made.starts_with("ui-text-reset-the-layout-to-its-default-"), "{made}");
        assert_eq!(made, id_of("Reset the layout to its default"), "the same words, the same id");
    }

    #[test]
    fn names_read_in_kebab_case() {
        assert_eq!(kebab("AGENT_NONE_HOW"), "agent-none-how");
        assert_eq!(kebab("eyeBlinkLeft"), "eye-blink-left");
    }

    #[test]
    fn placeholders_and_braces_become_fluent() {
        assert_eq!(to_fluent("Reset {label}"), "Reset { $label }");
        assert_eq!(to_fluent("{a} of {b}"), "{ $a } of { $b }");
        assert_eq!(to_fluent("such as {\"key\": 1}"), "such as {\"{\"}\"key\": 1{\"}\"}");
        assert_eq!(to_fluent(", "), "{\", \"}");
        assert_eq!(to_fluent("one\ntwo"), "\n    one\n    two");
    }
}
