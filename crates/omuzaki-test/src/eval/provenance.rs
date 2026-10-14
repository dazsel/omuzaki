//! Provenance and the private list (design 11): no private material and nothing without a clean record gets into a
//! fixture or an eval set.
//!
//! An item is refused when its path or hash is on the private list (even with a record), or when its record does not
//! say a commercial-use-clear source.

use std::collections::BTreeMap;

use serde::Deserialize;

/// `crates/omuzaki-test/evals/private.toml`: what must never produce an eval record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateList {
    /// Path prefixes such as `characters/example-private/`, matched as whole path components anywhere in a path (after `.`, `..`
    /// and doubled separators are resolved).
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// File hashes (`b3:<hex>`), matched without regard to case or the `b3:` marker.
    #[serde(default)]
    pub hashes: Vec<String>,
}

/// Where an item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Made by our code (with the generator's commit).
    Synthetic,
    /// Made for the project by its authors (with a consent record when a person is in it).
    Own,
    /// CC0.
    Cc0,
    /// Public domain.
    PublicDomain,
    /// Under a licence that allows commercial use and use in test fixtures.
    Licensed,
}

/// The record that says an item may be used.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Where it came from.
    pub source: Source,
    /// Must be `yes`.
    pub commercial: String,
    /// A pointer to the consent record, required for `own` items that show a person.
    #[serde(default)]
    pub consent: Option<String>,
    /// Whether the item shows a person (so `own` needs `consent`).
    #[serde(default)]
    pub person: bool,
    /// The licence, required for `licensed`.
    #[serde(default)]
    pub licence: Option<String>,
    /// The commit of the generator that made the item, required for `synthetic`.
    #[serde(default)]
    pub generator: Option<String>,
}

/// One thing a fixture or set refers to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// Content hash (`b3:<hex>`).
    pub hash: String,
    /// Where the file came from on disk, if known.
    #[serde(default)]
    pub path: Option<String>,
    /// The record; absent means refused.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

/// An eval-set manifest (design 11.1): `crates/<crate>/evalsets/<id>.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalSet {
    /// The set's id.
    pub id: String,
    /// `real` or `synthetic`.
    pub kind: String,
    /// `dev` or `holdout`.
    pub split: String,
    /// One line for people.
    pub summary: String,
    /// The members.
    #[serde(default, rename = "item")]
    pub items: Vec<Item>,
}

/// All known eval sets by id, so a fixture's `{ set, item }` can be resolved to a record.
pub type EvalSets = BTreeMap<String, EvalSet>;

/// Where `file` of a private-list message is shown from.
pub const PRIVATE_LIST_FILE: &str = "crates/omuzaki-test/evals/private.toml";

/// A path as lowercase components: `\` is a separator, empty and `.` components are dropped and `..` pops one, so
/// `characters/./Example-private//a.png` and `x/../characters/example-private/a.png` both read `characters, example-private, a.png`.
fn components(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in path.replace('\\', "/").to_lowercase().split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_owned()),
        }
    }
    out
}

fn bare_hash(hash: &str) -> String {
    hash.trim().to_lowercase().trim_start_matches("b3:").to_owned()
}

impl PrivateList {
    /// Why `item` is on the list, if it is.
    #[must_use]
    pub fn match_for(&self, item: &Item) -> Option<String> {
        if let Some(path) = &item.path {
            let path = components(path);
            for prefix in &self.prefixes {
                let wanted = components(prefix);
                // Whole components only, starting anywhere: `notcharacters/example-private` is not `characters/example-private`.
                if !wanted.is_empty() && path.windows(wanted.len()).any(|w| w == wanted.as_slice()) {
                    return Some(format!("path prefix `{prefix}`"));
                }
            }
        }
        let hash = bare_hash(&item.hash);
        self.hashes.iter().any(|h| bare_hash(h) == hash).then(|| format!("hash `{}`", item.hash))
    }
}

/// Every reason `item` may not be used; empty when it may.
#[must_use]
pub fn refusals(item: &Item, private: &PrivateList) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(why) = private.match_for(item) {
        reasons.push(format!(
            "item {} matches the private list ({PRIVATE_LIST_FILE}: {why}); it is refused even with a provenance record",
            item.hash
        ));
    }
    match &item.provenance {
        None => reasons.push(format!("item {} has no provenance record", item.hash)),
        Some(record) => reasons.extend(record_problems(&item.hash, record)),
    }
    reasons
}

/// Whether an optional record field is there and says something.
fn present(field: Option<&String>) -> bool {
    field.is_some_and(|text| !text.trim().is_empty())
}

