//! The application record (design D5; `receipt` per `docs/design/marketplace.md`, section 5.1): the pin, what was bound and written, and the item's rights.
//!
//! After an apply the document holds every value the recipe wrote. The record beside it, at
//! `characters/<id>/applied/<app>`, is the lock: `library.lock` reads these records, and `library.update` uses
//! `wrote` as the base of its three-way merge.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, Value as Node};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ItemId, ItemRights, Version};

/// The record format version.
pub const RECORD_SCHEMA: u32 = 1;

/// The sibling key of a character node that holds its application records.
pub const APPLIED_KEY: &str = "applied";

/// `characters/<id>/applied/<app>`: where the record of application `app` lives.
pub fn record_path(character: &CharId, app: &str) -> DocPath {
    paths::character(character).join(APPLIED_KEY).join(app)
}

/// Where the applied item was resolved from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// Compiled into the build.
    FirstParty,
    /// The person's own library.
    Mine,
    /// A file the person installed from.
    File,
    /// A team library (LB-07), by team id.
    Team(String),
}

impl Resolved {
    fn as_text(&self) -> String {
        match self {
            Self::FirstParty => "first-party".to_owned(),
            Self::Mine => "mine".to_owned(),
            Self::File => "file".to_owned(),
            Self::Team(id) => format!("team:{id}"),
        }
    }
}

impl Serialize for Resolved {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_text())
    }
}

impl<'de> Deserialize<'de> for Resolved {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        match text.as_str() {
            "first-party" => Ok(Self::FirstParty),
            "mine" => Ok(Self::Mine),
            "file" => Ok(Self::File),
            other => match other.strip_prefix("team:") {
                Some(id) if !id.is_empty() => Ok(Self::Team(id.to_owned())),
                _ => Err(serde::de::Error::custom(format!("{other:?} is not first-party, mine, file or team:<id>"))),
            },
        }
    }
}

/// What the apply bound the placeholders to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBindings {
    /// The final id of each part it created, by `creates` key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, String>,
    /// The existing part it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The member name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A nested item of a bundle, pinned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dep {
    /// The version applied.
    pub version: Version,
    /// Its manifest hash.
    pub hash: ContentHash,
}

/// Why a record node couldn't be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// Not a record: a missing or unknown key, or a wrong type.
    #[error("not a valid application record: {0}")]
    Shape(String),
    /// A record of a schema this build doesn't know.
    #[error("application record schema {found} is not {RECORD_SCHEMA}")]
    Schema {
        /// The schema found.
        found: u32,
    },
}

/// The record of one application of an item to a target. Build it with [`ApplicationRecord::new`] and set the optional
/// fields on the result: the struct is `#[non_exhaustive]` (ADR 0054), so a new field never breaks a caller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ApplicationRecord {
    /// [`RECORD_SCHEMA`].
    pub schema: u32,
    /// The pin: which item...
    pub item: ItemId,
    /// ...which version...
    pub version: Version,
    /// ...and exactly which manifest.
    pub hash: ContentHash,
    /// The publisher key's fingerprint, when the item had a verified publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Where it was resolved from.
    pub from: Resolved,
    /// The slot it holds, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// What the placeholders were bound to.
    #[serde(default)]
    pub bindings: RecordBindings,
    /// The parameter values used, defaults filled in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
    /// For bundles: each nested item, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deps: BTreeMap<String, Dep>,
    /// The item's rights as a source row.
    pub rights: ItemRights,
    /// The hash of the signed receipt for the item, when it came from the marketplace. Only the hash is kept here; the
    /// receipt itself is stored with the installed item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ContentHash>,
    /// The paths it created, relative to the target node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<String>,
    /// Every value it wrote, by path relative to the target node (created subtrees whole), excluding source rows
    /// and the record itself. The base of the three-way update.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wrote: BTreeMap<String, Value>,
}

impl ApplicationRecord {
    /// A record of `item` at `version` with manifest `hash`, resolved `from`, holding `rights`; everything else empty.
    pub fn new(item: ItemId, version: Version, hash: ContentHash, from: Resolved, rights: ItemRights) -> Self {
        Self {
            schema: RECORD_SCHEMA,
            item,
            version,
            hash,
            publisher: None,
            from,
            slot: None,
            bindings: RecordBindings::default(),
            params: BTreeMap::new(),
            deps: BTreeMap::new(),
            rights,
            receipt: None,
            created: Vec::new(),
            wrote: BTreeMap::new(),
        }
    }

    /// Reads a record from a document node. Unknown keys are refused, so a record from a newer build is not
    /// half-understood.
    ///
    /// # Errors
    /// [`RecordError`].
    pub fn from_node(node: &Node) -> Result<Self, RecordError> {
        let record: Self = serde_json::from_value(node.clone()).map_err(|e| RecordError::Shape(e.to_string()))?;
        if record.schema == RECORD_SCHEMA { Ok(record) } else { Err(RecordError::Schema { found: record.schema }) }
    }

