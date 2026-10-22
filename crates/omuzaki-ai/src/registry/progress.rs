//! Download progress for installs: one fraction for a model of several files, and what `downloading` reports per
//! slot while an install fetches.

use std::sync::PoisonError;

use omuzaki_ai_api::{Progress, SlotRef};

use super::Registry;

use crate::fetch::STAGE as DOWNLOAD;

/// One file's share of a model's download: its `download` fractions become the whole model's, weighting each file by
/// its size.
pub(super) struct FileShare<'a> {
    pub inner: &'a dyn Progress,
    /// Bytes of the files before this one.
    pub before: u64,
    pub size: u64,
    pub total: u64,
}

impl Progress for FileShare<'_> {
    fn stage(&self, name: &str, fraction: f32) {
        if name != DOWNLOAD || self.total == 0 {
            return self.inner.stage(name, fraction);
        }
        #[allow(clippy::cast_precision_loss)] // a fraction for people to read; f64 keeps it exact enough at any size
        let whole = (self.before as f64 + f64::from(fraction) * self.size as f64) / self.total as f64;
        #[allow(clippy::cast_possible_truncation)]
        self.inner.stage(name, whole.clamp(0.0, 1.0) as f32);
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

/// Passes progress on and records the download fraction for `slot`, so [`Registry::download_fraction`] can report it;
/// the record goes when this is dropped, however the install ends.
pub(super) struct SlotDownload<'a> {
    pub inner: &'a dyn Progress,
    pub registry: &'a Registry,
    pub slot: SlotRef,
}

impl Progress for SlotDownload<'_> {
    fn stage(&self, name: &str, fraction: f32) {
        if name == DOWNLOAD {
            let mut downloads = self.registry.downloads.lock().unwrap_or_else(PoisonError::into_inner);
            downloads.retain(|(slot, _)| slot != &self.slot);
            downloads.push((self.slot.clone(), fraction));
        }
        self.inner.stage(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

impl Drop for SlotDownload<'_> {
    fn drop(&mut self) {
        self.registry.downloads.lock().unwrap_or_else(PoisonError::into_inner).retain(|(slot, _)| slot != &self.slot);
    }
}

impl Registry {
    /// The download fraction of the install running for `slot`, if it is downloading.
    pub(super) fn download_fraction(&self, slot: &SlotRef) -> Option<f32> {
        let downloads = self.downloads.lock().unwrap_or_else(PoisonError::into_inner);
        downloads.iter().find(|(s, _)| s == slot).map(|(_, fraction)| *fraction)
    }
}
