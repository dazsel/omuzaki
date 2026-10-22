# omuzaki-ui: the shell and the gallery read panels, workspaces and stories from omuzaki-ui-api

- The shell no longer has its own `register_panel!`, `register_workspace!` or panel-view list: its tabs, docked panels and View menu come from `omuzaki_ui_api::registry::Registry`, so a panel or workspace registered in any crate appears without editing a shared file. The six shipped panels and the Build workspace are registrations of their own. An id registered twice keeps the first and is logged at startup.
- Panels are built on first show and draw through `Panel::ui` with a `PanelCtx` (runner, actor, the frame's events, view models, theme kind). A panel that is not `available` shows its registered empty state.
- New: View menu, *Show panel* lists every registered panel and shows one the workspace lacks; `Shell::show_panel`, `Shell::registry`.
- Workspace default layouts are converted from the contract's `layout::Node` and checked against the registry (unknown panels dropped and reported, repeated panels drawn once, unusable shares repaired). Saved layout files are unchanged.
- The gallery's stories register with `omuzaki_ui_api::register_story!`; `ds::gallery::Group` is the contract's `StoryGroup` (its heading is `group_title`), `Story` is `StoryDef`, and `ds::gallery::duplicates()` lists story ids registered twice.
