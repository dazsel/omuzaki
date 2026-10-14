//! Model manifests and model cards: the `model.toml` that says what a model is, which slot it fits, which files it is
//! made of and under what terms; and the registration of the models we ship.
//!
//! The same format serves shipped models (a TOML file next to the code that uses them, registered with
//! [`register_model!`](crate::register_model)) and user models (the file a person adds, or the one the app writes for
//! them, ADR 0016 rule 1).
//! Where the manifest came from decides its [`Origin`], never anything written in it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Metric, ModelId, PinnedFile, Provider, RuntimeId, Sha256, SlotRef, StreamSpec, Version};

/// Where a model came from. Decided by the registry from where it found the manifest.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// We ship it: pinned, licence-registered (`docs/licensing/register.md`), downloaded on demand.
    Shipped,
    /// A person added it from their own files. Its licence is theirs (ADR 0016 rule 2); it runs sandboxed.
    User,
    /// Authenticated by us and delivered after the build, through a signed catalogue (`ModelRegistry::sync_catalogue`).
    /// It follows the shipped rules; only that entry point assigns it, never a manifest.
    Catalogue,
}

/// A licence as its author states it. All three empty means unknown, which is what the app records when a user model
/// declares nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Licence {
    /// An SPDX expression (`MIT`, `Apache-2.0`, `CC-BY-NC-4.0`) or `LicenseRef-<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spdx: Option<String>,
    /// The licence's name or terms in words, when there's no SPDX id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Where the licence is published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Licence {
    /// True when nothing is declared.
    pub fn is_unknown(&self) -> bool {
        self.spdx.is_none() && self.text.is_none() && self.url.is_none()
    }
}

/// What a model was made from, for the model card's lineage. A model is only as clean as its whole chain: weights,
/// training data, base model and the code that trained it (research `2026-10-voice-changers.md`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    /// A dataset it was trained or fine-tuned on.
    TrainingData,
    /// The model it was fine-tuned or distilled from.
    BaseModel,
    /// The code that trained it.
    Code,
    /// A format conversion (`TFLite` to ONNX, a quantisation).
    Conversion,
}

/// One link in a model's lineage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lineage {
    /// What kind of link.
    pub kind: LineageKind,
    /// Its name and version.
    pub name: String,
    /// Its licence (SPDX or words), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// Where it is described.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The model card: where a model comes from, under what terms, and how good it is (spec ML-01).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelCard {
    /// Its licence.
    pub licence: Licence,
    /// For a shipped model, the `Item` of its row in `docs/licensing/register.md`, which the repo licence gate checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub register: Option<String>,
    /// Who made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<String>,
    /// Its home page or upstream model card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// The ONNX opset it needs, for ONNX models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opset: Option<u32>,
    /// The real person whose voice or likeness it reproduces, if any. Such a model can't be selected without that
    /// person's consent record and shows the impersonation warning (SE-05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imitates: Option<String>,
    /// What it was made from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<Lineage>,
    /// Measured quality, each on a named real or synthetic test set (never averaged across the two).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
}

/// One file of a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelFile {
    /// Its name, which runtimes look files up by: letters, digits and `._+-`, not starting with a dot.
    pub name: String,
    /// Its SHA-256.
    pub sha256: Sha256,
    /// Its exact size in bytes.
    pub size: u64,
    /// Where a shipped model's file is downloaded from: `https://` URLs of immutable revisions, tried in order (the
    /// upstream first; a mirror is one more entry). User models have none: we never download them (ADR 0016 rule 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

impl ModelFile {
    /// The download pin, for a file that has URLs.
    pub fn pin(&self) -> Option<PinnedFile> {
        (!self.urls.is_empty()).then(|| PinnedFile::new(self.urls.iter().cloned(), self.sha256, self.size))
    }
}

/// One graph of a multi-graph model (`[[stages]]`): the graphs run in order as one session, so the slot sees one model
/// (the `MediaPipe` face pipeline: detector, landmarks, blendshapes). The `io` table then maps each slot output to
/// `<stage>.<port>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Stage {
    /// Its name, which later stages and `io` use to name its outputs (`detector.boxes`): letters, digits, `_`, `-`.
    pub name: String,
    /// Its graph: one of the manifest's file names.
    pub file: String,
    /// Where each graph input comes from: a slot input by name (`image`), or an earlier stage's output
    /// (`detector.boxes`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
}

impl Stage {
    /// Splits `<stage>.<port>`; `None` for a plain slot port name.
    pub fn split_source(source: &str) -> Option<(&str, &str)> {
        source.split_once('.')
    }
}

