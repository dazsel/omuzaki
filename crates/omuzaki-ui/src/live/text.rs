//! The words the Live workspace says (`docs/design/ux/shell.md` sections 12 and 13, spec SE-03): messages in a table,
//! never literals in the drawing code, as `shell/text.rs` does for the shell.
//!
//! The table follows the design system's locale: the workspace speaks the pseudo-locale exactly when the components
//! do. Message ids follow Fluent conventions (`live-calibrate`), so a Fluent-backed table can replace
//! [`Word::english`] without touching a caller.

use std::borrow::Cow;

use egui::Context;

use crate::ds::strings::{PSEUDO_LOCALE, fill, pseudo};
use crate::ds::theme::ds;

macro_rules! words {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message the Live workspace says. The doc line of each variant is its English text.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Word {
            $($(#[$meta])* #[doc = concat!("`", $english, "`")] $name),+
        }

        impl Word {
            /// Every message, for tests and tools that export the table.
            pub const ALL: &'static [Self] = &[$(Self::$name),+];

            /// The Fluent message id.
            pub const fn id(self) -> &'static str {
                match self { $(Self::$name => $id),+ }
            }

            /// The English pattern, with `{name}` placeholders.
            pub const fn english(self) -> &'static str {
                match self { $(Self::$name => $english),+ }
            }
        }
    };
}

words! {
    Title = "live-title" => "Live",
    NotYet = "live-not-yet" => "Not available yet",
    NotYetCamera = "live-not-yet-camera" => "Not available yet: choosing a camera arrives with the input ops",
    NotYetHotkeys = "live-not-yet-hotkeys" => "Not available yet: binding hotkeys arrives with the trigger ops",
    NotYetLatency = "live-not-yet-latency" => "Not available yet: the performer does not report latency",
    NoOp = "live-no-op" => "Not available: this build has no {op} op",
    NoPerformer = "live-no-performer" => "The performer is not running",

    CharacterSection = "live-character-section" => "Character",
    CharacterLabel = "live-character-label" => "Character",
    CharacterKit = "live-character-kit" => "Soft SD (kit default)",
    DropVrm = "live-drop-vrm" => "Drop a VRM here to import it",
    Imported = "live-imported" => "Imported {name}",

    CameraSection = "live-camera-section" => "Camera",
    CameraLabel = "live-camera-label" => "Camera",
    CameraNone = "live-camera-none" => "No camera chosen",
    CameraPreview = "live-camera-preview" => "Mirrored preview",
    NoCameraHeadline = "live-no-camera-headline" => "No camera found",
    NoCameraLine = "live-no-camera-line" => "The life layer keeps your character moving until a camera tracks you.",
    ChooseCamera = "live-choose-camera" => "Choose a camera",

    CalibrateSection = "live-calibrate-section" => "Calibration",
    Calibrate = "live-calibrate" => "Calibrate",
    CalibratePrompt = "live-calibrate-prompt" => "Relax your face and look at the camera for 10 seconds.",
    Calibrating = "live-calibrating" => "Calibrating: {frames} of {of} frames",
    CalibrateDone = "live-calibrate-done" => "Calibrated over {frames} frames",
    CalibrateFailed = "live-calibrate-failed" => "Calibration stopped: the face was lost ({got} of {need} frames)",
    CalibrateIdle = "live-calibrate-idle" => "Not calibrated yet",

    SmoothingSection = "live-smoothing-section" => "Smoothing",
    Smoothing = "live-smoothing" => "Smoothing",
    SmoothingCost = "live-smoothing-cost" => "More smoothing steadies the face and adds latency.",

    HotkeysSection = "live-hotkeys-section" => "Hotkeys",
    HotkeyTrigger = "live-hotkey-trigger" => "Trigger",
    HotkeyAction = "live-hotkey-action" => "Action",
    HotkeyUnbound = "live-hotkey-unbound" => "Not bound",
    HotkeyTriggerOf = "live-hotkey-trigger-of" => "{action} trigger: {key}",
    HotkeyAdd = "live-hotkey-add" => "Add hotkey",
    HotkeyRemove = "live-hotkey-remove" => "Remove {action}",
    ExpressionHappy = "live-expression-happy" => "Happy",
    ExpressionSurprised = "live-expression-surprised" => "Surprised",
    ExpressionAngry = "live-expression-angry" => "Angry",
    ExpressionSad = "live-expression-sad" => "Sad",

    OutputSection = "live-output-section" => "Output",
    OutputWindow = "live-output-window" => "Output window",
    OutputUnavailable = "live-output-unavailable" => "This build has no output window",
    OutputSize = "live-output-size" => "{width} × {height} px",
    OutputFallback = "live-output-fallback" => "fallback: key colour, no transparent surface here",
    OutputMinimised = "live-output-minimised" => "Minimised: a minimised window can't be captured",

    HealthSection = "live-health-section" => "Health",
    HealthFps = "live-health-fps" => "{fps} fps",
    HealthLatency = "live-health-latency" => "Latency",
    HealthNoFrames = "live-health-no-frames" => "No frames yet",
    HealthTracking = "live-health-tracking" => "Face found",
    HealthLost = "live-health-lost" => "Face lost: the life layer has the face",
    HealthUnknown = "live-health-unknown" => "Waiting for the performer",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    // The pattern goes through the pseudo-locale first: it leaves `{placeholders}` alone, so a value is never mangled.
    fill(&text(ctx, word), args)
}

/// The text of `word`, in the language the design system speaks.
pub fn text(ctx: &Context, word: Word) -> Cow<'static, str> {
    let english = word.english();
    if ds(ctx).strings.locale() == PSEUDO_LOCALE { Cow::Owned(pseudo(english)) } else { Cow::Borrowed(english) }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ds::strings::{English, Pseudo};
    use crate::ds::theme::{Settings, apply_settings, set_strings};

    #[test]
    fn ids_are_unique_fluent_style_and_every_english_text_is_set() {
        let mut seen = std::collections::HashSet::new();
        for word in Word::ALL {
            let id = word.id();
            assert!(seen.insert(id), "duplicate message id {id}");
            assert!(id.starts_with("live-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
            assert!(!word.english().is_empty(), "{id} has no English text");
        }
    }

    #[test]
    fn the_pseudo_locale_changes_the_words_but_not_the_values() {
        let ctx = Context::default();
        apply_settings(&ctx, Settings::default());
        assert_eq!(say(&ctx, Word::HealthFps, &[("fps", "30")]), "30 fps");
        set_strings(&ctx, Arc::new(Pseudo(English)));
        let pseudo = say(&ctx, Word::HealthFps, &[("fps", "30")]);
        assert!(pseudo.contains("30") && pseudo != "30 fps", "{pseudo}");
    }
}
