//! The registry as ops, the UI and the app's wiring see it: an ops service (`ctx.service::<dyn ModelRegistry>()`),
//! implemented once in `omuzaki-ai`.
//!
//! **What an agent can see.** Everything in [`ModelEntry`] and [`SlotStatus`] (ids, versions, digests, slots,
//! licences, sizes, statuses, evaluation results) is plain data with a JSON schema, for `model.list`. No local path
//! appears in either: a user model's original location is the person's business (ADR 0013). Agents never hold a
//! [`Session`]; they may *request* a switch, and [`ModelRegistry::select`] refuses one to a model the person hasn't
//! acknowledged ([`RegistryError::NeedsPerson`], ADR 0016 rule 5).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlockSink, BlockSource, CatalogueEntry, EvalError, EvalReport, FetchError, ImportError, Licence, LoadError,
    ManifestError, Mismatch, ModelManifest, ModelRef, Origin, Progress, RuntimeId, Session, SlotInfo, SlotRef,
    StreamSpec, SyncReport,
};

/// The words shown, unchanged, when a person adds a model (ADR 0016 rule 2). One text for every surface.
pub const DISCLAIMER: &str = "This is your model. Its licence and terms are between you and its author.";

/// Bumped whenever [`DISCLAIMER`] changes, so an acknowledgement records which words were accepted.
pub const DISCLAIMER_VERSION: u32 = 2;

/// Where a model stands. When several apply, the first in this order wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelStatus {
    /// A catalogue version withdrawn by a later catalogue epoch: quarantined, never selectable or installable.
    Revoked,
    /// It can't be used, and the reason says why and what would fix it: a failed check or smoke test, a crash or
    /// stall, a file that changed, an app too old for it.
    Disabled {
        /// Why, in words.
        reason: String,
    },
    /// A shipped model whose files aren't downloaded yet ([`ModelRegistry::install`]).
    NotInstalled,
    /// A user model that passed its checks and waits for the person's acknowledgement.
    NeedsAcknowledgement,
    /// It reproduces a real person ([`ModelCard::imitates`](crate::ModelCard::imitates)) and that person's consent
    /// record is missing (SE-05).
    NeedsConsent,
    /// It can be selected and run.
    Ready,
}

/// One model as `model.list` shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelEntry {
    /// The model, exactly.
    pub model: ModelRef,
    /// The name people see.
    pub name: String,
    /// Its slot.
    pub slot: SlotRef,
    /// The runtime that loads it.
    pub runtime: RuntimeId,
    /// Shipped or added by the person.
    pub origin: Origin,
    /// Its licence as declared (for a user model, by its author through the person; unknown if nothing was).
    pub licence: Licence,
    /// Its download or disk size in bytes.
    pub size: u64,
    /// Where it stands.
    pub status: ModelStatus,
    /// True if its slot runs it now.
    pub active: bool,
    /// Its latest evaluation (the smoke test for user models and installed upgrades).
    pub eval: Option<EvalReport>,
    /// The real person it imitates, from its card ([`ModelCard::imitates`](crate::ModelCard::imitates)): every
    /// surface that lists it shows the impersonation warning (SE-05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imitates: Option<String>,
}

impl ModelEntry {
    /// What a recording, a voice or an export made with this model carries in its licence record.
    pub fn made_with(&self) -> MadeWith {
        MadeWith { model: self.model.clone(), origin: self.origin, licence: self.licence.clone() }
    }
}

/// "Made with model X": stored in the licence record of whatever a model produced (ADR 0016 rule 3). Projects and
/// exports name the model and its digest, never its weights.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MadeWith {
    /// The model, exactly.
    pub model: ModelRef,
    /// Shipped or the person's.
    pub origin: Origin,
    /// Its declared licence: export warns when a user model's is non-commercial or unknown.
    pub licence: Licence,
}

/// One slot as the Models page and `model.list` show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SlotStatus {
    /// The slot.
    pub info: SlotInfo,
    /// The person's choice, if any; `None` means the shipped default.
    pub selected: Option<ModelRef>,
    /// What runs now, which differs from the choice after a fallback; `None` when nothing can run.
    pub active: Option<ModelRef>,
    /// Why the slot isn't running what was chosen (a crash, a stall, missing files), for the health meter.
    pub fallback: Option<String>,
}

/// Who is asking for a switch. Only a person may switch to a user model they haven't acknowledged yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Requester {
    /// A person at the UI or CLI.
    Person,
    /// An agent, a plugin or an automation.
    Agent,
}