    /// The document node to store at [`record_path`].
    ///
    /// # Errors
    /// [`RecordError::Shape`] if the record can't be written as a node (it holds only strings, numbers and
    /// tables, so this is a defensive path).
    pub fn to_node(&self) -> Result<Node, RecordError> {
        serde_json::to_value(self).map_err(|e| RecordError::Shape(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_doc_api::{DocRead, DocReadExt, DocWrite, Patch};
    use omuzaki_doc_mock::MemDoc;
    use serde_json::json;

    fn record() -> ApplicationRecord {
        ApplicationRecord {
            schema: RECORD_SCHEMA,
            item: ItemId::new("omuzaki/outfits/school-uniform").unwrap(),
            version: Version::new(1, 0, 0),
            hash: ContentHash::of(b"manifest"),
            publisher: None,
            from: Resolved::FirstParty,
            slot: Some("outfit".into()),
            bindings: RecordBindings {
                parts: [("skirt".to_owned(), "skirt".to_owned())].into(),
                target: None,
                name: None,
            },
            params: [("length".to_owned(), json!(0.8))].into(),
            deps: [(
                "omuzaki/garments/pleated-skirt".to_owned(),
                Dep { version: Version::new(1, 2, 0), hash: ContentHash::of(b"skirt") },
            )]
            .into(),
            rights: ItemRights::first_party(),
            receipt: None,
            created: vec!["parts/skirt".into(), "sources/skirt".into()],
            wrote: [
                ("parts/skirt/params/length".to_owned(), json!(0.8)),
                ("palette/trim".to_owned(), json!("#2a2346")),
            ]
            .into(),
        }
    }

    #[test]
    fn a_record_round_trips_through_a_document_node() {
        let r = record();
        let c = CharId::new("mia").unwrap();
        let mut doc = MemDoc::from_value(json!({"characters": {"mia": {}}}));
        let path = record_path(&c, "outfit");
        assert_eq!(path.to_string(), "characters/mia/applied/outfit");
        let applied = path.split_last().unwrap().0;
        doc.apply(&Patch::new().with(applied, None, Some(json!({})))).unwrap();
        doc.apply(&Patch::new().with(path.clone(), None, Some(r.to_node().unwrap()))).unwrap();
        assert_eq!(ApplicationRecord::from_node(doc.get(&path).unwrap()).unwrap(), r);
        assert_eq!(doc.get_as::<ApplicationRecord>(&path).unwrap(), r);
    }

    #[test]
    fn a_populated_receipt_survives_the_node_round_trip() {
        let r = ApplicationRecord { receipt: Some(ContentHash::of(b"receipt")), ..record() };
        let node = r.to_node().unwrap();
        assert_eq!(node["receipt"], json!(ContentHash::of(b"receipt").to_string()));
        assert_eq!(ApplicationRecord::from_node(&node).unwrap(), r);
        assert_eq!(ApplicationRecord::from_node(&node).unwrap().receipt, Some(ContentHash::of(b"receipt")));
        let mut extra = node;
        extra["receipt_typo"] = json!(1);
        assert!(
            matches!(ApplicationRecord::from_node(&extra), Err(RecordError::Shape(m)) if m.contains("receipt_typo"))
        );
        let mut malformed = r.to_node().unwrap();
        malformed["receipt"] = json!("not a hash");
        assert!(ApplicationRecord::from_node(&malformed).is_err());
    }

    #[test]
    fn an_absent_receipt_is_omitted_and_legacy_records_without_it_still_read() {
        let node = record().to_node().unwrap();
        assert!(node.get("receipt").is_none(), "an absent receipt is not serialised: {node}");
        // A record written before the field existed is exactly a node without the key.
        let legacy = ApplicationRecord::from_node(&node).unwrap();
        assert_eq!(legacy.receipt, None);
        assert_eq!(legacy, record());
    }

    #[test]
    fn unknown_keys_and_other_schemas_are_refused() {
        let mut node = record().to_node().unwrap();
        node["extra"] = json!(1);
        assert!(matches!(ApplicationRecord::from_node(&node), Err(RecordError::Shape(m)) if m.contains("extra")));
        let mut other = record().to_node().unwrap();
        other["schema"] = json!(2);
        assert_eq!(ApplicationRecord::from_node(&other), Err(RecordError::Schema { found: 2 }));
        assert!(ApplicationRecord::from_node(&json!({"schema": 1})).is_err());
    }

    #[test]
    fn where_it_was_resolved_from_is_a_readable_string() {
        for (from, text) in [
            (Resolved::FirstParty, "first-party"),
            (Resolved::Mine, "mine"),
            (Resolved::File, "file"),
            (Resolved::Team("t1".into()), "team:t1"),
        ] {
            assert_eq!(serde_json::to_value(&from).unwrap(), json!(text));
            assert_eq!(serde_json::from_value::<Resolved>(json!(text)).unwrap(), from);
        }
        assert!(serde_json::from_value::<Resolved>(json!("team:")).is_err());
        assert!(serde_json::from_value::<Resolved>(json!("cloud")).is_err());
    }
}
