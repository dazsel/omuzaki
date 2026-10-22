//! The words the shell says (`docs/design/ux/shell.md`, spec SE-03): messages in a table, never literals in the
//! drawing code. Titles of registered panels and workspaces are English words of their registration and go through
//! [`title`], so they translate and run through the pseudo-locale like everything else.
//!
//! The table follows the design system's locale: the shell speaks the pseudo-locale exactly when the components do.
//! Message ids follow Fluent conventions (`shell-status-nothing-selected`), so a Fluent-backed table can replace
//! [`Word::english`] without touching a caller.

use std::borrow::Cow;

use egui::Context;

use crate::ds::strings::{PSEUDO_LOCALE, fill, pseudo};
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
    MenuName = "shell-menu-name" => "Menu",
    MenuView = "shell-menu-view" => "View",
    ResetLayout = "shell-reset-layout" => "Reset layout",
    LockLayout = "shell-lock-layout" => "Lock layout",
    ProjectName = "shell-project-name" => "Untitled project",
    WorkspaceTabs = "shell-workspace-tabs" => "Workspaces",
    SearchOrRun = "shell-search-or-run" => "Search or run",
    PaletteHint = "shell-palette-hint" => "Type to find an op, panel or workspace; > for ops, / for panels",
    PaletteResults = "shell-palette-results" => "Results",
    PaletteNothing = "shell-palette-nothing" => "No results for '{query}'",
    PalettePanel = "shell-palette-panel" => "Panel",
    PaletteWorkspace = "shell-palette-workspace" => "Workspace",
    PaletteRun = "shell-palette-run" => "Run",
    PaletteDryRun = "shell-palette-dry-run" => "Dry run",
    PaletteFixFields = "shell-palette-fix-fields" => "Fix the fields marked above first",
    PaletteCopyMcp = "shell-palette-copy-mcp" => "Copy as MCP call",
    PaletteCopyCli = "shell-palette-copy-cli" => "Copy as CLI",
    PaletteCopiedMcp = "shell-palette-copied-mcp" => "Copied the MCP call",
    PaletteCopiedCli = "shell-palette-copied-cli" => "Copied the command",
    PaletteSameAs = "shell-palette-same-as" => "Same as MCP tool {tool} · needs {scope}",
    PalettePreview = "shell-palette-preview" => "Live preview",
    PalettePreviewChanges = "shell-palette-preview-changes" => "Values it would change: {count}",
    PaletteWouldChange = "shell-palette-would-change" => "{op}: dry run, nothing changed. Values it would change: {count}",
    PaletteDone = "shell-palette-done" => "{op}: done. Values changed: {count}. Ctrl+Z undoes it",
    PaletteMoreLines = "shell-palette-more-lines" => "… {count} more lines",
    AgentChip = "shell-agent-chip" => "Agents",
    AgentChipCount = "shell-agent-chip-count" => "Agents: {count}",
    AgentChipWaiting = "shell-agent-chip-waiting" => "Waiting for you: {count}",
    WindowTitle = "shell-window-title" => "Omuzaki",
    WindowWaiting = "shell-window-waiting" => "\u{25cf} Omuzaki: waiting for you",
    GoLive = "shell-go-live" => "Go Live",
    GoLiveOff = "shell-go-live-off" => "No output is set up",
    OpenDrawer = "shell-open-drawer" => "Show {panel}",

    StatusBar = "shell-status-bar" => "Status",
    StatusSelection = "shell-status-selection" => "Nothing selected",
    StatusNothingToUndo = "shell-status-nothing-to-undo" => "Nothing to undo",
    StatusUndid = "shell-status-undid" => "Undid {label}",
    StatusRedid = "shell-status-redid" => "Redid {label}",
    StatusNothingToRedo = "shell-status-nothing-to-redo" => "Nothing to redo",
    StatusFence = "shell-status-fence" => "No agent is working",
    StatusNoFence = "shell-status-no-fence" => "No part is held",
    StatusHealth = "shell-status-health" => "Idle",


    LibraryHeadline = "shell-library-headline" => "The library is empty",
    LibraryLine = "shell-library-line" => "Parts and assets you add show up here.",
    HistoryHeadline = "shell-history-headline" => "Your changes will appear here",
    HistoryLine = "shell-history-line" => "Everything you or an agent does can be undone.",
    HistoryUndo = "shell-history-undo" => "Undo",
    HistoryRedo = "shell-history-redo" => "Redo",
    HistoryUndone = "shell-history-undone" => "undone",
    HistoryYou = "shell-history-you" => "You",
    HistorySystem = "shell-history-system" => "Omuzaki",
    HistoryUnavailable = "shell-history-unavailable" => "History is not available: {why}",
    HistoryFilter = "shell-history-filter" => "Filter history",
    HistoryWho = "shell-history-who" => "Whose steps",
    HistoryEveryone = "shell-history-everyone" => "Everyone",
    HistoryAgents = "shell-history-agents" => "Agents",
    HistoryArea = "shell-history-area" => "Area",
    HistoryAllAreas = "shell-history-all-areas" => "All areas",
    HistoryNoMatch = "shell-history-no-match" => "No steps match the filters",
    HistoryBy = "shell-history-by" => "By {who}",
    HistoryByBecause = "shell-history-by-because" => "By {who}, for: {cause}",
    HistoryUndoneLine = "shell-history-undone-line" => "Undone: Ctrl+Shift+Z redoes it",
    HistoryMoreChanges = "shell-history-more-changes" => "… and {count} more values",
    HistoryAdjust = "shell-history-adjust" => "Adjust",
    HistoryCopyMcp = "shell-history-copy-mcp" => "Copy as MCP call",
    HistoryNow = "shell-history-now" => "now",
    HistorySeconds = "shell-history-seconds" => "{count} s",
    HistoryMinutes = "shell-history-minutes" => "{count} m",
    HistoryHours = "shell-history-hours" => "{count} h",
    HistoryDays = "shell-history-days" => "{count} d",
    StatusCanUndo = "shell-status-can-undo" => "Ctrl+Z undoes {label}",
    PanelUnavailable = "shell-panel-unavailable" => "{panel} can't work right now",
    PanelUnavailableLine = "shell-panel-unavailable-line" => "It has not said why, and works again once what it needs is there.",
    ShowPanel = "shell-show-panel" => "Show panel",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    // The pattern goes through the pseudo-locale first: it leaves `{placeholders}` alone, so a value is never mangled.
    fill(&title(ctx, word.english()), args)
}

/// A registered title (a panel's or a workspace's English words) in the language the design system speaks.
pub fn title<'a>(ctx: &Context, english: &'a str) -> Cow<'a, str> {
    if ds(ctx).strings.locale() == PSEUDO_LOCALE { Cow::Owned(pseudo(english)) } else { Cow::Borrowed(english) }
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
            assert!(id.starts_with("shell-") && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{id}");
            assert!(!word.english().is_empty(), "{id} has no English text");
        }
    }
}
