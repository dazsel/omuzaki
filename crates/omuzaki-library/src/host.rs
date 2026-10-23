//! [`HostLibrary`]: the registry and search index (design D11, D13, D14).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use omuzaki_char_api::{BodyTemplateId, CharId, PartId};
use omuzaki_library_api::{
    Bindings, CharacterFacts, CompatStatus, CompatTarget, Facets, ItemDetail, ItemId, ItemInfo, ItemKind, ItemManifest,
    ItemScope, Library, LibraryError, Origin, Params, PublisherCheck, PublisherKey, Query, ResolvedItem, RightsBadge,
    SearchPage, Section, Step, Usage, Version, VersionReq, sources,
};
use omuzaki_ops_api::OpError;
use omuzaki_store_api::{ContentHash, licence_gate};
use serde_json::Value;

use crate::Acknowledgement;
use crate::apply::{MAX_DEPTH, Visitor, check_step, creates_keys, walk};
use crate::text::{Indexed, Query as TextQuery};

/// The most rows a page returns, and the page size when `limit` is 0.
const MAX_LIMIT: usize = 100;

/// A body template no `when` guard names, so the steps every guard excludes are validated too.
const UNGUARDED_TEMPLATE: &str = "unguarded";

/// The folder of the person's items under the library folder.
const ITEMS: &str = "items";

/// The precedence tier of a scope (design D11): first-party, then the person's library, then team scopes, all teams
/// one tier.
fn tier(scope: &ItemScope) -> u8 {
    match scope {
        ItemScope::FirstParty => 0,
        ItemScope::Mine => 1,
        ItemScope::Team(_) => 2,
    }
}

fn key_of(m: &ItemManifest) -> Option<String> {
    m.publisher.as_ref().map(|p| p.key.clone())
}

struct Entry {
    id: ItemId,
    version: Version,
    section: Section,
    scope: ItemScope,
    item: ResolvedItem,
    text: Indexed,
    /// Why it can't be applied in this build, worked out when the registry last changed.
    unavailable: Option<String>,
    /// Why what it makes can't be published, likewise.
    publish_blocker: Option<String>,
}

#[derive(Default)]
struct Registry {
    entries: Vec<Entry>,
    /// Per (id, scope, publisher), the indices of the versions search may show (listed, not deprecated), newest
    /// first. Built by `reindex`, so a query never regroups the registry.
    shown: Vec<Vec<usize>>,
}

/// The library of one running app: every first-party source linked into the binary and the person's own library
/// folder, behind one lookup and one search index. Installing writes the folder and updates the index at once.
///
/// It answers [`Library::search_for`] from the [`CharacterFacts`] the op reads from the document; a plain
/// [`Library::search`] with `compatible_with` set has no facts to go on and is refused as an unknown character.
pub struct HostLibrary {
    registry: RwLock<Registry>,
    /// `<data>/library`, when the app has a data folder.
    folder: Option<PathBuf>,
}

impl HostLibrary {
    /// A library of the first-party sources linked into this binary and, with `folder`, the person's items under
    /// `folder/items`. An item that fails its checks is left out with a warning naming it, so one bad file never
    /// hides the rest.
    pub fn open(folder: Option<&Path>) -> Self {
        let library = Self::empty(folder);
        let mut found = Vec::new();
        for source in sources() {
            found.extend(source.items().into_iter().map(|m| (m, ItemScope::FirstParty, source.id().to_owned())));
        }
        if let Some(dir) = folder {
            for (path, manifest) in read_folder(&dir.join(ITEMS)) {
                found.push((manifest, ItemScope::Mine, path.display().to_string()));
            }
        }
        {
            let mut registry = library.write();
            for (manifest, scope, from) in found {
                if let Err(error) = registry.insert(manifest, scope) {
                    tracing::warn!(%from, %error, "a library item was left out");
                }
            }
            registry.reindex();
        }
        library
    }

