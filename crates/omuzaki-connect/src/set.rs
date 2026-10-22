//! [`ConnectorSet`]: every connector this binary links, each manifest read, checked against its registration and
//! validated once at startup. A bad manifest is a [`SetError`] then, never a panic or a surprise later.

use omuzaki_connect_api::{
    Capability, Connector, ConnectorId, ConnectorManifest, ConnectorRegistration, ManifestError, registered,
};

/// One connector: its manifest and the adapter.
pub struct Entry {
    /// The validated manifest.
    pub manifest: ConnectorManifest,
    /// The adapter.
    pub connector: Box<dyn Connector>,
}

impl Entry {
    /// The connector's id.
    pub fn id(&self) -> &ConnectorId {
        &self.manifest.id
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry").field("id", &self.manifest.id).finish_non_exhaustive()
    }
}

/// Why a connector could not join the set. Every problem is reported at once.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SetError {
    /// The manifest does not read or breaks rules; each error names its field.
    #[error("the manifest of connector {id} is invalid: {}", list(errors))]
    #[non_exhaustive]
    Manifest {
        /// The id the registration names.
        id: String,
        /// Every problem.
        errors: Vec<ManifestError>,
    },
    /// The registration's id and the manifest's differ.
    #[error("the registration names {literal} but its manifest says {manifest}")]
    #[non_exhaustive]
    IdMismatch {
        /// The id the `register_connector!` line names.
        literal: String,
        /// The id the manifest names.
        manifest: String,
    },
    /// Two connectors use one id.
    #[error("two connectors use the id {0}")]
    Duplicate(String),
}

fn list<T: std::fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// Every connector the host can call, sorted by id.
#[derive(Debug, Default)]
pub struct ConnectorSet {
    entries: Vec<Entry>,
}

impl ConnectorSet {
    /// Everything `register_connector!` put in this binary.
    ///
    /// # Errors
    /// Every [`SetError`] found, together.
    pub fn from_registered() -> Result<Self, Vec<SetError>> {
        Self::from_registrations(registered())
    }

    /// The connectors of `registrations`: each manifest read, its id checked against the registration's, validated,
    /// and the ids checked for duplicates.
    ///
    /// # Errors
    /// Every [`SetError`] found, together.
    pub fn from_registrations<'a>(
        registrations: impl IntoIterator<Item = &'a ConnectorRegistration>,
    ) -> Result<Self, Vec<SetError>> {
        let mut errors = Vec::new();
        let mut entries = Vec::new();
        for r in registrations {
            let manifest = match ConnectorManifest::from_toml(r.manifest) {
                Ok(m) => m,
                Err(e) => {
                    errors.push(SetError::Manifest { id: r.id.to_owned(), errors: vec![e] });
                    continue;
                }
            };
            if manifest.id.as_str() != r.id {
                errors.push(SetError::IdMismatch { literal: r.id.to_owned(), manifest: manifest.id.to_string() });
                continue;
            }
            entries.push((manifest, (r.make)()));
        }
        match Self::from_entries(entries) {
            Ok(set) if errors.is_empty() => Ok(set),
            Ok(_) => Err(errors),
            Err(more) => {
                errors.extend(more);
                Err(errors)
            }
        }
    }

    /// A set built by hand (tests, and Apps that make connectors themselves): each manifest validated, ids unique.
    ///
    /// # Errors
    /// Every [`SetError`] found, together.
    pub fn from_entries(entries: Vec<(ConnectorManifest, Box<dyn Connector>)>) -> Result<Self, Vec<SetError>> {
        let mut errors = Vec::new();
        let mut kept: Vec<Entry> = Vec::with_capacity(entries.len());
        for (manifest, connector) in entries {
            if let Err(problems) = manifest.validate() {
                errors.push(SetError::Manifest { id: manifest.id.to_string(), errors: problems });
                continue;
            }
            kept.push(Entry { manifest, connector });
        }
        kept.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        for pair in kept.windows(2) {
            if pair[0].manifest.id == pair[1].manifest.id {
                let id = pair[0].manifest.id.to_string();
                if !errors.contains(&SetError::Duplicate(id.clone())) {
                    errors.push(SetError::Duplicate(id));
                }
            }
        }
        if errors.is_empty() { Ok(Self { entries: kept }) } else { Err(errors) }
    }

    /// The connector `id`.
    pub fn get(&self, id: &ConnectorId) -> Option<&Entry> {
        self.entries.binary_search_by(|e| e.manifest.id.cmp(id)).ok().map(|i| &self.entries[i])
    }

    /// Every connector, sorted by id.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// The connectors that declare `capability`, sorted by id.
    pub fn offering(&self, capability: Capability) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.manifest.declares(capability).is_some())
    }

    /// Every id, sorted.
    pub fn ids(&self) -> Vec<ConnectorId> {
        self.entries.iter().map(|e| e.manifest.id.clone()).collect()
    }

    /// How many connectors there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
