# omuzaki-ui: the desktop app shell

- `omuzaki-ui` is now a window (`cargo run -p omuzaki-ui`): workspace tabs, docked Outliner, Library, Viewport, Inspector, Agent and History panels showing their empty states, and a status bar (#70).
- Layouts are saved per workspace in the user's config folder when a drag ends, and come back on the next start; View > Reset layout forgets them.
- Keys: Ctrl+1 to 9 switch workspace, Ctrl+Z / Ctrl+Shift+Z undo and redo, Ctrl+Shift+L locks the layout, F6 moves between panels, Ctrl+Space maximises one.
- A window narrower or shorter than the full layout folds Library and Agent/History into tab groups, and a 1280-wide window shows the left dock as an icon rail.
- The shell is wired to a runner with no document until the app builds the real executor; nothing repaints while the window is idle.
