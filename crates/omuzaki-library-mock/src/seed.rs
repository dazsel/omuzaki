//! The per-crate seed checks (design D15). An owning crate's `tests/library_seed.rs` calls [`assert_source`] with
//! its [`ItemSource`] and its own mock character, so later seed pull requests only add TOML files.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use omuzaki_char_api::{BodyTemplateId, CharId, PartId, paths};
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_library_api::{Bindings, ItemManifest, ItemRights, ItemScope, ItemSource, check_ops};
use omuzaki_ops_api::find;

use crate::{MockLibrary, apply_with_library};

/// One seed rule an item breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedProblem {
    /// The item's id.
    pub item: String,
    /// The rule: `check`, `ops`, `rights`, `origin`, `assets`, `id`, `library` or `apply`.
    pub rule: &'static str,
    /// What is wrong.
    pub detail: String,
}

impl fmt::Display for SeedProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: rule `{}`: {}", self.item, self.rule, self.detail)
    }
}

/// Checks every item of `source`: it passes `check` and `check_ops` against the ops linked into this binary, carries
/// exactly the first-party rights and a non-empty `origin`, has no `assets`, has an id no other item of the source
/// uses (a repeat is reported whether or not it is identical, and whatever its version), and dry-runs with
/// [`apply_with_library`] on the character in `doc` (bundles resolve in the source's own items).
///
/// `$target` is bound to the one part of a `target_part` kind, and the dry run fails naming the candidates when there
/// is none or more than one. A bundle binds `$parts.<key>` only for the keys its own dependency closure creates, and
/// the `library` rule reports a key two of its items create (naming both), a cycle, or nesting past four levels.
///
/// The source has already been parsed by the time `items()` returns, so parse errors surface where the source is
/// built. Returns every problem, not just the first.
pub fn check_source(source: &dyn ItemSource, doc: &dyn DocRead) -> Vec<SeedProblem> {
    let items = source.items();
    let mut library = MockLibrary::new();
    library.set_known_ops(omuzaki_ops_api::registrations().map(|r| (r.meta)().name.to_owned()));
    let mut problems = Vec::new();
    // Ids are tracked here, not through `insert`, which treats an identical repeat as a no-op and a new version as new.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for m in &items {
        if let Some(first) = seen.get(m.id.as_str()) {
            let again = if *first == m.version { "twice".to_owned() } else { format!("at {first} and {}", m.version) };
            problems.push(SeedProblem {
                item: m.id.clone(),
                rule: "id",
                detail: format!("the id is used {again}; each id appears once in a source"),
            });
            continue;
        }
        seen.insert(&m.id, &m.version);
        if let Err(e) = library.insert(m.clone(), ItemScope::FirstParty) {
            problems.push(SeedProblem { item: m.id.clone(), rule: "id", detail: e.to_string() });
        }
    }
    for m in &items {
        problems.extend(check_item(m, &library, doc));
    }
    problems
}

/// [`check_source`], panicking with every problem listed. For `tests/library_seed.rs`.
///
/// # Panics
/// When any rule fails.
pub fn assert_source(source: &dyn ItemSource, doc: &dyn DocRead) {
    let problems = check_source(source, doc);
    assert!(problems.is_empty(), "{}", problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
}

fn check_item(m: &ItemManifest, library: &MockLibrary, doc: &dyn DocRead) -> Vec<SeedProblem> {
    let found = |rule, detail: String| SeedProblem { item: m.id.clone(), rule, detail };
    let mut out: Vec<SeedProblem> = Vec::new();
    // These three are rules of the seed, whatever the id's namespace; `check` reports them only for `omuzaki/`.
    let own = ["rights", "origin", "assets"];
    out.extend(
        m.check().into_iter().filter(|p| !own.contains(&p.field.as_str())).map(|p| found("check", p.to_string())),
    );
    out.extend(check_ops(m, |name| find(name).map(|r| (r.meta)())).into_iter().map(|p| found("ops", p.to_string())));
    if m.rights != ItemRights::first_party() {
        out.push(found("rights", "first-party items carry exactly ItemRights::first_party()".to_owned()));
    }
    if m.origin.as_deref().is_none_or(|o| o.trim().is_empty()) {
        out.push(found("origin", "say in one line where the values came from".to_owned()));
    }
    if !m.assets.is_empty() {
        out.push(found("assets", "first-party items carry no binary assets".to_owned()));
    }
    match library.closure_keys(m) {
        Err(why) => out.push(found("library", why)),
        Ok(owners) => match character_bindings(m, &owners.into_keys().collect(), doc) {
            Err(why) => out.push(found("apply", why)),
            Ok(bindings) => {
                if let Err(e) = apply_with_library(library, m, doc, &bindings) {
                    out.push(found("apply", e.to_string()));
                }
            }
        },
    }
    out
}

/// Bindings for a dry run on the first character in `doc`: a fresh id per part key, `$target` as the one part of a
/// `target_part` kind, the manifest's own name, and the character's body template.
fn character_bindings(m: &ItemManifest, keys: &BTreeSet<String>, doc: &dyn DocRead) -> Result<Bindings, String> {
    let characters = doc.get(&DocPath::parse("characters").map_err(|e| e.to_string())?);
    let id = characters
        .and_then(|c| c.as_object())
        .and_then(|c| c.keys().next())
        .ok_or("the document has no character to apply to")?;
    let character = CharId::new(id.as_str()).map_err(|e| e.to_string())?;
    let node = doc.get(&paths::character(&character));
    let mut bindings = Bindings { character: Some(character), name: m.name.clone(), ..Bindings::default() };
    for key in keys {
        bindings.parts.insert(key.clone(), PartId::new(format!("seed-{key}")).map_err(|e| e.to_string())?);
    }
    if !m.target_part.is_empty() {
        let mut eligible: Vec<&str> = node
            .and_then(|n| n.get("parts"))
            .and_then(|p| p.as_object())
            .into_iter()
            .flatten()
            .filter(|(_, part)| {
                part.get("kind").and_then(|k| k.as_str()).is_some_and(|k| m.target_part.iter().any(|t| t == k))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        eligible.sort_unstable();
        // A wrong id in the document is reported, not skipped, so a broken fixture can't pass by binding nothing.
        bindings.target = match eligible.as_slice() {
            [] => return Err(format!("`$target` needs a part of kind {:?}; the character has none", m.target_part)),
            [only] => Some(PartId::new(*only).map_err(|e| format!("part {only:?}: {e}"))?),
            many => {
                return Err(format!(
                    "`$target` is ambiguous: {} are all of kind {:?}; a caller must name one",
                    many.join(", "),
                    m.target_part
                ));
            }
        };
    }
    if let Some(template) = node.and_then(|n| n.get("template")).and_then(|t| t.as_str()) {
        bindings.template = Some(BodyTemplateId::new(template).map_err(|e| format!("template {template:?}: {e}"))?);
    }
    Ok(bindings)
}
