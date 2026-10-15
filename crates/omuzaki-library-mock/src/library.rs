//! [`MockLibrary`]: the naive oracle. Every question is answered by a linear scan over the held entries.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::{BodyTemplateId, CharId, PartId};
use omuzaki_library_api::{
    Bindings, CompatStatus, CompatTarget, Facets, ItemDetail, ItemId, ItemInfo, ItemKind, ItemManifest, ItemScope,
    Library, LibraryError, Origin, Params, PublisherCheck, PublisherKey, Query, ResolvedItem, RightsBadge, SearchPage,
    Section, Step, Usage, Version, VersionReq,
};
use omuzaki_ops_api::{OpError, find};
use omuzaki_store_api::{ContentHash, licence_gate};
use serde_json::Value;

use crate::apply::{MAX_DEPTH, OpVisitor, check_step, fresh_part_id, walk};
use crate::facts::{CharacterFacts, status};

/// The most rows a page returns, and the page size when `limit` is 0.
const MAX_LIMIT: usize = 100;

/// The ops fixtures use; `ghost.missing` is deliberately absent. See [`MockLibrary::set_known_ops`].
/// A body template no `when` guard names, to validate the steps that every guard excludes.
const UNGUARDED_TEMPLATE: &str = "unguarded";

const DEFAULT_OPS: [&str; 3] = ["part.add", "palette.set", "face.set_style"];

/// The precedence tier of a scope when an id is resolved without a pin (D11): first-party, then my library, then
/// team scopes. All teams share one tier, so no team wins by its name.
/// An [`OpError`] as the one-line reason an item is unavailable: its message, led by its field unless the message
/// already starts with it.
fn reason(e: &OpError) -> String {
    match &e.field {
        Some(field) if !e.message.starts_with(field.as_str()) => format!("{field}: {}", e.message),
        _ => e.message.clone(),
    }
}

/// The availability check's view of an op step: the op must be one this library was told about and, where this
/// binary registers it, within the recipe ceiling, which is the check an apply makes before it plans the step.
struct Availability<'a> {
    library: &'a MockLibrary,
}

impl OpVisitor for Availability<'_> {
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError> {
        if !self.library.known_ops.contains(op) {
            return Err(OpError::invalid(
                format!("step/{index}/op"),
                format!("`{op}` is not a registered op in this build"),
            ));
        }
        // An op the mock was told about but this binary doesn't register has no kind or scope to judge.
        if find(op).is_some() {
            check_step(index, op, &args)?;
        }
        Ok(())
    }
}

fn tier(scope: &ItemScope) -> u8 {
    match scope {
        ItemScope::FirstParty => 0,
        ItemScope::Mine => 1,
        ItemScope::Team(_) => 2,
    }
}

struct Entry {
    id: ItemId,
    version: Version,
    section: Section,
    scope: ItemScope,
    item: ResolvedItem,
}

/// An in-memory [`Library`]. Items are held per scope; compiled-in scope items are checked as compiled in, the
/// others as installed, so the same trust rules apply as in the real library.
pub struct MockLibrary {
    entries: Vec<Entry>,
    known_ops: BTreeSet<String>,
    characters: BTreeMap<CharId, CharacterFacts>,
}