/// A request to add a user model. The paths come from files the person picked (ADR 0013); the registry copies the
/// files into its cache, so later edits or moves of the originals change nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct AddModel {
    /// The slot it's meant for.
    pub slot: SlotRef,
    /// Its files, main file first.
    pub files: Vec<PathBuf>,
    /// Its manifest, if it came with one; otherwise the registry writes one (licence unknown).
    pub manifest: Option<ModelManifest>,
    /// The person who read [`DISCLAIMER`] and accepted it while adding, if a person is adding.
    pub acknowledged_by: Option<String>,
}

/// Both ends of a stream from [`ModelRegistry::open_stream`].
pub struct Stream {
    /// The model's block format; the caller converts to it.
    pub spec: StreamSpec,
    /// For the thread that produces blocks.
    pub sink: Box<dyn BlockSink>,
    /// For the thread that needs the results.
    pub source: Box<dyn BlockSource>,
}

/// Why the registry refused or failed. Messages say what to do.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// No slot is registered under that id and version.
    #[error("there is no slot {0}")]
    UnknownSlot(SlotRef),
    /// No model matches.
    #[error("there is no model {0}")]
    UnknownModel(String),
    /// The model is for another slot (`model.list` says which).
    #[error("{model} isn't a {wanted} model")]
    WrongSlot {
        /// The model.
        model: ModelRef,
        /// The slot asked for.
        wanted: SlotRef,
    },
    /// An agent asked to switch to a user model the person hasn't acknowledged.
    #[error("{0} needs the person to accept its licence notice first; ask them to select it")]
    NeedsPerson(ModelRef),
    /// The model can't be used now (see its [`ModelStatus`]).
    #[error("{model} can't be used: {reason}")]
    NotUsable {
        /// The model.
        model: ModelRef,
        /// Why.
        reason: String,
    },
    /// Nothing can run in this slot: no selection works and no shipped model is installed (or none exists, as for
    /// voice conversion). The slot's `without_model` behaviour applies.
    #[error("no model can run {slot}: {without_model}")]
    NoModel {
        /// The slot.
        slot: SlotRef,
        /// What the app does instead.
        without_model: String,
    },
    /// The slot is request/response and a stream was asked for, or the other way round.
    #[error("{0} isn't used that way (request/response against streaming)")]
    WrongMode(SlotRef),
    /// The manifest is refused.
    #[error("the manifest is refused: {}", list(.0))]
    Manifest(Vec<ManifestError>),
    /// The model doesn't fit the slot's interface.
    #[error("the model doesn't fit the slot: {}", list(.0))]
    Interface(Vec<Mismatch>),
    /// The runtime could not probe or load it.
    #[error("{0}")]
    Load(#[from] LoadError),
    /// A download failed.
    #[error("{0}")]
    Fetch(#[from] FetchError),
    /// The smoke test could not run.
    #[error("the smoke test could not run: {0}")]
    Eval(#[from] EvalError),
    /// The smoke test ran and a quality floor wasn't met; the model stays disabled.
    #[error("the model ran but didn't meet the quality floor")]
    BelowFloor(EvalReport),
    /// There is no earlier installed version to go back to.
    #[error("{0} has no earlier version to go back to")]
    NoRollback(SlotRef),
    /// The registry's own files could not be read or written.
    #[error("the model registry's files: {0}")]
    Io(String),
    /// The caller cancelled.
    #[error("cancelled")]
    Cancelled,
}

fn list<T: std::fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// The model registry. Cold: every method may take a lock, touch disk or (for `install` and `add`) run for minutes,
/// so they run on jobs and loader threads, never in a real-time loop. The sessions and streams it opens are the hot
/// part.
pub trait ModelRegistry: Send + Sync {
    /// Every registered slot, with what is chosen and what runs.
    fn slots(&self) -> Vec<SlotStatus>;

    /// Every known model, shipped and added, optionally for one slot.
    fn models(&self, slot: Option<&SlotRef>) -> Vec<ModelEntry>;

    /// The manifest of a known model, for what a slot's owner reads from it (a TTS model's `[[voices]]`). `None` for
    /// an unknown model, or a registry that doesn't keep manifests.
    fn manifest(&self, _model: &ModelRef) -> Option<ModelManifest> {
        None
    }

    /// A session for a request/response slot that follows it: it runs `model` if given (a character's own voice),
    /// otherwise the slot's choice, then its shipped default; a new choice is swapped in between runs; a crash or
    /// stall falls back down the same chain and is reported on the health meter. Shipped models run in this process,
    /// user models in a sandboxed worker.
    ///
    /// # Errors
    /// [`RegistryError::NoModel`] when nothing in the chain can run, among others.
    fn open(&self, slot: &SlotRef, model: Option<&ModelRef>) -> Result<Box<dyn Session>, RegistryError>;

    /// A stream for a streaming slot, with the same choice and fallback chain as [`open`](Self::open); a slot with
    /// no fallback model leaves every block [`Missed`](crate::Block::Missed) once the model stalls, so the caller's
    /// own fallback (the dry voice) plays.
    ///
    /// # Errors
    /// [`RegistryError::WrongMode`] for a request/response slot, among others.
    fn open_stream(&self, slot: &SlotRef, model: Option<&ModelRef>) -> Result<Stream, RegistryError>;

    /// Downloads and verifies a shipped model, then runs its slot's smoke test. A newer version becomes the default
    /// only once this passes; the version it replaces stays installed as the rollback target.
    ///
    /// # Errors
    /// [`RegistryError::Fetch`], [`RegistryError::BelowFloor`], among others. Nothing that failed replaces anything.
    fn install(&self, model: &ModelRef, progress: &dyn Progress) -> Result<(), RegistryError>;

    /// Goes back to the previous installed version of the slot's shipped default, and keeps it until a newer one
    /// is installed.
    ///
    /// # Errors
    /// [`RegistryError::NoRollback`] when there is none.
    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError>;

    /// Adds a user model (ADR 0016 rule 4): copies and hashes its files, writes a manifest if it has none, probes it
    /// in the sandboxed worker (refusing custom ops and native code), checks it against the slot's signature and
    /// stream mode, and runs the slot's smoke test. The entry it returns is `Ready`, `NeedsAcknowledgement` or
    /// `NeedsConsent`; a model that fails stays listed as `Disabled` with the reason.
    ///
    /// # Errors
    /// [`RegistryError`] for what stopped the checks from running at all.
    fn add(&self, request: AddModel, progress: &dyn Progress) -> Result<ModelEntry, RegistryError>;

    /// Records that `person` read [`DISCLAIMER`] for this model, once per digest. Only surfaces where a person acts
    /// call it; the op behind it refuses agents.
    ///
    /// # Errors
    /// [`RegistryError::UnknownModel`], [`RegistryError::Io`].
    fn acknowledge(&self, model: &ModelRef, person: &str) -> Result<(), RegistryError>;

    /// Chooses the slot's model; `None` goes back to the shipped default (always one call away).
    ///
    /// # Errors
    /// [`RegistryError::NeedsPerson`] when an agent picks an unacknowledged user model;
    /// [`RegistryError::NotUsable`] for a disabled one or one that needs consent.
    fn select(&self, slot: &SlotRef, model: Option<&ModelRef>, by: Requester) -> Result<(), RegistryError>;

    /// Removes a model's files: a user model is forgotten (its originals stay where the person keeps them); a
    /// shipped one is uninstalled and can be installed again. A slot running it falls back.
    ///
    /// # Errors
    /// [`RegistryError::UnknownModel`], [`RegistryError::Io`].
    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError>;

    /// How far the download of the install running for `slot` has got, from 0 to 1, if one is downloading: what
    /// `model.status` reports while a first run fetches the slot's model. The default says nothing is downloading.
    fn downloading(&self, slot: &SlotRef) -> Option<f32> {
        let _ = slot;
        None
    }

    /// Takes one verified model catalogue (`docs/design/updates.md` section 8), all or nothing: new versions are
    /// imported as [`Origin::Catalogue`] (installable like shipped ones, the version they supersede kept as the
    /// rollback target), re-listed entries are confirmed, and imports of an older `epoch` that the catalogue no longer
    /// lists are [`Revoked`](ModelStatus::Revoked). Only `omuzaki-update` calls it, after checking the signature, each
    /// entry's `manifest_sha256` and `min_app_version`; the replay decision is its too. `sequence` is for the log.
    ///
    /// The default refuses, so registries that take no catalogue (and every existing implementation) are unchanged.
    ///
    /// # Errors
    /// [`ImportError`]; nothing changed when it is returned.
    fn sync_catalogue(&self, epoch: u32, sequence: u64, entries: &[CatalogueEntry]) -> Result<SyncReport, ImportError> {
        let _ = (epoch, sequence, entries);
        Err(ImportError::Io("this model registry doesn't take a catalogue".into()))
    }
}
