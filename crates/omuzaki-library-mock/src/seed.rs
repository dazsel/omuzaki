//! The per-crate seed checks (design D15). An owning crate's `tests/library_seed.rs` calls [`assert_source`] with
//! its [`ItemSource`] and its own mock character, so later seed pull requests only add TOML files.

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
/// exactly the first-party rights and a non-empty `origin`, has no `assets`, has a unique id, and dry-runs with
/// [`apply_with_library`] on the character in `doc` (bundles resolve in the source's own items).
///
/// The source has already been parsed by the time `items()` returns, so parse errors surface where the source is
/// built. Returns every problem, not just the first.
pub fn check_source(source: &dyn ItemSource, doc: &dyn DocRead) -> Vec<SeedProblem> {
    let items = source.items();
    let mut library = MockLibrary::new();
    library.set_known_ops(omuzaki_ops_api::registrations().map(|r| (r.meta)().name.to_owned()));
    let mut problems = Vec::new();
    for m in &items {
        if let Err(e) = library.insert(m.clone(), ItemScope::FirstParty) {
            problems.push(SeedProblem { item: m.id.clone(), rule: "id", detail: e.to_string() });
        }
    }
    // A bundle's nested items create parts the bundle itself doesn't declare, so every part key in the source is bound.
    let keys: Vec<&String> = items.iter().flat_map(|m| m.creates.keys()).collect();
    for m in &items {
        problems.extend(check_item(m, &keys, &library, doc));
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

fn check_item(m: &ItemManifest, keys: &[&String], library: &MockLibrary, doc: &dyn DocRead) -> Vec<SeedProblem> {
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
    match character_bindings(m, keys, doc) {
        Err(why) => out.push(found("apply", why)),
        Ok(bindings) => {
            if let Err(e) = apply_with_library(library, m, doc, &bindings) {
                out.push(found("apply", e.to_string()));
            }
        }
    }
    out
}

/// Bindings for a dry run on the first character in `doc`: a fresh id per part key in the source, the character's first
/// part as `$target`, the manifest's own name, and its body template.
fn character_bindings(m: &ItemManifest, keys: &[&String], doc: &dyn DocRead) -> Result<Bindings, String> {
    let characters = doc.get(&DocPath::parse("characters").map_err(|e| e.to_string())?);
    let id = characters
        .and_then(|c| c.as_object())
        .and_then(|c| c.keys().next())
        .ok_or("the document has no character to apply to")?;
    let character = CharId::new(id.as_str()).map_err(|e| e.to_string())?;
    let node = doc.get(&paths::character(&character));
    let mut bindings = Bindings { character: Some(character), name: m.name.clone(), ..Bindings::default() };
    for key in keys {
        bindings.parts.insert((*key).clone(), PartId::new(format!("seed-{key}")).map_err(|e| e.to_string())?);
    }
    bindings.target = node
        .and_then(|n| n.get("parts"))
        .and_then(|p| p.as_object())
        .and_then(|p| p.keys().next())
        .and_then(|k| PartId::new(k.as_str()).ok());
    bindings.template =
        node.and_then(|n| n.get("template")).and_then(|t| t.as_str()).and_then(|t| BodyTemplateId::new(t).ok());
    Ok(bindings)
}
