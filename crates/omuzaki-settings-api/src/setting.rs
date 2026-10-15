//! The [`Setting`] trait: a typed key.

use std::fmt;

use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::SettingError;
use crate::meta::SettingMeta;

/// A typed setting. The marker type is the key: `settings.get::<UiScale>()`. Declare it next to the feature that owns
/// it and call [`register_setting!`](crate::register_setting) (nothing edits a central list, §39 rule 2).
///
/// ```
/// use omuzaki_settings_api::error::SettingError;
/// use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
/// use omuzaki_settings_api::setting::Setting;
///
/// /// UI scale in percent, 75 to 200. Agents may set it: appearance only.
/// pub struct UiScale;
/// impl Setting for UiScale {
///     type Value = u16;
///     const META: SettingMeta = SettingMeta::new(
///         "ui.scale", "UI scale in percent",
///         SettingScope::User, AgentWrite::Free, InProfile::Member, Apply::Live, 1,
///     );
///     fn default() -> u16 { 100 }
///     fn check(v: &u16) -> Result<(), SettingError> { SettingError::range("ui.scale", *v, 75, 200) }
/// }
/// assert!(UiScale::check(&900).is_err());
/// ```
pub trait Setting: 'static {
    /// The value. Its JSON Schema (with `x-ui.*` annotations, `shell.md` §7) generates the Settings page and the
    /// agent's schema. It must never be a secret: hold a [`SecretRef`](crate::secret::SecretRef) instead.
    type Value: Clone + PartialEq + fmt::Debug + Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static;

    /// Static facts about the key.
    const META: SettingMeta;

    /// Earlier keys this one replaces (a rename), newest first.
    const RENAMED_FROM: &'static [&'static str] = &[];

    /// The default. Always valid, so a missing or invalid stored value falls back to it.
    fn default() -> Self::Value;

    /// Rules a type cannot say (ranges, cross-field). Runs on every write and on load.
    ///
    /// # Errors
    /// A [`SettingError`] naming the rule. The registry puts this setting's key on it whatever key the error carries,
    /// so `SettingError::range("ui.scale", ..)` and an error built with an empty key read the same to a caller.
    fn check(_value: &Self::Value) -> Result<(), SettingError> {
        Ok(())
    }

    /// Brings a value stored at version `from` (below `META.version`) to the current shape.
    ///
    /// # Errors
    /// A [`SettingError`] when the old value cannot be carried over (the key then reads as its default). Re-keyed to
    /// this setting like [`check`](Self::check)'s.
    fn migrate(_from: u32, old: Value) -> Result<Value, SettingError> {
        Ok(old)
    }
}
