//! The V0 settings that survive a restart (SE-10, ADR 0026; V0 exit 9): what a person sets for the live performance
//! comes back the next time the app starts, with nothing to set again.
//!
//! | Key | Scope | Agent | Profile | Holds |
//! |---|---|---|---|---|
//! | `input.source` | Machine | Confirm | Excluded | the kind of source ([`InputSource`](super::input::InputSource)) |
//! | `input.camera` | Machine | Never | Excluded | the bound camera ([`InputCamera`](super::input::InputCamera)) |
//! | `perform.retarget` | User | Free | Member | the smoothing dial, exaggeration, head gain and mirror ([`PerformRetarget`]) |
//! | `perform.triggers` | User | Free | Member | the expression hotkeys ([`PerformTriggers`]) |
//!
//! The neutral-face calibrations are not settings. ADR 0026 classes them as *state*, with a store of their own
//! (`perform/calibration.toml` in the local data folder, [`CalibrationStore`]): an app writes one as a side effect of
//! calibrating, there is no op to set it and no default. [`restore`] reads the store it is given and [`save_calibration`]
//! writes it.
//!
//! The two input keys are written by the `input.*` ops themselves when the app has a settings service. The perform
//! keys are written by [`save`], which the app calls after a `perform.*` or `trigger.*` op succeeds and at exit: those
//! ops change the live performer only, so the performer can run without a store (the tests, the CLI) and V0 does not
//! wait for the `settings.*` ops. [`restore`] puts everything back at start.
//!
//! # Invariants
//!
//! 1. **Saving never loses a decision already made.** What [`save`] and [`save_calibration`] write is the live state
//!    *once every request made so far has run* ([`PerformHost::requested_retarget`], [`PerformHost::requested_calibration`],
//!    the hotkeys as bound), not what the performer has applied by now. A request is queued and applied on the next
//!    performer step, so saving in between with the applied view would write the previous value back.
//! 2. **Saving never writes over what could not be read.** A calibration file that cannot be read at all is left
//!    alone ([`CalibrationStore`] refuses to write), and an entry that cannot be used is written back as it was.
//! 3. **Restoring fits the performer's queue.** The performer takes [`COMMAND_QUEUE_LEN`](super::host::COMMAND_QUEUE_LEN) requests between two of its
//!    steps and refuses the rest. [`restore`] makes three: the dials, the whole set of hotkeys as one request
//!    ([`PerformHost::bind_triggers`]) and the calibration, however many hotkeys there are. Nothing it asks is dropped
//!    by a full queue; a refusal is reported in [`Restored::problems`].
//! 4. **Restoring never fails the start, and never hides what it lost.** A stored value that cannot be used reads as
//!    its default and is returned in [`Restored::findings`], naming its key; a file that cannot be read at all (a
//!    finding with no key) is returned there too, as is anything wrong in the calibration store
//!    ([`Restored::calibration_findings`]). The person is told why their settings are not back.
//! 5. **Never another character's or device's calibration.** [`restore`] applies the calibration saved for exactly the
//!    character and device class it is asked for.
//!
//! The output window's own key, `output.window` (Machine scope, written by `output.window.open`), is read here too:
//! [`restore`] hands back what was saved as [`Restored::output_window`], and the app opens the window with it. This
//! module does not write it, and does not depend on the crate that owns its type: the store validates the value, so a
//! bad one is a finding and `None`.

use omuzaki_anim::MAX_TRIGGERS;
use omuzaki_anim_api::DeviceProfile;
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::{SettingError, SettingFinding};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt, Source};
use omuzaki_settings_api::setting::Setting;
use omuzaki_track::calibration::{CalibrationError, CalibrationFinding, CalibrationStore};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::host::{PerformHost, RetargetChange};
use super::input::{InputCamera, InputError, InputHost, InputSource};
use super::triggers::{TriggerBinding, check_key, same_key};
use crate::ops::perform::set_retarget::{EXAGGERATION, HEAD_GAIN, SMOOTHING};

/// The keys V0 restores, so [`restore`] reports only findings about them (and the file-level ones, which name no key).
const KEYS: [&str; 5] = ["input.source", "input.camera", "perform.retarget", "perform.triggers", "output.window"];

/// The key the output window saves its last open request under (`output.window.open`, `omuzaki-stream`).
const OUTPUT_WINDOW: &str = "output.window";

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

/// What [`restore`] did, so the app can tell the person what did not come back.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Restored {
    /// Stored values of the V0 keys that were not used, each naming its key (the key read as its default), and the
    /// files that could not be read at all, which name no key (everything in them read as its default).
    pub findings: Vec<SettingFinding>,
    /// The camera that was bound but is not connected now. The binding is kept in the settings; the source runs
    /// without a capture until the camera is back or another is chosen.
    pub missing_camera: Option<CameraId>,
    /// Whether a saved calibration for this character and device was applied.
    pub calibration: bool,
    /// How many hotkeys were bound again.
    pub triggers: usize,
    /// The output window's last open request as saved (`output.window`), for the app to open the window with; `None`
    /// when nothing was saved or the stored value was not usable (then a finding names the key).
    pub output_window: Option<omuzaki_doc_api::Value>,
    /// What in the calibration file could not be used: entries kept as they were and not rewritten by a later save,
    /// or the whole file, which is then left alone (see [`CalibrationStore::findings`]).
    pub calibration_findings: Vec<CalibrationFinding>,
    /// What could not be applied, in words: a performer that is gone, a queue that is full.
    pub problems: Vec<String>,
}

