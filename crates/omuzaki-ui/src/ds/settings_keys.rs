//! How the UI looks, as settings (`docs/design/settings.md` D2, #949): `ui.theme`, `ui.density`, `ui.scale` and
//! `ui.reduce_motion`. Taste only, so an agent may set them (`AgentWrite::Free`), a profile may hold them, and they apply
//! at once. Their defaults are [`Settings::default`].
//!
//! [`SettingsLink`] keeps the design system in step with the settings service: it reads the four keys from the
//! service's snapshot once, then again only when a change event touches a `ui.*` key, and applies them. There is no
//! per-frame settings read: a frame with no event is one empty channel poll.

use std::sync::Arc;

use egui::Context;
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::event::{SettingsEvent, Subscription};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Settings as SettingsService, Snapshot};
use omuzaki_settings_api::setting::Setting;

use super::theme::{Settings, apply_settings};
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

/// `ui.scale`: the UI's size in percent, on top of the system's.
pub struct UiScale;

impl Setting for UiScale {
    type Value = u16;
    const META: SettingMeta = taste("ui.scale", "The studio's size in percent, on top of the system's: 75 to 200");

    fn default() -> u16 {
        Settings::default().scale.percent()
    }

    fn check(value: &u16) -> Result<(), SettingError> {
        SettingError::range(Self::META.key, *value, 75, 200)
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

impl Settings {
    /// The look the snapshot holds; a key it lacks or cannot read is its default. A scale between the fixed steps
    /// takes the nearest one.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let fallback = Self::default();
        Self {
            theme: snapshot.get::<UiTheme>().unwrap_or(fallback.theme),
            scale: snapshot.get::<UiScale>().map_or(fallback.scale, Scale::nearest),
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
        Self { service, events, applied: None }
    }

    /// The look last applied, if any.
    pub const fn applied(&self) -> Option<Settings> {
        self.applied
    }

    /// Applies the settings to `ctx` on the first call and after any change to a `ui.*` key; otherwise does nothing.
    /// Call it once per frame, before drawing.
    pub fn sync(&mut self, ctx: &Context) {
        let mut changed = self.applied.is_none();
        while let Some(event) = self.events.next() {
            changed |= touches_ui(&event);
        }
        if !changed {
            return;
        }
        let settings = Settings::from_snapshot(&self.service.snapshot());
        apply_settings(ctx, settings);
        self.applied = Some(settings);
    }
}
