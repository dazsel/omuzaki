//! Sheet text in scripts the embedded Latin font lacks: CJK, Arabic, Devanagari, Thai and emoji draw real
//! glyphs from fallback fonts instead of missing-glyph boxes.
//!
//! The fonts are the committed test subsets in `tests/fixtures/fonts` (made by `make_subsets.py` from the pinned Noto
//! files), so nothing is downloaded. All names are neutral samples. Set `OMUZAKI_FONT_SHEET_PNG=/some/dir` to write the
//! evidence sheet there.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Pixel maths on small page coordinates.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::RgbaImage;
use omuzaki_core::colour::Colour;
use omuzaki_stage::sheet::layout::{
    Align, DirSource, Dpi, Family, FontConfig, FontError, FontReport, FontSource, FontSpec, HanVariant, Orientation,
    Page, Paper, SheetLayout, TextStyle, Theme, TitleBlock, Weight,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts")
}

/// A directory source that remembers which fonts were asked for.
struct Recording {
    inner: DirSource,
    loaded: Arc<Mutex<Vec<&'static str>>>,
}

impl FontSource for Recording {
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
        self.loaded.lock().unwrap().push(spec.file_name);
        self.inner.load(spec)
    }
}

fn recording(han: HanVariant) -> (FontConfig, Arc<Mutex<Vec<&'static str>>>) {
    let loaded = Arc::new(Mutex::new(Vec::new()));
    let source = Recording { inner: DirSource::unverified(fixtures()), loaded: Arc::clone(&loaded) };
    (FontConfig::new(han, Arc::new(source)), loaded)
}

/// The bounding box (x0, y0, x1, y1) of the dark pixels, if any.
fn ink_box(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let dark: Vec<(u32, u32)> =
        image.enumerate_pixels().filter(|(_, _, p)| p.0[0] < 128).map(|(x, y, _)| (x, y)).collect();
    let (x0, x1) = (dark.iter().map(|d| d.0).min()?, dark.iter().map(|d| d.0).max()?);
    let (y0, y1) = (dark.iter().map(|d| d.1).min()?, dark.iter().map(|d| d.1).max()?);
    Some((x0, y0, x1, y1))
}

fn config(han: HanVariant) -> FontConfig {
    FontConfig::new(han, Arc::new(DirSource::unverified(fixtures())))
}

fn page() -> Page {
    Page::paper(Paper::A4, Orientation::Portrait, Dpi::D150)
}

fn sheet(fonts: FontConfig) -> SheetLayout {
    SheetLayout::with_fonts(page(), Theme::light(), fonts).unwrap()
}

fn style(weight: Weight) -> TextStyle {
    TextStyle::new(24.0, weight, Colour::BLACK)
}

/// Draws `text` alone on a small canvas and returns the sheet (for its report) and the picture of the strip.
fn draw(fonts: FontConfig, text: &str, weight: Weight) -> (FontReport, RgbaImage) {
    let mut s = sheet(fonts);
    s.text_on_baseline(text, &style(weight), (40.0, 200), Align::Left, None);
    let report = s.font_report();
    (report, s.finish())
}

const SAMPLES: [(&str, &str, &str); 6] = [
    ("Japanese", "山田 さくら", "田"),
    ("Chinese", "王小明", "明"),
    ("Korean", "김하늘", "늘"),
    ("Arabic", "مريم الأحمد", "ح"),
    ("Hindi", "आरव शर्मा", "र"),
    ("Thai", "สมชาย ใจดี", "ด"),
];

fn ink(image: &RgbaImage) -> usize {
    image.pixels().filter(|p| p.0[0] < 128).count()
}

#[test]
fn without_a_font_source_the_characters_are_reported_missing() {
    let (report, _) = draw(FontConfig::latin_only(), "山田", Weight::Regular);
    assert_eq!(report.faces, vec![]);
    let chars: Vec<char> = report.missing.iter().map(|m| m.character).collect();
    assert_eq!(chars, vec!['山', '田']);
    assert!(report.missing[0].reason.contains("could not be loaded"), "{}", report.missing[0].reason);
}