impl Default for MockLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl MockLibrary {
    /// An empty library that knows the ops the fixtures use.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            known_ops: DEFAULT_OPS.iter().map(|s| (*s).to_owned()).collect(),
            characters: BTreeMap::new(),
        }
    }

    /// Tells the library the characters `Query::compatible_with` may name, as the real one reads them from the project.
    /// [`fixtures::characters`](crate::fixtures::characters) is the set the fixture items are written against.
    pub fn set_characters<I: IntoIterator<Item = (CharId, CharacterFacts)>>(&mut self, characters: I) {
        self.characters = characters.into_iter().collect();
    }

    /// A library holding [`fixtures::all`](crate::fixtures::all).
    ///
    /// # Panics
    /// If a fixture is refused: the fixtures are valid by construction, so that is a bug in this crate, and a
    /// library that silently lacked one would make the conformance suite pass or fail for the wrong reason.
    pub fn fixtures() -> Self {
        let mut library = Self::new();
        for f in crate::fixtures::all() {
            if let Err(e) = library.insert(f.manifest, f.scope) {
                panic!("fixture refused: {e}");
            }
        }
        library
    }

    /// Replaces the ops items may use; an item with a step outside the set is listed as unavailable.
    pub fn set_known_ops<I: IntoIterator<Item = String>>(&mut self, ops: I) {
        self.known_ops = ops.into_iter().collect();
    }

    /// Adds an item to `scope`. Adding the same manifest to the same scope twice changes nothing.
    ///
    /// # Errors
    /// [`LibraryError::Storage`] when the id, version or section is malformed, when the publisher check refuses it,
    /// or when the same publisher key already holds this id and version in *any* scope with a different hash: a
    /// published `id@version` is immutable (D11), so Mine and Team can't disagree about it.
    pub fn insert(&mut self, manifest: ItemManifest, scope: ItemScope) -> Result<(), LibraryError> {
        let bad = |what: &dyn std::fmt::Display| LibraryError::Storage(format!("{}: {what}", manifest.id));
        let id = manifest.item_id().map_err(|e| bad(&e))?;
        let version = manifest.item_version().map_err(|e| bad(&e))?;
        let section = manifest.item_section().map_err(|e| bad(&e))?;
        let origin = if scope == ItemScope::FirstParty { Origin::CompiledIn } else { Origin::Installed };
        let publisher = manifest.verify(origin);
        if let PublisherCheck::Refused { reason } = &publisher {
            return Err(bad(&format!("refused: {reason}")));
        }
        let hash = manifest.hash();
        let key = |m: &ItemManifest| m.publisher.as_ref().map(|p| p.key.clone());
        let twins: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.id == id && e.version == version && key(&e.item.manifest) == key(&manifest))
            .collect();
        if let Some(held) = twins.iter().find(|e| e.item.hash != hash) {
            return Err(bad(&format!("{version} is already held in {:?} with a different hash", held.scope)));
        }
        if twins.iter().any(|e| e.scope == scope) {
            return Ok(());
        }
        self.entries.push(Entry {
            id,
            version,
            section,
            scope: scope.clone(),
            item: ResolvedItem { manifest, hash, scope, publisher },
        });
        Ok(())
    }

    /// Why `m` can't be applied in this build: the first failure of the checks an apply makes before it plans a step
    /// ([`walk`] with [`check_step`]: expansion, parameters, dependencies, the recipe ceiling, argument decoding), run
    /// once per body template a `when` guard names and once for a template none does, so a guarded step is validated
    /// whichever template the item is later applied to. What a plan finds in a particular document (a slot that isn't
    /// there) is not availability. Loading a bundle first refuses a part key two of its items create, and a cycle
    /// ([`closure_keys`](Self::closure_keys)).
    fn unavailable(&self, m: &ItemManifest) -> Option<String> {
        let owners = match self.closure_keys(m) {
            Ok(owners) => owners,
            Err(why) => return Some(why),
        };
        // A template no guard names, so the steps every guard excludes are covered by it too.
        let mut templates = self.guard_templates(m, 0);
        templates.insert(UNGUARDED_TEMPLATE.to_owned());
        templates.iter().find_map(|template| {
            let mut bindings = Bindings {
                character: CharId::new("sample").ok(),
                target: PartId::new("sample-target").ok(),
                name: Some(m.name.clone().unwrap_or_else(|| "sample".to_owned())),
                template: BodyTemplateId::new(template.as_str()).ok(),
                ..Bindings::default()
            };
            for key in owners.keys() {
                bindings.parts.extend(fresh_part_id(key).ok().map(|id| (key.clone(), id)));
            }
            walk(Some(self), m, &Params::new(), &bindings, 0, &mut Availability { library: self })
                .err()
                .map(|e| reason(&e))
        })
    }

    /// The body templates named by `when` guards in `m` and in every item its steps apply.
    fn guard_templates(&self, m: &ItemManifest, depth: usize) -> BTreeSet<String> {
        let mut templates = BTreeSet::new();
        for step in &m.steps {
            let (guard, nested) = match step {
                Step::Op(op) => (op.when.as_ref(), None),
                Step::Item(item) => (item.when.as_ref(), Some(item)),
            };
            templates.extend(guard.into_iter().flat_map(|w| w.templates.iter().filter(|t| *t != "*").cloned()));
            let found = nested.and_then(|item| {
                let (id, req) = (ItemId::new(&item.item).ok()?, VersionReq::parse(&item.version).ok()?);
                self.pick(&id, &req).ok()
            });
            if let Some(found) = found.filter(|_| depth < MAX_DEPTH) {
                templates.extend(self.guard_templates(&found.item.manifest, depth + 1));
            }
        }
        templates
    }

    /// Who creates each `$parts.<key>` across `m` and every item its steps apply, nested and sibling alike (design D7):
    /// key to the creating item's id. A key two items create is an error naming both, a repeated item counts as two,
    /// and so does a cycle (named) or nesting past [`MAX_DEPTH`]. Steps that don't resolve are skipped here; the
    /// availability walk and the apply report them.
    pub(crate) fn closure_keys(&self, m: &ItemManifest) -> Result<BTreeMap<String, String>, String> {
        let mut owners = BTreeMap::new();
        self.collect_keys(m, &mut Vec::new(), &mut owners)?;
        Ok(owners)
    }

    fn collect_keys(
        &self,
        m: &ItemManifest,
        path: &mut Vec<String>,
        owners: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        path.push(m.id.clone());
        for key in m.creates.keys() {
            if let Some(first) = owners.insert(key.clone(), m.id.clone()) {
                return Err(format!(
                    "`$parts.{key}` is created by both {first} and {}; a bundle can't hold both",
                    m.id
                ));
            }
        }
        for step in &m.steps {
            let Step::Item(nested) = step else { continue };
            let (Ok(id), Ok(req)) = (ItemId::new(&nested.item), VersionReq::parse(&nested.version)) else { continue };
            let Ok(found) = self.pick(&id, &req) else { continue };
            let found = &found.item.manifest;
            if path.contains(&found.id) {
                return Err(format!("bundles cycle: {} -> {}", path.join(" -> "), found.id));
            }
            if path.len() > MAX_DEPTH {
                return Err(format!("bundles nest deeper than {MAX_DEPTH}: {} -> {}", path.join(" -> "), found.id));
            }
            self.collect_keys(found, path, owners)?;
        }
        path.pop();
        Ok(())
    }

    /// Whether what `m` makes may be published: its own rights, and for a bundle every item it applies (the record
    /// carries the strictest join of the rights, design D8), so one non-commercial item blocks the whole bundle.
    fn publishable(&self, e: &Entry, depth: usize) -> bool {
        let m = &e.item.manifest;
        let own = licence_gate(&m.rights.to_provenance(&e.id, &e.version), false).is_ok();
        own && depth <= MAX_DEPTH
            && m.steps.iter().all(|step| {
                let Step::Item(nested) = step else { return true };
                let (Ok(id), Ok(req)) = (ItemId::new(&nested.item), VersionReq::parse(&nested.version)) else {
                    return true;
                };
                self.pick(&id, &req).map_or(true, |found| self.publishable(found, depth + 1))
            })
    }

    fn info(&self, e: &Entry, facts: Option<&CharacterFacts>) -> ItemInfo {
        let m = &e.item.manifest;
        ItemInfo {
            id: e.id.clone(),
            version: e.version,
            hash: e.item.hash,
            title: m.title.clone(),
            summary: m.summary.clone(),
            section: e.section,
            kind: m.kind,
            tags: m.tags.clone(),
            scope: e.scope.clone(),
            rights: RightsBadge {
                licence: m.rights.licence.clone(),
                commercial: m.rights.commercial,
                owned: m.rights.owned,
                private_test: m.rights.private_test,
                publish_ok: self.publishable(e, 0),
            },
            compat: Self::compat(e, facts),
            preview: None,
            unavailable: self.unavailable(m),
        }
    }

    /// Whether `e` fits `facts`; with no target named, everything does.
    fn compat(e: &Entry, facts: Option<&CharacterFacts>) -> CompatStatus {
        facts.map_or(CompatStatus::Compatible, |f| status(&e.item.manifest.compat, f))
    }

    /// Whether search may show this manifest: `listed = false` and deprecated manifests are hidden from search, though
    /// they stay resolvable for pins and dependencies. Asked of every candidate, never only of the newest.
    fn hidden_from_search(e: &Entry) -> bool {
        !e.item.manifest.listed || e.item.manifest.deprecated.is_some()
    }

    /// The versions of one (id, scope, publisher) that search may show, for a query with or without a character: each
    /// version is judged on its own, first for visibility and then, given a character, for compatibility, so an
    /// unlisted or deprecated release never hides a listed older one and never gets picked as a fallback.
    fn visible_candidates<'a>(group: &[&'a Entry], facts: Option<&CharacterFacts>) -> Vec<&'a Entry> {
        group
            .iter()
            .copied()
            .filter(|e| !Self::hidden_from_search(e))
            .filter(|e| !matches!(Self::compat(e, facts), CompatStatus::Incompatible { .. }))
            .collect()
    }

    /// The rows of a listing: per (id, scope, publisher key), the newest of its [`visible_candidates`]. Hidden and
    /// incompatible items drop out here, before ranking, paging and facets.
    ///
    /// [`visible_candidates`]: Self::visible_candidates
    fn rows(&self, facts: Option<&CharacterFacts>) -> Vec<&Entry> {
        let mut groups: BTreeMap<(&ItemId, &ItemScope, Option<&str>), Vec<&Entry>> = BTreeMap::new();
        for e in &self.entries {
            let key = (&e.id, &e.scope, e.item.manifest.publisher.as_ref().map(|p| p.key.as_str()));
            groups.entry(key).or_default().push(e);
        }
        groups
            .into_values()
            .filter_map(|group| {
                Self::visible_candidates(&group, facts).into_iter().max_by(|a, b| a.version.cmp(&b.version))
            })
            .collect()
    }

    fn page(
        &self,
        mut rows: Vec<(u8, &Entry)>,
        target: Option<&CharacterFacts>,
        offset: usize,
        limit: usize,
    ) -> SearchPage {
        // D11: the same id@version with the same hash and publisher held in several scopes is one entry, shown under
        // its first scope. A scope filter has already left only the rows of that scope, so a scoped query finds it.
        rows.sort_by(|a, b| a.1.scope.cmp(&b.1.scope));
        let mut seen = BTreeSet::new();
        rows.retain(|(_, e)| {
            let publisher = e.item.manifest.publisher.as_ref().map(|p| p.key.clone());
            seen.insert((e.id.clone(), e.version, e.item.hash.0, publisher))
        });
        // Ties go by section order, then id, then scope and hash, so the order never depends on insertion order.
        rows.sort_by(|(sa, a), (sb, b)| {
            sb.cmp(sa)
                .then(a.section.cmp(&b.section))
                .then(a.id.cmp(&b.id))
                .then(a.scope.cmp(&b.scope))
                .then(a.item.hash.0.cmp(&b.item.hash.0))
        });
        let mut facets = Facets::default();
        for (_, e) in &rows {
            *facets.sections.entry(e.section).or_default() += 1;
            *facets.kinds.entry(e.item.manifest.kind).or_default() += 1;
            for tag in &e.item.manifest.tags {
                *facets.tags.entry(tag.clone()).or_default() += 1;
            }
            *facets.scopes.entry(e.scope.clone()).or_default() += 1;
        }
        let limit = if limit == 0 { MAX_LIMIT } else { limit.min(MAX_LIMIT) };
        let items = rows.iter().skip(offset).take(limit).map(|(_, e)| self.info(e, target)).collect();
        SearchPage { total: rows.len(), items, facets }
    }

    /// The one pipeline every version lookup goes through: the item's precedence tier, then its publisher, then the
    /// caller's filter. The publisher is the one holding the item in the first tier that has it (`Ambiguous` when that
    /// tier holds several), and only that publisher's versions, in any tier, are its history: two publishers'
    /// histories are never merged, so no caller can reach the other's version by number. All teams are one tier.
    fn lineage(&self, item: &ItemId) -> Result<Vec<&Entry>, LibraryError> {
        let held: Vec<&Entry> = self.entries.iter().filter(|e| &e.id == item).collect();
        let Some(first) = held.iter().map(|e| tier(&e.scope)).min() else {
            return Err(LibraryError::NotFound(item.to_string()));
        };
        let leading: Vec<&Entry> = held.iter().copied().filter(|e| tier(&e.scope) == first).collect();
        ensure_one_publisher(item, &leading)?;
        let key = |e: &Entry| e.item.manifest.publisher.as_ref().map(|p| p.key.clone());
        let publisher = leading.first().and_then(|e| key(e));
        Ok(held.into_iter().filter(|e| key(e) == publisher).collect())
    }

    /// The entry of `item` matching `req`: through [`lineage`](Self::lineage), then the first tier with a match, then
    /// the newest version in it. The same version in two scopes of a tier is the same content (D11), so the scope
    /// only breaks the tie.
    fn pick(&self, item: &ItemId, req: &VersionReq) -> Result<&Entry, LibraryError> {
        let held = self.lineage(item)?;
        let tiers: BTreeSet<u8> = held.iter().map(|e| tier(&e.scope)).collect();
        for t in tiers {
            let best = held
                .iter()
                .copied()
                .filter(|e| tier(&e.scope) == t && req.matches(&e.version))
                .max_by(|a, b| a.version.cmp(&b.version).then_with(|| b.scope.cmp(&a.scope)));
            if let Some(best) = best {
                return Ok(best);
            }
        }
        let mut installed: Vec<Version> = held.iter().map(|e| e.version).collect();
        installed.sort();
        installed.dedup();
        Err(LibraryError::NoMatchingVersion { item: item.clone(), req: *req, installed })
    }

    /// The facts of the character a query names, or `NotFound` when the mock was not told about it.
    fn target(&self, query: &Query) -> Result<Option<&CharacterFacts>, LibraryError> {
        match &query.compatible_with {
            None => Ok(None),
            Some(CompatTarget::Character(id)) => {
                self.characters.get(id).map(Some).ok_or_else(|| LibraryError::NotFound(format!("character {id}")))
            }
        }
    }
}

