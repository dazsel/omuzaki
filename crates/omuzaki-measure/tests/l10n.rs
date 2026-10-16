//! The crate's messages: every one has a translator note, and the English Fluent file renders each of them without a
//! fallback. This is the check `omuzaki_l10n_api::check_crate!` will make once it lands; until then it is written out
//! here, so a message cannot ship without its English or its note.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_l10n_api::{Catalog, Locale, MessageTable, Resource};
use omuzaki_measure::messages;

const EN: &str = include_str!("../locales/en/omuzaki-measure.ftl");

fn catalog() -> Catalog {
    let resource = Resource { package: "omuzaki-measure", locale: "en", source: EN };
    Catalog::from_sources(Locale::new("en").unwrap(), &[], &[resource])
}

#[test]
fn every_declared_message_has_a_translator_note() {
    let mut seen = 0;
    for table in inventory::iter::<MessageTable> {
        if table.package != "omuzaki-measure" {
            continue;
        }
        for message in table.messages {
            seen += 1;
            assert!(!message.note.trim().is_empty(), "{} has no translator note", message.id);
        }
    }
    assert_eq!(seen, 12, "the messages declared in src/messages.rs");
}

#[test]
fn the_english_file_has_every_message_and_no_fallback() {
    let catalog = catalog();
    assert!(catalog.missing().is_empty(), "{:?}", catalog.missing());
}

#[test]
fn messages_render_their_english_with_their_arguments() {
    let catalog = catalog();
    let cases = [
        (
            messages::too_small(38.5, 64),
            "The eyes are 38.5 px apart; a measure needs at least 64 px. Give a larger image.",
        ),
        (messages::landmark_outside("mouth"), "The mouth lies outside the image."),
        (
            messages::eyes_swapped(),
            "The mouth is above the eye line: the eyes may be swapped, or the face is upside down.",
        ),
        (
            messages::low_contrast(12.0, 15),
            "The line art is too faint to bound the face: its contrast is 12, and at least 15 is needed.",
        ),
        (
            messages::leak("upper-left"),
            "The fill escaped through a gap in the line art near upper-left. Render larger, or close the gap.",
        ),
        (messages::no_chin(), "No outline sample reaches the chin: the chin is hidden or outside the image."),
        (messages::coverage("60-120"), "The outline is missing in 60-120: exclude less, or use a clearer view."),
        (
            messages::stale_target("front"),
            "The likeness target front was measured by another version of the measures: import its reference image again.",
        ),
        (messages::target_not_found("front"), "The character has no likeness target named front. Set it first."),
        (
            messages::image_too_large(10_000, 9_000, 8192, 64),
            "The image is 10,000 × 9,000 px. A measure reads at most 8,192 px a side and 64 MiB decoded.",
        ),
        (messages::image_unreadable("unexpected end of file"), "The image could not be read: unexpected end of file."),
        (
            messages::image_colour_unsupported(String::from("an embedded colour profile that cannot be read")),
            "The image's colour could not be converted to sRGB (an embedded colour profile that cannot be read). Export it as sRGB and try again.",
        ),
    ];
    for (text, english) in cases {
        assert_eq!(catalog.render(&text), english, "{}", text.id);
    }
    // Rendering is what records a message with no English, so the report is read after it.
    assert!(catalog.missing().is_empty(), "{:?}", catalog.missing());
}
