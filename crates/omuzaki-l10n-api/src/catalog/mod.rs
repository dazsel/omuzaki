//! `Catalog`: the text of one locale, ready to render (SE-03, design 6, 9 and 12).
//!
//! A catalog holds one Fluent bundle per language in its fallback chain (design 9.1): the locale's own, its parents
//! (a regional override such as `es-ES` is sparse and sits over `es-419`), and English. Only those are parsed.
//! Static text (summaries, titles, descriptions) is formatted once and cached as `Arc<str>`, so a lookup that was
//! asked before allocates nothing. Every fallback and formatting error is recorded once, for [`Catalog::missing`].

mod builtin;
mod layer;
mod numbers;
mod render;
mod report;
#[cfg(test)]
#[allow(clippy::assert_is_empty)] // `assert_eq!` on an empty report reads worse than `is_empty`
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use layer::{Layer, Role};
use numbers::Marks;
use report::Report;
pub use report::{Missing, MissingReason};

use crate::{Locale, MessageTable, Resource, Text, ids};

/// The rendered text of one locale. Cheap to clone (an `Arc` inside) and safe to share between threads.
#[derive(Clone)]
pub struct Catalog(Arc<Inner>);

struct Inner {
    locale: Locale,
    /// The locale's own layer, then its parents'. Empty for English, which has nothing to fall back from.
    translations: Vec<Layer>,
    english: Layer,
    /// The pseudo-locale only: English as written, for the messages marked `#[verbatim]`.
    plain_english: Option<Layer>,
    /// The pseudo-locale only: the ids of the `#[verbatim]` messages, which it leaves alone.
    verbatim: HashSet<&'static str>,
    marks: Marks,
    report: Report,
    cache: Cache,
}

impl Catalog {
    /// Builds the catalog of `locale` from every registered [`Resource`]: parses English and the locale's chain,
    /// nothing else.
    pub fn new(locale: Locale) -> Self {
        let resources: Vec<&Resource> = inventory::iter::<Resource>.into_iter().collect();
        let mut chain = Vec::new();
        let mut parent = locale.parent();
        while let Some(next) = parent {
            // `OVERRIDES` is a table; a cycle in it would otherwise loop here.
            if next == locale || chain.contains(&next) {
                break;
            }
            parent = next.parent();
            chain.push(next);
        }
        Self::build(locale, &chain, &resources)
    }