/// A model's manifest (`model.toml`).
///
/// Field order is the TOML order: plain keys first, then the tables.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelManifest {
    /// Its id. User models start with `user-`.
    pub id: ModelId,
    /// Its version.
    pub version: Version,
    /// The name people see.
    pub name: String,
    /// The slot and interface version it was made for.
    pub slot: SlotRef,
    /// The runtime that loads it.
    pub runtime: RuntimeId,
    /// The oldest app that can run it (a newer runtime or slot behaviour). The registry disables it on older apps.
    #[serde(default)]
    pub min_app_version: Version,
    /// Providers to try, best first; empty means the runtime's own order. A shipped model always keeps a CPU path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<Provider>,
    /// Its files; the first is the main one.
    pub files: Vec<ModelFile>,
    /// For a model made of several graphs: the graphs in the order they run. Empty for a single graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<Stage>,
    /// Renames from the slot's port names to the model's own, where they differ.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub io: BTreeMap<String, String>,
    /// For streaming slots: the model's rate, block size and own delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamSpec>,
    /// Its card.
    pub card: ModelCard,
}

/// Why a manifest is refused. People see these as-is, so each says what to change.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ManifestError {
    /// It isn't valid TOML for this format.
    #[error("the manifest can't be read: {0}")]
    Parse(String),
    /// It lists no files.
    #[error("a model needs at least one file")]
    NoFiles,
    /// A file name that could reach outside the model's folder or break on some OS.
    #[error("file name {0:?} isn't allowed: use letters, digits and ._+- and don't start with a dot")]
    FileName(String),
    /// Two files share a name.
    #[error("two files are called {0:?}")]
    DuplicateFile(String),
    /// A file of size zero.
    #[error("{0:?} has size 0")]
    EmptyFile(String),
    /// A shipped file without URLs, or with one that isn't `https://`.
    #[error("shipped file {0:?} needs https:// URLs of an immutable revision")]
    NoUrl(String),
    /// A user model with URLs: we never download user models.
    #[error("{0:?} has URLs, but Omuzaki never downloads a model you add: add it from your own files")]
    UserUrl(String),
    /// A stage name that is empty, repeated, or has characters other than letters, digits, `_` and `-`.
    #[error("stage name {0:?} must be unique and use only letters, digits, _ and -")]
    StageName(String),
    /// A stage whose graph isn't one of the files.
    #[error("stage {stage:?} runs {file:?}, which isn't one of the model's files")]
    StageFile {
        /// The stage.
        stage: String,
        /// The file it names.
        file: String,
    },
    /// A stage input that reads from a stage that doesn't run before it.
    #[error("stage {stage:?} reads {reads:?}, but only earlier stages' outputs can be read")]
    StageSource {
        /// The stage.
        stage: String,
        /// What it reads (`<stage>.<port>`). Not called `source`, which `thiserror` reserves for a cause.
        reads: String,
    },
    /// With stages, an `io` entry that doesn't point at `<stage>.<port>` of a known stage.
    #[error("with stages, io maps slot ports to <stage>.<port>; {0:?} doesn't name a stage")]
    StageOutput(String),
    /// A shipped model without its SPDX licence and register row.
    #[error("a shipped model needs an SPDX licence and its row in docs/licensing/register.md")]
    Unregistered,
    /// A shipped model with no CPU path.
    #[error("a shipped model must be able to run on the CPU: add cpu to providers")]
    NoCpu,
    /// A user model whose id doesn't start with `user-`.
    #[error("a model you add has an id starting with user- (got {0})")]
    UserId(String),
    /// A shipped or catalogue model whose id starts with `user-`, which is reserved for models people add.
    #[error("the id {0} is reserved for models people add: pick one that doesn't start with user-")]
    ReservedId(String),
    /// A stream spec out of range.
    #[error("the stream settings are out of range: 8-192 kHz, a block of at most one second, a delay under 10 s")]
    Stream,
    /// An empty name in the `io` table.
    #[error("the io table maps {0:?} to an empty name")]
    Io(String),
}

/// A file name that is safe on every OS and can't climb out of a folder.
fn file_name_ok(name: &str) -> bool {
    let mut bytes = name.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric());
    first_ok && name.len() <= 255 && bytes.all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

impl ModelManifest {
    /// Reads a `model.toml`.
    ///
    /// # Errors
    /// [`ManifestError::Parse`].
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        toml::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// Writes it as `model.toml` (what the app saves next to a user model that came without one).
    ///
    /// # Errors
    /// [`ManifestError::Parse`] if a value can't be written as TOML.
    pub fn to_toml(&self) -> Result<String, ManifestError> {
        toml::to_string_pretty(self).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// The text whose SHA-256 is the model's [`ModelRef::digest`](crate::ModelRef::digest): one line per file,
    /// sorted by name, `<sha256> <size> <name>\n`. It covers every file and nothing else, so renaming the model or
    /// editing its card keeps its identity, and changing any byte of any file changes it.
    pub fn digest_text(&self) -> String {
        let mut files: Vec<&ModelFile> = self.files.iter().collect();
        files.sort_by(|a, b| a.name.cmp(&b.name));
        files.iter().fold(String::new(), |mut text, f| {
            // Writing to a String cannot fail.
            let _ = writeln!(text, "{} {} {}", f.sha256, f.size, f.name);
            text
        })
    }

    /// The total size of its files.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).fold(0, u64::saturating_add)
    }

