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
    SearchOff = "shell-search-off" => "The command palette is not built yet",
    AgentChip = "shell-agent-chip" => "Agents",
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
    StatusHealth = "shell-status-health" => "Idle",


    OutlinerHeadline = "shell-outliner-headline" => "Pick a part to edit it",
    OutlinerLine = "shell-outliner-line" => "The part tree appears when a character is open.",
    LibraryHeadline = "shell-library-headline" => "The library is empty",
    LibraryLine = "shell-library-line" => "Parts and assets you add show up here.",
    ViewportHeadline = "shell-viewport-headline" => "Start with a character",
    ViewportLine = "shell-viewport-line" => "The live character appears here.",
    InspectorHeadline = "shell-inspector-headline" => "Select a part to edit it",
    InspectorLine = "shell-inspector-line" => "Its settings appear here.",
    HistoryHeadline = "shell-history-headline" => "Your changes will appear here",
    HistoryLine = "shell-history-line" => "Everything you or an agent does can be undone.",
    HistoryUnavailable = "shell-history-unavailable" => "History is not available: {why}",
    PanelNoView = "shell-panel-no-view" => "{panel} has nothing to show yet",
    AgentHeadline = "shell-agent-headline" => "Connect an agent",
    AgentLine = "shell-agent-line" => "Agents work through the same ops you do, and you approve what they ask for.",
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
