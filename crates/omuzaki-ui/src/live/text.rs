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
    LatencyUnknown = "live-latency-unknown" => "Shown once the first camera frame reaches the picture",
    LatencyDetail = "live-latency-detail" => "Camera to picture: the tracker's median plus the wait for the frame. The camera's own transport delay is not included.",
    NoOp = "live-no-op" => "Not available: this build has no {op} op",
    NoPerformer = "live-no-performer" => "The performer is not running",

    StageLabel = "live-stage-label" => "Stage preview: the program frame",
    StageStarting = "live-stage-starting" => "Starting the stage",

    CharacterSection = "live-character-section" => "Character",
    CharacterLabel = "live-character-label" => "Character",
    CharacterKit = "live-character-kit" => "Soft SD (kit default)",
    DropVrm = "live-drop-vrm" => "Drop a VRM here to import it",
    Imported = "live-imported" => "Imported {name}",

    CameraSection = "live-camera-section" => "Camera",
    CameraLabel = "live-camera-label" => "Camera",
    CameraNone = "live-camera-none" => "No camera chosen",
    CameraGone = "live-camera-gone" => "{camera} (not connected)",
    CameraTrack = "live-camera-track" => "Track my face",
    CameraChooseFirst = "live-camera-choose-first" => "Choose a camera first",
    CameraTracking = "live-camera-tracking" => "Tracking from {name}",
    CameraUnplugged = "live-camera-unplugged" => "Camera unplugged",
    CameraRetry = "live-camera-retry" => "Retry",
    CameraAllow = "live-camera-allow" => "Allow the camera",
    CameraDenied = "live-camera-denied" => "Camera access is off: allow it in the system settings",
    NoCameraHeadline = "live-no-camera-headline" => "No camera found",
    NoCameraLine = "live-no-camera-line" => "The life layer keeps your character moving until a camera tracks you.",

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
    HotkeyRemoveShort = "live-hotkey-remove-short" => "Remove",
    HotkeyWaiting = "live-hotkey-waiting" => "Press a key…",
    HotkeyPress = "live-hotkey-press" => "Press a key for {action}; Escape cancels",
    HotkeyNothingBound = "live-hotkey-nothing-bound" => "No key is bound",
    HotkeyEveryMood = "live-hotkey-every-mood" => "Every mood has a row",
    HotkeyMissing = "live-hotkey-missing" => "Not on this character",
    HotkeyShowing = "live-hotkey-showing" => "Showing",
    ExpressionHappy = "live-expression-happy" => "Happy",
    ExpressionSurprised = "live-expression-surprised" => "Surprised",
    ExpressionAngry = "live-expression-angry" => "Angry",
    ExpressionSad = "live-expression-sad" => "Sad",
    ExpressionRelaxed = "live-expression-relaxed" => "Relaxed",

    OutputSection = "live-output-section" => "Output",
    OutputWindow = "live-output-window" => "Output window",
    OutputBackdrop = "live-output-backdrop" => "Backdrop",
    OutputKey = "live-output-key" => "Key colour",
    OutputTransparent = "live-output-transparent" => "Transparent",
    OutputOnTop = "live-output-on-top" => "Keep on top",
    OutputSize = "live-output-size" => "{width} × {height} px",
    OutputOverKey = "live-output-over-key" => "{size} over {key}",
    OutputOverNothing = "live-output-over-nothing" => "{size}, transparent",
    OutputHint = "live-output-hint" => "Capture it as the window “{title}”",
    OutputFallback = "live-output-fallback" => "fallback: key colour, no transparent surface here",
    OutputMinimised = "live-output-minimised" => "Minimised: it can't be captured",

    HealthSection = "live-health-section" => "Health",
    HealthFps = "live-health-fps" => "{fps} fps",
    HealthLatency = "live-health-latency" => "Latency",
    HealthLatencyValue = "live-health-latency-value" => "Latency: {ms} ms",
    HealthNoFrames = "live-health-no-frames" => "No frames yet",
    HealthTracking = "live-health-tracking" => "Face found",
    HealthLost = "live-health-lost" => "Face lost",
    HealthLostLine = "live-health-lost-line" => "The life layer moves your character until the camera finds you again.",
    HealthUnknown = "live-health-unknown" => "Waiting for the performer",
    OpFailed = "live-op-failed" => "{op} did not run: {reason}",

    ChannelsTitle = "live-channels-title" => "Channels",
    ChannelsFace = "live-channels-face" => "Face",
    ChannelsHead = "live-channels-head" => "Head",
    ChannelsBody = "live-channels-body" => "Body",
    ChannelsGroup = "live-channels-group" => "Channel group",
    ChannelsSearch = "live-channels-search" => "Search channels",
    ChannelsGrid = "live-channels-grid" => "{group} channels",
    ChannelsNone = "live-channels-none" => "No channel matches",
    ChannelsNoneLine = "live-channels-none-line" => "Try another word, or clear the search.",
    ChannelsIdle = "live-channels-idle" => "No channels yet",
    ChannelsIdleLine = "live-channels-idle-line" => "The meters move once the performer runs.",
    ChannelsLegend = "live-channels-legend" => "Fill: what the character shows. Icon: who drives it.",
    ChannelCustom = "live-channel-custom" => "Custom {n}",

    StripItem = "live-strip-item" => "{name}  {key}",
    StripPress = "live-strip-press" => "Show {name} ({key})",
}