    /// Checks everything that doesn't need the slot or the files: names, sizes, URLs, the licence record and stream
    /// numbers, with the rules of its origin. The registry adds the checks that do (slot signature, stream mode,
    /// `min_app_version`, hashes).
    ///
    /// # Errors
    /// Every problem found.
    pub fn validate(&self, origin: Origin) -> Result<(), Vec<ManifestError>> {
        let mut out = Vec::new();
        if self.files.is_empty() {
            out.push(ManifestError::NoFiles);
        }
        for (i, f) in self.files.iter().enumerate() {
            if !file_name_ok(&f.name) {
                out.push(ManifestError::FileName(f.name.clone()));
            }
            if self.files[..i].iter().any(|g| g.name == f.name) {
                out.push(ManifestError::DuplicateFile(f.name.clone()));
            }
            if f.size == 0 {
                out.push(ManifestError::EmptyFile(f.name.clone()));
            }
            let urls_ok = match origin {
                Origin::Shipped | Origin::Catalogue => {
                    !f.urls.is_empty() && f.urls.iter().all(|u| u.starts_with("https://"))
                }
                Origin::User => f.urls.is_empty(),
            };
            if !urls_ok {
                let name = f.name.clone();
                out.push(if origin == Origin::User {
                    ManifestError::UserUrl(name)
                } else {
                    ManifestError::NoUrl(name)
                });
            }
        }
        self.validate_stages(&mut out);
        match origin {
            Origin::Shipped | Origin::Catalogue => {
                if origin == Origin::Catalogue && self.id.as_str().starts_with("user-") {
                    out.push(ManifestError::ReservedId(self.id.to_string()));
                }
                if self.card.licence.spdx.is_none() || self.card.register.is_none() {
                    out.push(ManifestError::Unregistered);
                }
                if !self.providers.is_empty() && !self.providers.contains(&Provider::Cpu) {
                    out.push(ManifestError::NoCpu);
                }
            }
            Origin::User if !self.id.as_str().starts_with("user-") => {
                out.push(ManifestError::UserId(self.id.to_string()));
            }
            Origin::User => {}
        }
        if self.stream.is_some_and(|s| !s.is_valid()) {
            out.push(ManifestError::Stream);
        }
        out.extend(
            self.io.iter().filter(|(k, v)| k.is_empty() || v.is_empty()).map(|(k, _)| ManifestError::Io(k.clone())),
        );
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    /// The `[[stages]]` rules: unique safe names, graphs that are files, sources from earlier stages only, and `io`
    /// pointing into stages.
    fn validate_stages(&self, out: &mut Vec<ManifestError>) {
        for (i, stage) in self.stages.iter().enumerate() {
            let earlier = &self.stages[..i];
            let name_ok = !stage.name.is_empty()
                && stage.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !earlier.iter().any(|s| s.name == stage.name);
            if !name_ok {
                out.push(ManifestError::StageName(stage.name.clone()));
            }
            if !self.files.iter().any(|f| f.name == stage.file) {
                out.push(ManifestError::StageFile { stage: stage.name.clone(), file: stage.file.clone() });
            }
            for source in stage.inputs.values() {
                let from_earlier = |(from, _): (&str, &str)| earlier.iter().any(|s| s.name == from);
                if Stage::split_source(source).is_some_and(|split| !from_earlier(split)) {
                    out.push(ManifestError::StageSource { stage: stage.name.clone(), reads: source.clone() });
                }
            }
        }
        if !self.stages.is_empty() {
            let known = |(stage, _): (&str, &str)| self.stages.iter().any(|s| s.name == stage);
            for target in self.io.values().filter(|t| !Stage::split_source(t).is_some_and(known)) {
                out.push(ManifestError::StageOutput(target.clone()));
            }
        }
    }
}

/// A model we ship, registered by the crate that uses it with [`register_model!`](crate::register_model). The manifest
/// is TOML text, normally `include_str!` of a `models/<id>.model.toml` file next to the code, so the repo licence gate
/// can read every shipped manifest without building anything.
pub struct ShippedModel {
    /// The model's id, as written in the registration; the registry checks that the manifest agrees.
    pub id: &'static str,
    /// The manifest, as TOML.
    pub manifest: &'static str,
}

inventory::collect!(ShippedModel);

/// Registers a shipped model: `register_model!("silero-vad", include_str!("../models/silero-vad.model.toml"));`.
/// Several versions of one model may be registered; the newest the app can run is the default and the others are
/// rollback targets.
#[macro_export]
macro_rules! register_model {
    ($id:literal, $manifest:expr) => {
        $crate::__inventory::submit! { $crate::ShippedModel { id: $id, manifest: $manifest } }
        $crate::__record_registration!("model", $id);
    };
}

/// Every registered shipped model, in link order.
pub fn shipped_models() -> impl Iterator<Item = &'static ShippedModel> {
    inventory::iter::<ShippedModel>.into_iter()
}