#[test]
fn different_characters_draw_different_glyphs_not_one_box() {
    for (language, text, _) in SAMPLES {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        let (first, second) = (chars[0].to_string(), chars[1].to_string());
        // The Latin-only render is the control: there every unknown character is the same box.
        let boxes = |t: &str| draw(FontConfig::latin_only(), t, Weight::Regular).1;
        if language != "Arabic" && language != "Hindi" && language != "Thai" {
            assert_eq!(boxes(&first).as_raw(), boxes(&second).as_raw(), "{language}: the control must be tofu");
        }
        let real = |t: &str| draw(config(HanVariant::Japanese), t, Weight::Regular).1;
        assert_ne!(real(&first).as_raw(), real(&second).as_raw(), "{language}: two characters, one glyph");
        assert_ne!(real(&first).as_raw(), boxes(&first).as_raw(), "{language}: still the box");
    }
}

#[test]
fn every_sample_name_draws_with_no_missing_glyphs() {
    let mut s = sheet(config(HanVariant::Japanese));
    for (_, text, _) in SAMPLES {
        s.text_on_baseline(text, &style(Weight::SemiBold), (40.0, 200), Align::Left, None);
    }
    s.text_on_baseline("Alice 山田 Hana \u{1F600}\u{1F3A8}", &style(Weight::Regular), (40.0, 300), Align::Left, None);
    let report = s.font_report();
    assert_eq!(report.missing, vec![]);
    let families: Vec<Family> = report.faces.iter().map(|f| f.family).collect();
    for family in [Family::Jp, Family::Kr, Family::Arabic, Family::Devanagari, Family::Thai, Family::Emoji] {
        assert!(families.contains(&family), "{family:?} not loaded: {families:?}");
    }
}

#[test]
fn only_the_fonts_a_sheet_needs_are_loaded() {
    let (report, _) = draw(config(HanVariant::Japanese), "Alice only", Weight::Regular);
    assert_eq!(report.faces, vec![], "Latin text must not load any fallback font");
}

#[test]
fn the_sheet_language_picks_the_han_font_and_records_it() {
    for (han, family) in [
        (HanVariant::Japanese, Family::Jp),
        (HanVariant::SimplifiedChinese, Family::Sc),
        (HanVariant::Korean, Family::Kr),
    ] {
        // 明 is in every CJK subset, so only the sheet's language decides.
        let (report, _) = draw(config(han), "明", Weight::Regular);
        assert_eq!(report.han, han);
        assert_eq!(report.faces.len(), 1);
        assert_eq!(report.faces[0].family, family);
        assert_eq!(report.faces[0].sha256.len(), 64);
    }
}

#[test]
fn arabic_letters_join_and_run_right_to_left() {
    let s = sheet(config(HanVariant::Japanese));
    let st = style(Weight::Regular);
    // Alone, every letter takes its isolated form; in a word the shaper picks initial, medial and final forms.
    let joined = s.text_width("مرم", &st);
    let isolated: f32 = "مرم".chars().map(|c| s.text_width(&c.to_string(), &st)).sum();
    assert!((joined - isolated).abs() > 1.0, "joined {joined} = isolated {isolated}: no contextual forms were applied");

    // The paragraph direction follows the first strong letter, so these two read the same on the page: the Arabic
    // word sits at the end of the line in an English paragraph and the English word at the start in an Arabic one.
    let draw_text = |t: &str| draw(config(HanVariant::Japanese), t, Weight::Regular).1;
    assert_eq!(draw_text("Ab م").as_raw(), draw_text("م Ab").as_raw());
    // Order matters: the Arabic letters stay right to left inside the English line.
    assert_ne!(draw_text("Ab مر").as_raw(), draw_text("Ab رم").as_raw());
}

