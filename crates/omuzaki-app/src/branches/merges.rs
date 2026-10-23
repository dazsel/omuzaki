//! The `merges` record (`docs/design/branches.md` 7.3): which points of other branches the document includes, kept in
//! the tree (`merges.toml`) so that every way of reverting a merge reverts its record too.

use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::{BranchId, Contains, POINT_FORMAT, PointId, Version};
use omuzaki_history_api::state::BranchStore;
use serde_json::json;

/// The record's key at the top of the tree.
pub const MERGES: &str = "merges";

/// What `tree` records as merged in: for each branch, the newest of its points merged. An entry that doesn't read (an
/// unknown shape, an id that isn't a branch id) is skipped, and `own` never counts itself.
pub fn merges_of(tree: &Value, own: &BranchId) -> Contains {
    let Some(entries) = tree.get(MERGES).and_then(Value::as_object) else { return Contains::new() };
    entries.iter().fold(Contains::new(), |merged, (key, entry)| {
        let branch = key.parse::<BranchId>().ok().filter(|b| b != own);
        let seq = entry.get("seq").and_then(Value::as_u64);
        let point = entry.get("point").and_then(Value::as_str).and_then(|p| p.parse::<PointId>().ok());
        match (branch, seq, point) {
            (Some(branch), Some(seq), Some(point)) => merged.with(branch, Version::new(seq, point)),
            _ => merged,
        }
    })
}

/// [`merges_of`], less what this project can't vouch for: a point the store doesn't hold (a project copied from
/// another machine), or one that belongs to another branch or has another number than the record says. A point then
/// never names a version that isn't there.
pub fn verified_merges(tree: &Value, own: &BranchId, store: &dyn BranchStore) -> Contains {
    merges_of(tree, own).iter().fold(Contains::new(), |verified, (branch, version)| match store.point(&version.point) {
        Ok(point) if point.branch == *branch && point.seq == version.seq => verified.with(branch.clone(), *version),
        _ => verified,
    })
}

/// The `merges` entity holding `record`, each branch with its current name (`name_of`), for the tree. `format` keeps
/// it one file: a new object holding only tables would become a folder.
pub fn record_value(record: &Contains, name_of: impl Fn(&BranchId) -> Option<String>) -> Value {
    let mut entity = serde_json::Map::new();
    entity.insert("format".to_owned(), json!(POINT_FORMAT));
    for (branch, version) in record.iter() {
        let mut entry = serde_json::Map::new();
        if let Some(name) = name_of(branch) {
            entry.insert("name".to_owned(), json!(name));
        }
        entry.insert("seq".to_owned(), json!(version.seq));
        entry.insert("point".to_owned(), json!(version.point.to_string()));
        entity.insert(branch.to_string(), Value::Object(entry));
    }
    Value::Object(entity)
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::ContentHash;

    use super::*;

    #[test]
    fn the_record_reads_back_and_never_names_its_own_branch() {
        let side = BranchId::new("9c41d07a5e2b1f30").unwrap_or_default();
        let point = PointId(ContentHash::of(b"p"));
        let record = Contains::new().with(side.clone(), Version::new(4, point));
        let tree = json!({ "merges": record_value(&record, |_| Some("aria/oval".to_owned())) });
        assert_eq!(tree["merges"]["format"], json!(1));
        assert_eq!(tree["merges"][side.as_str()]["name"], json!("aria/oval"));
        assert_eq!(merges_of(&tree, &BranchId::main()), record);
        assert!(merges_of(&tree, &side).is_empty(), "a branch never records itself");
        assert!(merges_of(&json!({"merges": {"format": 1, "nope": {"seq": 1}}}), &BranchId::main()).is_empty());
    }
}
