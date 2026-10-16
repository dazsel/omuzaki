//! The V0 settings that survive a restart (SE-10, ADR 0026; V0 exit 9): what a person sets for the live performance
//! comes back the next time the app starts, with nothing to set again.
//!
//! | Key | Scope | Agent | Profile | Holds |
//! |---|---|---|---|---|
//! | `input.source` | Machine | Confirm | Excluded | the kind of source ([`InputSource`](super::input::InputSource)) |
//! | `input.camera` | Machine | Never | Excluded | the bound camera ([`InputCamera`](super::input::InputCamera)) |
//! | `perform.retarget` | User | Free | Member | the smoothing dial, exaggeration, head gain and mirror ([`PerformRetarget`]) |
//! | `perform.triggers` | User | Free | Member | the expression hotkeys ([`PerformTriggers`]) |
//! | `perform.calibrations` | Machine | Confirm | Excluded | the neutral-face calibrations by character and device ([`PerformCalibrations`]) |
//!
//! The two input keys are written by the `input.*` ops themselves when the app has a settings service. The perform
//! keys are written by [`save`] and [`save_calibration`], which the app calls after a `perform.*` or `trigger.*` op
//! succeeds and at exit: those ops change the live performer only, so the performer can run without a store (the
//! tests, the CLI) and V0 does not wait for the `settings.*` ops. [`restore`] puts every key back at start.
//!
//! A stored value that cannot be used never fails the start. The store reads the key as its default and lists a
//! finding naming the key ([`Settings::findings`]); [`restore`] returns those findings for the V0 keys, and carries on
//! with the defaults. A calibration made for another character or another device class is never applied.
//!
//! Output (the borderless window's size, backdrop and key colour) is the output window's own settings and is
//! restored by the app beside these; nothing in this module reads it.

use omuzaki_anim::MAX_TRIGGERS;
use omuzaki_anim_api::{Calibration, DeviceProfile};
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::{SettingError, SettingFinding};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt};
use omuzaki_settings_api::setting::Setting;
use omuzaki_track::calibration::CalibrationStore;
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::host::{PerformHost, RetargetChange};
use super::input::{InputCamera, InputError, InputHost, InputSource};
use super::triggers::{TriggerBinding, check_key, same_key};
use crate::ops::perform::set_retarget::{EXAGGERATION, HEAD_GAIN, SMOOTHING};

/// The keys V0 restores, so [`restore`] reports only findings about them.
const KEYS: [&str; 5] =
    ["input.source", "input.camera", "perform.retarget", "perform.triggers", "perform.calibrations"];

/// The retarget's user-facing dials as saved. The library default keeps `mirror` off; a person on a webcam expects
/// the preview mirrored, so the saved default turns it on (lead ruling).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RetargetPrefs {
    /// The smoothing dial, `0` raw to `1` very smooth.
    pub smoothing: f32,
    /// Exaggeration, `1` literal.
    pub exaggeration: f32,
    /// Multiplier on head angles.
    pub head_gain: f32,
    /// Swap left and right, for a selfie-mirror preview.
    pub mirror: bool,
}

impl Default for RetargetPrefs {
    fn default() -> Self {
        let config = omuzaki_anim_api::RetargetConfig::default();
        Self {
            smoothing: config.smoothing,
            exaggeration: config.exaggeration,
            head_gain: config.head_gain,
            mirror: true,
        }
    }
}

impl RetargetPrefs {
    /// The change that puts every dial where these say.
    fn change(self) -> RetargetChange {
        RetargetChange {
            smoothing: Some(self.smoothing),
            exaggeration: Some(self.exaggeration),
            head_gain: Some(self.head_gain),
            mirror: Some(self.mirror),
        }
    }
}

/// The retarget dials, as the User setting `perform.retarget`. Taste, so an agent may set it.
pub struct PerformRetarget;

