//! The studio's words as Fluent files (spec SE-03, design `l10n-ops.md` sections 3.3 and 10.1, #206): every message the
//! UI says has an id, a translator note and English that parses with exactly its placeholders; the committed English
//! file is the one generated from the tables (`OMUZAKI_BLESS=1 cargo test -p omuzaki-ui --test l10n` rewrites it);
//! every translation file parses and matches it; and the English catalog renders each message as the table's English,
//! so a person reading English sees the same words through the catalog as without it.

use omuzaki_l10n_api::check::{self, report};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{ArgKind, Catalog, Locale, MessageDecl};
use omuzaki_ui::ds::lexicon;
use omuzaki_ui::ds::strings::{CatalogStrings, Strings, fill, placeholders};

const PACKAGE: &str = env!("CARGO_PKG_NAME");

/// Every message the UI says, as the declarations the l10n checks read. The strings live as long as the test binary.
fn package() -> PackageText {
    omuzaki_ui::link();
    let mut text = PackageText::new(PACKAGE);
    for (id, english) in lexicon::entries().into_iter().chain(lexicon::registered()) {
        let args: Vec<(&'static str, ArgKind)> = placeholders(english).map(|name| (leak(name), ArgKind::Id)).collect();
        text.messages.push(MessageDecl {
            id: leak(&id),
            english: leak(&lexicon::to_fluent(english)),
            note: leak(&format!(" {}", lexicon::note_of(&id))),
            args: Vec::leak(args),
            verbatim: lexicon::is_verbatim(&id),
        });
    }
    text
}

fn leak(text: &str) -> &'static str {
    String::leak(text.to_owned())
}

#[test]
fn every_message_has_an_id_a_note_and_english_with_its_placeholders() {
    let failures = check::messages(&package());
    assert!(failures.is_empty(), "\n{}", report(&failures));
}

#[test]
fn the_english_file_is_the_generated_one() {
    let bless = std::env::var("OMUZAKI_BLESS").is_ok_and(|value| value == "1");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let failures = check::english_current(&package(), dir, bless);
    assert!(failures.is_empty(), "\n{}", report(&failures));
}

#[test]
fn every_translation_parses_and_matches_the_english() {
    let text = package();
    let failures = check::translations(&text, &check::resources(PACKAGE));
    assert!(failures.is_empty(), "\n{}", report(&failures));
}

#[test]
fn the_english_catalog_says_what_the_tables_say() {
    omuzaki_ui::link();
    let strings = CatalogStrings::new(Catalog::new(Locale::EN));
    for (id, english) in lexicon::entries().into_iter().chain(lexicon::registered()) {
        let args: Vec<(&str, &str)> = placeholders(english).map(|name| (name, "‹x›")).collect();
        assert_eq!(fill(&strings.translate(&id, english), &args), fill(english, &args), "{id}");
    }
    assert!(strings.catalog().missing().is_empty(), "{:?}", strings.catalog().missing());
}
