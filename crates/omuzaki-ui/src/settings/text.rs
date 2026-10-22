//! The words the Settings window says (`docs/design/settings.md`, spec SE-03): messages in a table, never literals in
//! the drawing code, as `shell/text.rs` does for the shell. A setting's own words (its summary, the names of its
//! options) are its registration's English and go through [`title`] like a panel's title.

use egui::Context;

use crate::ds::strings::fill;
pub use crate::shell::text::title;

macro_rules! words {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message the Settings window says. The doc line of each variant is its English text.
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
    Title = "settings-title" => "Settings",
    Search = "settings-search" => "Search settings",
    NoMatch = "settings-no-match" => "No setting matches “{query}”",
    NoMatchLine = "settings-no-match-line" => "Try another word, or clear the search to see every setting.",
    Unavailable = "settings-unavailable" => "The settings can't be read",
    UnavailableLine = "settings-unavailable-line" => "{reason}",
    NotSaved = "settings-not-saved" => "Not saved: {reason}",
    NextStart = "settings-next-start" => "Applies after restart",
    Machine = "settings-machine" => "This computer",
    ChangedBy = "settings-changed-by" => "Last changed by",
    AreaUi = "settings-area-ui" => "Appearance",
    AreaInput = "settings-area-input" => "Input",
    AreaPerform = "settings-area-perform" => "Performing",
    AreaOutput = "settings-area-output" => "Output",
    AreaObs = "settings-area-obs" => "OBS",
    AreaPrivacy = "settings-area-privacy" => "Privacy",
    Pages = "settings-pages" => "Page",
    PageSettings = "settings-page-settings" => "Settings",
    PageModels = "settings-page-models" => "Models",
    PageConnectors = "settings-page-connectors" => "Connectors",
    Connectors = "settings-connectors" => "App",
    ObsTitle = "settings-obs-title" => "OBS Studio",
    ObsUnavailable = "settings-obs-unavailable" => "The OBS bridge isn't in this build",
    ObsNotAffiliated = "settings-obs-not-affiliated" => "Not affiliated with or endorsed by the OBS Project.",
    ObsConnected = "settings-obs-connected" => "Connected: OBS WebSocket {version}",
    ObsWhere = "settings-obs-where" => "On {host}, port {port}",
    ObsConnecting = "settings-obs-connecting" => "Connecting",
    ObsRetrying = "settings-obs-retrying" => "Reconnecting: {reason}",
    ObsAuthFailed = "settings-obs-auth-failed" => "Wrong or missing password",
    ObsAuthFailedLine = "settings-obs-auth-failed-line" => "Enter OBS's WebSocket password, Save it, then Connect",
    ObsFailed = "settings-obs-failed" => "Can't reach OBS: {reason}",
    ObsDisconnected = "settings-obs-disconnected" => "Not connected",
    ObsUnencrypted = "settings-obs-unencrypted" => "Not encrypted: use this only on a network you trust",
    ObsDisconnect = "settings-obs-disconnect" => "Disconnect",
    ObsConnect = "settings-obs-connect" => "Connect",
    ObsPort = "settings-obs-port" => "Port",
    ObsPortHint = "settings-obs-port-hint" => "4455",
    ObsPassword = "settings-obs-password" => "Password",
    ObsPasswordHint = "settings-obs-password-hint" => "OBS's WebSocket password",
    ObsPasswordEmpty = "settings-obs-password-empty" => "Type the password first",
    ObsSavePassword = "settings-obs-save-password" => "Save password",
    ObsPasswordSaved = "settings-obs-password-saved" => "Password saved",
    ObsForgetPassword = "settings-obs-forget-password" => "Forget",
    ObsNoKeychain = "settings-obs-no-keychain" => "This build has no keychain to keep a password in: use OBS without one.",
    ObsStep1 = "settings-obs-step-1" => "In OBS, open Tools, then WebSocket Server Settings",
    ObsStep2 = "settings-obs-step-2" => "Turn on Enable WebSocket server",
    ObsStep3 = "settings-obs-step-3" => "Click Show Connect Info for the port and the password",
    ObsStep4 = "settings-obs-step-4" => "Enter them here, then Connect",
    ObsStep = "settings-obs-step" => "{n}. {step}",
    ObsScenes = "settings-obs-scenes" => "Scenes",
    ObsOnProgram = "settings-obs-on-program" => "On program",
    ObsRemote = "settings-obs-remote" => "OBS on another computer",
    ObsRemoteWarning = "settings-obs-remote-warning" => "The link to another computer is not encrypted. The password itself is never sent, but everything else can be read on the network: do this only on a network you trust.",
    ObsAllowRemote = "settings-obs-allow-remote" => "Allow OBS on another computer",
    ObsHost = "settings-obs-host" => "Host",
    ObsHostHint = "settings-obs-host-hint" => "The other computer's address, like 192.168.1.20",
    VtsTitle = "settings-vts-title" => "VTube Studio",
    VtsUnavailable = "settings-vts-unavailable" => "The VTube Studio bridge isn't in this build",
    VtsNotAffiliated = "settings-vts-not-affiliated" => "Not affiliated with or endorsed by DenchiSoft or Live2D Inc. VTube Studio is a product of DenchiSoft.",
    VtsConnected = "settings-vts-connected" => "Connected: VTube Studio {version}",
    VtsNoModel = "settings-vts-no-model" => "No model loaded in VTube Studio",
    VtsModel = "settings-vts-model" => "Model: {name}",
    VtsAwaiting = "settings-vts-awaiting" => "Waiting for approval in VTube Studio",
    VtsConnecting = "settings-vts-connecting" => "Connecting",
    VtsRevoked = "settings-vts-revoked" => "Access revoked",
    VtsDenied = "settings-vts-denied" => "Denied",
    VtsAllowAgain = "settings-vts-allow-again" => "Allow access again in VTube Studio, then Connect",
    VtsFailed = "settings-vts-failed" => "Can't reach VTube Studio: {reason}",
    VtsDisconnected = "settings-vts-disconnected" => "Not connected",
    VtsDisconnect = "settings-vts-disconnect" => "Disconnect",
    VtsConnect = "settings-vts-connect" => "Connect",
    VtsPort = "settings-vts-port" => "Port",
    VtsPortHint = "settings-vts-port-hint" => "8001, or found automatically",
    VtsStep1 = "settings-vts-step-1" => "Open VTube Studio and load your model",
    VtsStep2 = "settings-vts-step-2" => "In its settings, turn on Allow Plugin API access",
    VtsStep3 = "settings-vts-step-3" => "Click Connect here, then Allow in VTube Studio",
    VtsStep = "settings-vts-step" => "{n}. {step}",
    VtsMapping = "settings-vts-mapping" => "Expressions to hotkeys",
    VtsMappingNone = "settings-vts-mapping-none" => "Nothing mapped yet: Suggest matches your expressions to the model's hotkeys by name.",
    VtsSuggested = "settings-vts-suggested" => "Suggested",
    VtsRemove = "settings-vts-remove" => "Remove",
    VtsConfirm = "settings-vts-confirm" => "Confirm",
    VtsConfirmRow = "settings-vts-confirm-row" => "Confirm {id}",
    VtsRemoveRow = "settings-vts-remove-row" => "Remove {id}",
    VtsRuns = "settings-vts-runs" => "runs {target}",
    VtsSuggest = "settings-vts-suggest" => "Suggest",
    VtsConfirmAll = "settings-vts-confirm-all" => "Confirm all",
    VtsAdvanced = "settings-vts-advanced" => "Advanced: an expression file",
    VtsExpressionWarning = "settings-vts-expression-warning" => "An expression file switched on by Omuzaki stays on if Omuzaki quits; a hotkey is safer.",
    VtsExpressionId = "settings-vts-expression-id" => "Omuzaki expression",
    VtsExpressionFile = "settings-vts-expression-file" => "Expression file",
    VtsExpressionIdHint = "settings-vts-expression-id-hint" => "Omuzaki expression, like cry",
    VtsExpressionFileHint = "settings-vts-expression-file-hint" => "Expression file, like Cry.exp3.json",
    VtsAddExpression = "settings-vts-add-expression" => "Add",
    VtsParameters = "settings-vts-parameters" => "Parameters to map in VTube Studio",
    VtsParametersLine = "settings-vts-parameters-line" => "Omuzaki sends these. Map each to your model's parameter in VTube Studio's model settings.",
    VtsParameterCount = "settings-vts-parameter-count" => "{count} parameters",
    VtsCopy = "settings-vts-copy" => "Copy the names",
    VtsShowNames = "settings-vts-show-names" => "The names, and the VTube Studio inputs",
    VtsInputs = "settings-vts-inputs" => "VTube Studio inputs Omuzaki can feed",
    VtsInputOn = "settings-vts-input-on" => "{name}: on",
    VtsInputOff = "settings-vts-input-off" => "{name}: off",
    VtsConflict = "settings-vts-conflict" => "{name} is owned by another plugin: Omuzaki adds to it instead of replacing it",
    ModelsNone = "settings-models-none" => "No model slots in this build",
    ModelsNoneLine = "settings-models-none-line" => "A slot appears here when a feature that runs a model is added.",
    ModelsUnavailable = "settings-models-unavailable" => "The models can't be listed",
    Running = "settings-models-running" => "Runs {name}",
    NothingRuns = "settings-models-nothing-runs" => "Nothing runs: {without}",
    FellBack = "settings-models-fell-back" => "Fell back to another model: {reason}",
    Shipped = "settings-models-shipped" => "Ships with Omuzaki",
    Yours = "settings-models-yours" => "Added by you",
    Catalogue = "settings-models-catalogue" => "Update from Omuzaki",
    LicenceUnknown = "settings-models-licence-unknown" => "Licence unknown",
    Licence = "settings-models-licence" => "Licence: {licence}",
    Ready = "settings-models-ready" => "Ready",
    InUse = "settings-models-in-use" => "In use",
    NotDownloaded = "settings-models-not-downloaded" => "Downloads when first used",
    NeedsAcceptance = "settings-models-needs-acceptance" => "Waiting for you to accept its notice",
    NeedsConsent = "settings-models-needs-consent" => "Needs the consent of the person it imitates",
    Disabled = "settings-models-disabled" => "Disabled: {reason}",
    Withdrawn = "settings-models-withdrawn" => "Withdrawn",
    SmokePassed = "settings-models-smoke-passed" => "Smoke test passed",
    SmokeFailed = "settings-models-smoke-failed" => "Smoke test failed: {checks}",
    NotFromUs = "settings-models-not-from-us" => "Not from Omuzaki",
    Accept = "settings-models-accept" => "Accept the notice",
    Use = "settings-models-use" => "Use this model",
    UseShipped = "settings-models-use-shipped" => "Back to the shipped model",
    Remove = "settings-models-remove" => "Remove",
    RemoveAsk = "settings-models-remove-ask" => "Remove {name}?",
    RemoveLine = "settings-models-remove-line" => "Its copy leaves the model cache; your own file stays where you keep it.",
    Cancel = "settings-models-cancel" => "Cancel",
    Drop = "settings-models-drop" => "Drop a model file here to add it to this slot",
    Adding = "settings-models-adding" => "Adding {name}: copying, checking and smoke-testing",
    AddFailed = "settings-models-add-failed" => "{name} wasn't added: {reason}",
    SizeKb = "settings-models-size-kb" => "{size} kB",
    SizeMb = "settings-models-size-mb" => "{size} MB",
    SizeGb = "settings-models-size-gb" => "{size} GB",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    fill(&title(ctx, word.english()), args)
}

/// The section title of a settings area (`settings-area-<area>`), or the area's name with a capital when this table has
/// none.
pub fn area_title(ctx: &Context, area: &str) -> String {
    let id = format!("settings-area-{area}");
    match Word::ALL.iter().find(|word| word.id() == id) {
        Some(word) => say(ctx, *word, &[]),
        None => title(ctx, &humanise(area)).into_owned(),
    }
}

/// A key segment or an option's value as words: `reduce_motion` and `high-contrast` read "Reduce motion" and
/// "High contrast".
pub fn humanise(id: &str) -> String {
    let spaced: String = id.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_english_text_is_set() {
        let mut seen = std::collections::HashSet::new();
        for word in Word::ALL {
            assert!(seen.insert(word.id()), "{} is used twice", word.id());
            assert!(word.id().starts_with("settings-"), "{}", word.id());
            assert!(!word.english().trim().is_empty(), "{} has no English", word.id());
        }
    }

    #[test]
    fn ids_read_as_words() {
        assert_eq!(humanise("reduce_motion"), "Reduce motion");
        assert_eq!(humanise("high-contrast"), "High contrast");
        assert_eq!(humanise(""), "");
    }
}