#[test]
fn thai_marks_stack_above_their_base_without_widening_the_run() {
    let s = sheet(config(HanVariant::Japanese));
    let st = style(Weight::Regular);
    // ด + U+0E35 (a vowel mark above): the mark has no advance of its own.
    let base = s.text_width("ด", &st);
    let marked = s.text_width("ดี", &st);
    assert!((marked - base).abs() < 0.5, "base {base}, with mark {marked}");
}

#[test]
fn mixed_latin_and_cjk_share_one_baseline() {
    let (_, image) = draw(config(HanVariant::Japanese), "HH田H", Weight::Regular);
    // Ink bottoms: the flat feet of H sit on the baseline (row 200); the kanji may dip at most a little below it.
    let bottom = |x0: u32, x1: u32| {
        (0..image.height()).rev().find(|&y| (x0..x1).any(|x| image.get_pixel(x, y).0[0] < 128)).unwrap()
    };
    let h = bottom(40, 60);
    assert!((199..=200).contains(&h), "H feet at row {h}");
    let width = sheet(config(HanVariant::Japanese)).text_width("HH", &style(Weight::Regular)) as u32;
    let kanji = bottom(40 + width + 2, 40 + width + 30);
    assert!((190..=206).contains(&kanji), "kanji bottom at row {kanji}");
}

#[test]
fn an_ellipsis_cut_keeps_cjk_names_inside_their_width() {
    let mut s = sheet(config(HanVariant::Japanese));
    let st = style(Weight::Regular);
    let full = s.text_width("山田さくら山田さくら", &st);
    let drawn = s.text_on_baseline("山田さくら山田さくら", &st, (40.0, 200), Align::Left, Some(full / 2.0));
    assert!(drawn <= full / 2.0 + 0.01, "{drawn} > {}", full / 2.0);
}

#[test]
fn rendering_is_deterministic_to_the_byte() {
    let one = draw(config(HanVariant::Korean), "Alice 山田 한국 مريم आरव สม 😀", Weight::SemiBold).1;
    let two = draw(config(HanVariant::Korean), "Alice 山田 한국 مريم आरव สม 😀", Weight::SemiBold).1;
    assert_eq!(one.as_raw(), two.as_raw());
    assert!(ink(&one) > 500);
}

/// The evidence sheet: a title block with a mixed Latin and CJK name, then one row per script.
#[test]
fn the_evidence_sheet_renders() {
    let mut s = sheet(config(HanVariant::Japanese));
    let content = s.title_block(&TitleBlock {
        kicker: "Character sheet".into(),
        name: "Alice 山田 Hana".into(),
        template: "soft-sd".into(),
        variant: "Game face".into(),
        build: "746".into(),
        date: None,
    });
    let styles = s.styles();
    let rows = [
        ("Japanese", "山田 さくら"),
        ("Chinese", "王小明"),
        ("Korean", "김하늘"),
        ("Arabic", "مريم الأحمد"),
        ("Hindi", "आरव शर्मा"),
        ("Thai", "สมชาย ใจดี"),
        ("Emoji", "Pixel \u{1F600} \u{1F3A8} \u{2728}"),
        ("Mixed", "Alice 山田 Hana"),
        ("RTL + brackets", "مريم (Ali) [٢٠٢٦]"),
        ("Decomposed", "Zoe\u{308} Andre\u{301} o\u{323}q\u{301}"),
    ];
    let mut baseline = content.y + 120;
    for (language, name) in rows {
        s.text_on_baseline(language, &styles.kicker, (content.x as f32, baseline), Align::Left, None);
        s.text_on_baseline(name, &styles.title, (content.x as f32 + 260.0, baseline), Align::Left, None);
        s.text_on_baseline(name, &styles.caption, (content.right() as f32, baseline), Align::Right, None);
        baseline += 130;
    }
    assert_eq!(s.font_report().missing, vec![]);
    let image = s.finish();
    if let Ok(dir) = std::env::var("OMUZAKI_FONT_SHEET_PNG") {
        std::fs::create_dir_all(&dir).unwrap();
        image.save(Path::new(&dir).join("sheet-fonts.png")).unwrap();
    }
}

