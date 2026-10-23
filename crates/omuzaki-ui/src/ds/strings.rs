//! The text the components say themselves (`design-system.md` section 12, spec SE-03).
//!
//! A component takes every label, hint and reason from its caller, who has the localised string. What is left is the
//! words a component adds on its own: "Dismiss notification" on a toast's close button, "Reset Width" on a slider's
//! reset button, "needs you" on an approval card. Those are messages in a table, never literals in the drawing code.
//!
//! The table is a [`Strings`] value kept in the egui context beside the theme ([`Ds`](super::theme::Ds)); a widget
//! reads it with [`Ds::text`](super::theme::Ds::text) and [`Ds::text_with`](super::theme::Ds::text_with). Every area's
//! words go through the same table by their ids ([`Strings::translate`]), and the drawing code's English constants
//! and registered titles find theirs in the [lexicon](super::lexicon). [`English`] is the default; [`CatalogStrings`]
//! speaks any shipped language from the Fluent files in `locales/` (design `l10n-ops.md`), falling back to English
//! for a word not translated yet, and the `ui.language` setting picks it ([`settings_keys`](super::settings_keys)).
//! Placeholders stay `{name}` and are filled by [`fill`] after the translation, so a value is never translated or
//! mangled. Plurals and locale-aware numbers in arguments are the next step: today a count is filled in as text.
//!
//! [`Pseudo`] wraps any table and turns each message into an accented, 40% longer version of itself. Running the
//! gallery in it is how CI proves no English is hard-coded: a string that still reads as plain ASCII did not come
//! from a table.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, PoisonError, RwLock};

use omuzaki_l10n_api::{Arg, Catalog, Text};
pub use omuzaki_l10n_api::{accent, pseudo};

/// The locale tag of the pseudo-locale (the conventional "English with accents" tag).
pub const PSEUDO_LOCALE: &str = "en-XA";

