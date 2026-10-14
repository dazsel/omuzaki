//! A part's source row goes where the part goes: `part.duplicate` copies it, `part.remove` takes it,
//! `part.add` never inherits a stale one, and `character.rename` leaves it where it is.
//!
//! The row at `characters/<id>/sources/<part>` says where a part came from and what its rights are (`licence.set_source`
//! in `omuzaki-io-ops`, which this crate may not depend on). Here a row is an opaque node: the ops must move it verbatim and
//! never read or change it. `licence.derive`'s reading of rows in the shape these ops leave is tested in
//! `omuzaki-io-ops/tests/licence_source.rs`.
//!
//! Each op is planned as the executor plans it, and `common::round_trip` checks the editing contract (a dry run changes
//! nothing, undo restores the document exactly, redo repeats the apply, no `null` is written).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{apply, at, build, mock_node, plan, round_trip};
use omuzaki_char as _;
use omuzaki_doc_mock::MemDoc;
use serde_json::{Value, json};

/// A row as a stand-in: the ops never read it, so only that it comes back byte for byte matters.
fn row(licence: &str) -> Value {
    json!({
        "schema": 1,
        "provenance": { "licence": licence, "commercial": "ok", "private_test": false },
        "terms": { "allowRedistribution": true },
        "owned": { "asserted_by": "Ann" },
    })
}

/// The mock project (body, head, hair, shirt, vest) with a row for each part named in `rows`.
fn project_with_rows(rows: &[(&str, Value)]) -> MemDoc {
    let mut node = mock_node();
    for (part, row) in rows {
        node["sources"][part] = row.clone();
    }
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

fn row_at(doc: &MemDoc, part: &str) -> Option<Value> {
    at(doc, &format!("characters/mock/sources/{part}"))
}

// ------------------------------------------------------------------------------------------------ part.duplicate

#[test]
fn duplicate_copies_the_source_row_verbatim_and_undo_removes_the_copy() {
    let mut doc = project_with_rows(&[("hair", row("CC-BY-4.0")), ("vest", row("CC0-1.0"))]);
    let planned =
        round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    assert_eq!(row_at(&doc, "hair-b"), Some(row("CC-BY-4.0")), "the copy is the same asset under the same licence");
    assert_eq!(row_at(&doc, "hair"), Some(row("CC-BY-4.0")), "the original keeps its row");
    assert_eq!(row_at(&doc, "vest"), Some(row("CC0-1.0")), "no other row is touched");
    assert_eq!(planned.output.unwrap()["source_row_copied"], json!(true));
}

#[test]
fn duplicate_of_a_part_without_a_row_makes_no_row() {
    let mut doc = project_with_rows(&[("vest", row("CC0-1.0"))]);
    let planned =
        round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    assert_eq!(row_at(&doc, "hair-b"), None, "nothing is invented: the copy is as restrictive as its original");
    assert!(planned.output.unwrap().get("source_row_copied").is_none());
    assert_eq!(planned.patch.changes.len(), 1, "only the part");
}

#[test]
fn duplicate_never_keeps_a_leftover_row_under_the_new_id() {
    // A row for `hair-b` from a part that no longer exists (a hand-edited project) must not give the copy its rights.
    let mut doc = project_with_rows(&[("hair-b", row("CC0-1.0"))]);
    round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    assert_eq!(row_at(&doc, "hair-b"), None, "the original has no row, so neither has the copy");

    // And when the original does have one, the copy gets the original's, not the leftover.
    let mut doc = project_with_rows(&[("hair", row("CC-BY-NC-4.0")), ("hair-b", row("CC0-1.0"))]);
    round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    assert_eq!(row_at(&doc, "hair-b"), Some(row("CC-BY-NC-4.0")));
}

#[test]
fn duplicating_twice_gives_each_copy_its_own_row_and_editing_one_leaves_the_others() {
    let mut doc = project_with_rows(&[("hair", row("CC-BY-4.0"))]);
    apply(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    apply(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair-b", "new_id": "hair-c"}));
    for part in ["hair", "hair-b", "hair-c"] {
        assert_eq!(row_at(&doc, part), Some(row("CC-BY-4.0")), "{part}");
    }
    apply(&mut doc, "part.remove", json!({"character": "mock", "part": "hair-b"}));
    assert_eq!((row_at(&doc, "hair-b"), row_at(&doc, "hair-c")), (None, Some(row("CC-BY-4.0"))));
}

// ------------------------------------------------------------------------------------------------ part.remove

#[test]
fn remove_takes_the_source_row_with_the_part_and_undo_brings_it_back() {
    let mut doc = project_with_rows(&[("hair", row("CC-BY-4.0")), ("vest", row("CC0-1.0"))]);
    let planned = round_trip(&mut doc, "part.remove", json!({"character": "mock", "part": "vest"}));
    assert_eq!(row_at(&doc, "vest"), None);
    assert_eq!(row_at(&doc, "hair"), Some(row("CC-BY-4.0")), "another part's row is not touched");
    assert_eq!(planned.output.unwrap()["source_row_removed"], json!(true));
}

#[test]
fn remove_of_a_part_without_a_row_changes_only_the_part() {
    let mut doc = project_with_rows(&[("hair", row("CC-BY-4.0"))]);
    let planned = round_trip(&mut doc, "part.remove", json!({"character": "mock", "part": "vest"}));
    assert_eq!(planned.patch.changes.len(), 1);
    assert!(planned.output.unwrap().get("source_row_removed").is_none());
}

#[test]
fn removing_the_last_row_leaves_an_empty_table_and_adding_the_part_back_starts_without_a_row() {
    let mut doc = project_with_rows(&[("vest", row("CC0-1.0"))]);
    apply(&mut doc, "part.remove", json!({"character": "mock", "part": "vest"}));
    assert_eq!(at(&doc, "characters/mock/sources"), Some(json!({})));
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "vest", "layer": 30}));
    assert_eq!(row_at(&doc, "vest"), None, "a part added back is a new part: it has recorded nothing yet");
}

