//! [`MockLibrary`]: the naive oracle. Every question is answered by a linear scan over the held entries.

use std::collections::BTreeSet;

use omuzaki_library_api::{
    CompatStatus, Facets, ItemDetail, ItemId, ItemInfo, ItemKind, ItemManifest, ItemScope, Library, LibraryError,
    Origin, PublisherCheck, PublisherKey, Query, ResolvedItem, RightsBadge, SearchPage, Section, Usage, Version,
    VersionReq,
};
use omuzaki_store_api::{ContentHash, licence_gate};

/// The most rows a page returns, and the page size when `limit` is 0.
const MAX_LIMIT: usize = 100;

/// The ops fixtures use; `ghost.missing` is deliberately absent. See [`MockLibrary::set_known_ops`].
const DEFAULT_OPS: [&str; 3] = ["part.add", "palette.set", "face.set_style"];

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
}

impl Default for MockLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl MockLibrary {
    /// An empty library that knows the ops the fixtures use.
    pub fn new() -> Self {
        Self { entries: Vec::new(), known_ops: DEFAULT_OPS.iter().map(|s| (*s).to_owned()).collect() }
    }

    /// A library holding [`fixtures::all`](crate::fixtures::all).
    pub fn fixtures() -> Self {
        let mut library = Self::new();
        for f in crate::fixtures::all() {
            // The fixtures are valid by construction; a refusal here is a bug the crate's tests catch.
            if let Err(e) = library.insert(f.manifest, f.scope) {
                debug_assert!(false, "fixture refused: {e}");
            }
        }
        library
    }

    /// Replaces the ops items may use; an item with a step outside the set is listed as unavailable.
    pub fn set_known_ops<I: IntoIterator<Item = String>>(&mut self, ops: I) {
        self.known_ops = ops.into_iter().collect();
    }

    /// Adds an item to `scope`. Adding the same manifest twice changes nothing.
    ///
    /// # Errors
    /// [`LibraryError::Storage`] when the id, version or section is malformed, when the publisher check refuses it,
    /// or when the same publisher key already holds this id and version in the scope with a different hash.
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
        let same = |e: &&Entry| e.id == id && e.version == version && e.scope == scope;
        for held in self.entries.iter().filter(same) {
            let same_key =
                held.item.manifest.publisher.as_ref().map(|p| &p.key) == manifest.publisher.as_ref().map(|p| &p.key);
            if same_key && held.item.hash == hash {
                return Ok(());
            }
            if same_key {
                return Err(bad(&format!("{version} is already held with a different hash")));
            }
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

    fn unavailable(&self, m: &ItemManifest) -> Option<String> {
        if let Some(why) = m.target.reserved() {
            return Some(why.to_owned());
        }
        m.steps.iter().find_map(|s| match s {
            omuzaki_library_api::Step::Op(op) if !self.known_ops.contains(&op.op) => {
                Some(format!("`{}` is not a registered op in this build", op.op))
            }
            _ => None,
        })
    }

    fn info(&self, e: &Entry) -> ItemInfo {
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
            // The mock has no character to compare with, so every item fits.
            compat: CompatStatus::Compatible,
            preview: None,
            unavailable: self.unavailable(m),
        }
    }

    /// The newest version per (id, scope, publisher key): the rows of a listing.
    fn rows(&self) -> Vec<&Entry> {
        let key =
            |e: &Entry| (e.id.clone(), e.scope.clone(), e.item.manifest.publisher.as_ref().map(|p| p.key.clone()));
        self.entries
            .iter()
            .filter(|e| !self.entries.iter().any(|o| key(o) == key(e) && o.version > e.version))
            .collect()
    }

    fn page(&self, mut rows: Vec<(u8, &Entry)>, offset: usize, limit: usize) -> SearchPage {
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
        let items = rows.iter().skip(offset).take(limit).map(|(_, e)| self.info(e)).collect();
        SearchPage { total: rows.len(), items, facets }
    }

    /// The entries of `item` matching `req` in the first scope that has any, by resolution order.
    fn pick(&self, item: &ItemId, req: &VersionReq) -> Result<&Entry, LibraryError> {
        let held: Vec<&Entry> = self.entries.iter().filter(|e| &e.id == item).collect();
        if held.is_empty() {
            return Err(LibraryError::NotFound(item.to_string()));
        }
        let scopes: BTreeSet<&ItemScope> = held.iter().map(|e| &e.scope).collect();
        for scope in scopes {
            let matching: Vec<&Entry> =
                held.iter().copied().filter(|e| &e.scope == scope && req.matches(&e.version)).collect();
            if matching.is_empty() {
                continue;
            }
            ensure_one_publisher(item, &matching)?;
            return matching
                .into_iter()
                .max_by(|a, b| a.version.cmp(&b.version))
                .ok_or_else(|| LibraryError::NotFound(item.to_string()));
        }
        let mut installed: Vec<Version> = held.iter().map(|e| e.version).collect();
        installed.sort();
        installed.dedup();
        Err(LibraryError::NoMatchingVersion { item: item.clone(), req: *req, installed })
    }
}

fn ensure_one_publisher(item: &ItemId, entries: &[&Entry]) -> Result<(), LibraryError> {
    let mut candidates: Vec<(String, String)> = entries
        .iter()
        .map(|e| match &e.item.manifest.publisher {
            Some(p) => (p.key.clone(), p.name.clone()),
            None => (String::new(), "unsigned".to_owned()),
        })
        .collect();
    candidates.sort();
    candidates.dedup();
    if candidates.len() < 2 {
        return Ok(());
    }
    let named = candidates
        .into_iter()
        .map(|(key, name)| {
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
        Ok(self.page(self.rows().into_iter().filter_map(|e| rank(&q, e).map(|_| (0, e))).collect(), offset, limit))
    }

    fn search(&self, query: &Query) -> Result<SearchPage, LibraryError> {
        query.check_supported()?;
        let rows = self
            .rows()
            .into_iter()
            .filter(|e| query.usable_for != Some(Usage::Publish) || self.info(e).rights.publish_ok)
            .filter_map(|e| rank(query, e).map(|r| (r, e)))
            .collect();
        Ok(self.page(rows, query.offset, query.limit))
    }

    fn describe(&self, item: &ItemId, version: Option<&Version>) -> Result<ItemDetail, LibraryError> {
        let req = version.map_or(VersionReq::Any, |v| VersionReq::Exact(*v));
        let entry = self.pick(item, &req)?;
        Ok(ItemDetail { info: self.info(entry), item: entry.item.clone(), versions: self.versions(item)? })
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
        // Ambiguity is judged where `resolve` would look first: the earliest scope that holds the item.
        let first = held.iter().map(|e| &e.scope).min();
        ensure_one_publisher(item, &held.iter().copied().filter(|e| Some(&e.scope) == first).collect::<Vec<_>>())?;
        let mut all: Vec<Version> = held.iter().map(|e| e.version).collect();
        all.sort();
        all.dedup();
        Ok(all)
    }
}