/// Declares [`Msg`] with, for each message, its Fluent id and its English text. One table is the single source of
/// truth for the enum, the ids and the English strings.
macro_rules! messages {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message a component says on its own. The doc line of each variant is its English text.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Msg {
            $($(#[$meta])* #[doc = concat!("`", $english, "`")] $name),+
        }

        impl Msg {
            /// Every message, for tests and for tools that export the table.
            pub const ALL: &'static [Self] = &[$(Self::$name),+];

            /// The Fluent message id: `ds-toast-dismiss`.
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

messages! {
    /// Neutral information.
    StatusInfo = "ds-status-info" => "Info",
    StatusOk = "ds-status-ok" => "OK",
    StatusWarning = "ds-status-warning" => "Warning",
    StatusError = "ds-status-error" => "Error",

    ActorPerson = "ds-actor-person" => "person",
    ActorAgent = "ds-actor-agent" => "agent",
    ActorPlugin = "ds-actor-plugin" => "plugin",
    ActorSystem = "ds-actor-system" => "system",
    ScopeOutside = "ds-scope-outside" => "Reaches outside the project",
    ScopeInside = "ds-scope-inside" => "Scope",
    CostEstimate = "ds-cost-estimate" => "Estimated cost: {parts}",
    GateFailed = "ds-gate-failed" => "{value}, below {threshold}, gate failed",
    GateAbove = "ds-gate-above" => "{value}, above {threshold}, gate failed",
    GatePassed = "ds-gate-passed" => "{value}, gate passed",
    GateOpens = "ds-gate-opens" => "Opens the evidence",
    /// Joins the parts of a spoken value ("0.62, default 0.50, modified"). Punctuation, left alone by the pseudo-locale.
    ListSeparator = "ds-list-separator" => ", ",

    OwnerTracker = "ds-owner-tracker" => "tracker",
    OwnerAudio = "ds-owner-audio" => "audio",
    OwnerAi = "ds-owner-ai" => "AI",
    OwnerLifeLayer = "ds-owner-life-layer" => "life layer",
    OwnerHotkey = "ds-owner-hotkey" => "hotkey",
    OwnerScore = "ds-owner-score" => "score",
    OwnerRest = "ds-owner-rest" => "resting",
    MeterRaw = "ds-meter-raw" => "raw {value}",
    MeterOwner = "ds-meter-owner" => "owner {owner}",
    MeterClipping = "ds-meter-clipping" => "clipping",
    MeterLowConfidence = "ds-meter-low-confidence" => "low confidence",

    RowVisible = "ds-row-visible" => "Visible",
    RowHidden = "ds-row-hidden" => "Hidden",
    RowLocked = "ds-row-locked" => "Locked",
    RowAgentTouched = "ds-row-agent-touched" => "Touched by an agent",
    RowGateWarning = "ds-row-gate-warning" => "Gate warning",
    RowRename = "ds-row-rename" => "Rename {name}",

    ToastDismiss = "ds-toast-dismiss" => "Dismiss notification",
    ToastRepeated = "ds-toast-repeated" => "{message}, repeated {count} times",
    /// What a screen reader hears for a field's error: the status word, then the message.
    FieldError = "ds-field-error" => "{status}: {message}",
    /// The same for a disabled field, followed by why it is disabled.
    FieldErrorDisabled = "ds-field-error-disabled" => "{status}: {message}. {reason}",
    FieldClear = "ds-field-clear" => "Clear {label}",

    ParamLocked = "ds-param-locked" => "Locked",
    ParamHeldBy = "ds-param-held-by" => "Held by {owner}",
    ParamResetTo = "ds-param-reset-to" => "Reset to {value}",
    ParamReset = "ds-param-reset" => "Reset {label}",
    ParamEdit = "ds-param-edit" => "Edit {label} value",
    ParamValue = "ds-param-value" => "{label} value",
    ParamRangeError = "ds-param-range-error" => "Enter a number from {min} to {max}",
    ParamDivideByZero = "ds-param-divide-by-zero" => "Can't divide by zero",
    ParamResetDefault = "ds-param-reset-default" => "Reset to default",
    ParamCopyValue = "ds-param-copy-value" => "Copy value",
    ParamPasteValue = "ds-param-paste-value" => "Paste value",
    ParamCopyOpCall = "ds-param-copy-op-call" => "Copy op call",
    ParamShowHistory = "ds-param-show-history" => "Show in History",
    ParamDefault = "ds-param-default" => "default {value}",
    ParamModified = "ds-param-modified" => "modified",
    ParamAtDefault = "ds-param-at-default" => "at default",
    ParamMixed = "ds-param-mixed" => "mixed values",
    ParamLockedBy = "ds-param-locked-by" => "locked by {owner}",
    /// The letter on the badge that marks a value overridden in the current variant. One letter in a 14 pt badge: no
    /// room to grow, so the pseudo-locale leaves it alone.
    ParamVariantBadge = "ds-param-variant-badge" => "V",

    ColourLinkTo = "ds-colour-link-to" => "Link to {slot}",
    ColourRecent = "ds-colour-recent" => "Recent colour {number}",
    ColourSatVal = "ds-colour-sat-val" => "Saturation and value",
    ColourSatValValue = "ds-colour-sat-val-value" => "saturation {saturation}%, value {value}%",
    ColourHue = "ds-colour-hue" => "Hue",
    ColourHueValue = "ds-colour-hue-value" => "{degrees} degrees",
    ColourAlpha = "ds-colour-alpha" => "Alpha",
    ColourAlphaValue = "ds-colour-alpha-value" => "{percent}%",
    ColourHex = "ds-colour-hex" => "Hex colour",
    ColourPick = "ds-colour-pick" => "Pick",
    ColourPickName = "ds-colour-pick-name" => "Pick a colour from the screen",
    ColourChannel = "ds-colour-channel" => "Colour {channel}",
    ColourContrast = "ds-colour-contrast" => "value contrast {ratio} with {slot}",
    ColourLinkedValue = "ds-colour-linked-value" => "{hex}, linked to {slot}",

    ApprovalNeedsYou = "ds-approval-needs-you" => "needs you",
    ApprovalGroup = "ds-approval-group" => "{agent} needs you: {what}",
    ApprovalReversible = "ds-approval-reversible" => "reversible: yes",
    ApprovalIrreversible = "ds-approval-irreversible" => "Can't be undone",
    ApprovalAsksAgain = "ds-approval-asks-again" => "asks again",
    ApprovalChanges = "ds-approval-changes" => "Changes:",
    ApprovalChangesName = "ds-approval-changes-name" => "Changes",
    /// Only used for two or more requests.
    ApprovalSimilar = "ds-approval-similar" => "{count} similar requests",
    ApprovalAll = "ds-approval-all" => "Approve all {count}",
    ApprovalAllName = "ds-approval-all-name" => "Approve all {count} similar requests",
    ApprovalAccept = "ds-approval-accept" => "Accept",
    ApprovalAcceptName = "ds-approval-accept-name" => "Accept the answer for {agent}",
    ApprovalDecline = "ds-approval-decline" => "Decline",
    ApprovalCancel = "ds-approval-cancel" => "Cancel",
    ApprovalConfirm = "ds-approval-confirm" => "Confirm",
    ApprovalApproveAsk = "ds-approval-approve-ask" => "Approve…",
    ApprovalApprovePlan = "ds-approval-approve-plan" => "Approve plan",
    ApprovalApprove = "ds-approval-approve" => "Approve",
    ApprovalSession = "ds-approval-session" => "Approve for this session",
    ApprovalProject = "ds-approval-project" => "Approve for this project",
    ApprovalReject = "ds-approval-reject" => "Reject…",
    ApprovalRejectName = "ds-approval-reject-name" => "Reject",
    /// The spoken name of a button: what it does, what it is about, who asked.
    ApprovalAction = "ds-approval-action" => "{action}: {what} ({agent})",
    ApprovalReasonName = "ds-approval-reason-name" => "Reason for rejecting",
    ApprovalReasonHint = "ds-approval-reason-hint" => "Tell {agent} why (it keeps working)",
    ApprovalRejectTell = "ds-approval-reject-tell" => "Reject and tell {agent}",
    ApprovalReasonFirst = "ds-approval-reason-first" => "Type a reason first",
    ApprovalRejectStop = "ds-approval-reject-stop" => "Reject and stop",
    ApprovalBack = "ds-approval-back" => "Back",

    FormEssentials = "ds-form-essentials" => "Essentials",
    FormAdvanced = "ds-form-advanced" => "Advanced",
    /// The badge on the collapsed Advanced section. Only used for two or more fields.
    FormMore = "ds-form-more" => "{count} more",
    FormUnsupported = "ds-form-unsupported" => "Not editable here ({type})",
    FormPatternError = "ds-form-pattern-error" => "Doesn't match the pattern {pattern}",
    FormLengthBetween = "ds-form-length-between" => "Use {min} to {max} characters",
    FormLengthMin = "ds-form-length-min" => "Use at least {min} characters",
    FormLengthMax = "ds-form-length-max" => "Use at most {max} characters",

    DropdownChoose = "ds-dropdown-choose" => "Choose…",

    TabClose = "ds-tab-close" => "Close {label}",
    TabAgentActive = "ds-tab-agent-active" => "an agent is working here",

    DialogCancel = "ds-dialog-cancel" => "Cancel",

    ThumbLoading = "ds-thumb-loading" => "Loading preview",
    ThumbMissing = "ds-thumb-missing" => "No preview",
    ThumbPreviewing = "ds-thumb-previewing" => "Previewing",

    JobCancel = "ds-job-cancel" => "Cancel",
    JobCancelName = "ds-job-cancel-name" => "Cancel {title}",
    JobRetry = "ds-job-retry" => "Retry",
    JobRetryName = "ds-job-retry-name" => "Retry {title}",
    JobQueued = "ds-job-queued" => "Waiting",
    JobWorking = "ds-job-working" => "Working",
    JobDone = "ds-job-done" => "Done",
    JobFailed = "ds-job-failed" => "Failed",
    JobElapsed = "ds-job-elapsed" => "{time} elapsed",
    JobState = "ds-job-state" => "{state}, {detail}",

    StepRewind = "ds-step-rewind" => "Rewind",
    StepExplain = "ds-step-explain" => "Explain",
    StepAdjust = "ds-step-adjust" => "Adjust",
    StepApprove = "ds-step-approve" => "Approve",
    StepReject = "ds-step-reject" => "Reject",
    StepRestore = "ds-step-restore" => "Restore",
    StepActionName = "ds-step-action-name" => "{action}: {label}",
    StepApplied = "ds-step-applied" => "Applied",
    StepPending = "ds-step-pending" => "Pending review",
    StepRejected = "ds-step-rejected" => "Rejected",
    StepGateFailed = "ds-step-gate-failed" => "Gate failed",
    StepBefore = "ds-step-before" => "Before",
    StepAfter = "ds-step-after" => "After",
    StepEvidence = "ds-step-evidence" => "Evidence",
    StepGroup = "ds-step-group" => "{actor}: {label}",
    MetricBetter = "ds-metric-better" => "improved",
    MetricWorse = "ds-metric-worse" => "worse",
    MetricValue = "ds-metric-value" => "{metric} {delta}, {change}",

    CurvePreset = "ds-curve-preset" => "Preset",
    CurveCustom = "ds-curve-custom" => "Custom",
    CurveLinear = "ds-curve-linear" => "Linear",
    CurveEaseIn = "ds-curve-ease-in" => "Ease in",
    CurveEaseOut = "ds-curve-ease-out" => "Ease out",
    CurveSCurve = "ds-curve-s-curve" => "S-curve",
    CurveDeadZone = "ds-curve-dead-zone" => "Dead zone",
    CurveInverted = "ds-curve-inverted" => "Inverted",
    CurveStep = "ds-curve-step" => "Step",
    CurveInput = "ds-curve-input" => "Input",
    CurveOutput = "ds-curve-output" => "Output",
    CurveTangent = "ds-curve-tangent" => "Tangent",
    CurveSummary = "ds-curve-summary" => "{label}: curve with {count} points. Input {x} gives {y}.",
    CurvePoint = "ds-curve-point" => "{label}, point {index} of {count}",
    CurvePointValue = "ds-curve-point-value" => "input {x}, output {y}",
    CurveEndPinned = "ds-curve-end-pinned" => "The first and last points stay at the ends of the axis",

    /// Key legends and format hints are conventions, not prose: the pseudo-locale leaves them as they are.
    HexHint = "ds-hex-hint" => "#RRGGBB",

    ThemeDark = "ds-theme-dark" => "Dark",
    ThemeLight = "ds-theme-light" => "Light",
    ThemeHighContrast = "ds-theme-high-contrast" => "High contrast",
    DensityComfortable = "ds-density-comfortable" => "Comfortable",
    DensityCompact = "ds-density-compact" => "Compact",
    DensityTouch = "ds-density-touch" => "Touch",
}

impl Msg {
    /// A key legend, a format hint, a one-letter badge or a list separator: a convention rather than prose, so [`Pseudo`] does not
    /// touch it.
    pub const fn is_verbatim(self) -> bool {
        matches!(self, Self::HexHint | Self::ParamVariantBadge | Self::ListSeparator)
    }
}

/// Declares a table of words as `&str` constants holding their English, which the drawing code passes to its `say`,
/// and two more constants for the [lexicon](super::lexicon): `ALL`, each constant's name beside its English, and
/// `PREFIX`, which an id starts with (`ui-panels-outliner` for `OUTLINER`).
macro_rules! lines {
    (prefix = $prefix:literal; $($(#[$meta:meta])* $vis:vis const $name:ident: &str = $text:expr;)+) => {
        $($(#[$meta])* $vis const $name: &str = $text;)+

        /// Every word of the table, by its constant's name, for the lexicon.
        pub(crate) const ALL: &[(&str, &str)] = &[$((stringify!($name), $name)),+];

        /// What the ids of this table's words start with.
        pub(crate) const PREFIX: &str = $prefix;
    };
}
pub(crate) use lines;

/// The message table of one language.
pub trait Strings: Debug + Send + Sync {
    /// The BCP 47 tag of the language ("en", "ja", [`PSEUDO_LOCALE`]).
    fn locale(&self) -> &str;

    /// The text of the message `id`, whose English is `english`, in this language, with its `{name}` placeholders in
    /// place for [`fill`]. Every message the studio says goes through here: the components' [`Msg`]s, each area's
    /// words, and registered English such as a panel's title ([`lexicon`](super::lexicon) gives those their ids).
    fn translate<'a>(&'a self, id: &str, english: &'a str) -> Cow<'a, str>;

    /// The pattern of `msg` with its `{name}` placeholders in place. A [`verbatim`](Msg::is_verbatim) message is the
    /// same in every language.
    fn pattern(&self, msg: Msg) -> Cow<'_, str> {
        if msg.is_verbatim() { Cow::Borrowed(msg.english()) } else { self.translate(msg.id(), msg.english()) }
    }
}

/// The English table, and the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct English;

impl Strings for English {
    fn locale(&self) -> &'static str {
        "en"
    }

    fn translate<'a>(&'a self, _: &str, english: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(english)
    }
}

/// Any table, made unreadable on purpose: accented letters and 40% more length, so a hard-coded string stands out as
/// plain ASCII and a layout that cannot take longer text breaks in front of a developer instead of a translator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pseudo<S>(pub S);

impl<S: Strings> Strings for Pseudo<S> {
    fn locale(&self) -> &str {
        PSEUDO_LOCALE
    }

    fn translate<'a>(&'a self, id: &str, english: &'a str) -> Cow<'a, str> {
        Cow::Owned(pseudo(&self.0.translate(id, english)))
    }
}

/// A language's table over its Fluent catalog (`omuzaki-l10n-api`, design `l10n-ops.md`): the translation when the
/// language has one, else the English. Each message's pattern is formatted once, with every placeholder given back
/// as itself, and kept, so drawing a frame looks a message up without formatting anything.
pub struct CatalogStrings {
    catalog: Catalog,
    tag: String,
    patterns: RwLock<HashMap<String, Arc<str>>>,
}

impl CatalogStrings {
    /// The table of `catalog`'s language.
    pub fn new(catalog: Catalog) -> Self {
        let tag = catalog.locale().as_str().to_owned();
        Self { catalog, tag, patterns: RwLock::new(HashMap::new()) }
    }

    /// The catalog it reads, for its report of missing text.
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

impl Debug for CatalogStrings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogStrings").field("locale", &self.tag).finish_non_exhaustive()
    }
}

impl Strings for CatalogStrings {
    fn locale(&self) -> &str {
        &self.tag
    }

    fn translate<'a>(&'a self, id: &str, english: &'a str) -> Cow<'a, str> {
        if let Some(pattern) = self.patterns.read().unwrap_or_else(PoisonError::into_inner).get(id) {
            return Cow::Owned(pattern.to_string());
        }
        // Each placeholder renders as itself, so the result is a pattern that `fill` completes like the English one.
        let args =
            placeholders(english).map(|name| (Cow::Owned(name.to_owned()), Arg::Verbatim(format!("{{{name}}}"))));
        let text = Text { id: Cow::Owned(id.to_owned()), args: args.collect() };
        let pattern: Arc<str> = self.catalog.render_stored(&text, english).into();
        let mut patterns = self.patterns.write().unwrap_or_else(PoisonError::into_inner);
        let pattern = Arc::clone(patterns.entry(id.to_owned()).or_insert(pattern));
        Cow::Owned(pattern.to_string())
    }
}

/// `english`, whose message id is `id`, in `strings`' language: borrowed when that is the English itself.
pub fn translated<'a>(strings: &dyn Strings, id: &str, english: &'a str) -> Cow<'a, str> {
    match strings.translate(id, english) {
        Cow::Borrowed(text) if std::ptr::eq(text, english) => Cow::Borrowed(english),
        other => Cow::Owned(other.into_owned()),
    }
}

/// English the drawing code holds (a table's constant, a registered title) in `strings`' language, found by its id in
/// the [lexicon](super::lexicon).
pub fn local<'a>(strings: &dyn Strings, english: &'a str) -> Cow<'a, str> {
    if strings.locale() == "en" {
        return Cow::Borrowed(english);
    }
    translated(strings, &super::lexicon::id_of(english), english)
}