/// Refuses with `Ambiguous` when `entries` come from more than one publisher key. The key is the identity; the name
/// is only what is shown, so a publisher renaming itself between versions is still one publisher.
fn ensure_one_publisher(item: &ItemId, entries: &[&Entry]) -> Result<(), LibraryError> {
    // Per key, the name its newest version shows (ties by name, so the choice never depends on insertion order).
    let mut by_key: BTreeMap<String, (Version, String)> = BTreeMap::new();
    for e in entries {
        let (key, name) = match &e.item.manifest.publisher {
            Some(p) => (p.key.clone(), p.name.clone()),
            None => (String::new(), "unsigned".to_owned()),
        };
        let shown = (e.version, name);
        match by_key.get_mut(&key) {
            Some(held) if shown > *held => *held = shown,
            Some(_) => {}
            None => {
                by_key.insert(key, shown);
            }
        }
    }
    if by_key.len() < 2 {
        return Ok(());
    }
    let named = by_key
        .into_iter()
        .map(|(key, (_, name))| {
            format!(
                "{name} ({})",
                PublisherKey::from_hex(&key).map_or_else(|_| "no key".to_owned(), |k| k.fingerprint())
            )
        })
        .collect();
    Err(LibraryError::Ambiguous { item: item.clone(), candidates: named })
}

