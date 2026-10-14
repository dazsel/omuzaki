//! A stand-in registry for callers of `sync_catalogue` (the updater): it records each catalogue it is given and
//! answers with a scripted result. Everything else the registry does is empty or refused.

use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_ai_api::{
    AddModel, CatalogueEntry, ImportError, ModelEntry, ModelRef, ModelRegistry, Progress, RegistryError, Requester,
    Session, SlotRef, SlotStatus, Stream, SyncReport,
};

/// One call to [`ModelRegistry::sync_catalogue`].
#[derive(Clone, Debug, PartialEq)]
pub struct SyncCall {
    /// The catalogue's root epoch.
    pub epoch: u32,
    /// The catalogue's sequence.
    pub sequence: u64,
    /// The entries, as given.
    pub entries: Vec<CatalogueEntry>,
}

/// Records catalogues and replies with what the test set up (an empty report by default).
#[derive(Default)]
pub struct CatalogueRecorder {
    calls: Mutex<Vec<SyncCall>>,
    reply: Mutex<Option<Result<SyncReport, ImportError>>>,
}

impl CatalogueRecorder {
    /// Replies with an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later sync return `reply`.
    pub fn reply_with(&self, reply: Result<SyncReport, ImportError>) {
        *lock(&self.reply) = Some(reply);
    }

    /// Every sync so far, oldest first.
    pub fn calls(&self) -> Vec<SyncCall> {
        lock(&self.calls).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn refused() -> RegistryError {
    RegistryError::Io("CatalogueRecorder only records catalogues".into())
}

impl ModelRegistry for CatalogueRecorder {
    fn slots(&self) -> Vec<SlotStatus> {
        Vec::new()
    }
    fn models(&self, _: Option<&SlotRef>) -> Vec<ModelEntry> {
        Vec::new()
    }
    fn open(&self, _: &SlotRef, _: Option<&ModelRef>) -> Result<Box<dyn Session>, RegistryError> {
        Err(refused())
    }
    fn open_stream(&self, _: &SlotRef, _: Option<&ModelRef>) -> Result<Stream, RegistryError> {
        Err(refused())
    }
    fn install(&self, _: &ModelRef, _: &dyn Progress) -> Result<(), RegistryError> {
        Err(refused())
    }
    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        Err(RegistryError::NoRollback(slot.clone()))
    }
    fn add(&self, _: AddModel, _: &dyn Progress) -> Result<ModelEntry, RegistryError> {
        Err(refused())
    }
    fn acknowledge(&self, model: &ModelRef, _: &str) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }
    fn select(&self, _: &SlotRef, _: Option<&ModelRef>, _: Requester) -> Result<(), RegistryError> {
        Err(refused())
    }
    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }

    fn sync_catalogue(&self, epoch: u32, sequence: u64, entries: &[CatalogueEntry]) -> Result<SyncReport, ImportError> {
        lock(&self.calls).push(SyncCall { epoch, sequence, entries: entries.to_vec() });
        lock(&self.reply).clone().unwrap_or_else(|| Ok(SyncReport::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_records_each_catalogue_and_replies_as_scripted() {
        let recorder = CatalogueRecorder::new();
        assert_eq!(recorder.sync_catalogue(1, 5, &[]), Ok(SyncReport::default()));
        recorder.reply_with(Err(ImportError::Io("disk".into())));
        assert_eq!(recorder.sync_catalogue(2, 6, &[]), Err(ImportError::Io("disk".into())));
        let calls = recorder.calls();
        assert_eq!(calls.iter().map(|c| (c.epoch, c.sequence)).collect::<Vec<_>>(), vec![(1, 5), (2, 6)]);
    }
}
