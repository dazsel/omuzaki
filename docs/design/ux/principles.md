# UX principles

> Part of the UX direction. Spec references are to `docs/product/02-studio-spec-v3.md`.

The goal is "an amazing, easy-to-use interface for both agents and humans". These twelve principles are how we get there. Each has a **So we do** list (what to build) and a **Check** (how a reviewer verifies it). Reviewers cite a principle by number ("violates P3"). Where a principle conflicts with a feature's convenience, the principle wins, and the conflict is raised in the PR.

| # | Principle | One line |
|---|---|---|
| P1 | One document, many hands | People and agents are peers over the same ops, and the UI shows who did what |
| P2 | See it change | Every edit previews live, in the viewport, before it commits |
| P3 | Everything is undoable, and undo tells the truth | One timeline, honest labels, redo always works |
| P4 | Agent work is visible and steerable | The Build Theatre, follow mode, approvals, budgets |
| P5 | Progressive disclosure by schema | Essentials first, Advanced on demand, never a Simple/Pro switch |
| P6 | Decide for them, ask for taste | Defaults for everything with one right answer; previews, not names, for choices |
| P7 | The first five minutes are the product | Live avatar in seconds, webcam in two minutes |
| P8 | Palette and keyboard parity with MCP | Every op is searchable, scriptable and bindable |
| P9 | Accessible by construction | The component library makes inaccessible UI hard to write |
| P10 | Live is sacred | The stream is never put at risk by the editor |
| P11 | Numbers where decisions happen | Gates and metrics sit next to what they check |
| P12 | Quiet, fast, calm | 60 fps, no nags, the character is the most colourful thing on screen |

---

## P1. One document, many hands

People, agents, plugins and automations are peers: every change is an op (spec §4.1), so the UI is one client among several.

**So we do**
- Every history row, step card and inspector header shows the **actor** (person, agent name and model, plugin, system) and the **cause** (the prompt line, chat message or rule that triggered it). The data is already in `OpRecord` (`actor`, `cause`).
- No UI-only state change. If a button changes the project, it runs an op. View state (selection, camera, layout, expanded sections) isn't project state, but agents can read it (see "Agent experience" below).
- Agents have a **presence**: a violet identity colour used for nothing else, a chip in the top bar, a row in the Agent panel, and a highlight on whatever they touch. A person's own edits get no decoration.
- When two actors touch the same field, the loser's patch conflicts (`ErrorCode::Conflict`) and the UI shows a **conflict card** ("Designer changed Width while you were dragging. Keep mine / Keep theirs"), never a silent overwrite.

**Check:** no document mutation outside `Runner::run`; a kittest drives an op as a fake agent actor and asserts the chip, the history row and the field highlight.

## P2. See it change

An edit you can't see is a guess. Previews are cheap because ops have dry-run (spec §6).

**So we do**
- Slider drags, colour picks and segmented choices change the viewport on the next frame. The drag is one open transaction (`begin` / `commit`); Esc or right-click rolls it back (`rollback`).
- The command palette's op form dry-runs on every argument change (debounced 120 ms) and shows the resulting preview in the viewport, marked with a "preview" chip and a dashed outline until committed. Enter commits, Esc cancels.
- Anything that can't preview in under 100 ms (renders, generation) shows its `CostHint` (seconds, GPU memory, money) **before** it runs, then a progress row in the job list.
- Libraries and presets preview on hover: hovering a hairstyle in the Library applies it as a dry-run to the viewport.

**Check:** a test drags a slider with the mock renderer and asserts the preview frame changed before the commit; another asserts Esc restores the exact pre-edit value.

## P3. Everything is undoable, and undo tells the truth

Other tools fail here in four ways: no undo at all (VTS, VSeeFace, VNyan), partial undo (VRoid poses, Warudo), undo that excludes agent edits (Figma: "Cmd/Ctrl+Z only covers your manual edits."), and restores that can't be redone (Cursor).

