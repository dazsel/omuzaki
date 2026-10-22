//! The words the Broadcast workspace's automation panels say (`docs/design/automation.md` D11, D17): messages in a
//! table, never literals in the drawing code, as `live/text.rs` does for the Live workspace. Ids follow Fluent
//! conventions (`ui-broadcast-start`).

use std::borrow::Cow;

use egui::Context;

use crate::ds::strings::{fill, translated};
use crate::ds::theme::ds;

macro_rules! words {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message the Broadcast workspace says. The doc line of each variant is its English text.
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
    RulesTitle = "ui-broadcast-rules-title" => "Rules",
    GraphTitle = "ui-broadcast-graph-title" => "Graph",
    ValuesTitle = "ui-broadcast-values-title" => "Values",
    Refused = "ui-broadcast-refused" => "Not done: {reason}",
    ShowUnreadable = "ui-broadcast-show-unreadable" => "The show could not be read: {reason}",

    NoShow = "ui-broadcast-no-show" => "No rules yet",
    NoShowLine = "ui-broadcast-no-show-line" => "Rules react to chat, redeems and timers: when something happens, your character does something.",
    NoShowGraphLine = "ui-broadcast-no-show-graph-line" => "Make a show in Rules, then build its rules here as graphs.",
    MakeShow = "ui-broadcast-make-show" => "Make a show",
    FirstShowName = "ui-broadcast-first-show-name" => "Main",

    Running = "ui-broadcast-running" => "Running",
    Paused = "ui-broadcast-paused" => "Paused",
    Stopped = "ui-broadcast-stopped" => "Stopped",
    Ready = "ui-broadcast-ready" => "Ready",
    Off = "ui-broadcast-off" => "Off",
    On = "ui-broadcast-on" => "On",
    Suspended = "ui-broadcast-suspended" => "Suspended",
    NeedsApproval = "ui-broadcast-needs-approval" => "Needs your OK",
    Faulted = "ui-broadcast-faulted" => "{count} stopped by an error",
    Inert = "ui-broadcast-inert" => "Won't run",
    Armed = "ui-broadcast-armed" => "Ready to run",
    Start = "ui-broadcast-start" => "Start",
    Pause = "ui-broadcast-pause" => "Pause",
    Resume = "ui-broadcast-resume" => "Resume",
    Stop = "ui-broadcast-stop" => "Stop",
    OtherRunning = "ui-broadcast-other-running" => "The show {show} is running: stop it first",
    ResumeGraph = "ui-broadcast-resume-graph" => "Resume",

    ApprovalTitle = "ui-broadcast-approval-title" => "This show needs your OK",
    ApprovalLine = "ui-broadcast-approval-line" => "Until you allow these on this computer, the rules that need them won't run.",
    Allow = "ui-broadcast-allow" => "Allow on this machine",
    NotNow = "ui-broadcast-not-now" => "Not now",
    NoApprover = "ui-broadcast-no-approver" => "This build can't save approvals",
    SecretField = "ui-broadcast-secret-field" => "Secret {name}, sent only to {host}",
    SaveSecret = "ui-broadcast-save-secret" => "Save",
    TypeSecretFirst = "ui-broadcast-type-secret-first" => "Type the secret first",
    NoKeychain = "ui-broadcast-no-keychain" => "This build has no keychain to keep it in",

    NeedScope = "ui-broadcast-need-scope" => "Use the {scope} permission",
    NeedOp = "ui-broadcast-need-op" => "Run {op}: {summary}",
    NeedOpUnknown = "ui-broadcast-need-op-unknown" => "Run {op}",
    NeedHost = "ui-broadcast-need-host" => "Send to {address}",
    NeedFileRead = "ui-broadcast-need-file-read" => "Read the file {path}",
    NeedFileWrite = "ui-broadcast-need-file-write" => "Write the file {path}",
    NeedProgram = "ui-broadcast-need-program" => "Run the program {command} (this exact file, fingerprint {hash})",
    NeedProgramMissing = "ui-broadcast-need-program-missing" => "Run the program {command} (the file isn't there yet)",
    NeedSecret = "ui-broadcast-need-secret" => "Send the secret {name}, only to {host}",
    NeedProcess = "ui-broadcast-need-process" => "See whether {name} is running",
    NeedVariable = "ui-broadcast-need-variable" => "The variable {name}",
    NeedGroup = "ui-broadcast-need-group" => "The group {group}",

    NoGraphs = "ui-broadcast-no-graphs" => "This show has no rules yet",
    NoGraphsLine = "ui-broadcast-no-graphs-line" => "Add a rule with New rule, or ask an agent to build one.",
    Ungrouped = "ui-broadcast-ungrouped" => "Not in a group",
    GroupOn = "ui-broadcast-group-on" => "{group} starts on",
    When = "ui-broadcast-when" => "{trigger} → {steps}",
    NodeCount = "ui-broadcast-node-count" => "{count} steps",
    EditInline = "ui-broadcast-edit-inline" => "Edit",
    DoneEditing = "ui-broadcast-done-editing" => "Done",
    OpenEditor = "ui-broadcast-open-editor" => "Open in the editor",

    GraphPicker = "ui-broadcast-graph-picker" => "Rule",
    NewGraph = "ui-broadcast-new-graph" => "New rule",
    NewGraphName = "ui-broadcast-new-graph-name" => "New rule",
    AddNode = "ui-broadcast-add-node" => "Add node",
    FindNode = "ui-broadcast-find-node" => "Find a node",
    Close = "ui-broadcast-close" => "Close",
    Nodes = "ui-broadcast-nodes" => "Nodes",
    NothingSelected = "ui-broadcast-nothing-selected" => "Select a node to edit it.",
    GraphSuspended = "ui-broadcast-graph-suspended" => "This rule was suspended for looping too fast. Resume it in Values or Rules.",
    MoveNodes = "ui-broadcast-move-nodes" => "Move nodes",
    EditNode = "ui-broadcast-edit-node" => "Edit node",
    LastValue = "ui-broadcast-last-value" => "Last value: {value}",
    EventToValue = "ui-broadcast-event-to-value" => "An event can only go to an event input, and a value to a value input ({from} to {to})",
    TypesDontMatch = "ui-broadcast-types-dont-match" => "A {from} can't become a {to}",

    NodeLabel = "ui-broadcast-node-label" => "Label",
    Constants = "ui-broadcast-constants" => "Inputs with no wire",
    ConnectTo = "ui-broadcast-connect-to" => "Connect to…",
    Output = "ui-broadcast-output" => "Output",
    Input = "ui-broadcast-input" => "Input",
    Connect = "ui-broadcast-connect" => "Connect",
    NoTargets = "ui-broadcast-no-targets" => "No input of another node takes this output.",
    DeleteNode = "ui-broadcast-delete-node" => "Delete node",
    InertUnknownKind = "ui-broadcast-inert-unknown-kind" => "Its kind isn't in this build (a missing plugin?)",
    InertBadParams = "ui-broadcast-inert-bad-params" => "Its settings no longer pass their check",
    InertOpNotAllowed = "ui-broadcast-inert-op-not-allowed" => "Its op isn't there or isn't allowed in a show",
    InertBadWiring = "ui-broadcast-inert-bad-wiring" => "Its wires break a rule (edited by hand?)",
    InertUndeclared = "ui-broadcast-inert-undeclared" => "It names a variable or group the show doesn't declare",
    InertUnknownEvent = "ui-broadcast-inert-unknown-event" => "It listens for an event nothing sends",
    InertFaulted = "ui-broadcast-inert-faulted" => "It stopped on an error; restart the show to retry",
    InertOther = "ui-broadcast-inert-other" => "It won't run",

    NotRunning = "ui-broadcast-not-running" => "The show isn't running",
    NotRunningLine = "ui-broadcast-not-running-line" => "Start it in Rules to watch its variables and what fires.",
    ValuesUnreadable = "ui-broadcast-values-unreadable" => "The values can't be read: {reason}",
    Variables = "ui-broadcast-variables" => "Variables",
    FilterVariables = "ui-broadcast-filter-variables" => "Filter variables",
    NoVariables = "ui-broadcast-no-variables" => "No variables match.",
    ScopeSession = "ui-broadcast-scope-session" => "This run",
    ScopeShow = "ui-broadcast-scope-show" => "This show",
    ScopeGlobal = "ui-broadcast-scope-global" => "Every show",
    ValueOf = "ui-broadcast-value-of" => "Value of {name}",
    Changed = "ui-broadcast-changed" => "changed {seconds} s ago",
    ChangedBy = "ui-broadcast-changed-by" => "changed {seconds} s ago by {node}",
    Unchanged = "ui-broadcast-unchanged" => "not changed this run",
    Groups = "ui-broadcast-groups" => "Groups",
    Graphs = "ui-broadcast-graphs" => "Rules",
    InertNodes = "ui-broadcast-inert-nodes" => "{count} won't run",
    Firings = "ui-broadcast-firings" => "What fired",
    NoFirings = "ui-broadcast-no-firings" => "Nothing has fired yet.",
    Events = "ui-broadcast-events" => "Events",
    NoEvents = "ui-broadcast-no-events" => "No events yet.",
    Dropped = "ui-broadcast-dropped" => "Dropped",
    DropRate = "ui-broadcast-drop-rate" => "Over the rate limit: {count}",
    DropInbox = "ui-broadcast-drop-inbox" => "Inbox full: {count}",
    DropProducer = "ui-broadcast-drop-producer" => "At the source: {count}",
    DropInert = "ui-broadcast-drop-inert" => "For rules that won't run: {count}",
    DropInactive = "ui-broadcast-drop-inactive" => "For rules turned off: {count}",
    DropReload = "ui-broadcast-drop-reload" => "During a reload: {count}",
    DropPaused = "ui-broadcast-drop-paused" => "While paused: {count}",
    DropDuplicate = "ui-broadcast-drop-duplicate" => "Duplicates: {count}",
    MissingApprovals = "ui-broadcast-missing-approvals" => "{count} still need your OK",
}

/// `word` in the language the design system speaks, with its `{placeholders}` filled from `args`.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    fill(&text(ctx, word), args)
}

/// The text of `word`, in the language the design system speaks.
pub fn text(ctx: &Context, word: Word) -> Cow<'static, str> {
    translated(&*ds(ctx).strings, word.id(), word.english())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_fluent_style_and_every_english_text_is_set() {
        let mut seen = std::collections::HashSet::new();
        for word in Word::ALL {
            let id = word.id();
            assert!(seen.insert(id), "duplicate message id {id}");
            assert!(id.starts_with("ui-broadcast-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
            assert!(!word.english().is_empty(), "{id} has no English text");
        }
    }
}
