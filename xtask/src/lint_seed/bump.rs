//! The version rules against the merge base (library D16).
//!
//! A changed item must bump its version, and a seed item's bump must be at least as big as its change. The change is
//! classified by the D16 table: removed or renamed params, changed created ids, slot, kind or target, and narrower
//! compatibility are major; new optional params, new tags, new created parts, appended steps and wider compatibility
//! are minor. Everything else (values, titles, previews) is patch. A step that is removed is major, since that changes
//! what the item does.
//!
//! Adapter data (a TOML with a `version` that is not a manifest) has no D16 classification: its whole file, minus the
//! version, is its content, so any change without a bump is refused and any bump passes.

use std::fmt;

use omuzaki_library_api::{ItemManifest, Version};
use serde::Serialize;

/// The compatibility entry that allows everything: `*` means every template or style of the kinds named.
const WILDCARD: &str = "*";

/// How big a version bump is. Ordered: a major bump is more than a minor, which is more than a patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    /// Values only.
    Patch,
    /// Compatible additions.
    Minor,
    /// Incompatible changes.
    Major,
}

impl fmt::Display for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        })
    }
}

/// One version of an item as the version rules compare it: the version, the content with the version left out, and for
/// a seed item the manifest the D16 classification reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    version: Version,
    content: Vec<u8>,
    manifest: Option<ItemManifest>,
}

impl Release {
    /// A seed item's release: its canonical form is its content.
    ///
    /// # Errors
    /// The version is not `MAJOR.MINOR.PATCH`.
    pub fn of_manifest(manifest: ItemManifest) -> Result<Self, String> {
        let version = manifest.item_version().map_err(|error| format!("invalid version: {error}"))?;
        let mut blanked = manifest.clone();
        blanked.version = String::new();
        Ok(Self { version, content: blanked.canonical_bytes(), manifest: Some(manifest) })
    }

    /// Adapter data's release: the whole file minus its version, as canonical JSON, so key order and formatting never
    /// count as a change.
    ///
    /// # Errors
    /// The file has no `version`, or it is not a version.
    pub fn of_adapter(table: &toml::Table) -> Result<Self, String> {
        let text = table.get("version").and_then(toml::Value::as_str).ok_or("has no `version` string")?;
        let version = Version::parse(text).map_err(|error| format!("invalid version: {error}"))?;
        let mut content = table.clone();
        content.remove("version");
        // JSON has no nan or infinity: serialising them would write `null`, hiding a change of one value for another.
        if !content.values().all(all_finite) {
            return Err("holds a number that is not finite (nan or inf), which its JSON form cannot carry".to_owned());
        }
        let value = serde_json::to_value(&content).map_err(|error| format!("cannot read the content: {error}"))?;
        Ok(Self { version, content: canonical_json(&value).into_bytes(), manifest: None })
    }
}

/// Whether every float under `value` is finite.
fn all_finite(value: &toml::Value) -> bool {
    match value {
        toml::Value::Float(number) => number.is_finite(),
        toml::Value::Array(items) => items.iter().all(all_finite),
        toml::Value::Table(table) => table.values().all(all_finite),
        _ => true,
    }
}

/// The JSON text with object keys sorted at every level.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", serde_json::Value::String(key.clone()), canonical_json(&map[key.as_str()])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        other => other.to_string(),
    }
}