    /// A library with no items, writing the person's items under `folder` when given.
    pub fn empty(folder: Option<&Path>) -> Self {
        Self { registry: RwLock::new(Registry::default()), folder: folder.map(Path::to_path_buf) }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.registry.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.registry.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an item to `scope` in memory, as a host with another source (a team cache, a test) does. Adding the same
    /// manifest to the same scope twice changes nothing.
    ///
    /// # Errors
    /// [`LibraryError::Storage`] naming what is wrong: a manifest that fails its checks, a refused publisher, or the
    /// same publisher's `id@version` already held with different content.
    pub fn insert(&self, manifest: ItemManifest, scope: ItemScope) -> Result<(), LibraryError> {
        let mut registry = self.write();
        registry.insert(manifest, scope)?;
        registry.reindex();
        Ok(())
    }

    /// Adds many items at once and indexes once, as a host loading a team cache does: one by one, each
    /// [`insert`](Self::insert) re-checks every item. Returns the ones refused, by id, with why.
    pub fn insert_all(
        &self,
        items: impl IntoIterator<Item = (ItemManifest, ItemScope)>,
    ) -> Vec<(String, LibraryError)> {
        let mut registry = self.write();
        let mut refused = Vec::new();
        for (manifest, scope) in items {
            let id = manifest.id.clone();
            if let Err(error) = registry.insert(manifest, scope) {
                refused.push((id, error));
            }
        }
        registry.reindex();
        refused
    }

    /// Installs `manifest` in the person's library (design D9, D11): checks it as [`insert`](Self::insert) does,
    /// writes `items/<publisher>/<name…>/<version>.toml` and indexes it. Returns whether it was new.
    ///
    /// # Errors
    /// As [`insert`](Self::insert); [`LibraryError::Storage`] when there is no library folder or the file can't be
    /// written; a first-party id is refused, since those come with the build.
    pub fn install(&self, manifest: &ItemManifest) -> Result<bool, LibraryError> {
        let id = manifest.item_id().map_err(|e| LibraryError::Storage(e.to_string()))?;
        if id.is_first_party() && manifest.publisher.is_none() {
            return Err(LibraryError::Storage(format!(
                "{id}: first-party items come with Omuzaki and aren't installed"
            )));
        }
        let path = self.item_path(manifest)?;
        let mut registry = self.write();
        let before = registry.entries.len();
        registry.insert(manifest.clone(), ItemScope::Mine)?;
        let new = registry.entries.len() > before;
        if new {
            if let Err(error) = write_item(&path, manifest) {
                registry.entries.pop();
                return Err(error);
            }
            registry.reindex();
        }
        Ok(new)
    }

    /// Removes `item@version` from the person's library and returns its manifest. Projects keep their pins: a
    /// project never reads the library to build.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`] when the person's library doesn't hold it; [`LibraryError::Storage`] when the file
    /// can't be removed.
    pub fn uninstall(&self, item: &ItemId, version: &Version) -> Result<ItemManifest, LibraryError> {
        let mut registry = self.write();
        let at = registry
            .entries
            .iter()
            .position(|e| e.id == *item && e.version == *version && e.scope == ItemScope::Mine)
            .ok_or_else(|| LibraryError::NotFound(format!("{item}@{version} in your library")))?;
        let manifest = registry.entries[at].item.manifest.clone();
        let path = self.item_path(&manifest)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LibraryError::Storage(format!("{}: {e}", path.display()))),
        }
        registry.entries.remove(at);
        registry.reindex();
        Ok(manifest)
    }

    /// Where the person's copy of `manifest` lives.
    fn item_path(&self, manifest: &ItemManifest) -> Result<PathBuf, LibraryError> {
        let folder = self.folder.as_ref().ok_or_else(|| {
            LibraryError::Storage("this app has no data folder, so it has no library of your own".to_owned())
        })?;
        let id = manifest.item_id().map_err(|e| LibraryError::Storage(e.to_string()))?;
        let version = manifest.item_version().map_err(|e| LibraryError::Storage(e.to_string()))?;
        let dir = id.as_str().split('/').fold(folder.join(ITEMS), |p, s| p.join(s));
        // Two publishers may hold one id and version (design D2): a signed item's file carries its key.
        Ok(match &manifest.publisher {
            Some(publisher) => {
                // The key is the manifest's own text: only its hex digits reach the file name.
                let key: String = publisher.key.chars().filter(char::is_ascii_hexdigit).take(16).collect();
                dir.join(format!("{version}.{key}.toml"))
            }
            None => dir.join(format!("{version}.toml")),
        })
    }

    /// Records that the person accepted the disclaimer for an item (design D8): once per publisher key and item hash,
    /// in `acks.toml` in the library folder. Returns whether it was new.
    ///
    /// # Errors
    /// [`LibraryError::Storage`] when there is no library folder or the file can't be read or written.
    pub fn acknowledge(&self, ack: Acknowledgement) -> Result<bool, LibraryError> {
        let _writing = self.write();
        crate::acks::add(&self.acks_path()?, ack)
    }

    /// Every acknowledgement on record, sorted.
    ///
    /// # Errors
    /// As [`acknowledge`](Self::acknowledge).
    pub fn acknowledgements(&self) -> Result<Vec<Acknowledgement>, LibraryError> {
        crate::acks::read(&self.acks_path()?)
    }

    fn acks_path(&self) -> Result<PathBuf, LibraryError> {
        let folder = self.folder.as_ref().ok_or_else(|| {
            LibraryError::Storage("this app has no data folder, so it has no library of your own".to_owned())
        })?;
        Ok(folder.join(crate::acks::ACKS))
    }
}

