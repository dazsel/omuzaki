//! The bundled fonts (OFL-1.1) and the Phosphor icon fonts (MIT), installed into egui (`design-system.md` sections 3
//! and 5).
//!
//! Each weight is its own named family, never a synthetic bold. The three text faces ship as the unmodified
//! variable fonts from Google Fonts; egui 0.36 can pin a variable font's axes per entry, so one file serves several
//! families and no static instances need cutting (the spec allowed for cutting them; this avoids modifying the fonts).
//! Licence texts sit next to the font files in `fonts/` and are listed in `docs/licensing/register.md`.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{FontData, FontDefinitions, FontFamily, FontTweak};

/// Instrument Sans, variable (`wdth` 75 to 100, `wght` 400 to 700).
const INSTRUMENT_SANS: &[u8] = include_bytes!("fonts/InstrumentSans.ttf");
/// Bricolage Grotesque, variable (`opsz` 12 to 96, `wdth` 75 to 100, `wght` 200 to 800).
const BRICOLAGE_GROTESQUE: &[u8] = include_bytes!("fonts/BricolageGrotesque.ttf");
/// `JetBrains Mono`, variable (`wght` 100 to 800).
const JETBRAINS_MONO: &[u8] = include_bytes!("fonts/JetBrainsMono.ttf");

/// Family name of Instrument Sans Medium (500): button labels, selected rows.
pub const SANS_MEDIUM: &str = "sans-medium";
/// Family name of `Instrument Sans` `SemiBold` (600): titles and section headers.
pub const SANS_SEMIBOLD: &str = "sans-semibold";
/// Family name of `Bricolage Grotesque` `SemiBold`: splash and empty-state headlines.
pub const DISPLAY: &str = "display";
/// Family name of the regular Phosphor icons.
pub const ICONS: &str = "icons";
/// Family name of the filled Phosphor icons: "on" states.
pub const ICONS_FILL: &str = "icons-fill";

/// A family of Instrument Sans at one weight.
fn instrument(weight: f32) -> FontData {
    FontData::from_static(INSTRUMENT_SANS).tweak(FontTweak {
        coords: VariationCoords::new([(b"wght", weight), (b"wdth", 100.0)]),
        ..FontTweak::default()
    })
}

/// The complete font table: our faces first, egui's defaults last so a missing glyph is never a box.
pub fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallback_proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fallback_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();

    let mut install = |key: &str, data: FontData, family: FontFamily, fallback: &[String]| {
        fonts.font_data.insert(key.to_owned(), Arc::new(data));
        let mut chain = vec![key.to_owned()];
        chain.extend(fallback.iter().cloned());
        fonts.families.insert(family, chain);
    };

    install("InstrumentSans-400", instrument(400.0), FontFamily::Proportional, &fallback_proportional);
    install("InstrumentSans-500", instrument(500.0), FontFamily::Name(SANS_MEDIUM.into()), &fallback_proportional);
    install("InstrumentSans-600", instrument(600.0), FontFamily::Name(SANS_SEMIBOLD.into()), &fallback_proportional);
    install(
        "BricolageGrotesque-600",
        FontData::from_static(BRICOLAGE_GROTESQUE).tweak(FontTweak {
            // Optical size 28 matches the Display style; the font's own default (96) is for posters.
            coords: VariationCoords::new([(b"wght", 600.0), (b"wdth", 100.0), (b"opsz", 28.0)]),
            ..FontTweak::default()
        }),
        FontFamily::Name(DISPLAY.into()),
        &fallback_proportional,
    );
    install(
        "JetBrainsMono-400",
        FontData::from_static(JETBRAINS_MONO)
            .tweak(FontTweak { coords: VariationCoords::new([(b"wght", 400.0)]), ..FontTweak::default() }),
        FontFamily::Monospace,
        &fallback_mono,
    );

    egui_phosphor::add_font_bytes_as_family(&mut fonts, ICONS, egui_phosphor::bytes::regular::FONT);
    egui_phosphor::add_font_bytes_as_family(&mut fonts, ICONS_FILL, egui_phosphor::bytes::fill::FONT);
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_the_text_styles_use_is_bound() {
        let fonts = definitions();
        for name in [SANS_MEDIUM, SANS_SEMIBOLD, DISPLAY, ICONS, ICONS_FILL] {
            assert!(fonts.families.contains_key(&FontFamily::Name(name.into())), "{name} is not bound");
        }
        assert!(fonts.families[&FontFamily::Proportional][0].starts_with("InstrumentSans"));
        assert!(fonts.families[&FontFamily::Monospace][0].starts_with("JetBrainsMono"));
    }

    fn be16(bytes: &[u8], at: usize) -> Option<usize> {
        Some(usize::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
    }

    fn be32(bytes: &[u8], at: usize) -> Option<usize> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
    }

    /// Whether the font's character map (the `cmap` table, formats 4 and 12) lists `c`. egui's own `has_glyph` cannot
    /// say: with one face it reports every character as missing, and with fallbacks it cannot tell whose glyph it is.
    fn font_has(font: &[u8], c: char) -> Option<bool> {
        let tables = be16(font, 4)?;
        let cmap = (0..tables).find_map(|i| {
            let record = 12 + 16 * i;
            (font.get(record..record + 4)? == b"cmap").then(|| be32(font, record + 8)).flatten()
        })?;
        let c = c as usize;
        for i in 0..be16(font, cmap + 2)? {
            let subtable = cmap + be32(font, cmap + 4 + 8 * i + 4)?;
            match be16(font, subtable)? {
                4 => {
                    let segments = be16(font, subtable + 6)? / 2;
                    let (ends, starts) = (subtable + 14, subtable + 16 + 2 * segments);
                    if (0..segments).any(|s| {
                        matches!((be16(font, starts + 2 * s), be16(font, ends + 2 * s)), (Some(a), Some(b)) if (a..=b).contains(&c))
                    }) {
                        return Some(true);
                    }
                }
                12 => {
                    let groups = be32(font, subtable + 12)?;
                    if (0..groups).any(|g| {
                        let at = subtable + 16 + 12 * g;
                        matches!((be32(font, at), be32(font, at + 4)), (Some(a), Some(b)) if (a..=b).contains(&c))
                    }) {
                        return Some(true);
                    }
                }
                _ => {}
            }
        }
        Some(false)
    }

    /// The pseudo-locale writes with letters the bundled text faces draw themselves: a letter missing from them would
    /// fall back to egui's fonts, or to a box when those lack it too.
    #[test]
    fn every_letter_of_the_pseudo_locale_is_in_each_bundled_text_face() {
        assert_eq!(font_has(INSTRUMENT_SANS, 'a'), Some(true), "the character map reader works");
        assert_eq!(font_has(INSTRUMENT_SANS, '\u{1E41}'), Some(false), "and tells a missing letter");
        for (name, bytes) in [
            ("Instrument Sans", INSTRUMENT_SANS),
            ("Bricolage Grotesque", BRICOLAGE_GROTESQUE),
            ("JetBrains Mono", JETBRAINS_MONO),
        ] {
            let missing: String = ('a'..='z')
                .chain('A'..='Z')
                .map(crate::ds::strings::accent)
                .filter(|c| font_has(bytes, *c) != Some(true))
                .collect();
            assert!(missing.is_empty(), "{name} lacks {missing}");
        }
    }

    #[test]
    fn egui_default_fonts_stay_last_as_fallback() {
        let fonts = definitions();
        let chain = &fonts.families[&FontFamily::Proportional];
        assert!(chain.len() > 1, "a missing glyph must fall back to egui's fonts");
        assert_eq!(chain[0], "InstrumentSans-400");
    }
}
