//! Acceptance tests for the `assets.lock` reader and canonical writer.

use omuzaki_assets_lock::{AssetsLock, LockError, LockRow};
use omuzaki_store_api::{Commercial, ContentHash};
use proptest::prelude::*;

/// A 64-character lowercase hash, as the example in the design writes it.
const EXAMPLE_HASH: &str = "9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c";

/// The example from `docs/design/journal.md` §7.1.1, with `private_test` present.
const EXAMPLE: &str = "[[asset]]\n\
hash = \"9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c\"\n\
name = \"hero-face.png\"\n\
licence = \"CC0-1.0\"\n\
commercial = \"ok\"\n\
source = \"made in-house\"\n\
private_test = true\n";

fn hash(byte: u8) -> ContentHash {
    ContentHash([byte; 32])
}

fn row(byte: u8, name: &str) -> LockRow {
    LockRow {
        hash: hash(byte),
        name: name.to_owned(),
        licence: "CC0-1.0".to_owned(),
        commercial: Commercial::Ok,
        source: "made in-house".to_owned(),
        private_test: false,
    }
}

fn sorted(mut rows: Vec<LockRow>) -> Vec<LockRow> {
    rows.sort_by_key(|row| row.hash);
    rows
}

#[test]
fn the_example_parses_to_one_full_row() {
    let lock = AssetsLock::parse(EXAMPLE).expect("the example is valid");
    assert_eq!(lock.rows.len(), 1);
    let only = &lock.rows[0];
    assert_eq!(only.hash.to_string(), EXAMPLE_HASH);
    assert_eq!(only.name, "hero-face.png");
    assert_eq!(only.licence, "CC0-1.0");
    assert_eq!(only.commercial, Commercial::Ok);
    assert_eq!(only.source, "made in-house");
    assert!(only.private_test);
}

#[test]
fn to_text_of_the_example_is_the_example_byte_for_byte() {
    let lock = AssetsLock::parse(EXAMPLE).expect("the example is valid");
    assert_eq!(lock.to_text(), EXAMPLE);
}

#[test]
fn private_test_is_absent_when_false() {
    let lock = AssetsLock { rows: vec![row(1, "a.png")] };
    let text = lock.to_text();
    assert!(!text.contains("private_test"), "{text}");
    assert_eq!(AssetsLock::parse(&text).expect("valid"), lock);
}

#[test]
fn an_absent_private_test_reads_as_false() {
    let lock = AssetsLock::parse(&lock_text(&[row(1, "a.png")])).expect("valid");
    assert!(!lock.rows[0].private_test);
}

#[test]
fn empty_and_whitespace_only_files_are_zero_rows() {
    for text in ["", "   \n\t\n", "\r\n"] {
        assert_eq!(AssetsLock::parse(text).expect("valid"), AssetsLock::default(), "{text:?}");
    }
    assert_eq!(AssetsLock::default().to_text(), "");
}

#[test]
fn rows_come_back_sorted_by_hash() {
    let lock = AssetsLock::parse(&lock_text(&[row(9, "z.png"), row(2, "b.png"), row(5, "m.png")])).expect("valid");
    let bytes: Vec<u8> = lock.rows.iter().map(|row| row.hash.0[0]).collect();
    assert_eq!(bytes, vec![2, 5, 9]);
}

#[test]
fn to_text_is_order_independent() {
    let forward = AssetsLock { rows: vec![row(1, "a.png"), row(2, "b.png"), row(3, "c.png")] };
    let backward = AssetsLock { rows: vec![row(3, "c.png"), row(1, "a.png"), row(2, "b.png")] };
    assert_eq!(forward.to_text(), backward.to_text());
}

#[test]
fn crlf_parses_to_the_same_rows_as_lf() {
    let lf = lock_text(&[row(1, "a.png"), row(2, "b.png")]);
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(AssetsLock::parse(&crlf).expect("valid"), AssetsLock::parse(&lf).expect("valid"));
}

#[test]
fn unknown_keys_are_ignored_and_not_kept() {
    let text = "future = 1\n[[asset]]\nhash = \"9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c\"\n\
name = \"a.png\"\nlicence = \"CC0-1.0\"\ncommercial = \"no\"\nsource = \"x\"\nnote = \"dropped\"\n";
    let lock = AssetsLock::parse(text).expect("unknown keys are fine");
    assert_eq!(lock.rows.len(), 1);
    assert_eq!(lock.rows[0].commercial, Commercial::No);
    let rewritten = lock.to_text();
    assert!(!rewritten.contains("note") && !rewritten.contains("future"), "{rewritten}");
}

#[test]
fn a_repeated_hash_is_refused_at_the_later_row() {
    // The writer sorts, so the repeat is appended by hand: it sits after the two canonical rows, at index 2.
    let text = format!("{}\n[[asset]]\n{}", lock_text(&[row(1, "a.png"), row(2, "b.png")]), row_body(1));
    assert_row_error(&text, 2);
}

