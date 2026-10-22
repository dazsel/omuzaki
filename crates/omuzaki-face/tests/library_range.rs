//! The built-in expression library (#320) holds its range on the default face: no two moods or accents read the same at
//! a small stream size (56 px), none reads as the resting face, and at emote size (28 px) at least ten still read apart.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_face::preview::FaceSubject;
use omuzaki_face::preview::range::{RangeOptions, compare, paint_all};
use omuzaki_face_api::{ExpressionKind, ExpressionSet, FaceProfile, Palette};

#[test]
fn the_library_reads_apart_at_stream_and_emote_sizes() {
    let library = ExpressionSet::library().unwrap();
    let visemes: Vec<String> =
        library.0.iter().filter(|(_, e)| e.kind == ExpressionKind::Viseme).map(|(id, _)| id.to_string()).collect();
    let profile = FaceProfile { expressions: library, ..FaceProfile::default() };
    let face = FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    };
    let options = RangeOptions { sizes: vec![28, 56], ..RangeOptions::default() };
    let painted = paint_all(&face, &options, &|| false).unwrap().unwrap();
    let report = compare(&painted, &options, &face);

    let at_56: Vec<_> = report.similar.iter().filter(|p| p.size == 56).collect();
    assert!(at_56.is_empty(), "pairs that read the same at 56 px: {at_56:?}");
    assert!(report.neutral.is_empty(), "expressions that read as the rest face: {:?}", report.neutral);
    let emote = report.sizes.iter().find(|s| s.size == 28).unwrap();
    assert!(emote.distinct >= 10, "{:?}", report.sizes);
    // At emote size only visemes and the faintest accents may blur together; a mood never reads as another mood.
    let mood_pairs: Vec<_> = report
        .similar
        .iter()
        .filter(|p| !visemes.contains(&p.a) && p.a != "rest" && p.b != "rest" && p.size == 28)
        .collect();
    assert!(mood_pairs.is_empty(), "moods that read the same at 28 px: {mood_pairs:?}");
}
