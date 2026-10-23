//! How the UI looks and speaks, as settings (`docs/design/settings.md` D2, #949, #206): `ui.theme`, `ui.density`,
//! `ui.scale`, `ui.reduce_motion` and `ui.language`. Taste only, so an agent may set them (`AgentWrite::Free`), a
//! profile may hold them, and they apply at once. The look's defaults are [`Settings::default`]; the language's is
//! the computer's own (`system`), negotiated from the OS preference list (design `l10n-ops.md` section 6).
//!
//! [`SettingsLink`] keeps the design system in step with the settings service: it reads the keys from the service's
//! snapshot once, then again only when a change event touches a `ui.*` key, and applies them. There is no per-frame
//! settings read: a frame with no event is one empty channel poll. A new language swaps the design system's
//! [`Strings`] table on the next frame, with no restart.

use std::sync::Arc;

use egui::Context;
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::event::{SettingsEvent, Subscription};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Settings as SettingsService, Snapshot};
use omuzaki_settings_api::setting::Setting;

use omuzaki_l10n_api::{Catalog, Locale, SHIPPED, negotiate, os_locales};

use super::script_fonts::ScriptFonts;
use super::strings::{CatalogStrings, English, Strings};
use super::theme::{Settings, apply_settings, set_strings};
use super::tokens::{Density, Scale, ThemeKind};

/// The area the four keys share.
pub const AREA: &str = "ui";

const fn taste(key: &'static str, summary: &'static str) -> SettingMeta {
    SettingMeta::new(key, summary, SettingScope::User, AgentWrite::Free, InProfile::Member, Apply::Live, 1)
}

/// `ui.theme`: dark, light or high contrast.
pub struct UiTheme;

impl Setting for UiTheme {
    type Value = ThemeKind;
    const META: SettingMeta = taste("ui.theme", "The studio's colours: dark, light or high contrast");

    fn default() -> ThemeKind {
        Settings::default().theme
    }
}

register_setting!(UiTheme);

/// `ui.density`: how tightly rows are packed.
pub struct UiDensity;

impl Setting for UiDensity {
    type Value = Density;
    const META: SettingMeta = taste("ui.density", "How tightly controls are packed: comfortable, compact or touch");

    fn default() -> Density {
        Settings::default().density
    }
}

register_setting!(UiDensity);

/// A UI scale in percent: one of [`Scale::STEPS`]. Stored as the bare number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
#[schemars(extend("enum" = [75, 90, 100, 110, 125, 150, 175, 200], "x-ui" = { "unit": "%" }))]
pub struct ScalePercent(pub u16);

/// `ui.scale`: the UI's size in percent, on top of the system's.
pub struct UiScale;

impl Setting for UiScale {
    type Value = ScalePercent;
    const META: SettingMeta = taste("ui.scale", "The studio's size in percent, on top of the system's: 75 to 200");

    fn default() -> ScalePercent {
        ScalePercent(Settings::default().scale.percent())
    }

    fn check(value: &ScalePercent) -> Result<(), SettingError> {
        SettingError::range(Self::META.key, value.0, 75, 200)?;
        if Scale::STEPS.contains(&value.0) {
            return Ok(());
        }
        let steps: Vec<String> = Scale::STEPS.iter().map(u16::to_string).collect();
        Err(SettingError::invalid(Self::META.key, format!("one of the steps {}", steps.join(", "))))
    }
}

register_setting!(UiScale);

/// `ui.reduce_motion`: no animation, and still markers in place of pulses.
pub struct UiReduceMotion;

impl Setting for UiReduceMotion {
    type Value = bool;
    const META: SettingMeta = taste("ui.reduce_motion", "Turn off animation; show still markers in place of pulses");

    fn default() -> bool {
        Settings::default().reduce_motion
    }
}

register_setting!(UiReduceMotion);

/// The language the studio speaks: `system` (the computer's, negotiated from its preference list) or a shipped
/// language's tag. Each option's title is the language's own name, which reads the same in every language.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
#[schemars(extend("oneOf" = [
    { "const": "system" },
    { "const": "en", "title": "English" },
    { "const": "ja", "title": "日本語" },
    { "const": "ko", "title": "한국어" },
    { "const": "zh-Hans", "title": "简体中文" },
    { "const": "es-419", "title": "Español (Latinoamérica)" },
    { "const": "pt-BR", "title": "Português (Brasil)" },
]))]
pub struct Language(pub String);

impl Language {
    /// The computer's language.
    pub const SYSTEM: &'static str = "system";