impl Setting for PerformRetarget {
    type Value = RetargetPrefs;
    const META: SettingMeta = SettingMeta::new(
        "perform.retarget",
        "How the face is smoothed, exaggerated and mirrored",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> RetargetPrefs {
        RetargetPrefs::default()
    }

    fn check(value: &RetargetPrefs) -> Result<(), SettingError> {
        let key = Self::META.key;
        for (name, value, range) in [
            ("smoothing", value.smoothing, &SMOOTHING),
            ("exaggeration", value.exaggeration, &EXAGGERATION),
            ("head_gain", value.head_gain, &HEAD_GAIN),
        ] {
            if !range.contains(&value) {
                return Err(SettingError::invalid(
                    key,
                    format!("{name} {value} is outside {} to {}", range.start(), range.end()),
                ));
            }
        }
        Ok(())
    }
}

register_setting!(PerformRetarget);

/// The expression hotkeys, as the User setting `perform.triggers`: at most 16, each key valid and none repeated.
pub struct PerformTriggers;

impl Setting for PerformTriggers {
    type Value = Vec<TriggerBinding>;
    const META: SettingMeta = SettingMeta::new(
        "perform.triggers",
        "The keys that show face expressions",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> Vec<TriggerBinding> {
        Vec::new()
    }

    fn check(value: &Vec<TriggerBinding>) -> Result<(), SettingError> {
        let key = Self::META.key;
        if value.len() > MAX_TRIGGERS {
            return Err(SettingError::invalid(key, format!("{} bindings; at most {MAX_TRIGGERS}", value.len())));
        }
        for (i, binding) in value.iter().enumerate() {
            check_key(&binding.key).map_err(|e| SettingError::invalid(key, format!("binding {i}: {e}")))?;
            if value[..i].iter().any(|earlier| same_key(&earlier.key, &binding.key)) {
                return Err(SettingError::invalid(key, format!("the key '{}' is bound twice", binding.key)));
            }
        }
        Ok(())
    }
}

register_setting!(PerformTriggers);

/// The neutral-face calibrations by character and device, as the Machine setting `perform.calibrations`. A
/// calibration fits one face on one camera, so it stays on this computer. Only a person recalibrates (an agent's write
/// is queued for confirmation).
pub struct PerformCalibrations;

impl Setting for PerformCalibrations {
    type Value = CalibrationStore;
    const META: SettingMeta = SettingMeta::new(
        "perform.calibrations",
        "The saved neutral-face calibrations, by character and device",
        SettingScope::Machine,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> CalibrationStore {
        CalibrationStore::new()
    }

    fn check(value: &CalibrationStore) -> Result<(), SettingError> {
        value.check().map_err(|e| SettingError::invalid(Self::META.key, e.to_string()))
    }
}

register_setting!(PerformCalibrations);

/// What [`restore`] did, so the app can tell the person what did not come back.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Restored {
    /// Stored values of the V0 keys that were not used, each naming its key. The key read as its default.
    pub findings: Vec<SettingFinding>,
    /// The camera that was bound but is not connected now. The binding is kept in the settings; the source runs
    /// without a capture until the camera is back or another is chosen.
    pub missing_camera: Option<CameraId>,
    /// Whether a saved calibration for this character and device was applied.
    pub calibration: bool,
    /// How many hotkeys were bound again.
    pub triggers: usize,
    /// What could not be applied, in words: a performer that is gone, a queue that is full.
    pub problems: Vec<String>,
}

/// Puts the saved settings back: the retarget dials, the hotkeys, the calibration saved for `character` on `profile`,
/// and the input source and camera. Never fails; see [`Restored`] for what did not come back.
///
/// Hotkeys whose expression the character does not have are bound anyway and listed as missing by `trigger.list`, so
/// they work again when the expression returns.
pub fn restore(
    host: &dyn PerformHost,
    input: &dyn InputHost,
    settings: &dyn Settings,
    character: &str,
    profile: DeviceProfile,
) -> Restored {
    let findings = settings.findings().into_iter().filter(|f| KEYS.contains(&f.error.key.as_str())).collect();
    let mut problems = Vec::new();
    let mut note = |what: &str, error: &dyn std::fmt::Display| problems.push(format!("{what}: {error}"));

    let prefs = settings.get::<PerformRetarget>().unwrap_or_default();
    if let Err(error) = host.set_retarget(prefs.change()) {
        note("the retarget dials", &error);
    }

    let mut bound = 0;
    for binding in settings.get::<PerformTriggers>().unwrap_or_default() {
        match host.bind_trigger(binding) {
            Ok(_) => bound += 1,
            Err(error) => note("a hotkey", &error),
        }
    }

    let calibrations = settings.get::<PerformCalibrations>().unwrap_or_default();
    let mut calibrated = false;
    if let Some(calibration) = calibrations.for_character(character, profile) {
        match host.set_calibration(calibration) {
            Ok(()) => calibrated = true,
            Err(error) => note("the calibration", &error),
        }
    }

    let source = settings.get::<InputSource>().unwrap_or_default();
    if let Err(error) = input.select(source, Mode::Apply) {
        note("the input source", &error);
    }
    let mut missing_camera = None;
    if let Some(camera) = settings.get::<InputCamera>().unwrap_or_default() {
        match input.bind_camera(Some(camera.clone()), Mode::Apply) {
            Ok(_) => {}
            Err(InputError::NoCamera { .. }) => missing_camera = Some(camera),
            Err(error) => note("the camera", &error),
        }
    }

    Restored { findings, missing_camera, calibration: calibrated, triggers: bound, problems }
}

/// Writes the live retarget dials and hotkeys to the settings: what [`restore`] reads. Call it after a `perform.*` or
/// `trigger.*` op succeeds and at exit. The dials saved are the ones the performer applied.
///
/// # Errors
/// The first [`SettingError`] the store refuses; nothing after it is written.
pub fn save(host: &dyn PerformHost, settings: &dyn Settings, by: &Actor) -> Result<(), SettingError> {
    let applied = host.status().retarget;
    let prefs = RetargetPrefs {
        smoothing: applied.smoothing,
        exaggeration: applied.exaggeration,
        head_gain: applied.head_gain,
        mirror: applied.mirror,
    };
    settings.set::<PerformRetarget>(&prefs, Layer::Auto, Mode::Apply, by)?;
    let bindings: Vec<TriggerBinding> = host.triggers().into_iter().map(|info| info.binding).collect();
    settings.set::<PerformTriggers>(&bindings, Layer::Auto, Mode::Apply, by)?;
    Ok(())
}

/// Saves the host's calibration for `character` (under the device class it was made for), keeping the calibrations of
/// every other character and device.
///
/// # Errors
/// [`SettingError`] when the calibration or the character id cannot be kept, or the store refuses the write.
pub fn save_calibration(
    host: &dyn PerformHost,
    settings: &dyn Settings,
    character: &str,
    by: &Actor,
) -> Result<(), SettingError> {
    let key = PerformCalibrations::META.key;
    let mut store = settings.get::<PerformCalibrations>()?;
    let calibration: Calibration = host.calibration();
    store.save(character, calibration).map_err(|e| SettingError::invalid(key, e.to_string()))?;
    settings.set::<PerformCalibrations>(&store, Layer::Auto, Mode::Apply, by)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_and_mirror_is_on() {
        assert_eq!(PerformRetarget::check(&PerformRetarget::default()), Ok(()));
        assert!(RetargetPrefs::default().mirror, "VTuber apps mirror the webcam");
        assert_eq!(PerformTriggers::check(&PerformTriggers::default()), Ok(()));
        assert_eq!(PerformCalibrations::check(&PerformCalibrations::default()), Ok(()));
    }

    #[test]
    fn out_of_range_dials_are_refused_naming_the_key() {
        let prefs = RetargetPrefs { smoothing: 4.0, ..RetargetPrefs::default() };
        let error = PerformRetarget::check(&prefs).unwrap_err();
        assert_eq!(error.key, "perform.retarget");
        assert!(error.to_string().contains("smoothing"), "{error}");
    }
}