/// Every `*.toml` under `dir` that parses as a manifest; the others are named in a warning and left out.
fn read_folder(dir: &Path) -> Vec<(PathBuf, ItemManifest)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else { continue };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "toml") {
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|text| ItemManifest::parse(&text).map_err(|e| e.to_string()))
                {
                    Ok(manifest) => found.push((path, manifest)),
                    Err(error) => tracing::warn!(path = %path.display(), %error, "a library item doesn't read"),
                }
            }
        }
    }
    found
}

/// Writes `manifest` at `path` through a temporary file, so a crash leaves the old file or the new one.
fn write_item(path: &Path, manifest: &ItemManifest) -> Result<(), LibraryError> {
    let storage = |e: &dyn std::fmt::Display| LibraryError::Storage(format!("{}: {e}", path.display()));
    let text = toml::to_string(manifest).map_err(|e| storage(&e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| storage(&e))?;
    }
    let partial = path.with_extension("toml.partial");
    std::fs::write(&partial, text).map_err(|e| storage(&e))?;
    std::fs::rename(&partial, path).map_err(|e| storage(&e))
}

/// The one-line reason an item is unavailable: its message, led by its field unless the message starts with it.
fn reason(e: &OpError) -> String {
    match &e.field {
        Some(field) if !e.message.starts_with(field.as_str()) => format!("{field}: {}", e.message),
        _ => e.message.clone(),
    }
}

/// The availability check's view of an op step: what an apply checks before planning it.
struct Availability;

impl Visitor for Availability {
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError> {
        check_step(index, op, &args)
    }
}