#[test]
fn a_63_character_hash_is_refused() {
    let short = &EXAMPLE_HASH[..63];
    let text = EXAMPLE.replace(EXAMPLE_HASH, short);
    assert_row_error(&text, 0);
}

#[test]
fn an_uppercase_hash_is_refused() {
    let upper = EXAMPLE_HASH.to_uppercase();
    let text = EXAMPLE.replace(EXAMPLE_HASH, &upper);
    assert_row_error(&text, 0);
}

#[test]
fn an_unknown_commercial_value_is_refused() {
    let text = EXAMPLE.replace("commercial = \"ok\"", "commercial = \"maybe\"");
    assert_row_error(&text, 0);
}

#[test]
fn a_row_without_a_name_is_refused_with_its_index() {
    let first_missing = lock_text(&[row(1, "a.png")]).replace("name = \"a.png\"\n", "");
    assert_row_error(&first_missing, 0);

    let second_missing = format!("{}\n[[asset]]\n{}", lock_text(&[row(1, "a.png")]), row_body_without_name(2));
    assert_row_error(&second_missing, 1);
}

#[test]
fn invalid_toml_is_a_toml_error() {
    let error = AssetsLock::parse("not toml [").expect_err("invalid");
    assert!(matches!(error, LockError::Toml(_)), "{error:?}");
}

#[test]
fn asset_that_is_not_a_list_is_a_toml_error_for_the_whole_document() {
    let error = AssetsLock::parse("asset = 3\n").expect_err("invalid");
    assert!(matches!(error, LockError::Toml(_)), "{error:?}");
}

#[test]
fn an_entry_that_is_not_a_table_is_a_row_error_at_its_index() {
    // Valid TOML, so the document shape is right: the first entry is a string, so it is the row to fix.
    assert_row_error("asset = [\"x\", 3]\n", 0);
    assert_row_error("asset = [3]\n", 0);
}

proptest! {
    #[test]
    fn parse_reads_back_what_to_text_writes(rows in arbitrary_rows()) {
        let lock = AssetsLock { rows: rows.clone() };
        let text = lock.to_text();
        let parsed = AssetsLock::parse(&text).expect("the writer's output parses");
        prop_assert_eq!(parsed.rows, sorted(rows));
    }

    #[test]
    fn to_text_is_stable_under_a_round_trip(rows in arbitrary_rows()) {
        let text = AssetsLock { rows }.to_text();
        let again = AssetsLock::parse(&text).expect("the writer's output parses").to_text();
        prop_assert_eq!(again, text);
    }

    #[test]
    fn to_text_ignores_row_order(rows in arbitrary_rows()) {
        let mut reversed = rows.clone();
        reversed.reverse();
        prop_assert_eq!(AssetsLock { rows }.to_text(), AssetsLock { rows: reversed }.to_text());
    }
}

/// Random rows with distinct hashes and names, sources and licences full of characters TOML must escape.
fn arbitrary_rows() -> impl Strategy<Value = Vec<LockRow>> {
    let text = r#"[a-z"\\#\n\r\t\x00\x01\x7fé日🎉\[\]= ]{0,24}"#;
    let row = (any::<[u8; 32]>(), text, text, text, any::<bool>(), 0u8..3).prop_map(
        |(bytes, name, licence, source, private_test, commercial)| LockRow {
            hash: ContentHash(bytes),
            name,
            licence,
            commercial: match commercial {
                0 => Commercial::Ok,
                1 => Commercial::No,
                _ => Commercial::Unknown,
            },
            source,
            private_test,
        },
    );
    prop::collection::vec(row, 0..8).prop_map(|mut rows| {
        rows.sort_by_key(|row| row.hash);
        rows.dedup_by_key(|row| row.hash);
        rows
    })
}

/// The `[[asset]]` table body for `byte`'s hash, with a name.
fn row_body(byte: u8) -> String {
    format!("hash = \"{}\"\nname = \"x.png\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nsource = \"s\"\n", hash(byte))
}

fn row_body_without_name(byte: u8) -> String {
    row_body(byte).replace("name = \"x.png\"\n", "")
}

/// A lock file with `rows` written by the canonical writer.
fn lock_text(rows: &[LockRow]) -> String {
    AssetsLock { rows: rows.to_vec() }.to_text()
}

/// Asserts that `text` is refused with a row error at `index`.
fn assert_row_error(text: &str, index: usize) {
    match AssetsLock::parse(text) {
        Err(LockError::Row { index: got, reason }) => {
            assert_eq!(got, index, "reason: {reason}");
        }
        other => panic!("expected a row error at {index}, got {other:?}"),
    }
}