    /// The English catalog, built on first use and shared.
    pub fn english() -> &'static Self {
        static ENGLISH: OnceLock<Catalog> = OnceLock::new();
        ENGLISH.get_or_init(|| Self::new(Locale::EN))
    }

    /// A catalog over the given resources and parent chain instead of the registered ones: tests and benchmarks use
    /// it for a fixture override (`es-ES` over `es-419`) or a synthetic message set. Not part of the stable API.
    #[doc(hidden)]
    pub fn from_sources(locale: Locale, parents: &[Locale], resources: &[Resource]) -> Self {
        let resources: Vec<&Resource> = resources.iter().collect();
        Self::build(locale, parents, &resources)
    }

    fn build(locale: Locale, parents: &[Locale], resources: &[&Resource]) -> Self {
        let report = Report::new(locale.clone());
        let marks = Marks::of(&locale);
        let pseudo = locale == Locale::PSEUDO;
        let english_family = locale.as_str().split('-').next() == Some("en");
        let layer = |tag: &Locale, role| {
            Layer::build(tag, role, marks, resources, builtin::list_message(tag.as_str()), &report)
        };
        let translations = if english_family {
            Vec::new()
        } else {
            std::iter::once(&locale).chain(parents).map(|tag| layer(tag, Role::Plain)).collect()
        };
        let (english, plain_english, verbatim) = if pseudo {
            let verbatim = inventory::iter::<MessageTable>
                .into_iter()
                .flat_map(|table| table.messages)
                .filter(|decl| decl.verbatim)
                .map(|decl| decl.id)
                .collect();
            (layer(&Locale::EN, Role::Pseudo), Some(layer(&Locale::EN, Role::Plain)), verbatim)
        } else {
            (layer(&Locale::EN, Role::Plain), None, HashSet::new())
        };
        Self(Arc::new(Inner {
            locale,
            translations,
            english,
            plain_english,
            verbatim,
            marks,
            report,
            cache: Cache::default(),
        }))
    }

    /// The language this catalog renders.
    pub fn locale(&self) -> &Locale {
        &self.0.locale
    }

    /// Renders `text` in this catalog's language, falling back per design 9.1: the translation, English, then the
    /// last-resort `⟦id: name=value, …⟧`. Never blank, never a panic. Each nested [`Text`] falls back on its own.
    pub fn render(&self, text: &Text) -> String {
        self.0.render(text, None, true)
    }

    /// [`render`](Self::render) with step 3 of the chain: `english` is the English stored with the record
    /// (`OpError.message`), shown when neither this language nor English knows the id.
    pub fn render_stored(&self, text: &Text, english: &str) -> String {
        self.0.render(text, Some(english), true)
    }

    /// The summary of the op `name` (`face.set_profile`), or `None` when no language has one.
    pub fn op_summary(&self, name: &str) -> Option<Arc<str>> {
        self.0.cached(&self.0.cache.op_summary, name, || (ids::op(name), None))
    }

    /// The long description of the op `name`, or `None`.
    pub fn op_description(&self, name: &str) -> Option<Arc<str>> {
        self.0.cached(&self.0.cache.op_description, name, || (ids::op(name), Some("description")))
    }

    /// The title of the part kind `id` (`twin_tails`), or `None`.
    pub fn kind_title(&self, id: &str) -> Option<Arc<str>> {
        self.0.cached(&self.0.cache.kind_title, id, || (ids::kind(id), None))
    }

    /// The summary of the part kind `id`, or `None`.
    pub fn kind_summary(&self, id: &str) -> Option<Arc<str>> {
        self.0.cached(&self.0.cache.kind_summary, id, || (ids::kind(id), Some("summary")))
    }

    /// The title of an argument: `owner_id` is the derived id of its op or part kind, `path` the schema keys down to
    /// it (design 3.1), or `None`.
    pub fn arg_title(&self, owner_id: &str, path: &[&str]) -> Option<Arc<str>> {
        self.0.arg_text(owner_id, path, false)
    }

    /// The description of an argument, addressed as [`arg_title`](Self::arg_title), or `None`.
    pub fn arg_description(&self, owner_id: &str, path: &[&str]) -> Option<Arc<str>> {
        self.0.arg_text(owner_id, path, true)
    }

    /// Everything that fell back or failed to format so far, once per id and kind of problem, ordered by id.
    pub fn missing(&self) -> Vec<Missing> {
        self.0.report.all()
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Catalog").field("locale", &self.0.locale).finish_non_exhaustive()
    }
}

/// A cached lookup: `None` is cached too, so asking for a description that does not exist is also free.
type Memo = RwLock<HashMap<Box<str>, Option<Arc<str>>>>;

/// One argument's cached title or description, found by comparing its path with the owner's few entries.
struct ArgEntry {
    path: Box<[Box<str>]>,
    description: bool,
    text: Option<Arc<str>>,
}

#[derive(Default)]
struct Cache {
    op_summary: Memo,
    op_description: Memo,
    kind_title: Memo,
    kind_summary: Memo,
    args: RwLock<HashMap<Box<str>, Vec<ArgEntry>>>,
}

impl Inner {
    /// Looks `key` up in `memo`; on a miss, formats the text of the `(id, attribute)` that `address` names and keeps
    /// it. The hit path takes a read lock and clones an `Arc`.
    fn cached(
        &self,
        memo: &Memo,
        key: &str,
        address: impl FnOnce() -> (String, Option<&'static str>),
    ) -> Option<Arc<str>> {
        if let Some(hit) = memo.read().unwrap_or_else(PoisonError::into_inner).get(key) {
            return hit.clone();
        }
        let (id, attr) = address();
        let text = self.static_text(&id, attr);
        // Two threads may both compute; the first insert wins and both return the same text.
        memo.write().unwrap_or_else(PoisonError::into_inner).entry(key.into()).or_insert(text).clone()
    }

    fn arg_text(&self, owner: &str, path: &[&str], description: bool) -> Option<Arc<str>> {
        let matches =
            |e: &ArgEntry| e.description == description && e.path.iter().map(|k| &**k).eq(path.iter().copied());
        if let Some(entries) = self.cache.args.read().unwrap_or_else(PoisonError::into_inner).get(owner)
            && let Some(hit) = entries.iter().find(|e| matches(e))
        {
            return hit.text.clone();
        }
        let text = self.static_text(&ids::arg(owner, path), description.then_some("description"));
        let mut args = self.cache.args.write().unwrap_or_else(PoisonError::into_inner);
        let entries = args.entry(owner.into()).or_default();
        if let Some(found) = entries.iter().find(|e| matches(e)) {
            return found.text.clone();
        }
        entries.push(ArgEntry {
            path: path.iter().map(|k| Box::<str>::from(*k)).collect(),
            description,
            text: text.clone(),
        });
        text
    }
}