impl Registry {
    fn insert(&mut self, manifest: ItemManifest, scope: ItemScope) -> Result<(), LibraryError> {
        let bad = |what: &dyn std::fmt::Display| LibraryError::Storage(format!("{}: {what}", manifest.id));
        let problems = manifest.check();
        if !problems.is_empty() {
            let all: Vec<String> = problems.iter().map(ToString::to_string).collect();
            return Err(bad(&format!("fails check: {}", all.join("; "))));
        }
        let id = manifest.item_id().map_err(|e| bad(&e))?;
        let version = manifest.item_version().map_err(|e| bad(&e))?;
        let section = manifest.item_section().map_err(|e| bad(&e))?;
        let origin = if scope == ItemScope::FirstParty { Origin::CompiledIn } else { Origin::Installed };
        let publisher = manifest.verify(origin);
        if let PublisherCheck::Refused { reason } = &publisher {
            return Err(bad(&format!("refused: {reason}")));
        }
        let hash = manifest.hash();
        let twins: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.id == id && e.version == version && key_of(&e.item.manifest) == key_of(&manifest))
            .collect();
        if let Some(held) = twins.iter().find(|e| e.item.hash != hash) {
            return Err(bad(&format!(
                "{version} is already held in {:?} with different content; bump the version",
                held.scope
            )));
        }
        if twins.iter().any(|e| e.scope == scope) {
            return Ok(());
        }
        let text = Indexed::of(&id, &manifest);
        self.entries.push(Entry {
            id,
            version,
            section,
            scope: scope.clone(),
            item: ResolvedItem { manifest, hash, scope, publisher },
            text,
            unavailable: None,
            publish_blocker: None,
        });
        Ok(())
    }

    /// Works out every entry's availability and publishability again: both depend on the items a bundle applies.
    fn reindex(&mut self) {
        let worked: Vec<(Option<String>, Option<String>)> =
            self.entries.iter().map(|e| (self.unavailable(&e.item.manifest), self.publish_blocker(e, 0))).collect();
        for (entry, (unavailable, blocker)) in self.entries.iter_mut().zip(worked) {
            entry.unavailable = unavailable;
            entry.publish_blocker = blocker;
        }
        let mut groups: BTreeMap<(&ItemId, &ItemScope, Option<&str>), Vec<usize>> = BTreeMap::new();
        for (at, e) in self.entries.iter().enumerate() {
            if e.item.manifest.listed && e.item.manifest.deprecated.is_none() {
                let key = (&e.id, &e.scope, e.item.manifest.publisher.as_ref().map(|p| p.key.as_str()));
                groups.entry(key).or_default().push(at);
            }
        }
        let entries = &self.entries;
        self.shown = groups
            .into_values()
            .map(|mut group| {
                group.sort_by(|a, b| entries[*b].version.cmp(&entries[*a].version));
                group
            })
            .collect();
    }

    fn resolver(&self) -> impl Fn(&ItemId, &VersionReq) -> Result<ResolvedItem, LibraryError> + '_ {
        |id, req| self.pick(id, req).map(|e| e.item.clone())
    }

    /// The first failure of the checks an apply makes before it plans a step, run once per body template a `when`
    /// guard names and once for a template none does.
    fn unavailable(&self, m: &ItemManifest) -> Option<String> {
        let resolve = self.resolver();
        let owners = match creates_keys(&resolve, m) {
            Ok(owners) => owners,
            Err(why) => return Some(why),
        };
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
            for (key, (_, creates)) in &owners {
                bindings.parts.extend(PartId::new(creates.id.as_str()).ok().map(|id| (key.clone(), id)));
            }
            walk(&resolve, m, &Params::new(), &bindings, 0, &mut Availability).err().map(|e| reason(&e))
        })
    }

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

    /// The first reason what `e` makes may not be published: its rights, its assets (whose provenance this library
    /// can't see yet, so they fail closed) and every item a bundle applies.
    fn publish_blocker(&self, e: &Entry, depth: usize) -> Option<String> {
        let m = &e.item.manifest;
        if depth > MAX_DEPTH {
            return Some("nested too deeply".to_owned());
        }
        if let Err(why) = licence_gate(&m.rights.to_provenance(&e.id, &e.version), false) {
            return Some(why.to_string());
        }
        if let Some(name) = m.assets.keys().next() {
            return Some(format!("asset `{name}` has unknown provenance"));
        }
        m.steps.iter().find_map(|step| {
            let Step::Item(nested) = step else { return None };
            let Ok(id) = ItemId::new(&nested.item) else {
                return Some(format!("step names an invalid item id `{}`", nested.item));
            };
            let Ok(req) = VersionReq::parse(&nested.version) else {
                return Some(format!("step `{}` has an invalid version requirement `{}`", nested.item, nested.version));
            };
            match self.pick(&id, &req) {
                Ok(found) => self.publish_blocker(found, depth + 1),
                Err(_) => {
                    Some(format!("step `{}@{}` is not held, so its rights are unknown", nested.item, nested.version))
                }
            }
        })
    }

    fn info(e: &Entry, facts: Option<&CharacterFacts>) -> ItemInfo {
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
                publish_ok: e.publish_blocker.is_none(),
            },
            compat: Self::compat(e, facts),
            preview: None,
            unavailable: e.unavailable.clone(),
        }
    }

    fn compat(e: &Entry, facts: Option<&CharacterFacts>) -> CompatStatus {
        facts.map_or(CompatStatus::Compatible, |f| f.status(&e.item.manifest.compat))
    }

    /// Per (id, scope, publisher), the newest version search may show: listed, not deprecated and, given a
    /// character, not incompatible with it.
    fn rows(&self, facts: Option<&CharacterFacts>) -> impl Iterator<Item = &Entry> {
        self.shown.iter().filter_map(move |group| {
            group
                .iter()
                .map(|at| &self.entries[*at])
                .find(|e| !matches!(Self::compat(e, facts), CompatStatus::Incompatible { .. }))
        })
    }

    fn page(mut rows: Vec<(u8, &Entry)>, facts: Option<&CharacterFacts>, offset: usize, limit: usize) -> SearchPage {
        // The same id@version, hash and publisher in several scopes is one entry, under its first scope (D11).
        fn same(e: &Entry) -> (Version, [u8; 32], Option<&str>) {
            (e.version, e.item.hash.0, e.item.manifest.publisher.as_ref().map(|p| p.key.as_str()))
        }
        rows.sort_by(|(_, a), (_, b)| a.id.cmp(&b.id).then(same(a).cmp(&same(b))).then(a.scope.cmp(&b.scope)));
        rows.dedup_by(|(_, later), (_, kept)| later.id == kept.id && same(later) == same(kept));
        rows.sort_by(|(sa, a), (sb, b)| {
            sb.cmp(sa)
                .then(a.section.cmp(&b.section))
                .then(a.id.cmp(&b.id))
                .then(a.scope.cmp(&b.scope))
                .then(a.item.hash.0.cmp(&b.item.hash.0))
        });
        let mut counts = Facets::default();
        let mut tags: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, e) in &rows {
            *counts.sections.entry(e.section).or_default() += 1;
            *counts.kinds.entry(e.item.manifest.kind).or_default() += 1;
            for tag in &e.item.manifest.tags {
                *tags.entry(tag.as_str()).or_default() += 1;
            }
            *counts.scopes.entry(e.scope.clone()).or_default() += 1;
        }
        counts.tags = tags.into_iter().map(|(tag, n)| (tag.to_owned(), n)).collect();
        let limit = if limit == 0 { MAX_LIMIT } else { limit.min(MAX_LIMIT) };
        let items = rows.iter().skip(offset).take(limit).map(|(_, e)| Self::info(e, facts)).collect();
        SearchPage { total: rows.len(), items, facets: counts }
    }

    /// The versions of `item` that are its history: the publisher holding it in the first tier that has it
    /// (`Ambiguous` when that tier holds several), in any tier.
    fn lineage(&self, item: &ItemId) -> Result<Vec<&Entry>, LibraryError> {
        let held: Vec<&Entry> = self.entries.iter().filter(|e| &e.id == item).collect();
        let Some(first) = held.iter().map(|e| tier(&e.scope)).min() else {
            return Err(LibraryError::NotFound(item.to_string()));
        };
        let leading: Vec<&Entry> = held.iter().copied().filter(|e| tier(&e.scope) == first).collect();
        ensure_one_publisher(item, &leading)?;
        let publisher = leading.first().and_then(|e| key_of(&e.item.manifest));
        Ok(held.into_iter().filter(|e| key_of(&e.item.manifest) == publisher).collect())
    }

    /// The entry of `item` matching `req`: the first tier with a match, the newest version in it.
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
}