/// With `OMUZAKI_REAL_FONTS_DIR` pointing at the downloaded catalog files, the real fonts (hash-checked by `DirSource`)
/// draw the same sample names with nothing missing. Skipped when unset, so CI never needs them.
#[test]
fn the_pinned_real_fonts_pass_their_hashes_and_draw_the_samples() {
    let Ok(dir) = std::env::var("OMUZAKI_REAL_FONTS_DIR") else { return };
    let mut s = SheetLayout::with_fonts(
        page(),
        Theme::light(),
        FontConfig::new(HanVariant::Japanese, Arc::new(DirSource::new(dir))),
    )
    .unwrap();
    for (i, (_, text, _)) in SAMPLES.iter().enumerate() {
        s.text_on_baseline(text, &style(Weight::SemiBold), (40.0, 100 + 120 * i as i32), Align::Left, None);
    }
    s.text_on_baseline("Pixel \u{1F600} Alice 山田", &style(Weight::Regular), (40.0, 900), Align::Left, None);
    let report = s.font_report();
    assert_eq!(report.missing, vec![]);
    assert_eq!(report.faces.len(), 7);
    if let Ok(out) = std::env::var("OMUZAKI_FONT_SHEET_PNG") {
        s.finish().save(Path::new(&out).join("sheet-fonts-real.png")).unwrap();
    }
}

#[test]
fn a_character_the_chosen_han_font_lacks_downloads_exactly_one_more_font() {
    let (config, loaded) = recording(HanVariant::Japanese);
    let mut s = sheet(config);
    s.text_on_baseline("明", &style(Weight::Regular), (40.0, 200), Align::Left, None);
    assert_eq!(*loaded.lock().unwrap(), vec!["NotoSansJP-Regular.otf"]);
    // 们 is only in the Simplified Chinese font; the coverage table says so, so no other font is fetched to ask.
    s.text_on_baseline("明们", &style(Weight::Regular), (40.0, 300), Align::Left, None);
    assert_eq!(*loaded.lock().unwrap(), vec!["NotoSansJP-Regular.otf", "NotoSansSC-Regular.otf"]);
    assert_eq!(s.font_report().missing, vec![]);

    let (config, loaded) = recording(HanVariant::Korean);
    let mut s = sheet(config);
    s.text_on_baseline("们", &style(Weight::Regular), (40.0, 200), Align::Left, None);
    assert_eq!(*loaded.lock().unwrap(), vec!["NotoSansSC-Regular.otf"], "one font, not the whole CJK chain");
}

#[test]
fn a_character_no_font_covers_downloads_nothing() {
    let (config, loaded) = recording(HanVariant::Japanese);
    let mut s = sheet(config);
    // U+3FFFD is unassigned: no pinned font has it.
    s.text_on_baseline("\u{3FFFD}", &style(Weight::Regular), (40.0, 200), Align::Left, None);
    assert_eq!(*loaded.lock().unwrap(), Vec::<&str>::new());
    assert_eq!(s.font_report().missing.len(), 1);
}

#[test]
fn the_semibold_role_is_the_medium_cut_in_every_script() {
    let (config, loaded) = recording(HanVariant::Japanese);
    let mut s = sheet(config);
    for (_, text, _) in SAMPLES {
        s.text_on_baseline(text, &style(Weight::SemiBold), (40.0, 200), Align::Left, None);
    }
    let mut files = loaded.lock().unwrap().clone();
    files.sort_unstable();
    assert_eq!(
        files,
        vec![
            "NotoSansArabic-Medium.ttf",
            "NotoSansDevanagari-Medium.ttf",
            "NotoSansJP-Medium.otf",
            "NotoSansKR-Medium.otf",
            "NotoSansThai-Medium.ttf",
        ]
    );
}