fn record_problems(hash: &str, record: &Provenance) -> Vec<String> {
    let mut reasons = Vec::new();
    if record.commercial != "yes" {
        reasons.push(format!("item {hash}: commercial must be \"yes\" (it is \"{}\")", record.commercial));
    }
    if record.source == Source::Synthetic && !present(record.generator.as_ref()) {
        reasons.push(format!("item {hash}: a `synthetic` item needs the `generator` commit that made it"));
    }
    if record.source == Source::Own && record.person && !present(record.consent.as_ref()) {
        reasons.push(format!("item {hash}: an `own` item showing a person needs a `consent` record"));
    }
    if record.source == Source::Licensed && !present(record.licence.as_ref()) {
        reasons.push(format!("item {hash}: a `licensed` item needs its `licence`"));
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private() -> PrivateList {
        PrivateList { prefixes: vec!["characters/example-private/".into()], hashes: vec!["b3:DEAD".into()] }
    }

    fn record(source: Source) -> Provenance {
        Provenance {
            source,
            commercial: "yes".into(),
            consent: None,
            person: false,
            licence: None,
            generator: (source == Source::Synthetic).then(|| "0123abc".to_owned()),
        }
    }

    fn item(path: Option<&str>, hash: &str, provenance: Option<Provenance>) -> Item {
        Item { hash: hash.into(), path: path.map(Into::into), provenance }
    }

    #[test]
    fn a_clean_synthetic_item_passes() {
        assert_eq!(
            refusals(&item(Some("sheets/a.png"), "b3:01", Some(record(Source::Synthetic))), &private()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn example_private_is_refused_even_with_a_yes_record() {
        let found = refusals(
            &item(Some("E:\\assets\\characters\\Example-private\\concept\\a.png"), "b3:01", Some(record(Source::Own))),
            &private(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("private list") && found[0].contains("private.toml"), "{found:?}");
    }

    #[test]
    fn a_private_hash_is_refused_whatever_its_spelling() {
        let found = refusals(&item(None, "dead", Some(record(Source::Cc0))), &private());
        assert!(found.iter().any(|f| f.contains("private list")), "{found:?}");
    }

    #[test]
    fn a_prefix_must_start_a_path_segment() {
        let near_miss = item(Some("notcharacters/example-private/x"), "b3:01", Some(record(Source::Cc0)));
        assert_eq!(refusals(&near_miss, &private()), Vec::<String>::new());
    }

    #[test]
    fn dot_and_doubled_separators_and_parent_steps_do_not_hide_a_private_path() {
        for path in [
            "characters/./example-private/a.png",
            "characters//example-private/a.png",
            "x/../characters/example-private/a.png",
            "F:\\a\\.\\Characters\\Example-private\\a.png",
        ] {
            let found = refusals(&item(Some(path), "b3:01", Some(record(Source::Cc0))), &private());
            assert_eq!(found.len(), 1, "{path}: {found:?}");
        }
    }

    #[test]
    fn a_synthetic_record_needs_a_generator_and_blank_fields_do_not_count() {
        let mut bare = record(Source::Synthetic);
        bare.generator = None;
        assert_eq!(refusals(&item(None, "b3:01", Some(bare.clone())), &private()).len(), 1);
        bare.generator = Some("  ".into());
        assert_eq!(refusals(&item(None, "b3:01", Some(bare)), &private()).len(), 1);
        let mut own = record(Source::Own);
        (own.person, own.consent) = (true, Some(String::new()));
        assert_eq!(refusals(&item(None, "b3:02", Some(own)), &private()).len(), 1);
        let mut licensed = record(Source::Licensed);
        licensed.licence = Some(" ".into());
        assert_eq!(refusals(&item(None, "b3:03", Some(licensed)), &private()).len(), 1);
    }

    #[test]
    fn no_record_is_refused() {
        assert_eq!(refusals(&item(None, "b3:01", None), &private()).len(), 1);
    }

    #[test]
    fn commercial_must_be_yes() {
        let mut bad = record(Source::PublicDomain);
        bad.commercial = "unknown".into();
        assert_eq!(refusals(&item(None, "b3:01", Some(bad)), &private()).len(), 1);
    }

    #[test]
    fn own_with_a_person_needs_consent_and_licensed_needs_a_licence() {
        let mut own = record(Source::Own);
        own.person = true;
        assert_eq!(refusals(&item(None, "b3:01", Some(own.clone())), &private()).len(), 1);
        own.consent = Some("consent/own.pdf#abc".into());
        assert_eq!(refusals(&item(None, "b3:01", Some(own)), &private()), Vec::<String>::new());
        assert_eq!(refusals(&item(None, "b3:02", Some(record(Source::Licensed))), &private()).len(), 1);
    }

    #[test]
    fn a_record_with_an_unknown_source_does_not_parse() {
        let text = "hash='b3:01'\n[provenance]\nsource='scraped'\ncommercial='yes'";
        assert!(toml::from_str::<Item>(text).is_err());
    }
}
