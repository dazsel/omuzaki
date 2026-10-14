//! Picking a locale from a preference list (design 6): the OS list on first run, `--lang` and `OMUZAKI_LANG` for the CLI.

use unic_langid::LanguageIdentifier;
use unic_langid::subtags::Script;

use crate::{Locale, OVERRIDES};

/// The locale for the first usable entry of `preferences`, most wanted first, or English.
///
/// Entries may be BCP 47 (`pt-BR`) or POSIX (`pt_BR.UTF-8`, `de_DE@euro`); `C` and `POSIX` mean English. The table:
/// - `en-*`, `ja-*`, `ko-*` give `en`, `ja`, `ko`; `pt-*` gives `pt-BR`;
/// - `es-*` gives `es-419`, or the shipped override that matches the entry (`es-ES` once it ships);
/// - `zh-Hans`, `zh-CN`, `zh-SG` and a bare `zh` give `zh-Hans`; `zh-Hant`, `zh-TW`, `zh-HK` and `zh-MO` give `en`,
///   never `zh-Hans` (nobody is switched to Simplified Chinese silently), until SE-09 ships `zh-Hant`;
/// - a language Omuzaki does not ship is skipped in favour of the next entry; if none is usable the answer is `en`.
pub fn negotiate(preferences: &[&str]) -> Locale {
    negotiate_with(preferences, OVERRIDES)
}

/// The OS preference list, most wanted first (through `sys-locale`: `LC_ALL`, `LC_MESSAGES` and `LANG` on Unix, the
/// user's UI languages on Windows). Feed it to [`negotiate()`].
pub fn os_locales() -> Vec<String> {
    sys_locale::get_locales().collect()
}

/// [`negotiate`] over an explicit override table, so the override rule is testable while `OVERRIDES` is empty.
fn negotiate_with(preferences: &[&str], overrides: &[(Locale, Locale)]) -> Locale {
    preferences.iter().find_map(|tag| pick(tag, overrides)).unwrap_or(Locale::EN)
}

fn pick(tag: &str, overrides: &[(Locale, Locale)]) -> Option<Locale> {
    // POSIX: `de_DE.UTF-8@euro` is `de-DE`; `C` and `POSIX` are the "no language" locales.
    let bare = tag.split(['.', '@']).next().unwrap_or_default().trim();
    if bare.eq_ignore_ascii_case("c") || bare.eq_ignore_ascii_case("posix") {
        return Some(Locale::EN);
    }
    let id: LanguageIdentifier = bare.replace('_', "-").parse().ok()?;
    match id.language.as_str() {
        "en" => Some(Locale::EN),
        "ja" => Some(Locale::JA),
        "ko" => Some(Locale::KO),
        "pt" => Some(Locale::PT_BR),
        "es" => Some(
            overrides
                .iter()
                .map(|(over, _)| over)
                .find(|over| matches_override(over, &id))
                .cloned()
                .unwrap_or(Locale::ES_419),
        ),
        "zh" => Some(chinese(&id)),
        _ => None,
    }
}

/// An override matches an entry of the same language and region (`es-ES`), whatever else the entry carries.
fn matches_override(over: &Locale, id: &LanguageIdentifier) -> bool {
    over.as_str().parse::<LanguageIdentifier>().is_ok_and(|o| o.language == id.language && o.region == id.region)
}

/// Simplified for an explicit `Hans` or a mainland / Singapore region; Traditional (English for now) for `Hant`
/// or Taiwan, Hong Kong and Macau. The script wins over the region (`zh-Hans-TW` is Simplified).
fn chinese(id: &LanguageIdentifier) -> Locale {
    let traditional = match id.script.as_ref().map(Script::as_str) {
        Some("Hans") => false,
        Some("Hant") => true,
        _ => id.region.as_ref().is_some_and(|r| matches!(r.as_str(), "TW" | "HK" | "MO")),
    };
    if traditional { Locale::EN } else { Locale::ZH_HANS }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_negotiation_table() {
        let table: &[(&[&str], Locale)] = &[
            (&["en-US"], Locale::EN),
            (&["en"], Locale::EN),
            (&["ja-JP"], Locale::JA),
            (&["ko-KR"], Locale::KO),
            (&["pt-PT"], Locale::PT_BR),
            (&["pt-BR"], Locale::PT_BR),
            (&["pt"], Locale::PT_BR),
            (&["es-MX"], Locale::ES_419),
            (&["es-ES"], Locale::ES_419),
            (&["es"], Locale::ES_419),
            (&["zh-Hans"], Locale::ZH_HANS),
            (&["zh-CN"], Locale::ZH_HANS),
            (&["zh-SG"], Locale::ZH_HANS),
            (&["zh"], Locale::ZH_HANS),
            (&["zh-Hans-TW"], Locale::ZH_HANS),
            (&["zh-Hant"], Locale::EN),
            (&["zh-TW"], Locale::EN),
            (&["zh-HK"], Locale::EN),
            (&["zh-MO"], Locale::EN),
            (&["zh-Hant-CN"], Locale::EN),
            (&["C"], Locale::EN),
            (&["POSIX"], Locale::EN),
            (&["C.UTF-8"], Locale::EN),
            (&["fr-FR"], Locale::EN),
            (&["fr-FR", "ko"], Locale::KO),
            (&["???", "ja"], Locale::JA),
            (&["pt_BR.UTF-8"], Locale::PT_BR),
            (&["ja_JP.eucJP@x"], Locale::JA),
            (&["ZH-cn"], Locale::ZH_HANS),
            (&["zh-TW", "ja"], Locale::EN),
            (&[], Locale::EN),
            (&[""], Locale::EN),
        ];
        for (preferences, expected) in table {
            assert_eq!(&negotiate(preferences), expected, "{preferences:?}");
        }
    }

    #[test]
    fn a_shipped_override_wins_over_the_neutral_spanish() {
        let es_es = Locale::new("es-ES").unwrap();
        let table = [(es_es.clone(), Locale::ES_419)];
        assert_eq!(negotiate_with(&["es-ES"], &table), es_es);
        assert_eq!(negotiate_with(&["es-MX"], &table), Locale::ES_419);
        assert_eq!(negotiate_with(&["es"], &table), Locale::ES_419);
    }

    #[test]
    fn the_os_list_is_made_of_tags() {
        // The machine's own list varies; whatever it is, negotiating it yields a shipped locale.
        let list = os_locales();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        assert!(crate::SHIPPED.contains(&negotiate(&refs)));
    }
}