#[test]
fn a_letter_and_its_accent_draw_alike_composed_or_decomposed() {
    let draw_text = |t: &str| draw(config(HanVariant::Japanese), t, Weight::Regular);
    let (composed_report, composed) = draw_text("caf\u{E9}");
    let (decomposed_report, decomposed) = draw_text("cafe\u{301}");
    assert_eq!(composed.as_raw(), decomposed.as_raw(), "NFC makes the two spellings one");
    assert_eq!(composed_report.missing, vec![]);
    assert_eq!(decomposed_report.missing, vec![]);
}

#[test]
fn a_mark_with_no_precomposed_form_sits_on_its_base_and_is_not_a_box() {
    for mark in ['\u{301}', '\u{308}', '\u{323}'] {
        let with_mark = format!("o{mark}");
        let (report, marked) = draw(config(HanVariant::Japanese), &with_mark, Weight::Regular);
        assert_eq!(report.missing, vec![], "{mark:?}");
        let (_, bare) = draw(config(HanVariant::Japanese), "o", Weight::Regular);
        let (b, m) = (ink_box(&bare).unwrap(), ink_box(&marked).unwrap());
        // The mark adds ink but no width: it is over (or under) the letter, inside its column.
        assert_ne!(bare.as_raw(), marked.as_raw(), "{mark:?} drew nothing");
        assert!(m.0 + 2 >= b.0 && m.2 <= b.2 + 3, "{mark:?} strays sideways: {b:?} vs {m:?}");
        assert!(m.1 < b.1 || m.3 > b.3, "{mark:?} is not above or below the letter: {b:?} vs {m:?}");
    }
    // A capital's mark rides higher than a small letter's.
    let top = |t: &str| ink_box(&draw(config(HanVariant::Japanese), t, Weight::Regular).1).unwrap().1;
    assert!(top("E\u{323}") >= top("E") && top("Q\u{301}") < top("q\u{301}"));
}

#[test]
fn marks_stay_with_their_base_in_a_right_to_left_run() {
    let (report, image) = draw(config(HanVariant::Japanese), "Ab \u{645}\u{64E}\u{631} q\u{301}", Weight::Regular);
    assert_eq!(report.missing, vec![]);
    assert!(ink_box(&image).is_some());
}

#[test]
fn rtl_alignment_is_physical_for_left_right_and_follows_the_text_for_start_and_end() {
    let x = 400.0;
    let place = |text: &str, align: Align| {
        let mut s = sheet(config(HanVariant::Japanese));
        s.text_on_baseline(text, &style(Weight::Regular), (x, 200), align, None);
        ink_box(&s.finish()).unwrap()
    };
    let arabic = "مريم";
    // Left is the left edge whatever the script: an Arabic name sits where an English one would.
    assert!((place(arabic, Align::Left).0 as f32 - x).abs() < 6.0);
    assert!((place("Mary", Align::Left).0 as f32 - x).abs() < 6.0);
    assert!((place(arabic, Align::Right).2 as f32 - x).abs() < 6.0);
    // Start and End read the paragraph direction: right-to-left text starts at its right edge.
    assert!((place(arabic, Align::Start).2 as f32 - x).abs() < 6.0);
    assert!((place(arabic, Align::End).0 as f32 - x).abs() < 6.0);
    assert!((place("Mary", Align::Start).0 as f32 - x).abs() < 6.0);
    assert!((place("Mary", Align::End).2 as f32 - x).abs() < 6.0);
    // The direction is the first strong letter's: an Arabic word first makes the whole run right to left.
    assert!((place("مريم Ali", Align::Start).2 as f32 - x).abs() < 6.0);
    assert!((place("Ali مريم", Align::Start).0 as f32 - x).abs() < 6.0);
}
