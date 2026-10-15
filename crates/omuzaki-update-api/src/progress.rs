//! Download progress.

/// Receives progress from `Updater::download` and can cancel it. Not `omuzaki_ai_api::Progress`, which reports stages.
pub trait Progress: Send + Sync {
    /// `done` of `total` bytes so far.
    fn report(&self, done: u64, total: u64);
    /// Whether the person cancelled. Checked between chunks and between files.
    fn cancelled(&self) -> bool;
}