/// The names the Channels panel shows, by the channel's stable name (`omuzaki_anim_api::Channel::name`). The custom
/// slots are named from [`Word::ChannelCustom`].
const CHANNEL_NAMES: &[(&str, &str)] = &[
    ("browDownLeft", "Brow down L"),
    ("browDownRight", "Brow down R"),
    ("browInnerUp", "Brow inner up"),
    ("browOuterUpLeft", "Brow outer up L"),
    ("browOuterUpRight", "Brow outer up R"),
    ("cheekPuff", "Cheek puff"),
    ("cheekSquintLeft", "Cheek squint L"),
    ("cheekSquintRight", "Cheek squint R"),
    ("eyeBlinkLeft", "Blink L"),
    ("eyeBlinkRight", "Blink R"),
    ("eyeLookDownLeft", "Look down L"),
    ("eyeLookDownRight", "Look down R"),
    ("eyeLookInLeft", "Look in L"),
    ("eyeLookInRight", "Look in R"),
    ("eyeLookOutLeft", "Look out L"),
    ("eyeLookOutRight", "Look out R"),
    ("eyeLookUpLeft", "Look up L"),
    ("eyeLookUpRight", "Look up R"),
    ("eyeSquintLeft", "Eye squint L"),
    ("eyeSquintRight", "Eye squint R"),
    ("eyeWideLeft", "Eye wide L"),
    ("eyeWideRight", "Eye wide R"),
    ("jawForward", "Jaw forward"),
    ("jawLeft", "Jaw left"),
    ("jawOpen", "Jaw open"),
    ("jawRight", "Jaw right"),
    ("mouthClose", "Mouth close"),
    ("mouthDimpleLeft", "Dimple L"),
    ("mouthDimpleRight", "Dimple R"),
    ("mouthFrownLeft", "Frown L"),
    ("mouthFrownRight", "Frown R"),
    ("mouthFunnel", "Mouth funnel"),
    ("mouthLeft", "Mouth left"),
    ("mouthLowerDownLeft", "Lower lip down L"),
    ("mouthLowerDownRight", "Lower lip down R"),
    ("mouthPressLeft", "Lip press L"),
    ("mouthPressRight", "Lip press R"),
    ("mouthPucker", "Pucker"),
    ("mouthRight", "Mouth right"),
    ("mouthRollLower", "Roll lower lip"),
    ("mouthRollUpper", "Roll upper lip"),
    ("mouthShrugLower", "Shrug lower lip"),
    ("mouthShrugUpper", "Shrug upper lip"),
    ("mouthSmileLeft", "Smile L"),
    ("mouthSmileRight", "Smile R"),
    ("mouthStretchLeft", "Stretch L"),
    ("mouthStretchRight", "Stretch R"),
    ("mouthUpperUpLeft", "Upper lip up L"),
    ("mouthUpperUpRight", "Upper lip up R"),
    ("noseSneerLeft", "Sneer L"),
    ("noseSneerRight", "Sneer R"),
    ("tongueOut", "Tongue out"),
    ("headYaw", "Turn"),
    ("headPitch", "Nod"),
    ("headRoll", "Tilt"),
    ("headShiftX", "Shift sideways"),
    ("headShiftY", "Shift up"),
    ("lean", "Lean"),
    ("bodyTurn", "Body turn"),
    ("breath", "Breath"),
    ("eyeArcLeft", "Happy eye L"),
    ("eyeArcRight", "Happy eye R"),
    ("eyesHeart", "Heart eyes"),
    ("eyesStar", "Star eyes"),
    ("eyesGloom", "Gloom eyes"),
    ("eyesShock", "Shock eyes"),
    ("eyesSqueeze", "Squeezed eyes"),
    ("eyesSpiral", "Spiral eyes"),
    ("sparkle", "Sparkle"),
    ("tears", "Tears"),
    ("sweatDrop", "Sweat drop"),
    ("blush", "Blush"),
    ("fangs", "Fangs"),
];

/// The name the Channels panel shows for the channel whose stable name is `stable` ("Blink L" for `eyeBlinkLeft`), in
/// the language the design system speaks. A custom slot is "Custom 1" to "Custom 16"; a name the table lacks is shown
/// as it is, since it names the channel in every language.
pub fn channel_name(ctx: &Context, stable: &str) -> String {
    if let Some(n) = stable.strip_prefix("custom").and_then(|n| n.parse::<u32>().ok()) {
        return say(ctx, Word::ChannelCustom, &[("n", &(n + 1).to_string())]);
    }
    let Some((_, english)) = CHANNEL_NAMES.iter().find(|(name, _)| *name == stable) else {
        return stable.to_owned();
    };
    if ds(ctx).strings.locale() == PSEUDO_LOCALE { pseudo(english) } else { (*english).to_owned() }
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
    fn every_channel_has_a_name_of_its_own() {
        let ctx = Context::default();
        apply_settings(&ctx, Settings::default());
        let mut seen = std::collections::HashSet::new();
        for info in omuzaki_anim_api::CHANNELS {
            let name = channel_name(&ctx, info.name);
            assert_ne!(name, info.name, "{} has no name in the table", info.name);
            assert!(seen.insert(name.clone()), "two channels are called {name}");
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