/// Refuses with `Ambiguous` when `entries` come from more than one publisher key.
fn ensure_one_publisher(item: &ItemId, entries: &[&Entry]) -> Result<(), LibraryError> {
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
            let fingerprint = PublisherKey::from_hex(&key).map_or_else(|_| "no key".to_owned(), |k| k.fingerprint());
            format!("{name} ({fingerprint})")
        })
        .collect();
    Err(LibraryError::Ambiguous { item: item.clone(), candidates: named })
}

/// The text rank of `e` for `q` (0 with no text), or `None` when a filter or the text rules it out.
fn rank(q: &Query, text: Option<&TextQuery>, e: &Entry) -> Option<u8> {
    let m = &e.item.manifest;
    let wanted = q.section.is_none_or(|s| s == e.section)
        && q.kind.is_none_or(|k: ItemKind| k == m.kind)
        && q.scope.as_ref().is_none_or(|s| *s == e.scope)
        && q.tags.iter().all(|t| m.tags.contains(t));
    if !wanted {
        return None;
    }
    match text {
        None => Some(0),
        Some(text) => e.text.rank(text),
    }
}

impl Library for HostLibrary {
    fn list(
        &self,
        section: Option<Section>,
        scope: Option<&ItemScope>,
        offset: usize,
        limit: usize,
    ) -> Result<SearchPage, LibraryError> {
        let registry = self.read();
        let q = Query { section, scope: scope.cloned(), ..Query::default() };
        let rows = registry.rows(None).filter_map(|e| rank(&q, None, e).map(|_| (0, e))).collect();
        Ok(Registry::page(rows, None, offset, limit))
    }