/// The text rank of `e` for `q` (0 with no text), or `None` when a filter or the text rules it out.
fn rank(q: &Query, e: &Entry) -> Option<u8> {
    let m = &e.item.manifest;
    let wanted = q.section.is_none_or(|s| s == e.section)
        && q.kind.is_none_or(|k: ItemKind| k == m.kind)
        && q.scope.as_ref().is_none_or(|s| *s == e.scope)
        && q.tags.iter().all(|t| m.tags.contains(t));
    if !wanted {
        return None;
    }
    match q.text.as_deref().filter(|t| !t.trim().is_empty()) {
        None => Some(0),
        Some(text) => crate::text::rank(text, &e.id, m),
    }
}

impl Library for MockLibrary {
    fn list(
        &self,
        section: Option<Section>,
        scope: Option<&ItemScope>,
        offset: usize,
        limit: usize,
    ) -> Result<SearchPage, LibraryError> {
        let q = Query { section, scope: scope.cloned(), ..Query::default() };
        Ok(self.page(
            self.rows(None).into_iter().filter_map(|e| rank(&q, e).map(|_| (0, e))).collect(),
            None,
            offset,
            limit,
        ))
    }

    fn search(&self, query: &Query) -> Result<SearchPage, LibraryError> {
        query.check_supported()?;
        let facts = self.target(query)?;
        let rows = self
            .rows(facts)
            .into_iter()
            .filter(|e| query.usable_for != Some(Usage::Publish) || self.info(e, facts).rights.publish_ok)
            // A style mismatch stays, but ranks below an equally good text match that fits.
            .filter_map(|e| {
                let fits = u8::from(Self::compat(e, facts) == CompatStatus::Compatible);
                rank(query, e).map(|r| (r * 2 + fits, e))
            })
            .collect();
        Ok(self.page(rows, facts, query.offset, query.limit))
    }

    fn describe(&self, item: &ItemId, version: Option<&Version>) -> Result<ItemDetail, LibraryError> {
        let req = version.map_or(VersionReq::Any, |v| VersionReq::Exact(*v));
        let entry = self.pick(item, &req)?;
        Ok(ItemDetail { info: self.info(entry, None), item: entry.item.clone(), versions: self.versions(item)? })
    }

    fn resolve(&self, item: &ItemId, req: &VersionReq) -> Result<ResolvedItem, LibraryError> {
        self.pick(item, req).map(|e| e.item.clone())
    }

    fn by_hash(&self, hash: &ContentHash) -> Result<ResolvedItem, LibraryError> {
        let mut found: Vec<&Entry> = self.entries.iter().filter(|e| e.item.hash == *hash).collect();
        found.sort_by(|a, b| a.scope.cmp(&b.scope));
        found.first().map(|e| e.item.clone()).ok_or_else(|| LibraryError::NotFound(hash.to_string()))
    }

    fn versions(&self, item: &ItemId) -> Result<Vec<Version>, LibraryError> {
        let mut all: Vec<Version> = self.lineage(item)?.iter().map(|e| e.version).collect();
        all.sort();
        all.dedup();
        Ok(all)
    }
}
