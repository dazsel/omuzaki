//! The missing-text report (design 9.2): each fallback and each formatting error, once per (locale, id).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use crate::Locale;

/// One piece of text the catalog could not give in the reader's language, or could not format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The catalog's locale.
    pub locale: Locale,
    /// The message id; `id.attribute` for an attribute (`op-face-set_profile.description`).
    pub id: String,
    /// What went wrong.
    pub reason: MissingReason,
}

/// Why a message is in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingReason {
    /// The locale has no translation, so English was shown. Never raised for a regional override's absent entries
    /// that its parent has, nor for English itself.
    Untranslated,
    /// Not even English knows the id: an old log entry or a record from a newer build. The English stored with the
    /// record, or the last-resort `⟦id: …⟧` form, was shown.
    UnknownId,
    /// Fluent reported an error (an unknown argument, a duplicate message, a syntax error); the text says which.
    /// Whatever Fluent produced was still shown.
    Format(String),
}

impl MissingReason {
    /// Orders the report and keys the once-only rule: one entry per id and kind of problem.
    fn rank(&self) -> u8 {
        match self {
            Self::Untranslated => 0,
            Self::UnknownId => 1,
            Self::Format(_) => 2,
        }
    }
}

/// The collected report of one catalog.
pub(super) struct Report {
    locale: Locale,
    seen: Mutex<BTreeMap<(String, u8), Missing>>,
}

impl Report {
    pub(super) fn new(locale: Locale) -> Self {
        Self { locale, seen: Mutex::new(BTreeMap::new()) }
    }

    /// Records a problem unless this id already has one of the same kind. Debug builds log the first time.
    pub(super) fn record(&self, id: &str, reason: MissingReason) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (id.to_owned(), reason.rank());
        if seen.contains_key(&key) {
            return;
        }
        if cfg!(debug_assertions) {
            tracing::debug!(locale = %self.locale, id, ?reason, "l10n: text fell back");
        }
        seen.insert(key, Missing { locale: self.locale.clone(), id: id.to_owned(), reason });
    }

    /// Everything recorded so far, ordered by id.
    pub(super) fn all(&self) -> Vec<Missing> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).values().cloned().collect()
    }
}
