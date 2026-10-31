//! The words the shell says (`docs/design/ux/shell.md`, spec SE-03): messages in a table, never literals in the
//! drawing code. Titles of registered panels and workspaces are English words of their registration and go through
//! [`title`], so they translate and run through the pseudo-locale like everything else.
//!
//! The table follows the design system's locale: the shell speaks the pseudo-locale exactly when the components do.
//! Message ids follow Fluent conventions (`shell-status-nothing-selected`), so a Fluent-backed table can replace
//! [`Word::english`] without touching a caller.

use std::borrow::Cow;

use egui::Context;

use crate::ds::strings::{fill, local, translated};
use crate::ds::theme::ds;

macro_rules! words {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message the shell says on its own. The doc line of each variant is its English text.
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
    MenuName = "ui-shell-menu-name" => "Menu",
    MenuView = "ui-shell-menu-view" => "View",
    PanelMenu = "ui-shell-panel-menu" => "{panel} options",
    PanelMoveLeft = "ui-shell-panel-move-left" => "Move to the left",
    PanelMoveRight = "ui-shell-panel-move-right" => "Move to the right",
    PanelClose = "ui-shell-panel-close" => "Close panel",
    PanelShow = "ui-shell-panel-show" => "Show {panel}",
    PanelLocked = "ui-shell-panel-locked" => "The layout is locked",
    MenuEdit = "ui-shell-menu-edit" => "Edit",
    MenuAgents = "ui-shell-menu-agents" => "Agents",
    MenuHelp = "ui-shell-menu-help" => "Help",
    MenuUndo = "ui-shell-menu-undo" => "Undo {label}",
    MenuNothingToUndo = "ui-shell-menu-nothing-to-undo" => "Nothing to undo",
    MenuRedo = "ui-shell-menu-redo" => "Redo",
    MenuConnectAgent = "ui-shell-menu-connect-agent" => "Connect an agent",
    MenuPauseAgent = "ui-shell-menu-pause-agent" => "Pause {name}",
    MenuResumeAgent = "ui-shell-menu-resume-agent" => "Resume {name}",
    MenuRunSetup = "ui-shell-menu-run-setup" => "Run setup again",
    MenuShortcuts = "ui-shell-menu-shortcuts" => "Keyboard shortcuts",
    MenuSettings = "ui-shell-menu-settings" => "Settings",
    SheetTitle = "ui-shell-sheet-title" => "Keyboard shortcuts",
    SheetClose = "ui-shell-sheet-close" => "Close",
    SheetPalette = "ui-shell-sheet-palette" => "Search or run anything",
    SheetUndo = "ui-shell-sheet-undo" => "Undo",
    SheetRedo = "ui-shell-sheet-redo" => "Redo",
    SheetWorkspaces = "ui-shell-sheet-workspaces" => "Switch workspace (1 to 9)",
    SheetPanels = "ui-shell-sheet-panels" => "Next or previous panel",
    SheetMaximise = "ui-shell-sheet-maximise" => "Maximise or restore the panel",
    SheetLock = "ui-shell-sheet-lock" => "Lock the layout",
    SheetSheet = "ui-shell-sheet-sheet" => "Show these shortcuts",
    SheetSettings = "ui-shell-sheet-settings" => "Open the settings",
    SheetPause = "ui-shell-sheet-pause" => "Pause or resume the agent",
    SheetRecord = "ui-shell-sheet-record" => "Start or stop recording a macro",
    SheetDivider = "ui-shell-sheet-divider" => "Move a focused divider (arrow keys)",
    StatusAgentPaused = "ui-shell-status-agent-paused" => "{name} is paused",
    StatusAgentResumed = "ui-shell-status-agent-resumed" => "{name} is working again",
    StatusNoAgent = "ui-shell-status-no-agent" => "No agent is connected",
    ResetLayout = "ui-shell-reset-layout" => "Reset layout",
    LockLayout = "ui-shell-lock-layout" => "Lock layout",
    ProjectName = "ui-shell-project-name" => "Untitled project",
    WorkspaceTabs = "ui-shell-workspace-tabs" => "Workspaces",
    SearchOrRun = "ui-shell-search-or-run" => "Search or run",
    PaletteHint = "ui-shell-palette-hint" => "Type to find an op, panel or workspace; > for ops, / for panels",
    PaletteResults = "ui-shell-palette-results" => "Results",
    PaletteNothing = "ui-shell-palette-nothing" => "No results for '{query}'",
    PalettePanel = "ui-shell-palette-panel" => "Panel",
    PaletteWorkspace = "ui-shell-palette-workspace" => "Workspace",
    PaletteDescribe = "ui-shell-palette-describe" => "Make a character from these words",
    PaletteRun = "ui-shell-palette-run" => "Run",
    PaletteDryRun = "ui-shell-palette-dry-run" => "Dry run",
    PaletteFixFields = "ui-shell-palette-fix-fields" => "Fix the fields marked above first",
    PaletteCopyMcp = "ui-shell-palette-copy-mcp" => "Copy as MCP call",
    PaletteCopyCli = "ui-shell-palette-copy-cli" => "Copy as CLI",
    PaletteCopiedMcp = "ui-shell-palette-copied-mcp" => "Copied the MCP call",
    PaletteCopiedCli = "ui-shell-palette-copied-cli" => "Copied the command",
    PaletteSameAs = "ui-shell-palette-same-as" => "Same as MCP tool {tool} · needs {scope}",
    PalettePreview = "ui-shell-palette-preview" => "Live preview",
    PalettePreviewChanges = "ui-shell-palette-preview-changes" => "Values it would change: {count}",
    PaletteWouldChange = "ui-shell-palette-would-change" => "{op}: dry run, nothing changed. Values it would change: {count}",
    PaletteDone = "ui-shell-palette-done" => "{op}: done. Values changed: {count}. Ctrl+Z undoes it",
    PaletteMoreLines = "ui-shell-palette-more-lines" => "… {count} more lines",
    AgentChip = "ui-shell-agent-chip" => "Agents",
    AgentChipCount = "ui-shell-agent-chip-count" => "Agents: {count}",
    AgentChipWaiting = "ui-shell-agent-chip-waiting" => "Waiting for you: {count}",
    WindowTitle = "ui-shell-window-title" => "Omuzaki",
    WindowWaiting = "ui-shell-window-waiting" => "\u{25cf} Omuzaki: waiting for you",
    GoLive = "ui-shell-go-live" => "Go Live",
    GoLiveOff = "ui-shell-go-live-off" => "No output is set up yet: opens Live, where you set one up",
    OpenDrawer = "ui-shell-open-drawer" => "Show {panel}",

    StatusBar = "ui-shell-status-bar" => "Status",
    StatusSelection = "ui-shell-status-selection" => "Nothing selected",
    StatusSelected = "ui-shell-status-selected" => "Selected: {part}",
    StatusSelectedMany = "ui-shell-status-selected-many" => "{count} parts selected",
    StatusNothingToUndo = "ui-shell-status-nothing-to-undo" => "Nothing to undo",
    StatusUndid = "ui-shell-status-undid" => "Undid {label}",
    StatusRedid = "ui-shell-status-redid" => "Redid {label}",
    StatusNothingToRedo = "ui-shell-status-nothing-to-redo" => "Nothing to redo",
    StatusFence = "ui-shell-status-fence" => "No agent is working",
    StatusNoFence = "ui-shell-status-no-fence" => "No part is held",
    StatusHealth = "ui-shell-status-health" => "Idle",


    LibraryHeadline = "ui-shell-library-headline" => "The library is empty",
    LibraryLine = "ui-shell-library-line" => "Parts and assets you add show up here.",
    HistoryHeadline = "ui-shell-history-headline" => "Your changes will appear here",
    HistoryLine = "ui-shell-history-line" => "Everything you or an agent does can be undone.",
    HistoryUndo = "ui-shell-history-undo" => "Undo",
    HistoryRedo = "ui-shell-history-redo" => "Redo",
    HistoryUndone = "ui-shell-history-undone" => "undone",
    HistoryYou = "ui-shell-history-you" => "You",
    DockSplitter = "ui-shell-dock-splitter" => "Resize {first} and {second}",
    DockTabs = "ui-shell-dock-tabs" => "{panels} tabs",
    HistorySystem = "ui-shell-history-system" => "Omuzaki",
    HistoryUnavailable = "ui-shell-history-unavailable" => "History is not available: {why}",
    HistoryFilter = "ui-shell-history-filter" => "Filter history",
    HistoryWho = "ui-shell-history-who" => "Whose steps",
    HistoryEveryone = "ui-shell-history-everyone" => "Everyone",
    HistoryAgents = "ui-shell-history-agents" => "Agents",
    HistoryArea = "ui-shell-history-area" => "Area",
    HistoryAllAreas = "ui-shell-history-all-areas" => "All areas",
    HistoryNoMatch = "ui-shell-history-no-match" => "No steps match the filters",
    HistoryBy = "ui-shell-history-by" => "By {who}",
    HistoryByBecause = "ui-shell-history-by-because" => "By {who}, for: {cause}",
    HistoryUndoneLine = "ui-shell-history-undone-line" => "Undone: Ctrl+Shift+Z redoes it",
    HistoryMoreChanges = "ui-shell-history-more-changes" => "… and {count} more values",
    HistoryAdjust = "ui-shell-history-adjust" => "Adjust",
    HistoryCopyMcp = "ui-shell-history-copy-mcp" => "Copy as MCP call",
    HistoryNow = "ui-shell-history-now" => "now",
    HistorySeconds = "ui-shell-history-seconds" => "{count} s",
    HistoryMinutes = "ui-shell-history-minutes" => "{count} m",
    HistoryHours = "ui-shell-history-hours" => "{count} h",
    HistoryDays = "ui-shell-history-days" => "{count} d",
    HistoryViewing = "ui-shell-history-viewing" => "Viewing: after {label}",
    HistoryViewingChip = "ui-shell-history-viewing-chip" => "viewing",
    HistoryBack = "ui-shell-history-back" => "Back to now",
    HistoryNoPreview = "ui-shell-history-no-preview" => "This step can't be shown: {why}",
    HistoryRestore = "ui-shell-history-restore" => "Restore this state",
    HistoryName = "ui-shell-history-name" => "Name this version",
    HistoryNotInBuild = "ui-shell-history-not-in-build" => "This build can't do that yet: it has no {op} op",
    HistoryConflict = "ui-shell-history-conflict" => "Couldn't undo",
    HistoryConflictLine = "ui-shell-history-conflict-line" => "A later change touches the same values. Undo that change first, or keep both.",
    HistoryDismiss = "ui-shell-history-dismiss" => "Dismiss",
    HistoryCompare = "ui-shell-history-compare" => "Compare with…",
    HistoryComparePick = "ui-shell-history-compare-pick" => "Choose a step to compare with {label}. Esc cancels.",
    HistoryCompareHint = "ui-shell-history-compare-hint" => "Ctrl+click another step to compare the two.",
    HistoryCompareTitle = "ui-shell-history-compare-title" => "Compare steps",
    HistoryCompareBefore = "ui-shell-history-compare-before" => "Before: {label}",
    HistoryCompareAfter = "ui-shell-history-compare-after" => "After: {label}",
    HistoryCompareSplit = "ui-shell-history-compare-split" => "Before and after, side by side",
    HistoryCompareHeatmap = "ui-shell-history-compare-heatmap" => "Show what changed",
    HistoryCompareChanged = "ui-shell-history-compare-changed" => "{percent}% of the picture changed",
    HistoryCompareNoPictures = "ui-shell-history-compare-no-pictures" => "There are no pictures of these steps to compare.",
    HistoryCompareWaiting = "ui-shell-history-compare-waiting" => "Waiting for the pictures of these steps…",
    HistoryCompareSizes = "ui-shell-history-compare-sizes" => "The two pictures are different sizes, so they can't be laid over each other.",
    HistoryCompareSame = "ui-shell-history-compare-same" => "Nothing differs between these two states.",
    HistoryCompareChanges = "ui-shell-history-compare-changes" => "{count} changed",
    HistoryCompareSelect = "ui-shell-history-compare-select" => "Select {part}",
    HistoryCompareFailed = "ui-shell-history-compare-failed" => "These steps can't be compared: {why}",
    HistoryCompareClose = "ui-shell-history-compare-close" => "Close",
    StatusCanUndo = "ui-shell-status-can-undo" => "Ctrl+Z undoes {label}",
    PanelUnavailable = "ui-shell-panel-unavailable" => "{panel} can't work right now",
    PanelUnavailableLine = "ui-shell-panel-unavailable-line" => "It has not said why, and works again once what it needs is there.",
    ShowPanel = "ui-shell-show-panel" => "Show panel",
    BranchChip = "ui-shell-branch-chip" => "Branch: {branch}",
    BranchChipReady = "ui-shell-branch-chip-ready" => "Branch: {branch} · {count} ready",
    BranchMenu = "ui-shell-branch-menu" => "Branches",
    BranchOn = "ui-shell-branch-on" => "You're working on {branch}",
    BranchAlone = "ui-shell-branch-alone" => "Make a branch to try an idea without changing {branch}. Switch back any time; nothing is lost.",
    BranchGroupReady = "ui-shell-branch-group-ready" => "Ready for review",
    BranchGroupYours = "ui-shell-branch-group-yours" => "Your branches",
    BranchGroupAgents = "ui-shell-branch-group-agents" => "Agents' branches",
    BranchGroupAuto = "ui-shell-branch-group-auto" => "Kept for you",
    BranchGroupBin = "ui-shell-branch-group-bin" => "Bin",
    BranchVersion = "ui-shell-branch-version" => "version {seq}",
    BranchHere = "ui-shell-branch-here" => "in the project folder",
    BranchAgentsOn = "ui-shell-branch-agents-on" => "{count} agents working",
    BranchNew = "ui-shell-branch-new" => "New branch",
    BranchName = "ui-shell-branch-name" => "Name",
    BranchTitle = "ui-shell-branch-title" => "Title, optional",
    BranchNewFrom = "ui-shell-branch-new-from" => "It starts as {branch} is now.",
    BranchMake = "ui-shell-branch-make" => "Make branch",
    BranchSwitch = "ui-shell-branch-switch" => "Switch",
    BranchCompare = "ui-shell-branch-compare" => "Compare",
    BranchMergeInto = "ui-shell-branch-merge-into" => "Merge into {branch}",
    BranchRename = "ui-shell-branch-rename" => "Rename",
    BranchSave = "ui-shell-branch-save" => "Save",
    BranchDelete = "ui-shell-branch-delete" => "Delete",
    BranchRestore = "ui-shell-branch-restore" => "Restore",
    BranchCancel = "ui-shell-branch-cancel" => "Cancel",
    BranchSwitchAgents = "ui-shell-branch-switch-agents" => "{count} agents are working on {branch} and keep working there.",
    BranchSwitchAnyway = "ui-shell-branch-switch-anyway" => "Switch to {branch}",
    BranchReady = "ui-shell-branch-ready" => "{who}: {branch} is ready",
    BranchNote = "ui-shell-branch-note" => "“{note}”",
    BranchSendBack = "ui-shell-branch-send-back" => "Send back",
    BranchSendBackWhat = "ui-shell-branch-send-back-what" => "What to change",
    BranchSentBack = "ui-shell-branch-sent-back" => "sent back",
    BranchMerged = "ui-shell-branch-merged" => "merged",
    BranchRefused = "ui-shell-branch-refused" => "{reason}",
    BranchCompareTitle = "ui-shell-branch-compare-title" => "Compare branches",
    BranchCompareAgainst = "ui-shell-branch-compare-against" => "Each is compared with {branch}",
    BranchCompareReference = "ui-shell-branch-compare-reference" => "Compare with this",
    BranchCompareAsOf = "ui-shell-branch-compare-as-of" => "as of version {seq}",
    BranchCompareRefresh = "ui-shell-branch-compare-refresh" => "Refresh",
    BranchCompareSame = "ui-shell-branch-compare-same" => "The same as {branch}",
    BranchCompareChanges = "ui-shell-branch-compare-changes" => "{count} differ",
    BranchCompareMissing = "ui-shell-branch-compare-missing" => "No {character} here",
    BranchCompareWaiting = "ui-shell-branch-compare-waiting" => "Drawing…",
    BranchCompareNoPicture = "ui-shell-branch-compare-no-picture" => "No picture: {why}",
    BranchCompareAdd = "ui-shell-branch-compare-add" => "Add a branch",
    BranchCompareRemove = "ui-shell-branch-compare-remove" => "Remove",
    BranchCompareOrbit = "ui-shell-branch-compare-orbit" => "Drag a picture to turn them all",
    BranchCompareSelect = "ui-shell-branch-compare-select" => "Select {part}",
    BranchCompareUnread = "ui-shell-branch-compare-unread" => "Can't be read: {why}",
    BranchClose = "ui-shell-branch-close" => "Close",
    BranchMergeTitle = "ui-shell-branch-merge-title" => "Merge",
    BranchMergeHeader = "ui-shell-branch-merge-header" => "Merge {from} (version {seq}) into {into}",
    BranchMergeNeedsChoice = "ui-shell-branch-merge-needs-choice" => "Needs a choice",
    BranchMergeClean = "ui-shell-branch-merge-clean" => "Merges cleanly",
    BranchMergeKeepMine = "ui-shell-branch-merge-keep-mine" => "Keep mine",
    BranchMergeTakeTheirs = "ui-shell-branch-merge-take-theirs" => "Take theirs",
    BranchMergeKeepBoth = "ui-shell-branch-merge-keep-both" => "Keep both",
    BranchMergePerParameter = "ui-shell-branch-merge-per-parameter" => "Choose per parameter",
    BranchMergeMine = "ui-shell-branch-merge-mine" => "Mine",
    BranchMergeTheirs = "ui-shell-branch-merge-theirs" => "Theirs",
    BranchMergeBase = "ui-shell-branch-merge-base" => "Base",
    BranchMergeApply = "ui-shell-branch-merge-apply" => "Merge",
    BranchMergeApplySelected = "ui-shell-branch-merge-apply-selected" => "Merge selected",
    BranchMergeChooseFirst = "ui-shell-branch-merge-choose-first" => "Make a choice on each card above first.",
    BranchMergeNothing = "ui-shell-branch-merge-nothing" => "{from} has nothing new for {into}.",
    BranchMergeDone = "ui-shell-branch-merge-done" => "Merged {from} into {into}. Ctrl+Z undoes it.",
    BranchMergeUndo = "ui-shell-branch-merge-undo" => "Undo the merge",
    BranchMergeUndone = "ui-shell-branch-merge-undone" => "The merge was undone.",
    BranchMergeAgain = "ui-shell-branch-merge-again" => "Preview again",
    BranchMergeFailed = "ui-shell-branch-merge-failed" => "The merge didn't apply: {why}",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    // The pattern is translated first: the table leaves `{placeholders}` alone, so a value is never mangled.
    fill(&translated(&*ds(ctx).strings, word.id(), word.english()), args)
}

/// A registered title (a panel's or a workspace's English words) in the language the design system speaks.
pub fn title<'a>(ctx: &Context, english: &'a str) -> Cow<'a, str> {
    local(&*ds(ctx).strings, english)
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
            assert!(id.starts_with("ui-shell-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
            assert!(!word.english().is_empty(), "{id} has no English text");
        }
    }
}