**So we do**
- **One timeline** for you and your agents: the `Runner` log. Ctrl+Z undoes the latest committed transaction made by you or by an agent working for you (undo is per principal; README decision 1). If an agent session is running, the first Ctrl+Z **pauses that session** and then undoes, with a toast: "Paused Designer and undid 'Hair: bangs 9 locks'. [Resume] [Redo]". This stops a person and an agent fighting over one stack.
- Undo and redo are always labelled where you look: the status bar ("Undo: Head › face profile width 0.62") and the Edit menu.
- **Restore never destroys.** Restoring an old point keeps the abandoned future as an auto-named branch ("Before restore 14:32"), so Redo and "Go back" always work (the Figma pattern, avoiding Cursor's trap).
- **Irreversible effects are flagged before they run.** Ops with `Undo::Explicit` or `Undo::None` (go live, publish, spend, writing outside the project) show a "Can't be undone" chip in the palette form and the approval card, and a Confirm step. They still log, with a Compensate action where one exists (Stop stream).
- **Autosave** after every committed transaction batch (at most every 30 s, and on window blur) plus **crash recovery** on next launch. The op log persists with the project, unlike Blender's history, which is lost on quit.
- A continuous gesture is one transaction. One drag is one undo step, never 200.

**Check:** the op contract test (apply then undo equals identical doc) already exists (spec §34); add a UI test that kills the app mid-edit and asserts recovery offers the last committed state.

## P4. Agent work is visible, attributable and steerable

The spec's Build Theatre (§13) is the answer; this principle says what is non-negotiable in every workspace, not only in the Theatre.

**So we do**
- An agent never acts invisibly. There is no silent mode for edit ops. Each applied op produces a highlight on the touched control or part (violet, 900 ms) and a one-line caption ("Hair: adding fringe, 9 locks").
- **Follow agent** is on by default in the Theatre and one toggle elsewhere. It moves the viewport camera and opens the inspector section for each step, like Zed's follow mode and Figma's follow.
- **Pause, step and stop** are one click from any workspace, in the top-bar agent chip's popover. Pausing takes effect after the current transaction; nothing is left half-applied.
- **Approve or reject with a comment.** A rejection comment goes back to the agent as the reason, and the agent keeps working (Claude Code's pattern). A bare reject stops the turn.
- **Budgets are visible and hard.** Time, tokens and connector money show next to the agent, with a stop limit the agent cannot raise.
- **Grants are explicit and scoped** (`once`, `this session`, `this project`). The prompt names exactly what the grant covers ("allow `render` for this session"), and the Agent panel lists current grants with a revoke button.
- **Draw to direct:** circle a region of the viewport and type a note. It reaches the agent as an image plus coordinates, queued with other notes and applied together (Figma Make's Draw).

**Check:** a scripted agent session in the mock runner passes through pause, step, reject-with-comment and stop; every applied op yields a highlight event.

## P5. Progressive disclosure by schema

Beginners drown in options (VRoid's 17 face categories, Warudo's 300+ nodes); experts hate a dumbed-down mode. Disclosure should be a property of each control, not a global switch.

**So we do**
- Every op argument carries a **level** in its schema: `essential` (at most 7 per op or panel section), `standard` or `advanced`. The Soft SD "7 decisions" (`docs/design/style-guide-soft-sd.md`) are the essentials of the character builder.
- The inspector shows Essentials expanded and Advanced collapsed with a count badge ("6 more"). The state is remembered per section.
- **Search reveals everything.** Typing in the inspector's search or the palette finds hidden controls and expands their section.
- No Simple/Pro toggle anywhere. The one global control is **Density** (comfortable, compact), which changes size, not content.
- The same levels feed the MCP tool descriptions, so a person and an agent see the same "main" parameters.

**Check:** a schema lint fails CI when an op has more than 7 `essential` arguments or when an argument has no level.

## P6. Decide for them, ask for taste

The Soft SD style exists so users make "almost no decisions". The UI must not undo that by asking settings questions.

**So we do**
- Ask only what only the person can answer: hair colour, voice, persona. Never ask what analysis or detection can answer (one camera plugged in means no camera picker).
- Show choices as **previews, not names**: face profiles as thumbnails on the person's own character, hairstyles as a grid, light rigs as a lit preview.
- Every default is visible and one click from changing (the ↺ and "modified" dot, P5), and every choice stays revisitable (P3).
- Agent questions use the same card as approvals (MCP elicitation: accept, decline, cancel) with thumbnails, answerable by keyboard.

**Check:** the first-run flow asks no question a probe could answer; a review reads every dialog for "could the app decide this?".

## P7. The first five minutes are the product

The spec's targets: startup to a live avatar in 2 s (§10), webcam path in under 2 minutes, iPhone in under 5 (§2), SE-01 first-run in under 2 minutes.

**So we do**
- The **original sample character** is loaded and moving on screen before the first question, so the first thing you see is a living avatar.
- Setup is a **dock panel next to a live viewport**, not a modal maze (see `shell.md`, first-run flow). Every step shows what it will do and how long it takes, can be skipped, and can be reopened from Help.
- Camera access is requested at the moment of use, with a reason on the screen that asks for it.
- No account, no licence wall. One screen states that camera frames never leave the device and telemetry is off (SE-04).
- Every empty state names the next action (see `shell.md`, empty states). A blank panel is a bug.
- An opt-in "Let an assistant set this up" button starts the "Set up my first stream" prompt (spec §11) when an assistant is configured. Otherwise it shows how to connect Claude Code or any MCP client.

**Check:** a timed first-run test on the reference laptop with a scripted webcam: launch to calibrated and outputting under 2 minutes, with a stopwatch result attached to the PR.

## P8. Palette and keyboard parity with MCP

The op registry generates the MCP tools, the CLI and the palette (spec §6). Learning the palette should teach the vocabulary agents use.

**So we do**
- Ctrl+K searches every op, panel, workspace, setting, part and library item. Each op result shows its scope badge, its keybinding and its kind (edit, query, job).
- Selecting an op opens a **form generated from its schema**, with the same validation and defaults as the MCP tool, plus live preview (P2). The form has **Copy as MCP call** and **Copy as CLI**.
- Menu items and buttons show their op name on hover ("face.set_profile"), and Ctrl+Shift+C on a focused control copies the op call as an MCP call. Blender's Python tooltips do this behind a preference; we do it by default.
- The last history row has **Adjust**, which reopens the op's form with its arguments (Blender's Adjust Last Operation).
- Every pointer action has a keyboard path. Single-key shortcuts only work when the viewport has focus and can all be remapped (WCAG 2.1.4).

**Check:** a CI test walks the registry and asserts every op is findable in the palette by its summary text; another asserts every toolbar button declares the op or action it triggers.

## P9. Accessible by construction

Accessibility is cheap when the component library enforces it and expensive when audited later (SE-02, spec §34).

**So we do**
- Every widget in `omuzaki-ui`'s component library **requires an accessible label** in its constructor (not an `Option`), exposes its role, value and state to AccessKit, and is reachable and operable by keyboard.
- **Focus is always visible** (WCAG 2.4.7) and never hidden behind a floating panel (2.4.11). Targets are at least 24×24 points (2.5.8). Every drag has a non-drag alternative (2.5.7): click on a slider track, type a value, nudge with arrow keys, move a dock tab with a menu command.
- Text scales from 75% to 200% (1.4.4). Contrast is at least 4.5:1 for text and 3:1 for control borders and focus rings (1.4.3, 1.4.11), computed for every token pair in CI.
- **Colour is never the only signal** (1.4.1). Status uses an icon and a word; selection uses a bar or outline; switches show a check inside the knob. The palette was simulated for protan, deutan and tritan vision (`design-system.md`).
- **Reduce motion** is honoured (OS setting plus a manual toggle): no slides, no pulses, glows become a static marker with the same meaning (2.3.3, 2.2.2).
- A high-contrast theme ships with v1.0. Locale and IME input work in every text field.

**Check:** egui_kittest finds every interactive widget by accessible label; a lint asserts none is unlabeled; contrast ratios are asserted per token pair; a keyboard-only walkthrough of first-run is a release gate (spec §34).

## P10. Live is sacred

A streamer in front of an audience can't afford a surprise. The editor must never put the output at risk.

**So we do**
- While any output is on, the top bar shows a persistent **LIVE** marker (text plus icon) and a **Panic** control (spec BR-14: freeze avatar, hide AI, mute redeems) that is always reachable by pointer and by a global shortcut.
- The viewport that feeds the output wears a **PROGRAM** badge. Parameter tweaks apply live. **Structural ops** (delete a part, swap the character, import, export) ask for confirmation while live, with the line "This changes what your audience sees".
- The output renders independently of editor repaint. The UI never delays an output frame (spec §10 budgets), and tests prove it with the editor under load.
- No window, toast or dialog is ever drawn into the output. Dialogs don't steal focus from the game or window the streamer is using unless the app is focused.

**Check:** a soak test runs the editor UI at stress (an agent applying 30 ops per second) while the output frame times are recorded: p99 within budget.

## P11. Numbers where decisions happen

The spec says "measure, don't eyeball" (§4.3). Metrics only help if they show up where the decision is made.

**So we do**
- **Gate chips** sit next to the thing they check: likeness beside the face profile, readability beside the emote set, budget beside the hair. A chip is a number plus an icon plus a word (`✓ likeness 0.88`, `▲ likeness 0.81 < 0.85`), never colour alone.
- A failing gate links to its **evidence** (the compare image) and offers "Ask an agent to fix". In the Theatre a failed gate is a card (spec §15).
- Live meters show raw and processed values (tracking, latency, budget use). Other tools show nothing (VSeeFace) or a red dot (VTS).

**Check:** every `measure.*` op has a named chip component in the gallery; a failed-gate story renders in the Theatre test.

## P12. Quiet, fast, calm

A creative tool should disappear. The character is the most colourful thing on screen.

**So we do**
- 60 fps in the editor, input-to-pixel within one frame, an idle editor repainting nothing (`design-system.md`, performance rules).
- One accent colour in the chrome (gold), one identity colour (violet for agents), semantic colours only for status. Neutral surfaces, warm-tinted.
- No modal dialogs except for irreversible actions. Toasts never take focus and never carry a decision that can't wait. Everything a toast said is kept in a notification log.
- Motion is at most 180 ms, has a reason (showing where something went) and respects reduce-motion.
- No ads, nags, upsells or "rate us" prompts in the app (spec §37). Sound is off by default; one opt-in chime says "an agent needs you".

**Check:** the idle-CPU and frame-time benches in `design-system.md` pass in CI; a screenshot review confirms no more than one accent colour is in use in a non-viewport panel.

---

## Agent experience (what the UI owes agents)

An agent is a user too. Its "interface" is the op schema plus a few read-only views of what the person sees. Rules for UI work that touches agents:

1. **`omuzaki://ui/context`** is a read-only resource (scope `read`) listing the selection (part ids), active workspace, focused panel, viewport camera and the pending "draw to direct" notes with their screenshots. This is how "make this rounder" resolves "this". The built-in assistant receives it automatically (spec §12). It's session state, not project state, so it doesn't break "no UI-only features".
2. **`ui.focus` and `ui.select`** are ephemeral ops (scope `read`, not undoable, not logged as edits, rate limited) that let an agent point at something: frame it in the viewport, highlight it, open its inspector section. They are how an agent says "I mean this one" to a person.
3. **Renders are resources.** A Theatre thumbnail is the same `omuzaki://render/<id>.png` an agent receives, so a person and an agent discuss the same picture.
4. **Errors say what to do next** (`OpError` already carries a hint). The UI shows the same sentence in the toast that an agent would read.
5. **Copy screen as text.** The AccessKit tree is already a faithful textual description of the UI. A "Copy screen as text" command dumps it, so a person can paste exactly what they see into any agent. It also gives us an agent-readable test oracle.
6. **Questions are elicitations.** When an agent needs a decision it asks through MCP elicitation, and the UI shows a question card with accept, decline and cancel (spec: MCP elicitation, <https://modelcontextprotocol.io/specification/latest/client/elicitation>).

## Definition of done for a panel

A panel PR is not reviewable until it has:
- [ ] a registered panel (self-registering, `shell.md` section 3), with a title, an icon, a minimum size and an empty state
- [ ] every control from the component library (no raw egui widgets for parameters), each with an accessible label
- [ ] live preview (P2) and one transaction per gesture (P3)
- [ ] every action reachable from the palette and the keyboard (P8)
- [ ] a gallery entry rendered in dark and light at 100% and 200% scale (kittest snapshots)
- [ ] the performance rules in `design-system.md` section 9, with the bench numbers in the PR
- [ ] a screenshot at 1366×768 and at 1920×1080

## Anti-patterns (reject in review)

- A graph UI for a task a table would do (Warudo hotkeys, VNyan toggles).
- A "Simple mode" toggle.
- A progress bar with no cost estimate or cancel.
- A modal confirm for something that can be undone.
- A panel that closes itself when the mode changes.
- A colour dot as the only status.
- A setting that exists only in a config file.
- A tooltip that is the only place a control's meaning is written.
- A dialog that asks a question the app could answer.