/// Puts the saved settings back: the retarget dials, the hotkeys, the calibration saved for `character` on `profile`
/// (from `calibrations`), and the input source and camera. Never fails; see [`Restored`] for what did not come back.
///
/// Hotkeys whose expression the character does not have are bound anyway and listed as missing by `trigger.list`, so
/// they work again when the expression returns.
pub fn restore(
    host: &dyn PerformHost,
    input: &dyn InputHost,
    settings: &dyn Settings,
    calibrations: &CalibrationStore,
    character: &str,
    profile: DeviceProfile,
) -> Restored {
    // A finding with no key is a whole file that did not parse or could not be read: every key in it read as its
    // default, and the person has to be told why (invariant 4).
    let findings = settings
        .findings()
        .into_iter()
        .filter(|f| f.error.key.is_empty() || KEYS.contains(&f.error.key.as_str()))
        .collect();
    let mut problems = Vec::new();
    let mut note = |what: &str, error: &dyn std::fmt::Display| problems.push(format!("{what}: {error}"));

    // Three requests in all, whatever the stored values: the dials, the hotkeys as one, the calibration (invariant 3).
    let prefs = settings.get::<PerformRetarget>().unwrap_or_default();
    if let Err(error) = host.set_retarget(prefs.change()) {
        note("the retarget dials", &error);
    }

    let mut bound = 0;
    let hotkeys = settings.get::<PerformTriggers>().unwrap_or_default();
    if !hotkeys.is_empty() {
        match host.bind_triggers(hotkeys) {
            Ok(outcomes) => bound = outcomes.len(),
            Err(error) => note("the hotkeys", &error),
        }
    }

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
    if let Some(camera) = settings.get::<InputCamera>().unwrap_or_default().id {
        match input.bind_camera(Some(camera.clone()), Mode::Apply) {
            Ok(_) => {}
            Err(InputError::NoCamera { .. }) => missing_camera = Some(camera),
            Err(error) => note("the camera", &error),
        }
    }

    let snapshot = settings.snapshot();
    let output_window = match snapshot.source(OUTPUT_WINDOW) {
        Some(Source::Default) | None => None,
        Some(_) => snapshot.value(OUTPUT_WINDOW).cloned(),
    };
    Restored {
        findings,
        missing_camera,
        calibration: calibrated,
        triggers: bound,
        output_window,
        calibration_findings: calibrations.findings().to_vec(),
        problems,
    }
}

/// Writes the live retarget dials and hotkeys to the settings: what [`restore`] reads. Call it after a `perform.*` or
/// `trigger.*` op succeeds and at exit. The dials saved are the ones requested so far, applied or not (invariant 1).
///
/// # Errors
/// The first [`SettingError`] the store refuses; nothing after it is written.
pub fn save(host: &dyn PerformHost, settings: &dyn Settings, by: &Actor) -> Result<(), SettingError> {
    let requested = host.requested_retarget();
    let prefs = RetargetPrefs {
        smoothing: requested.smoothing,
        exaggeration: requested.exaggeration,
        head_gain: requested.head_gain,
        mirror: requested.mirror,
    };
    settings.set::<PerformRetarget>(&prefs, Layer::Auto, Mode::Apply, by)?;
    let bindings: Vec<TriggerBinding> = host.triggers().into_iter().map(|info| info.binding).collect();
    settings.set::<PerformTriggers>(&bindings, Layer::Auto, Mode::Apply, by)?;
    Ok(())
}

/// Saves the host's calibration for `character` (under the device class it was made for) in `calibrations`, which
/// writes its file, and keeps the calibrations of every other character and device. The calibration saved is the one
/// requested so far, applied or not (invariant 1).
///
/// Every other entry is written back as it was read, an unusable one included (invariant 2).
///
/// # Errors
/// [`CalibrationError`] when the calibration or the character id cannot be kept, when the stored calibrations could
/// not be read at all (nothing is written then), or when the write fails.
pub fn save_calibration(
    host: &dyn PerformHost,
    calibrations: &mut CalibrationStore,
    character: &str,
) -> Result<(), CalibrationError> {
    calibrations.save(character, host.requested_calibration())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_and_mirror_is_on() {
        assert_eq!(PerformRetarget::check(&PerformRetarget::default()), Ok(()));
        assert!(RetargetPrefs::default().mirror, "VTuber apps mirror the webcam");
        assert_eq!(PerformTriggers::check(&PerformTriggers::default()), Ok(()));
    }

    #[test]
    fn out_of_range_dials_are_refused_naming_the_key() {
        let prefs = RetargetPrefs { smoothing: 4.0, ..RetargetPrefs::default() };
        let error = PerformRetarget::check(&prefs).unwrap_err();
        assert_eq!(error.key, "perform.retarget");
        assert!(error.to_string().contains("smoothing"), "{error}");
    }
}
