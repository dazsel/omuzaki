//! The voice catalog (§3.4): a local model's voices are the `[[voices]]` rows of its `model.toml`, named
//! `<model id>/<voice>`; a hosted connector lists its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_ai_api::{ModelEntry, ModelRegistry, ModelStatus};
use omuzaki_l10n_api::Locale;
use omuzaki_performer_api::ids::{ConnectorId, VoiceId};
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::{TtsPrice, VoiceCatalog, VoiceInfo, VoiceProvider};

use crate::slots::tts_slot;

/// One local voice with where its model stands, for `voice.list`.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalVoice {
    /// The voice.
    pub info: VoiceInfo,
    /// The model's name, as the Models page shows it.
    pub model_name: String,
    /// Whether the model is installed and may run now; otherwise its status says why not.
    pub status: ModelStatus,
}

/// Every `tts@1` model's voices, read from the registry each time (a model added or removed shows at once).
pub struct LocalVoices {
    registry: Arc<dyn ModelRegistry>,
}

impl LocalVoices {
    /// The voices of the models in `registry`.
    pub fn new(registry: Arc<dyn ModelRegistry>) -> Self {
        Self { registry }
    }

    /// Each voice once, with its model's status: the slot's active model first, then the newest version, so a voice
    /// two versions of a model share shows the one that runs.
    pub fn list(&self) -> Vec<LocalVoice> {
        local_voices(self.registry.as_ref())
    }
}

/// [`LocalVoices::list`] over `registry`.
pub fn local_voices(registry: &dyn ModelRegistry) -> Vec<LocalVoice> {
    let mut models: Vec<ModelEntry> = registry.models(Some(&tts_slot()));
    models.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| b.model.version.cmp(&a.model.version)));
    let mut seen = BTreeMap::new();
    for entry in &models {
        let Some(manifest) = registry.manifest(&entry.model) else { continue };
        for row in &manifest.voices {
            let Ok(id) = VoiceId::new(&format!("{}/{}", entry.model.id, row.id)) else { continue };
            if seen.contains_key(&id) {
                continue;
            }
            let mut info =
                VoiceInfo::new(id.clone(), row.languages.iter().filter_map(|l| Locale::new(l).ok()).collect());
            // A model whose card imitates someone makes every one of its voices theirs.
            info.imitates = row.imitates.clone().or_else(|| entry.imitates.clone());
            info.cloned = row.cloned;
            seen.insert(id, LocalVoice { info, model_name: entry.name.clone(), status: entry.status.clone() });
        }
    }
    let mut voices: Vec<LocalVoice> = seen.into_values().collect();
    voices.sort_by(|a, b| a.info.id.cmp(&b.info.id));
    voices
}

impl VoiceCatalog for LocalVoices {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        match provider {
            VoiceProvider::Local => Ok(self.list().into_iter().map(|v| v.info).collect()),
            VoiceProvider::Hosted(connector) => Err(not_connected(connector)),
            _ => Err(VoiceError::Provider("an unknown kind of voice provider".into())),
        }
    }
}

fn not_connected(connector: &ConnectorId) -> VoiceError {
    VoiceError::Provider(format!("{connector} isn't connected: connect it, or pick a local voice"))
}

/// The app's catalog: the local models' voices and each connected hosted provider's.
pub struct Catalog {
    local: LocalVoices,
    hosted: BTreeMap<ConnectorId, Arc<dyn VoiceCatalog>>,
}

impl Catalog {
    /// Local voices only, from `registry`.
    pub fn new(registry: Arc<dyn ModelRegistry>) -> Self {
        Self { local: LocalVoices::new(registry), hosted: BTreeMap::new() }
    }

    /// Adds a connected provider: `catalog` answers for [`VoiceProvider::Hosted`]`(connector)`.
    #[must_use]
    pub fn with_hosted(mut self, connector: ConnectorId, catalog: Arc<dyn VoiceCatalog>) -> Self {
        self.hosted.insert(connector, catalog);
        self
    }
}

impl VoiceCatalog for Catalog {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        match provider {
            VoiceProvider::Hosted(connector) => {
                let catalog = self.hosted.get(connector).ok_or_else(|| not_connected(connector))?;
                // A connector lists only its own voices: anything else it returns is dropped.
                Ok(catalog.voices(provider)?.into_iter().filter(|v| v.offered_by(provider)).collect())
            }
            other => self.local.voices(other),
        }
    }

    fn price(&self, provider: &VoiceProvider) -> Option<TtsPrice> {
        match provider {
            VoiceProvider::Hosted(connector) => self.hosted.get(connector)?.price(provider),
            _ => None,
        }
    }
}
