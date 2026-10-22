//! The words the Live workspace says (`docs/design/ux/shell.md` sections 12 and 13, spec SE-03): messages in a table,
//! never literals in the drawing code, as `shell/text.rs` does for the shell.
//!
//! The table follows the design system's locale: the workspace speaks the pseudo-locale exactly when the components
//! do. Message ids follow Fluent conventions (`live-calibrate`), so a Fluent-backed table can replace
//! [`Word::english`] without touching a caller.

use std::borrow::Cow;

use egui::Context;

use crate::ds::strings::{fill, local, translated};
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
    Title = "ui-live-title" => "Live",
    LatencyUnknown = "ui-live-latency-unknown" => "Shown once the first camera frame reaches the picture",
    LatencyDetail = "ui-live-latency-detail" => "Camera to picture: the tracker's median plus the wait for the frame. The camera's own transport delay is not included.",
    NoOp = "ui-live-no-op" => "Not available: this build has no {op} op",
    NoPerformer = "ui-live-no-performer" => "The performer is not running",

    StageLabel = "ui-live-stage-label" => "Stage preview: the program frame",
    StageStarting = "ui-live-stage-starting" => "Starting the stage",

    CharacterSection = "ui-live-character-section" => "Character",
    CharacterLabel = "ui-live-character-label" => "Character",
    CharacterKit = "ui-live-character-kit" => "Soft SD (kit default)",
    DropVrm = "ui-live-drop-vrm" => "Drop a VRM here to import it",
    Imported = "ui-live-imported" => "Imported {name}",

    CameraSection = "ui-live-camera-section" => "Camera",
    CameraLabel = "ui-live-camera-label" => "Camera",
    CameraNone = "ui-live-camera-none" => "No camera chosen",
    CameraGone = "ui-live-camera-gone" => "{camera} (not connected)",
    CameraTrack = "ui-live-camera-track" => "Track my face",
    CameraChooseFirst = "ui-live-camera-choose-first" => "Choose a camera first",
    CameraTracking = "ui-live-camera-tracking" => "Tracking from {name}",
    CameraUnplugged = "ui-live-camera-unplugged" => "Camera unplugged",
    CameraRetry = "ui-live-camera-retry" => "Retry",
    CameraAllow = "ui-live-camera-allow" => "Allow the camera",
    CameraDenied = "ui-live-camera-denied" => "Camera access is off: allow it in the system settings",
    NoCameraHeadline = "ui-live-no-camera-headline" => "No camera found",
    NoCameraLine = "ui-live-no-camera-line" => "The life layer keeps your character moving until a camera tracks you.",

    CalibrateSection = "ui-live-calibrate-section" => "Calibration",
    Calibrate = "ui-live-calibrate" => "Calibrate",
    CalibratePrompt = "ui-live-calibrate-prompt" => "Relax your face and look at the camera for 10 seconds.",
    Calibrating = "ui-live-calibrating" => "Calibrating: {frames} of {of} frames",
    CalibrateDone = "ui-live-calibrate-done" => "Calibrated over {frames} frames",
    CalibrateFailed = "ui-live-calibrate-failed" => "Calibration stopped: the face was lost ({got} of {need} frames)",
    CalibrateIdle = "ui-live-calibrate-idle" => "Not calibrated yet",

    SmoothingSection = "ui-live-smoothing-section" => "Smoothing",
    Smoothing = "ui-live-smoothing" => "Smoothing",
    SmoothingCost = "ui-live-smoothing-cost" => "More smoothing steadies the face and adds latency.",

    HotkeysSection = "ui-live-hotkeys-section" => "Hotkeys",
    HotkeyTrigger = "ui-live-hotkey-trigger" => "Trigger",
    HotkeyAction = "ui-live-hotkey-action" => "Action",
    HotkeyUnbound = "ui-live-hotkey-unbound" => "Not bound",
    HotkeyTriggerOf = "ui-live-hotkey-trigger-of" => "{action} trigger: {key}",
    HotkeyAdd = "ui-live-hotkey-add" => "Add hotkey",
    HotkeyRemove = "ui-live-hotkey-remove" => "Remove {action}",
    HotkeyRemoveShort = "ui-live-hotkey-remove-short" => "Remove",
    HotkeyWaiting = "ui-live-hotkey-waiting" => "Press a key…",
    HotkeyPress = "ui-live-hotkey-press" => "Press a key for {action}; Escape cancels",
    HotkeyNothingBound = "ui-live-hotkey-nothing-bound" => "No key is bound",
    HotkeyEveryMood = "ui-live-hotkey-every-mood" => "Every mood has a row",
    HotkeyMissing = "ui-live-hotkey-missing" => "Not on this character",
    HotkeyShowing = "ui-live-hotkey-showing" => "Showing",
    ExpressionHappy = "ui-live-expression-happy" => "Happy",
    ExpressionSurprised = "ui-live-expression-surprised" => "Surprised",
    ExpressionAngry = "ui-live-expression-angry" => "Angry",
    ExpressionSad = "ui-live-expression-sad" => "Sad",
    ExpressionRelaxed = "ui-live-expression-relaxed" => "Relaxed",

    OutputSection = "ui-live-output-section" => "Output",
    OutputWindow = "ui-live-output-window" => "Output window",
    OutputBackdrop = "ui-live-output-backdrop" => "Backdrop",
    OutputKey = "ui-live-output-key" => "Key colour",
    OutputTransparent = "ui-live-output-transparent" => "Transparent",
    OutputOnTop = "ui-live-output-on-top" => "Keep on top",
    OutputSize = "ui-live-output-size" => "{width} × {height} px",
    OutputOverKey = "ui-live-output-over-key" => "{size} over {key}",
    OutputOverNothing = "ui-live-output-over-nothing" => "{size}, transparent",
    OutputHint = "ui-live-output-hint" => "Capture it as the window “{title}”",
    OutputFallback = "ui-live-output-fallback" => "fallback: key colour, no transparent surface here",
    OutputMinimised = "ui-live-output-minimised" => "Minimised: it can't be captured",
    OutputPlace = "ui-live-output-place" => "Show the character",
    OutputCapture = "ui-live-output-capture" => "For capture",
    OutputDesktop = "ui-live-output-desktop" => "On the desktop",
    OutputNoDesktop = "ui-live-output-no-desktop" => "This computer can't show the character on the desktop yet",
    OutputDesktopUnverified = "ui-live-output-desktop-unverified" => "Showing the character on the desktop needs a transparent window, which hasn't been verified on this system; a window for capture works",
    OutputDesktopHint = "ui-live-output-desktop-hint" => "Drag the character to move it",
    OutputOnDesktop = "ui-live-output-on-desktop" => "{size} on the desktop at {opacity}%",
    OutputOpacity = "ui-live-output-opacity" => "Opacity",
    OutputClicks = "ui-live-output-clicks" => "Clicks",
    ClicksOff = "ui-live-clicks-off" => "Window takes them",
    ClicksEmpty = "ui-live-clicks-empty" => "Empty pixels pass",
    ClicksAll = "ui-live-clicks-all" => "All pass through",
    ClicksAllNote = "ui-live-clicks-all-note" => "Every click passes through the character: turn it back here",

    HealthSection = "ui-live-health-section" => "Health",
    HealthFps = "ui-live-health-fps" => "{fps} fps",
    HealthLatency = "ui-live-health-latency" => "Latency",
    HealthLatencyValue = "ui-live-health-latency-value" => "Latency: {ms} ms",
    HealthNoFrames = "ui-live-health-no-frames" => "No frames yet",
    HealthTracking = "ui-live-health-tracking" => "Face found",
    HealthLost = "ui-live-health-lost" => "Face lost",
    HealthLostLine = "ui-live-health-lost-line" => "The life layer moves your character until the camera finds you again.",
    HealthUnknown = "ui-live-health-unknown" => "Waiting for the performer",
    HealthOff = "ui-live-health-off" => "No live readings",
    HealthOffLine = "ui-live-health-off-line" => "Health shows the stage, the camera and the audio while the live slice runs.",
    HealthCameraFps = "ui-live-health-camera-fps" => "Camera {fps} fps",
    HealthFrameRate = "ui-live-health-frame-rate" => "Frame rate",
    HealthFrameTime = "ui-live-health-frame-time" => "Frame time",
    HealthFrameTimeValue = "ui-live-health-frame-time-value" => "{ms} ms, worst {worst} ms",
    HealthMs = "ui-live-health-ms" => "{ms} ms",
    HealthPercent = "ui-live-health-percent" => "{n}%",
    HealthMb = "ui-live-health-mb" => "{n} MB",
    HealthNoFace = "ui-live-health-no-face" => "No face in use",
    HealthNotReporting = "ui-live-health-not-reporting" => "— not reporting",
    HealthNotApplicable = "ui-live-health-not-applicable" => "n/a",
    HealthLateFrames = "ui-live-health-late-frames" => "Late frames",
    HealthDroppedFrames = "ui-live-health-dropped-frames" => "Dropped frames",
    HealthTrackingDrops = "ui-live-health-tracking-drops" => "Tracking drops",
    HealthUnderruns = "ui-live-health-underruns" => "Audio underruns",
    HealthBudgets = "ui-live-health-budgets" => "Budgets",
    HealthNoBudgets = "ui-live-health-no-budgets" => "No budget readings yet",
    HealthBudgetLive = "ui-live-health-budget-live" => "{name} · live",
    HealthBudgetValue = "ui-live-health-budget-value" => "{value} of {budget} ({percent}%)",
    HealthOver = "ui-live-health-over" => "Over budget: {line}",
    HealthPartial = "ui-live-health-partial" => "Partial",
    HealthPartialDetail = "ui-live-health-partial-detail" => "Partial: measured from the tracker frame's capture to the frame's submit; the camera's exposure and the display are not included.",
    HealthLiveDetail = "ui-live-health-live-detail" => "The value right now against its budget, not the release gate's statistic.",
    HealthStoryState = "ui-live-health-story-state" => "State",
    HealthStoryIdle = "ui-live-health-story-idle" => "Idle",
    HealthStoryOver = "ui-live-health-story-over" => "Over budget",
    HealthStoryLost = "ui-live-health-story-lost" => "Tracking lost",
    BudgetGpuFrame = "ui-live-budget-gpu-frame" => "GPU frame",
    BudgetFacePaint = "ui-live-budget-face-paint" => "Face paint",
    BudgetTrackFace = "ui-live-budget-track-face" => "Face tracking",
    BudgetVoiceFx = "ui-live-budget-voice-fx" => "Voice effects",
    BudgetLatency = "ui-live-budget-latency" => "Latency",
    BudgetPhysics = "ui-live-budget-physics" => "Physics step",
    BudgetCpu = "ui-live-budget-cpu" => "CPU",
    BudgetRam = "ui-live-budget-ram" => "Memory",
    BudgetOther = "ui-live-budget-other" => "Budget",
    OpFailed = "ui-live-op-failed" => "{op} did not run: {reason}",

    ChannelsTitle = "ui-live-channels-title" => "Channels",
    ChannelsFace = "ui-live-channels-face" => "Face",
    ChannelsHead = "ui-live-channels-head" => "Head",
    ChannelsBody = "ui-live-channels-body" => "Body",
    ChannelsGroup = "ui-live-channels-group" => "Channel group",
    ChannelsSearch = "ui-live-channels-search" => "Search channels",
    ChannelsGrid = "ui-live-channels-grid" => "{group} channels",
    ChannelsNone = "ui-live-channels-none" => "No channel matches",
    ChannelsNoneLine = "ui-live-channels-none-line" => "Try another word, or clear the search.",
    ChannelsIdle = "ui-live-channels-idle" => "No channels yet",
    ChannelsIdleLine = "ui-live-channels-idle-line" => "The meters move once the performer runs.",
    ChannelsLegend = "ui-live-channels-legend" => "Fill: what the character shows. Icon: who drives it.",
    ChannelCustom = "ui-live-channel-custom" => "Custom {n}",

    StripItem = "ui-live-strip-item" => "{name}  {key}",
    StripPress = "ui-live-strip-press" => "Show {name} ({key})",
}

/// The names the Channels panel shows, by the channel's stable name (`omuzaki_anim_api::Channel::name`). The custom
/// slots are named from [`Word::ChannelCustom`].
pub(crate) const CHANNEL_NAMES: &[(&str, &str)] = &[
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
    local(&*ds(ctx).strings, english).into_owned()
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    // The pattern goes through the pseudo-locale first: it leaves `{placeholders}` alone, so a value is never mangled.
    fill(&text(ctx, word), args)
}

/// The text of `word`, in the language the design system speaks.
pub fn text(ctx: &Context, word: Word) -> Cow<'static, str> {
    translated(&*ds(ctx).strings, word.id(), word.english())
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
            assert!(id.starts_with("ui-live-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
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