/// The names of the `{name}` placeholders of `pattern`, in order.
pub fn placeholders(pattern: &str) -> impl Iterator<Item = &str> {
    pattern.split('{').skip(1).filter_map(|part| {
        let name = part.split_once('}')?.0;
        let ident = name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        ident.then_some(name)
    })
}

/// Fills the `{name}` placeholders of `pattern` from `args`. A placeholder with no argument stays as written, which
/// makes a missing argument visible on screen instead of silently blank.
pub fn fill(pattern: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
            out.push_str(value);
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_fluent_style_and_every_english_text_is_set() {
        let mut seen = std::collections::HashSet::new();
        for msg in Msg::ALL {
            let id = msg.id();
            assert!(seen.insert(id), "duplicate message id {id}");
            assert!(id.starts_with("ds-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
            assert!(!msg.english().is_empty(), "{id} has no English text");
        }
    }

    #[test]
    fn placeholders_are_filled_and_a_missing_argument_stays_visible() {
        assert_eq!(fill("Reset {label}", &[("label", "Width")]), "Reset Width");
        assert_eq!(fill("{a} then {b}", &[("a", "1")]), "1 then {b}");
        assert_eq!(fill("no placeholders", &[]), "no placeholders");
        assert_eq!(fill("open {brace", &[]), "open {brace");
    }

    #[test]
    fn pseudo_leaves_format_hints_alone_and_fills_like_the_original() {
        let table = Pseudo(English);
        assert_eq!(table.pattern(Msg::HexHint), "#RRGGBB");
        let filled = fill(&table.pattern(Msg::ParamReset), &[("label", "Largeur")]);
        assert!(filled.contains("Largeur") && !filled.contains('{'), "{filled}");
    }
}