/// What is wrong with `now` against `previous`, the same item's release on the merge base (`None` for a new item).
/// Returns one message per problem, without the file name.
pub fn check(seed: bool, now: &Release, previous: Option<&Release>) -> Vec<String> {
    let Some(before) = previous else { return Vec::new() };
    if now.version < before.version {
        return vec![format!("version {} is lower than {} on the merge base", now.version, before.version)];
    }
    if now.content == before.content {
        return Vec::new();
    }
    if now.version == before.version {
        return vec![format!("the content changed but the version stayed {}: bump the version", now.version)];
    }
    let (Some(old), Some(new), true) = (&before.manifest, &now.manifest, seed) else { return Vec::new() };
    let changes = changes(old, new);
    let required = changes.iter().map(|(bump, _)| *bump).max().unwrap_or(Bump::Patch);
    let actual = bump_between(before.version, now.version);
    if actual >= required {
        return Vec::new();
    }
    let reasons: Vec<&str> =
        changes.iter().filter(|(bump, _)| *bump == required).map(|(_, reason)| reason.as_str()).collect();
    vec![format!(
        "{} needs a {required} bump; {} to {} is a {actual} bump",
        reasons.join(", "),
        before.version,
        now.version
    )]
}

/// The bump `old` to `new` is: the first field that moved, counting from the major.
fn bump_between(old: Version, new: Version) -> Bump {
    if new.major != old.major {
        Bump::Major
    } else if new.minor != old.minor {
        Bump::Minor
    } else {
        Bump::Patch
    }
}

/// Every change from `old` to `new` that D16 classifies, each with the bump it needs and why.
fn changes(old: &ItemManifest, new: &ItemManifest) -> Vec<(Bump, String)> {
    let mut found = Vec::new();
    let mut note = |bump: Bump, reason: String| found.push((bump, reason));
    if old.id != new.id {
        note(Bump::Major, "the id changed".to_owned());
    }
    if old.kind != new.kind {
        note(Bump::Major, "the kind changed".to_owned());
    }
    if old.target != new.target {
        note(Bump::Major, "the target changed".to_owned());
    }
    if old.slot != new.slot {
        note(Bump::Major, "the slot changed".to_owned());
    }
    for key in old.params.keys().filter(|key| !new.params.contains_key(*key)) {
        note(Bump::Major, format!("param `{key}` removed"));
    }
    for key in new.params.keys().filter(|key| !old.params.contains_key(*key)) {
        note(Bump::Minor, format!("param `{key}` added"));
    }
    for (key, before) in &old.creates {
        match new.creates.get(key) {
            None => note(Bump::Major, format!("created part `{key}` removed")),
            Some(after) if after.id != before.id => note(Bump::Major, format!("created id of `{key}` changed")),
            Some(after) if after.kind != before.kind => note(Bump::Major, format!("created kind of `{key}` changed")),
            Some(_) => {}
        }
    }
    for key in new.creates.keys().filter(|key| !old.creates.contains_key(*key)) {
        note(Bump::Minor, format!("created part `{key}` added"));
    }
    if new.steps.len() < old.steps.len() {
        note(Bump::Major, "a step was removed".to_owned());
    } else if new.steps.len() > old.steps.len() {
        note(Bump::Minor, "a step was added".to_owned());
    }
    if new.tags.iter().any(|tag| !old.tags.contains(tag)) {
        note(Bump::Minor, "new tags".to_owned());
    }
    allow_list(&mut note, "compat kinds", &names(&old.compat.kinds), &names(&new.compat.kinds));
    allow_list(&mut note, "compat templates", &names(&old.compat.templates), &names(&new.compat.templates));
    allow_list(&mut note, "compat styles", &names(&old.compat.styles), &names(&new.compat.styles));
    require_list(&mut note, &old.compat.requires, &new.compat.requires);
    allow_list(&mut note, "target_part", &names(&old.target_part), &names(&new.target_part));
    found
}

/// The names a list of enum values or strings holds, as written in the manifest (`humanoid`, `*`, `head`).
fn names<T: Serialize>(items: &[T]) -> Vec<String> {
    items.iter().filter_map(|item| serde_json::to_value(item).ok()?.as_str().map(str::to_owned)).collect()
}

/// A list where empty allows anything. Allowing less is major; allowing more is minor.
fn allow_list(note: &mut impl FnMut(Bump, String), field: &str, old: &[String], new: &[String]) {
    if old == new {
        return;
    }
    if narrows(old, new) {
        note(Bump::Major, format!("{field} narrowed"));
    } else {
        note(Bump::Minor, format!("{field} widened"));
    }
}

