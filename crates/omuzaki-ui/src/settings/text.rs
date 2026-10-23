//! The words the Settings window says (`docs/design/settings.md`, spec SE-03): messages in a table, never literals in
//! the drawing code, as `shell/text.rs` does for the shell. A setting's own words (its summary, the names of its
//! options) are its registration's English and go through [`title`] like a panel's title.

use egui::Context;

use crate::ds::strings::{fill, translated};
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
    Title = "ui-settings-title" => "Settings",
    Search = "ui-settings-search" => "Search settings",
    NoMatch = "ui-settings-no-match" => "No setting matches “{query}”",
    NoMatchLine = "ui-settings-no-match-line" => "Try another word, or clear the search to see every setting.",
    Unavailable = "ui-settings-unavailable" => "The settings can't be read",
    UnavailableLine = "ui-settings-unavailable-line" => "{reason}",
    NotSaved = "ui-settings-not-saved" => "Not saved: {reason}",
    NextStart = "ui-settings-next-start" => "Applies after restart",
    Machine = "ui-settings-machine" => "This computer",
    ChangedBy = "ui-settings-changed-by" => "Last changed by",
    AreaUi = "ui-settings-area-ui" => "Appearance",
    AreaInput = "ui-settings-area-input" => "Input",
    AreaPerform = "ui-settings-area-perform" => "Performing",
    AreaOutput = "ui-settings-area-output" => "Output",
    AreaObs = "ui-settings-area-obs" => "OBS",
    AreaPrivacy = "ui-settings-area-privacy" => "Privacy",
    Pages = "ui-settings-pages" => "Page",
    PageSettings = "ui-settings-page-settings" => "Settings",
    PageModels = "ui-settings-page-models" => "Models",
    PageConnectors = "ui-settings-page-connectors" => "Connectors",
    Connectors = "ui-settings-connectors" => "App",
    ObsTitle = "ui-settings-obs-title" => "OBS Studio",
    ObsUnavailable = "ui-settings-obs-unavailable" => "The OBS bridge isn't in this build",
    ObsNotAffiliated = "ui-settings-obs-not-affiliated" => "Not affiliated with or endorsed by the OBS Project.",
    ObsConnected = "ui-settings-obs-connected" => "Connected: OBS WebSocket {version}",
    ObsWhere = "ui-settings-obs-where" => "On {host}, port {port}",
    ObsConnecting = "ui-settings-obs-connecting" => "Connecting",
    ObsRetrying = "ui-settings-obs-retrying" => "Reconnecting: {reason}",
    ObsAuthFailed = "ui-settings-obs-auth-failed" => "Wrong or missing password",
    ObsAuthFailedLine = "ui-settings-obs-auth-failed-line" => "Enter OBS's WebSocket password, Save it, then Connect",
    ObsFailed = "ui-settings-obs-failed" => "Can't reach OBS: {reason}",
    ObsDisconnected = "ui-settings-obs-disconnected" => "Not connected",
    ObsUnencrypted = "ui-settings-obs-unencrypted" => "Not encrypted: use this only on a network you trust",
    ObsDisconnect = "ui-settings-obs-disconnect" => "Disconnect",
    ObsConnect = "ui-settings-obs-connect" => "Connect",
    ObsPort = "ui-settings-obs-port" => "Port",
    ObsPortHint = "ui-settings-obs-port-hint" => "4455",
    ObsPassword = "ui-settings-obs-password" => "Password",
    ObsPasswordHint = "ui-settings-obs-password-hint" => "OBS's WebSocket password",
    ObsPasswordEmpty = "ui-settings-obs-password-empty" => "Type the password first",
    ObsSavePassword = "ui-settings-obs-save-password" => "Save password",
    ObsPasswordSaved = "ui-settings-obs-password-saved" => "Password saved",
    ObsForgetPassword = "ui-settings-obs-forget-password" => "Forget",
    ObsNoKeychain = "ui-settings-obs-no-keychain" => "This build has no keychain to keep a password in: use OBS without one.",
    ObsStep1 = "ui-settings-obs-step-1" => "In OBS, open Tools, then WebSocket Server Settings",
    ObsStep2 = "ui-settings-obs-step-2" => "Turn on Enable WebSocket server",
    ObsStep3 = "ui-settings-obs-step-3" => "Click Show Connect Info for the port and the password",
    ObsStep4 = "ui-settings-obs-step-4" => "Enter them here, then Connect",
    ObsStep = "ui-settings-obs-step" => "{n}. {step}",
    ObsScenes = "ui-settings-obs-scenes" => "Scenes",
    ObsOnProgram = "ui-settings-obs-on-program" => "On program",
    ObsRemote = "ui-settings-obs-remote" => "OBS on another computer",
    ObsRemoteWarning = "ui-settings-obs-remote-warning" => "The link to another computer is not encrypted. The password itself is never sent, but everything else can be read on the network: do this only on a network you trust.",
    ObsAllowRemote = "ui-settings-obs-allow-remote" => "Allow OBS on another computer",
    ObsHost = "ui-settings-obs-host" => "Host",
    ObsHostHint = "ui-settings-obs-host-hint" => "The other computer's address, like 192.168.1.20",
    VtsTitle = "ui-settings-vts-title" => "VTube Studio",
    VtsUnavailable = "ui-settings-vts-unavailable" => "The VTube Studio bridge isn't in this build",
    VtsNotAffiliated = "ui-settings-vts-not-affiliated" => "Not affiliated with or endorsed by DenchiSoft or Live2D Inc. VTube Studio is a product of DenchiSoft.",
    VtsConnected = "ui-settings-vts-connected" => "Connected: VTube Studio {version}",
    VtsNoModel = "ui-settings-vts-no-model" => "No model loaded in VTube Studio",
    VtsModel = "ui-settings-vts-model" => "Model: {name}",
    VtsAwaiting = "ui-settings-vts-awaiting" => "Waiting for approval in VTube Studio",
    VtsConnecting = "ui-settings-vts-connecting" => "Connecting",
    VtsRevoked = "ui-settings-vts-revoked" => "Access revoked",
    VtsDenied = "ui-settings-vts-denied" => "Denied",
    VtsAllowAgain = "ui-settings-vts-allow-again" => "Allow access again in VTube Studio, then Connect",
    VtsFailed = "ui-settings-vts-failed" => "Can't reach VTube Studio: {reason}",
    VtsDisconnected = "ui-settings-vts-disconnected" => "Not connected",
    VtsDisconnect = "ui-settings-vts-disconnect" => "Disconnect",
    VtsConnect = "ui-settings-vts-connect" => "Connect",
    VtsPort = "ui-settings-vts-port" => "Port",
    VtsPortHint = "ui-settings-vts-port-hint" => "8001, or found automatically",
    VtsStep1 = "ui-settings-vts-step-1" => "Open VTube Studio and load your model",
    VtsStep2 = "ui-settings-vts-step-2" => "In its settings, turn on Allow Plugin API access",
    VtsStep3 = "ui-settings-vts-step-3" => "Click Connect here, then Allow in VTube Studio",
    VtsStep = "ui-settings-vts-step" => "{n}. {step}",
    VtsMapping = "ui-settings-vts-mapping" => "Expressions to hotkeys",
    VtsMappingNone = "ui-settings-vts-mapping-none" => "Nothing mapped yet: Suggest matches your expressions to the model's hotkeys by name.",
    VtsSuggested = "ui-settings-vts-suggested" => "Suggested",
    VtsRemove = "ui-settings-vts-remove" => "Remove",
    VtsConfirm = "ui-settings-vts-confirm" => "Confirm",
    VtsConfirmRow = "ui-settings-vts-confirm-row" => "Confirm {id}",
    VtsRemoveRow = "ui-settings-vts-remove-row" => "Remove {id}",
    VtsRuns = "ui-settings-vts-runs" => "runs {target}",
    VtsSuggest = "ui-settings-vts-suggest" => "Suggest",
    VtsConfirmAll = "ui-settings-vts-confirm-all" => "Confirm all",
    VtsAdvanced = "ui-settings-vts-advanced" => "Advanced: an expression file",
    VtsExpressionWarning = "ui-settings-vts-expression-warning" => "An expression file switched on by Omuzaki stays on if Omuzaki quits; a hotkey is safer.",
    VtsExpressionId = "ui-settings-vts-expression-id" => "Omuzaki expression",
    VtsExpressionFile = "ui-settings-vts-expression-file" => "Expression file",
    VtsExpressionIdHint = "ui-settings-vts-expression-id-hint" => "Omuzaki expression, like cry",
    VtsExpressionFileHint = "ui-settings-vts-expression-file-hint" => "Expression file, like Cry.exp3.json",
    VtsAddExpression = "ui-settings-vts-add-expression" => "Add",
    VtsParameters = "ui-settings-vts-parameters" => "Parameters to map in VTube Studio",
    VtsParametersLine = "ui-settings-vts-parameters-line" => "Omuzaki sends these. Map each to your model's parameter in VTube Studio's model settings.",
    VtsParameterCount = "ui-settings-vts-parameter-count" => "{count} parameters",
    VtsCopy = "ui-settings-vts-copy" => "Copy the names",
    VtsShowNames = "ui-settings-vts-show-names" => "The names, and the VTube Studio inputs",
    VtsInputs = "ui-settings-vts-inputs" => "VTube Studio inputs Omuzaki can feed",
    VtsInputOn = "ui-settings-vts-input-on" => "{name}: on",
    VtsInputOff = "ui-settings-vts-input-off" => "{name}: off",
    VtsConflict = "ui-settings-vts-conflict" => "{name} is owned by another plugin: Omuzaki adds to it instead of replacing it",
    ModelsNone = "ui-settings-models-none" => "No model slots in this build",
    ModelsNoneLine = "ui-settings-models-none-line" => "A slot appears here when a feature that runs a model is added.",
    ModelsUnavailable = "ui-settings-models-unavailable" => "The models can't be listed",
    Running = "ui-settings-models-running" => "Runs {name}",
    NothingRuns = "ui-settings-models-nothing-runs" => "Nothing runs: {without}",
    FellBack = "ui-settings-models-fell-back" => "Fell back to another model: {reason}",
    Shipped = "ui-settings-models-shipped" => "Ships with Omuzaki",
    Yours = "ui-settings-models-yours" => "Added by you",
    Catalogue = "ui-settings-models-catalogue" => "Update from Omuzaki",
    LicenceUnknown = "ui-settings-models-licence-unknown" => "Licence unknown",
    Licence = "ui-settings-models-licence" => "Licence: {licence}",
    Ready = "ui-settings-models-ready" => "Ready",
    InUse = "ui-settings-models-in-use" => "In use",
    NotDownloaded = "ui-settings-models-not-downloaded" => "Downloads when first used",
    NeedsAcceptance = "ui-settings-models-needs-acceptance" => "Waiting for you to accept its notice",
    NeedsConsent = "ui-settings-models-needs-consent" => "Needs the consent of the person it imitates",
    Disabled = "ui-settings-models-disabled" => "Disabled: {reason}",
    Withdrawn = "ui-settings-models-withdrawn" => "Withdrawn",
    SmokePassed = "ui-settings-models-smoke-passed" => "Smoke test passed",
    SmokeFailed = "ui-settings-models-smoke-failed" => "Smoke test failed: {checks}",
    NotFromUs = "ui-settings-models-not-from-us" => "Not from Omuzaki",
    Accept = "ui-settings-models-accept" => "Accept the notice",
    Use = "ui-settings-models-use" => "Use this model",
    UseShipped = "ui-settings-models-use-shipped" => "Back to the shipped model",
    Remove = "ui-settings-models-remove" => "Remove",
    RemoveAsk = "ui-settings-models-remove-ask" => "Remove {name}?",
    RemoveLine = "ui-settings-models-remove-line" => "Its copy leaves the model cache; your own file stays where you keep it.",
    Cancel = "ui-settings-models-cancel" => "Cancel",
    Drop = "ui-settings-models-drop" => "Drop a model file here to add it to this slot",
    Adding = "ui-settings-models-adding" => "Adding {name}: copying, checking and smoke-testing",
    AddFailed = "ui-settings-models-add-failed" => "{name} wasn't added: {reason}",
    SizeKb = "ui-settings-models-size-kb" => "{size} kB",
    SizeMb = "ui-settings-models-size-mb" => "{size} MB",
    SizeGb = "ui-settings-models-size-gb" => "{size} GB",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    fill(&translated(&*crate::ds::theme::ds(ctx).strings, word.id(), word.english()), args)
}

/// The section title of a settings area (`ui-settings-area-<area>`), or the area's name with a capital when this table has
/// none.
pub fn area_title(ctx: &Context, area: &str) -> String {
    let id = format!("ui-settings-area-{area}");
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
            assert!(word.id().starts_with("ui-settings-"), "{}", word.id());
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