    fn search(&self, query: &Query) -> Result<SearchPage, LibraryError> {
        self.search_for(query, None)
    }

    fn search_for(&self, query: &Query, character: Option<&CharacterFacts>) -> Result<SearchPage, LibraryError> {
        query.check_supported()?;
        let facts = match (&query.compatible_with, character) {
            (None, _) => None,
            (Some(_), Some(facts)) => Some(facts),
            (Some(CompatTarget::Character(id)), None) => {
                return Err(LibraryError::NotFound(format!("character {id}")));
            }
        };
        let text = query.text.as_deref().filter(|t| !t.trim().is_empty()).map(TextQuery::new);
        let registry = self.read();
        let rows = registry
            .rows(facts)
            .filter(|e| query.usable_for != Some(Usage::Publish) || e.publish_blocker.is_none())
            // A style mismatch stays, but ranks below an equally good text match that fits.
            .filter_map(|e| {
                let fits = u8::from(Registry::compat(e, facts) == CompatStatus::Compatible);
                rank(query, text.as_ref(), e).map(|r| (r * 2 + fits, e))
            })
            .collect();
        Ok(Registry::page(rows, facts, query.offset, query.limit))
    }

    fn describe(&self, item: &ItemId, version: Option<&Version>) -> Result<ItemDetail, LibraryError> {
        let req = version.map_or(VersionReq::Any, |v| VersionReq::Exact(*v));
        let registry = self.read();
        let entry = registry.pick(item, &req)?;
        let mut versions: Vec<Version> = registry.lineage(item)?.iter().map(|e| e.version).collect();
        versions.sort();
        versions.dedup();
        Ok(ItemDetail { info: Registry::info(entry, None), item: entry.item.clone(), versions })
    }

    fn resolve(&self, item: &ItemId, req: &VersionReq) -> Result<ResolvedItem, LibraryError> {
        self.read().pick(item, req).map(|e| e.item.clone())
    }

    fn by_hash(&self, hash: &ContentHash) -> Result<ResolvedItem, LibraryError> {
        let registry = self.read();
        let mut found: Vec<&Entry> = registry.entries.iter().filter(|e| e.item.hash == *hash).collect();
        found.sort_by(|a, b| (&a.scope, key_of(&a.item.manifest)).cmp(&(&b.scope, key_of(&b.item.manifest))));
        found.first().map(|e| e.item.clone()).ok_or_else(|| LibraryError::NotFound(hash.to_string()))
    }

    fn versions(&self, item: &ItemId) -> Result<Vec<Version>, LibraryError> {
        let registry = self.read();
        let mut all: Vec<Version> = registry.lineage(item)?.iter().map(|e| e.version).collect();
        all.sort();
        all.dedup();
        Ok(all)
    }
}
