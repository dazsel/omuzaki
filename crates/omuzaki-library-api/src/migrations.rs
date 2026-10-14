//! Append-only migrations of the manifest format (design D16).
//!
//! `schema` is checked when a manifest loads. An older schema is migrated in memory by the steps here, one per
//! schema step, never edited once shipped (the project store does the same). A newer schema makes the item
//! unavailable ("made by a newer Omuzaki"). Schema 1 is the first, so [`STEPS`] is empty.
//!
//! Hashes are defined on the current form; the ADR that adds the first migration must say whether pins taken on
//! an older schema keep their hash, because that decides if the migration may rewrite values.

use serde_json::Value;

/// The manifest schema this build reads and writes.
pub const CURRENT_SCHEMA: u32 = 1;

/// One migration: rewrites a manifest of schema `from` into schema `from + 1`.
pub struct Migration {
    /// The schema it reads.
    pub from: u32,
    /// The rewrite, on the manifest as JSON.
    pub apply: fn(&mut Value),
}

/// Every migration, ordered by `from`. Append only.
pub const STEPS: &[Migration] = &[];

/// Why a manifest's schema can't be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// Written by a newer Omuzaki.
    #[error("made by a newer Omuzaki (schema {found}; this build reads up to {CURRENT_SCHEMA})")]
    Newer {
        /// The schema found.
        found: u32,
    },
    /// Older than any migration reaches (schema 0 never existed).
    #[error("schema {found} is not a schema this build can migrate (the first is 1)")]
    Unknown {
        /// The schema found.
        found: u32,
    },
}

/// Migrates `manifest` from schema `from` to [`CURRENT_SCHEMA`], in place.
///
/// # Errors
/// [`SchemaError::Newer`] above the current schema; [`SchemaError::Unknown`] below 1 or with a gap in [`STEPS`].
pub fn to_current(manifest: &mut Value, from: u32) -> Result<(), SchemaError> {
    if from > CURRENT_SCHEMA {
        return Err(SchemaError::Newer { found: from });
    }
    if from == 0 {
        return Err(SchemaError::Unknown { found: from });
    }
    for schema in from..CURRENT_SCHEMA {
        let step = STEPS.iter().find(|m| m.from == schema).ok_or(SchemaError::Unknown { found: schema })?;
        (step.apply)(manifest);
    }
    if let Value::Object(map) = manifest {
        map.insert("schema".to_owned(), Value::from(CURRENT_SCHEMA));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_current_schema_passes_through_untouched() {
        let mut m = json!({"schema": 1, "id": "a/b"});
        to_current(&mut m, 1).unwrap();
        assert_eq!(m, json!({"schema": 1, "id": "a/b"}));
        assert!(STEPS.is_empty(), "schema 1 is the first");
    }

    #[test]
    fn newer_and_zero_schemas_are_refused() {
        let mut m = json!({});
        assert_eq!(to_current(&mut m, 2), Err(SchemaError::Newer { found: 2 }));
        assert!(to_current(&mut m, 2).unwrap_err().to_string().contains("newer Omuzaki"));
        assert_eq!(to_current(&mut m, 0), Err(SchemaError::Unknown { found: 0 }));
    }
}