/// `requires` names what the target must have, so empty requires nothing. Requiring more is major.
fn require_list(note: &mut impl FnMut(Bump, String), old: &[String], new: &[String]) {
    if old == new {
        return;
    }
    if superset(old, new) {
        note(Bump::Minor, "compat requires relaxed".to_owned());
    } else {
        note(Bump::Major, "compat requires narrowed".to_owned());
    }
}

/// Whether allowing `new` allows less than `old` did: it allows something `old` would not. An empty list, and a list with
/// `*`, allow everything, so moving to `*` from a list is widening and moving from `*` to a list is narrowing.
fn narrows(old: &[String], new: &[String]) -> bool {
    if covers_all(old) { !covers_all(new) } else { !covers_all(new) && !superset(new, old) }
}

/// Whether a compatibility list allows everything (empty, or `*`).
fn covers_all(list: &[String]) -> bool {
    list.is_empty() || list.iter().any(|item| item == WILDCARD)
}

/// Whether `outer` contains every element of `inner`.
fn superset(outer: &[String], inner: &[String]) -> bool {
    inner.iter().all(|item| outer.contains(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> ItemManifest {
        ItemManifest::parse(text).expect("the test manifest parses")
    }

    fn release(text: &str) -> Release {
        Release::of_manifest(manifest(text)).expect("the test version is valid")
    }

    fn bump_of(old: &str, new: &str) -> Vec<(Bump, String)> {
        changes(&manifest(old), &manifest(new))
    }

    fn check_seed(now: &str, previous: &str) -> Vec<String> {
        check(true, &release(now), Some(&release(previous)))
    }

    const BASE: &str = "schema = 1\nid = \"omuzaki/hair/a\"\nversion = \"1.0.0\"\nsection = \"hair\"\nkind = \"part\"\n\
        target = \"character\"\nslot = \"hairstyle\"\ntitle = \"A\"\nsummary = \"A.\"\ntags = [\"hair\"]\n\
        origin = \"design target\"\ncompat = { kinds = [\"humanoid\"] }\ncreates.hair = { kind = \"hair\", id = \"hair\", layer = 5 }\n\
        params.len = { type = \"number\", default = 2.0 }\n[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\n\
        commercial = \"ok\"\nowned = true\n";

    #[test]
    fn a_removed_param_is_major_and_an_added_one_minor() {
        let without = BASE.replace("params.len = { type = \"number\", default = 2.0 }\n", "");
        assert_eq!(bump_of(BASE, &without), vec![(Bump::Major, "param `len` removed".to_owned())]);
        let with_more = BASE.replace("[rights]", "params.wide = { type = \"number\", default = 1.0 }\n[rights]");
        assert_eq!(bump_of(BASE, &with_more), vec![(Bump::Minor, "param `wide` added".to_owned())]);
    }

    #[test]
    fn narrower_compatibility_is_major_and_wider_is_minor() {
        // An empty list allows any kind, so dropping the list widens and restricting it narrows.
        let any_kind = BASE.replace("kinds = [\"humanoid\"]", "kinds = []");
        let both = BASE.replace("kinds = [\"humanoid\"]", "kinds = [\"humanoid\", \"creature\"]");
        assert_eq!(bump_of(BASE, &any_kind), vec![(Bump::Minor, "compat kinds widened".to_owned())]);
        assert_eq!(bump_of(&any_kind, BASE), vec![(Bump::Major, "compat kinds narrowed".to_owned())]);
        assert_eq!(bump_of(&both, BASE), vec![(Bump::Major, "compat kinds narrowed".to_owned())]);
        assert_eq!(bump_of(BASE, &both), vec![(Bump::Minor, "compat kinds widened".to_owned())]);
    }

    #[test]
    fn a_wildcard_widens_a_template_list_and_narrowing_from_it_is_major() {
        let named = BASE.replace("kinds = [\"humanoid\"]", "kinds = [\"humanoid\"], templates = [\"soft-sd\"]");
        let every = BASE.replace("kinds = [\"humanoid\"]", "kinds = [\"humanoid\"], templates = [\"*\"]");
        assert_eq!(bump_of(&named, &every), vec![(Bump::Minor, "compat templates widened".to_owned())]);
        assert_eq!(bump_of(&every, &named), vec![(Bump::Major, "compat templates narrowed".to_owned())]);
    }

    #[test]
    fn a_changed_created_id_is_major_and_new_tags_are_minor() {
        let renamed = BASE.replace("id = \"hair\", layer", "id = \"hair-b\", layer");
        assert_eq!(bump_of(BASE, &renamed), vec![(Bump::Major, "created id of `hair` changed".to_owned())]);
        let tagged = BASE.replace("tags = [\"hair\"]", "tags = [\"hair\", \"long\"]");
        assert_eq!(bump_of(BASE, &tagged), vec![(Bump::Minor, "new tags".to_owned())]);
    }

    #[test]
    fn values_alone_need_only_a_patch() {
        let retitled = BASE.replace("title = \"A\"", "title = \"B\"");
        assert!(bump_of(BASE, &retitled).is_empty(), "a title is not in the D16 table, so it is patch-level");
    }

    #[test]
    fn a_patch_bump_for_a_value_passes_and_a_minor_for_a_removed_param_fails() {
        let patched = BASE.replace("title = \"A\"", "title = \"B\"").replace("1.0.0", "1.0.1");
        assert_eq!(check_seed(&patched, BASE), Vec::<String>::new());
        let removed = BASE.replace("params.len = { type = \"number\", default = 2.0 }\n", "").replace("1.0.0", "1.1.0");
        assert_eq!(
            check_seed(&removed, BASE),
            vec!["param `len` removed needs a major bump; 1.0.0 to 1.1.0 is a minor bump".to_owned()]
        );
    }

    #[test]
    fn a_changed_value_at_the_same_version_is_refused() {
        let retitled = BASE.replace("title = \"A\"", "title = \"B\"");
        assert_eq!(
            check_seed(&retitled, BASE),
            vec!["the content changed but the version stayed 1.0.0: bump the version".to_owned()]
        );
    }

    #[test]
    fn a_lower_version_is_refused() {
        let lower = BASE.replace("1.0.0", "0.9.0");
        assert_eq!(check_seed(&lower, BASE), vec!["version 0.9.0 is lower than 1.0.0 on the merge base".to_owned()]);
    }

    #[test]
    fn a_non_seed_item_is_checked_for_its_version_but_not_its_bump_size() {
        let removed = BASE.replace("params.len = { type = \"number\", default = 2.0 }\n", "").replace("1.0.0", "1.0.1");
        assert_eq!(check(false, &release(&removed), Some(&release(BASE))), Vec::<String>::new());
    }

    #[test]
    fn adapter_data_is_compared_as_its_whole_content_minus_the_version() {
        let adapter = "version = \"1.0.0\"\nid = \"curvy\"\nheight = 0.0\ncurves = 1.0\n";
        let table: toml::Table = adapter.parse().expect("the adapter fixture parses");
        let reordered: toml::Table =
            "curves = 1.0\n\nversion = \"1.0.0\"\nheight = 0.0\nid = \"curvy\"\n".parse().expect("parses");
        let changed: toml::Table = adapter.replace("curves = 1.0", "curves = 2.0").parse().expect("parses");
        let before = Release::of_adapter(&table).expect("valid");
        assert_eq!(check(false, &Release::of_adapter(&reordered).expect("valid"), Some(&before)), Vec::<String>::new());
        assert_eq!(
            check(false, &Release::of_adapter(&changed).expect("valid"), Some(&before)),
            vec!["the content changed but the version stayed 1.0.0: bump the version".to_owned()]
        );
    }
}