    /// The locale this choice speaks: a shipped language as chosen, or the computer's preference list negotiated.
    /// Traditional Chinese preferences give English until it ships, never Simplified Chinese (design section 6).
    pub fn locale(&self) -> Locale {
        if self.0 == Self::SYSTEM {
            let preferences = os_locales();
            let preferences: Vec<&str> = preferences.iter().map(String::as_str).collect();
            return negotiate(&preferences);
        }
        SHIPPED.iter().find(|locale| locale.as_str() == self.0).cloned().unwrap_or(Locale::EN)
    }
}

/// `ui.language`: the language of the studio's words.
pub struct UiLanguage;

impl Setting for UiLanguage {
    type Value = Language;
    const META: SettingMeta = taste("ui.language", "The studio's language; Traditional Chinese is on its way");
    fn default() -> Language {
        Language(Language::SYSTEM.to_owned())
    }
    fn check(value: &Language) -> Result<(), SettingError> {
        if value.0 == Language::SYSTEM || SHIPPED.iter().any(|locale| locale.as_str() == value.0) {
            return Ok(());
        }
        let tags: Vec<&str> = SHIPPED.iter().map(Locale::as_str).collect();
        Err(SettingError::invalid(Self::META.key, format!("system or one of {}", tags.join(", "))))
    }
}
register_setting!(UiLanguage);

/// The table that speaks `locale`: English as written, or the language's Fluent catalog.
pub fn strings_for(locale: &Locale) -> Arc<dyn Strings> {
    if *locale == Locale::EN { Arc::new(English) } else { Arc::new(CatalogStrings::new(Catalog::new(locale.clone()))) }
}

impl Settings {
    /// The look the snapshot holds; a key it lacks or cannot read is its default. A scale between the fixed steps
    /// takes the nearest one.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let fallback = Self::default();
        Self {
            theme: snapshot.get::<UiTheme>().unwrap_or(fallback.theme),
            scale: snapshot.get::<UiScale>().map_or(fallback.scale, |percent| Scale::nearest(percent.0)),
            density: snapshot.get::<UiDensity>().unwrap_or(fallback.density),
            reduce_motion: snapshot.get::<UiReduceMotion>().unwrap_or(fallback.reduce_motion),
        }
    }
}

/// Whether `event` changes a `ui.*` key.
fn touches_ui(event: &SettingsEvent) -> bool {
    event.changes.iter().any(|change| change.key.split('.').next() == Some(AREA))
}

/// The design system following the settings service.
pub struct SettingsLink {
    service: Arc<dyn SettingsService>,
    events: Subscription<SettingsEvent>,
    applied: Option<Settings>,
    /// The language last applied.
    speaks: Option<Locale>,
    /// The faces a language's script needs, fetched when it is chosen.
    fonts: Option<ScriptFonts>,
}

impl std::fmt::Debug for SettingsLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsLink").field("applied", &self.applied).finish_non_exhaustive()
    }
}

impl SettingsLink {
    /// A link over `service`, subscribed now so no change made after this call is missed.
    pub fn new(service: Arc<dyn SettingsService>) -> Self {
        let events = service.subscribe();
        Self { service, events, applied: None, speaks: None, fonts: None }
    }

    /// The same link, fetching the faces a chosen language's script needs from `fonts` (#205).
    #[must_use]
    pub fn with_fonts(mut self, fonts: ScriptFonts) -> Self {
        self.fonts = Some(fonts);
        self
    }

    /// The script faces, when the link fetches them.
    pub const fn fonts(&mut self) -> Option<&mut ScriptFonts> {
        self.fonts.as_mut()
    }

    /// The look last applied, if any.
    pub const fn applied(&self) -> Option<Settings> {
        self.applied
    }

    /// The language last applied, if any.
    pub const fn speaks(&self) -> Option<&Locale> {
        self.speaks.as_ref()
    }

    /// Applies the settings to `ctx` on the first call and after any change to a `ui.*` key; otherwise does nothing.
    /// Call it once per frame, before drawing.
    pub fn sync(&mut self, ctx: &Context) {
        if let Some(fonts) = self.fonts.as_mut() {
            fonts.poll(ctx);
        }
        let mut changed = self.applied.is_none();
        while let Some(event) = self.events.next() {
            changed |= touches_ui(&event);
        }
        if !changed {
            return;
        }
        let snapshot = self.service.snapshot();
        let settings = Settings::from_snapshot(&snapshot);
        apply_settings(ctx, settings);
        self.applied = Some(settings);
        let locale = snapshot.get::<UiLanguage>().unwrap_or_else(|_| UiLanguage::default()).locale();
        if self.speaks.as_ref() != Some(&locale) {
            // Building a catalog parses the language's files: once per switch, never per frame.
            set_strings(ctx, strings_for(&locale));
            if let Some(fonts) = self.fonts.as_mut() {
                fonts.want(&locale);
            }
            self.speaks = Some(locale);
        }
    }
}
