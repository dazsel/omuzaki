//! [`MockLibrary`]: the naive oracle. Every question is answered by a linear scan over the held entries.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::CharId;
use omuzaki_library_api::{
    CompatStatus, CompatTarget, Facets, ItemDetail, ItemId, ItemInfo, ItemKind, ItemManifest, ItemScope, ItemStep,
    Library, LibraryError, Origin, PublisherCheck, PublisherKey, Query, ResolvedItem, RightsBadge, SearchPage, Section,
    Step, Usage, Version, VersionReq,
};
use omuzaki_ops_api::{OpKind, Scope, find};
use omuzaki_store_api::{ContentHash, licence_gate};

use crate::apply::MAX_DEPTH;
use crate::facts::{CharacterFacts, status};

/// The most rows a page returns, and the page size when `limit` is 0.
const MAX_LIMIT: usize = 100;

/// The ops fixtures use; `ghost.missing` is deliberately absent. See [`MockLibrary::set_known_ops`].
const DEFAULT_OPS: [&str; 3] = ["part.add", "palette.set", "face.set_style"];

/// The precedence tier of a scope when an id is resolved without a pin (D11): first-party, then my library, then
/// team scopes. All teams share one tier, so no team wins by its name.
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

    /// Why `m` can't be applied in this build, as `apply_with_library` would find out: a reserved target, an op that
    /// isn't registered or is above the recipe ceiling, or a bundle item that doesn't resolve or is itself unavailable.
    /// The first failure in step order is the reason.
    fn unavailable(&self, m: &ItemManifest) -> Option<String> {
        self.unavailable_at(m, 0)
    }

    fn unavailable_at(&self, m: &ItemManifest, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return Some(format!("bundles nest deeper than {MAX_DEPTH}; is there a cycle?"));
        }
        if let Some(why) = m.target.reserved() {
            return Some(why.to_owned());
        }
        m.steps.iter().enumerate().find_map(|(index, step)| match step {
            Step::Op(op) => self.op_unavailable(&op.op),
            Step::Item(nested) => self.item_unavailable(index, nested, depth),
        })
    }

    fn op_unavailable(&self, name: &str) -> Option<String> {
        if !self.known_ops.contains(name) {
            return Some(format!("`{name}` is not a registered op in this build"));
        }
        // An op the mock was told about but this binary doesn't register has no kind or scope to judge.
        let meta = (find(name)?.meta)();
        if meta.kind != OpKind::Edit {
            return Some(format!("`{name}` is a {:?}, and recipes may only call edit ops", meta.kind));
        }
        (!matches!(meta.scope, Scope::EditCharacter | Scope::EditScene)).then(|| {
            format!("`{name}` needs the {} scope; recipes are limited to edit:character and edit:scene", meta.scope)
        })
    }

    fn item_unavailable(&self, index: usize, nested: &ItemStep, depth: usize) -> Option<String> {
        let at = format!("step/{index}");
        let (id, req) = match (ItemId::new(&nested.item), VersionReq::parse(&nested.version)) {
            (Ok(id), Ok(req)) => (id, req),
            (Err(e), _) => return Some(format!("{at}: {e}")),
            (_, Err(e)) => return Some(format!("{at}: {e}")),
        };
        match self.pick(&id, &req) {
            Err(e) => Some(format!("{at}: {e}")),
            Ok(found) => self
                .unavailable_at(&found.item.manifest, depth + 1)
                .map(|why| format!("{at}: `{}` is unavailable: {why}", nested.item)),
        }
    }

    fn info(&self, e: &Entry, facts: Option<&CharacterFacts>) -> ItemInfo {
        let m = &e.item.manifest;
        let provenance = m.rights.to_provenance(&e.id, &e.version);
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
                publish_ok: licence_gate(&provenance, false).is_ok(),
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

    /// The newest version per (id, scope, publisher key) that search may show: the rows of a listing. `listed = false`
    /// and deprecated items are hidden here, before ranking, paging and facets, but stay resolvable. The newest version
    /// decides, so a deprecated release never lets an older one resurface. Given a character, items that don't fit it
    /// drop out, and a row shows the newest version that does.
    fn rows(&self, facts: Option<&CharacterFacts>) -> Vec<&Entry> {
        let mut groups: BTreeMap<(&ItemId, &ItemScope, Option<&str>), Vec<&Entry>> = BTreeMap::new();
        for e in &self.entries {
            let key = (&e.id, &e.scope, e.item.manifest.publisher.as_ref().map(|p| p.key.as_str()));
            groups.entry(key).or_default().push(e);
        }
        groups
            .into_values()
            .filter_map(|group| {
                let newest = group.iter().copied().max_by(|a, b| a.version.cmp(&b.version))?;
                if !newest.item.manifest.listed || newest.item.manifest.deprecated.is_some() {
                    return None;
                }
                // With a target, the row shows the newest version that fits it (the contract's "newest compatible").
                group
                    .into_iter()
                    .filter(|e| !matches!(Self::compat(e, facts), CompatStatus::Incompatible { .. }))
                    .max_by(|a, b| a.version.cmp(&b.version))
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

    /// The entry of `item` matching `req` in the first tier that has any, by resolution order. A tier is judged as a
    /// whole: two teams holding the item are one tier, so the newest version wins across them and two publishers are
    /// ambiguous, whatever the teams are called.
    fn pick(&self, item: &ItemId, req: &VersionReq) -> Result<&Entry, LibraryError> {
        let held: Vec<&Entry> = self.entries.iter().filter(|e| &e.id == item).collect();
        if held.is_empty() {
            return Err(LibraryError::NotFound(item.to_string()));
        }
        let tiers: BTreeSet<u8> = held.iter().map(|e| tier(&e.scope)).collect();
        for t in tiers {
            let matching: Vec<&Entry> =
                held.iter().copied().filter(|e| tier(&e.scope) == t && req.matches(&e.version)).collect();
            if matching.is_empty() {
                continue;
            }
            ensure_one_publisher(item, &matching)?;
            // The same version in two scopes of a tier is the same content (D11), so the scope only breaks the tie.
            return matching
                .into_iter()
                .max_by(|a, b| a.version.cmp(&b.version).then_with(|| b.scope.cmp(&a.scope)))
                .ok_or_else(|| LibraryError::NotFound(item.to_string()));
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

fn rank(q: &Query, e: &Entry) -> Option<u8> {
    let m = &e.item.manifest;
    let wanted = q.section.is_none_or(|s| s == e.section)
        && q.kind.is_none_or(|k: ItemKind| k == m.kind)
        && q.scope.as_ref().is_none_or(|s| *s == e.scope)
        && q.tags.iter().all(|t| m.tags.contains(t));
    if !wanted {
        return None;
    }
    let Some(text) = q.text.as_deref().map(str::to_lowercase).filter(|t| !t.is_empty()) else { return Some(0) };
    let has = |s: &str| s.to_lowercase().contains(&text);
    if e.id.as_str() == text {
        Some(4)
    } else if has(&m.title) || m.titles.values().any(|t| has(t)) {
        Some(3)
    } else if m.tags.iter().any(|t| has(t)) {
        Some(2)
    } else if has(&m.summary) || has(e.id.as_str()) {
        Some(1)
    } else {
        None
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
        let held: Vec<&Entry> = self.entries.iter().filter(|e| &e.id == item).collect();
        if held.is_empty() {
            return Err(LibraryError::NotFound(item.to_string()));
        }
        // Ambiguity is judged where `resolve` would look first: the earliest tier that holds the item.
        let first = held.iter().map(|e| tier(&e.scope)).min();
        ensure_one_publisher(
            item,
            &held.iter().copied().filter(|e| Some(tier(&e.scope)) == first).collect::<Vec<_>>(),
        )?;
        let mut all: Vec<Version> = held.iter().map(|e| e.version).collect();
        all.sort();
        all.dedup();
        Ok(all)
    }
}