// ------------------------------------------------------------------------------------------------ part.add

#[test]
fn add_clears_a_row_a_vanished_part_left_under_its_id() {
    let mut doc = project_with_rows(&[("jacket", row("CC0-1.0")), ("hair", row("CC-BY-4.0"))]);
    let planned =
        round_trip(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "jacket"}));
    assert_eq!(row_at(&doc, "jacket"), None, "a new part never inherits the rights of an older one with its name");
    assert_eq!(row_at(&doc, "hair"), Some(row("CC-BY-4.0")), "no other row is touched");
    assert_eq!(planned.output.unwrap()["stale_source_row_removed"], json!(true));
}

#[test]
fn add_with_nothing_stale_touches_no_row() {
    let mut doc = project_with_rows(&[("hair", row("CC-BY-4.0"))]);
    let planned =
        round_trip(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "jacket"}));
    assert_eq!(planned.patch.changes.len(), 1, "only the part");
    assert!(planned.output.unwrap().get("stale_source_row_removed").is_none());
}

// ------------------------------------------------------------------------------------------------ the character

#[test]
fn renaming_the_character_leaves_every_row_where_it_is() {
    // The rows live inside the character's node under its id, so the display name is all a rename touches.
    let doc = project_with_rows(&[("hair", row("CC-BY-4.0"))]);
    let planned = plan(&doc, "character.rename", json!({"character": "mock", "name": "Mira Two"})).unwrap();
    let touched: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(touched, ["characters/mock/name"]);
}

#[test]
fn a_character_that_carries_rows_still_reads_and_builds() {
    let doc = project_with_rows(&[("hair", row("CC-BY-4.0")), ("head", row("CC0-1.0"))]);
    assert_eq!(build(&doc, "mock").report.parts.len(), 5);
}
