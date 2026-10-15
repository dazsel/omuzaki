---
title: "Design system"
summary: "Design system"
area: design
---

# Design system

> Part of the UX direction. Principles are in `principles.md` (P1 to P12), screens in `shell.md`. This page is the contract for the component library in `omuzaki-ui` (`omuzaki-ui::ds`): tokens, type, spacing, icons, motion, component specs, the egui mapping and the UI performance rules.

**Look in one sentence:** a warm, quiet, dark-first editor (neutral surfaces tinted slightly warm, candle-gold accent from the prototypes, violet reserved for agents) in which the character is the most colourful thing on screen. The prototype pages set the mood: gold accent, key-cap hints, gold focus outlines, `aria-pressed` toggles, reduced-motion support. The editor drops their frosted-glass panels (a blur is expensive in egui and adds nothing at editor density) and keeps glass only for overlays on the viewport.

## 1. Structure

```
crates/omuzaki-ui/src/ds/
  tokens.rs      colours, sizes, durations as consts, one Theme per palette
  theme.rs       apply(ctx, &Theme, Scale, Density): fonts, Visuals, Style
  icons.rs       generated glyph constants (never hand-numbered)
  widgets/       one file per component: param_slider.rs, toggle.rs, ...
  gallery/       a story per component (self-registering), shown by `omuzaki --ui-gallery`
```

Rules: no raw colour outside `tokens.rs` (the colour picker's own chrome, the saturation-value square and the hue bar, is the one exception: it must show true colours); sizes come from the tokens, and a component's own internal geometry (an icon slot, a padding) is a named constant in its file rather than a number in a call; no raw egui parameter widgets (`Slider`, `DragValue`, `Checkbox`) in panels, only ours; every component takes a **required** accessible label; **no user-facing word is written into a component** (section 12).

## 2. Colour

Tokens are semantic. Hex values below are final; the ratios are WCAG 2.x contrast computed from them (a CI test recomputes every pair in section 10). **Dark is the design baseline** (the wireframes use it) because streaming rooms are dim and the viewport content is bright. **Light** and **High contrast** ship with v1.0 (P9). First run follows the OS setting and falls back to dark when the OS has none.

### 2.1 Surfaces and lines

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg.0` | `#0E0D11` | `#E9E3D6` | window background, viewport surround, scrollbar tracks |
| `bg.1` | `#16141A` | `#F7F3EA` | panels |
| `bg.2` | `#1E1B23` | `#FFFFFF` | raised: inputs, cards, tab bar, top and status bars |
| `bg.3` | `#28242F` | `#EFE9DB` | hover fill, secondary buttons, code background |
| `bg.4` | `#342F3D` | `#E3DBC9` | selected row, pressed fill, slider rails |
| `line` | `#3A3542` | `#CFC6B2` | decorative dividers only (not required to meet 3:1) |
| `border.control` | `#7D7589` | `#8A816E` | the outline of every input, button and checkbox |

`border.control` is 4.2:1 on `bg.1` and 3.9:1 on `bg.2` in dark, and 3.5:1 and 3.9:1 in light, so every control passes WCAG 1.4.11 (3:1).

### 2.2 Text

| Token | Dark | Light | On `bg.1` / `bg.2` / `bg.3` (dark) | On `bg.1` / `bg.2` / `bg.3` (light) |
|---|---|---|---|---|
| `text` | `#EFE8D8` | `#1F1B24` | 15.0 / 13.9 / 12.4 | 15.3 / 16.9 / 14.0 |
| `text.2` | `#B9B2C0` | `#4C4655` | 8.9 / 8.2 / 7.4 | 8.2 / 9.1 / 7.5 |
| `text.3` | `#A09AAB` | `#635C6C` | 6.7 / 6.2 / 5.6 | 5.8 / 6.4 / 5.3 |
| `text.disabled` | `#6B6575` | `#8C8596` | 3.3 on `bg.1` | 3.2 on `bg.1` |

`text.3` is the dimmest text allowed for information (4.8:1 on `bg.4`). Disabled text is exempt from WCAG but stays legible, and always comes with a reason (P9).

**How a disabled control is drawn.** A control disabled by its own `disabled(reason)` draws its text in the `text.disabled` token (never a faded copy of `text.2`: 55% of `text.2` is 2.7:1 on the light panel) and its fills and strokes at 55% opacity, and keeps its place and its focus stop. A control inside a disabled `Ui` is faded by egui as a whole, so it must not dim itself again (0.55 x 0.55 left disabled text at 2.4:1). A disabled selected segment keeps its underline but not its `bg.4` fill, and a disabled primary button is filled `bg.2` with the `line` border: `text.disabled` on `bg.4` is only 2.3:1.

### 2.3 Accent, agent and status

| Token | Dark | Light | Ratios | Use |
|---|---|---|---|---|
| `accent` | `#E6C36A` | `#9A6B00` | 10.8 on `bg.1` (dark); 4.2 on `bg.1` (light) | focus ring, selection bar, primary button fill, the "modified" dot |
| `accent.text` | `#E6C36A` | `#7F5700` | 10.8 (dark); 5.8 (light) | accent used as text or a link |
| `accent.hover` / `accent.press` | `#F0D285` / `#CFAA4E` | `#7F5700` / `#664600` | | button states |
| `on.accent` | `#1D1508` | `#FFFFFF` | 10.6 (dark); 4.7 (light) | text on an accent fill |
| `agent` | `#B9A2FF` | `#5B3FC4` | 8.4 (dark); 6.4 (light) | **agents only**: chip, highlight, presence, plan "running" ring |
| `on.agent` | `#17102B` | `#FFFFFF` | 8.4 (dark); 7.1 (light) | text on an agent fill |
| `ok` | `#5FD7B8` | `#0B7259` | 10.4 / 5.3 | passed, connected, done |
| `warn` | `#F58A1F` | `#8A3A00` | 7.4 / 7.1 | failed gate, degraded, caution |
| `danger` | `#F25A72` | `#B3243A` | 5.6 / 5.9 | errors, destructive, LIVE and recording |

Rules:
- **Violet means an agent, nothing else.** Gold means "you": selection, focus, primary action. Status colours mean status. Nothing else gets a hue; neutral chrome stays neutral.
- Status colours as text are allowed on `bg.0` to `bg.3`; on `bg.4` use `text` plus the icon.
- Selection and glow fills are the accent or agent colour at 22% over the surface (`#443A2C` for a selected gold row in dark; `text` on it is 9.1:1).
- A status-outlined control (the danger button) tints its fill with the status colour: 10% on hover and **16%** pressed. 22% was the first choice and leaves `danger` text at 4.2:1 in dark and 4.1:1 in light, so the pressed tint stops where text still has 4.5:1 (7:1 in high contrast).
- A primary button's key cap is `accent.press` under `on.accent` text (8.2:1 dark, 8.6:1 light); a lighter cap than the face left light at 3.8:1.
- **Colour is never the only signal** (P9, WCAG 1.4.1). Status is always icon, word and colour; selection is always bar or outline.

**Colour-vision check.** The colours that carry a meaning were simulated for protan, deutan and tritan vision (Machado et al. 2009 matrices at severity 1.0, from `omuzaki-core`) and compared with CIEDE2000 ΔE. The bar is ΔE ≥ 7 for every pair, in every theme and deficiency: the earlier CIE76 bar of about 10 (two colours look the same below it) is about 6 to 7 in CIEDE2000 for these hues, a just-noticeable difference is 2.3, and the marks are small and seen apart rather than side by side. The test `tests/cvd.rs` asserts it for all three themes over every status pair (`info` is `text.2`), person (`text`), agent and accent, each of agent and accent against each status hue, and the meter's dimmest and full fill against its track. Gaps, as protan / deutan / tritan:

| Pair | Dark (protan / deutan / tritan) | Light | High contrast |
|---|---|---|---|
| info vs ok | 16.8 / 7.4 / 35.5 | 20.5 / 13.5 / 30.4 | 12.6 / 7.4 / 24.6 |
| info vs warn | 35.6 / 33.8 / 25.8 | 30.1 / 31.2 / 24.4 | 29.5 / 27.6 / 26.7 |
| info vs danger | 21.7 / 23.8 / 30.4 | 17.4 / 29.4 / 29.0 | 18.0 / 16.6 / 29.6 |
| ok vs warn | 25.1 / 26.3 / 63.7 | 18.2 / 20.1 / 53.2 | 21.4 / 22.2 / 57.4 |
| ok vs danger | 24.5 / 17.7 / 71.2 | 10.8 / 15.4 / 57.8 | 20.6 / 10.3 / 64.8 |
| warn vs danger | 25.1 / 15.5 / 8.4 | 13.3 / 8.2 / 8.4 | 24.7 / 17.1 / 7.1 |
| person vs agent | 33.6 / 34.4 / 17.8 | 30.7 / 28.8 / 27.0 | 23.2 / 23.4 / 14.9 |
| person vs accent | 20.1 / 18.8 / 16.4 | 40.9 / 43.3 / 36.0 | 29.6 / 28.0 / 20.9 |
| agent vs accent | 51.0 / 51.4 / 24.4 | 58.8 / 61.1 / 39.4 | 51.8 / 51.7 / 21.9 |
| agent vs ok | 33.6 / 24.4 / 25.9 | 38.0 / 31.9 / 18.5 | 35.1 / 28.2 / 26.5 |
| agent vs warn | 54.9 / 55.2 / 31.4 | 55.0 / 57.2 / 43.2 | 50.0 / 50.1 / 26.7 |
| agent vs danger | 33.2 / 42.2 / 35.5 | 39.3 / 51.5 / 47.4 | 24.3 / 33.7 / 28.5 |
| accent vs ok | 16.8 / 22.4 / 47.3 | 17.5 / 23.8 / 46.2 | 19.4 / 22.5 / 45.1 |
| accent vs warn | 13.2 / 9.2 / 15.9 | 14.4 / 12.1 / 14.0 | 9.4 / 7.1 / 11.2 |
| accent vs danger | 29.5 / 16.3 / 23.7 | 21.2 / 10.3 / 13.7 | 29.4 / 20.4 / 17.3 |
| meter dimmest fill vs meter track | 37.5 / 39.1 / 32.1 | 37.6 / 34.4 / 35.0 | 63.7 / 68.2 / 63.9 |
| meter full fill vs meter track | 66.3 / 68.9 / 62.0 | 37.6 / 34.4 / 35.0 | 72.5 / 74.8 / 69.5 |

The closest pairs are high contrast `warn`/`danger` for tritan vision (7.1) and `accent`/`warn` for deutan vision (7.1), dark and high contrast `info`/`ok` for deutan vision (7.4) and light `warn`/`danger` (8.2 deutan, 8.4 tritan). Two decisions moved colours to get here: the draft's light `warn` `#A64B00` collapsed with the gold accent for deutan vision, so Director decision 3 moved it to the darker rust `#8A3A00`; and high contrast `warn` `#FFA040` sat at ΔE 4.7 from `danger` under tritan vision, so it moved to `#F6AB40` (maintainer sign-off), the smallest move that clears 7 and keeps 7:1 on black, `bg.3` and `bg.4`. `info` isn't a hue (information uses `text.2` and an "i" icon) because a blue collided with the violet. Colour is still never the only signal: ok and danger always carry a check or a cross and a word, accent marks selection by shape (a bar or outline), and warn is always a triangle plus a word. The same test checks the gallery, so a shape in a status colour with no icon or word of its own fails. Don't use any of these hues without those cues.

### 2.4 High contrast

Surfaces `#000000`, text `#FFFFFF` (21:1), `border.control` `#FFFFFF` at 2 pt, focus ring 3 pt white with a 1 pt black outer edge, accent `#FFD54A` (14.9:1) with `on.accent` `#000000`, agent `#C9B8FF` (11.8), ok `#7CF5C8` (15.8), warn `#F6AB40` (10.8), danger `#FF8C9B` (9.5). Selected rows use a 3 pt bar and bold text. All statuses already pass AAA (7:1). The spec fixes surfaces, text, border, focus ring and the five hues; the rest of the palette is chosen to stay above 7:1 against black: `bg.3` `#1A1A1A` (hover fill), `bg.4` `#2A2A2A` (selected and pressed: `warn` on it is 7.4:1, so a gate warning on a selected row stays AAA; `#333333` gave 6.2:1), `line` `#8F8F8F`, `text.2` `#E6E6E6`, `text.3` `#CFCFCF`, `text.disabled` `#8A8A8A`, `accent.hover` `#FFE680`, `accent.press` `#E6B800`. The contrast test (`ds::contrast`) asserts every pair of every theme.

### 2.5 Viewport overlay colours

Overlays must read on any backdrop and any character, so they are drawn **two-tone**: a 1.5 pt black outer stroke at 70% opacity plus a 2 pt coloured inner stroke.

| Overlay | Colour |
|---|---|
| selection | `accent` |
| hover | `accent` 1 pt, no outer |
| agent edit region | `agent`, dashed, with a glow at 22% |
| difference heatmap | single-hue ramp (`warn` family) with a legend; never a red-green ramp |
| fence / locked part | `text.2` hatching |

## 3. Typography

All fonts are OFL (`docs/licensing/policy.md`); the register already lists Instrument Sans, Bricolage Grotesque and JetBrains Mono (prototype UI). The PR that bundles them adds the font files, their notices and a register row.

| Style | Size / line (pt) | Weight and family | Use | egui `TextStyle` |
|---|---|---|---|---|
| Display | 28 / 34 | Bricolage Grotesque 600 | splash, empty-state headline, first-run titles | `Name("display")` |
| Title | 18 / 24 | Instrument Sans 600 | dialog and workspace titles | `Heading` |
| Section | 14 / 20 | Instrument Sans 600 | section and panel headers | `Name("section")` |
| Body | 13 / 18 | Instrument Sans 400 | controls, labels, tree rows | `Body` |
| Body strong | 13 / 18 | Instrument Sans 500 | selected rows, button labels | `Button` |
| Small | 12 / 16 | Instrument Sans 400 | secondary text, tooltips, chips | `Small` |
| Micro | 11 / 14 | Instrument Sans 500 | badges and units only, never information alone | `Name("micro")` |
| Mono | 12.5 / 18 | JetBrains Mono 400 | numbers, op names, ids, logs, key caps | `Monospace` |

Rules:
- Body is 13 pt at 100%. Nothing smaller than 11 pt, and 11 pt only for badges. No all-caps for long text and no italics in the UI.
- **Numbers are always Mono**, right-aligned in their fields. We don't rely on OpenType tabular figures (feature control in egui text is unverified, spike item 3); a monospaced face gives stable digit widths.
- Weights are separate named font families (`FontDefinitions`). egui 0.36 pins a variable font's axes per family (`FontTweak::coords`), so the bundled files are the unmodified Google Fonts variable fonts and no static instances are cut. Never synthetic bold.
- **Fallbacks:** Instrument Sans, then a CJK face (Noto Sans CJK subset, OFL), loaded only when the locale or text needs it (installer ≤ 150 MB, spec §10). Keep egui's `default_fonts` last, so a missing glyph is never a box. Emoji are never used as icons.
- **UI scale** 75% to 200% in steps (75, 90, 100, 110, 125, 150, 175, 200), set through `pixels_per_point = os_scale × user_scale`. All sizes are in points. WCAG 1.4.4 needs 200%.

## 4. Spacing, size, shape, elevation

- **Grid:** 4 pt, with a 2 pt half step. Scale: 2, 4, 6, 8, 12, 16, 24, 32.
- **Density.** *Comfortable* (default) and *Compact*; *Touch* turns on automatically for coarse pointers.

| | Comfortable | Compact | Touch |
|---|---|---|---|
| Control row height | 28 | 24 | 40 |
| Tree and list row | 26 | 24 | 40 |
| Icon button hit area | 28 × 28 | 24 × 24 | 40 × 40 |
| Panel padding | 8 | 6 | 12 |

Nothing interactive is smaller than 24 × 24 pt (WCAG 2.5.8). A 14 pt slider handle sits inside a 28 pt tall hit row.
- **Panel header** 28; **top bar** 40; **status bar** 24; **dock tab** 28; **section header** 26; **label column** 38% of the row (min 96, max 160); **numeric field** 56 × 22 (mono).
- **Radii:** 4 controls, 6 cards and panels, 8 popups, windows and the palette, full for chips and switches.
- **Strokes:** 1 pt lines and control borders, 1.5 pt emphasis (selected outline), **2 pt focus ring** (1 pt outside the widget; inside the row for dense lists).
- **Elevation** is lightness in dark (`bg.0` < `bg.1` < `bg.2`), plus a shadow only on popups, windows and the palette: offset (0, 8), blur 24, black at 35% (dark) or 18% (light).
- **Layers** from back to front: viewport, docks, overlays on the viewport, popups and menus, command palette, dialogs, toasts. Toasts are above dialogs but never take focus.

## 5. Iconography

- **Set:** Phosphor (MIT; 1,248 icons, Regular and Fill weights) through the `egui-phosphor` crate (MIT OR Apache-2.0, 0.14.0 targets egui 0.36), registered as icon fonts. Glyphs are text, so they take the text colour, scale with the UI, cost one shaped run, and need no texture. Adding the crate and the icon fonts needs a `docs/licensing/register.md` row in the PR.
- **Weights and meaning:** Regular is the default. **Fill marks "on"**: a visible eye is an open eye, a hidden one is a slashed eye, a toggled tool is its Fill glyph. That gives state a shape cue, not only a colour (P9).
- **Sizes:** 14 (inline in text and chips), 16 (default), 20 (toolbars), 24 (empty states).
- **Domain icons** (face profile, brow, mouth, fringe, outfit, expression, channel, op, agent, theatre, gate) are drawn by us as SVG on a 24 pt grid, 1.5 pt stroke, round caps, to match Phosphor Regular. They live in `assets/icons/*.svg` (small text files, in the repo), are compiled into a second icon font by an `xtask`, and are exposed as generated constants.
- **Rules:** an icon-only button has a tooltip and an accessible label (the tooltip names the op and shows the shortcut). An icon never carries meaning alone: status is icon plus word. No icon on a menu item unless it adds meaning. No emoji, ever.

## 6. Motion

Motion explains where something went. It is never decoration.

| Event | Duration | Easing |
|---|---|---|
| Hover and press colour | 90 ms | ease-out |
| Section expand and collapse | 140 ms | ease-out |
| Palette and popup open | 120 ms (opacity and 4 pt rise) | ease-out |
| Toast in | 140 ms; out 100 ms | ease-out |
| Agent touch glow | 900 ms fade | linear |
| Panel focus change | 0 (instant) | n/a |

`Style::animation_time` is 0.14 s. **Reduce motion** (the OS preference where the platform exposes it, plus a manual toggle in Settings) sets every duration to 0, replaces pulses with static markers that carry the same meaning (a LIVE or recording dot stays but doesn't pulse; the agent glow becomes a 2 s static outline), and removes parallax and slides (WCAG 2.3.3, 2.2.2). Nothing loops for more than 5 s without a way to pause it.

## 7. Components

### 7.0 Inventory

Nine components have full specs below. The rest are listed with their essentials; each gets its own file and gallery story.

| Component | Essentials |
|---|---|
| Button | primary (accent fill), secondary (`bg.3` fill, control border), ghost (text only), danger (outlined `danger`). Height = row height. Label is a verb. Disabled keeps its place and shows the reason on hover and focus. A button never grows past the room it is given (`paint::fit_width`: what is left on the line, or the whole line in a wrapping row): a longer label ends in an ellipsis and its tooltip carries the whole label. |
| Text field | `bg.2` fill, `border.control`, placeholder in `text.3`, clear button when non-empty (a 24 pt target with its own focus ring), Esc clears then blurs. Search fields add a leading icon. A field in the error state keeps its `danger` border and adds the standard ring when focused. |
| Dropdown | looks like a button with a chevron; opens a list under it (a popup, not a window; above it when there is no room below); ≤ 5 exclusive options belong in a Segmented control instead. A **select-only combobox**: the button keeps the focus while the list is open and the list only shows the active row. Enter, Space, Down and Up open it; Up, Down, Page Up, Page Down, Home and End move the active row, Enter or Space chooses it, Esc closes without choosing, Tab chooses and moves on; typing jumps to the next row that starts with the typed letters (forgotten after a second). Screen readers get `Increment` and `Decrement`, `SetValue` (a choice's text), `Expand` and `Collapse`. |
| Listbox | the list half of a combobox, shared by the dropdown and the command palette (`shell.md` section 8): fixed-height rows (virtualised past eight), a check mark on the chosen row, a bar on the active row, quiet detail text at the right, disabled rows that say why. Rows are not Tab stops: the owner (the dropdown button, the palette's text field) keeps the focus and is linked to the list with `link_combobox` (AccessKit `ComboBox` or `EditableComboBox`, `controls` and `active_descendant`). Each option is a named `ListBoxOption` in its set. |
| Section header | chevron, title (Section style), optional badge ("6 more"), click toggles, Ctrl+click opens it and collapses the rest. |
| Tab (workspace and dock) | label plus 2 pt accent underline on the active tab; the workspace tab shows a violet dot for agent activity (also said in words); middle-click, Delete or a menu closes a dock tab. A bar is **one tab stop**: Left, Right, Home and End move and select (roving focus); AccessKit `TabList` of `Tab`s. Inactive workspace tabs can be icon-only (label as name and tooltip); a bar short of room shrinks its tabs to an icon and an ellipsis. |
| Actor chip | circle with the initial (agent: violet fill, person: accent fill, plugin and system: neutral) plus the name in `text.2`. Always includes the name. |
| Scope badge | `edit:character`-style chip, mono, neutral outline. `broadcast`, `publish`, `spend`, `native` get a `warn` icon. |
| Cost chip | `~40 s · 1.2 GB · $0.12` from `CostHint`; omitted when trivial. |
| Gate chip | icon, metric name, value, threshold: `▲ likeness 0.81 < 0.85` (warn) or `✓ likeness 0.88` (ok). Click opens the evidence. |
| Tooltip | 500 ms hover delay, immediate on keyboard focus; name, shortcut, then the op name in mono; max width 320; Esc dismisses. |
| Context menu | at most 9 items, dividers between groups, shortcuts right-aligned, disabled items show why. |
| Dialog | centred, 8 pt radius, title (Title style), body, verb-named primary on the right; traps focus (pointer and Tab) and gives it back to what had it; Esc closes unless the action is irreversible-in-progress (`busy`). A **dangerous** primary is outlined in `danger`, opens on Cancel and takes the approval card's two-step confirm (`ds::confirm`: the first press turns it into "Confirm: delete 3 parts", the second must be deliberate, after the dwell). AccessKit `Dialog` (`AlertDialog` when dangerous), modal. |
| Form row | a visible label in the 38% label column (96 to 160 pt, the same as a slider row), then any control; the control takes the label as its accessible name, so the name a screen reader says is the text a person sees. A label wider than its column ends in an ellipsis with the whole text in a tooltip, and the column gives way first in a narrow row. Optional help line and "modified" dot. |
| Banner | inline at the top of a panel for a persistent condition, with one action. |
| Progress / job row | title, bar (determinate when known), cost estimate, elapsed, Cancel; failure shows the error and Retry. A fixed height (the control row plus 48 pt) so a list can be virtualised. With no known fraction the bar is **hatched and still**, with the word "Working": nothing loops (WCAG 2.2.2) and an idle list costs no frames. Every state is an icon, a word and a colour; the node is a `ProgressIndicator` whose text is "Working, 40%, ~40 s, 1.2 GB, 0:17 elapsed". |
| Thumbnail tile | 4:3 preview, label under it, hover applies a dry-run preview (P2). Lazy texture with a placeholder of the same size: the tile never loads anything, it reports whether it is on screen and still waiting (`wants_texture`), so a grid of a thousand loads only what can be seen; a picture that cannot be made says "No preview" in words. One Tab stop and one named `Button` with a selected state (outline, check mark, strong weight); hover or focus reports `previewing`. |
| Number field | a mono, right-aligned number in a 56 × 22 box that you type into: click, Enter or F2 types (all selected), Enter or leaving commits, Esc reverts, bad text keeps it open with "Enter a number from 0 to 1". Up, Down and Page Up and Down nudge, Ctrl+Backspace resets to a given default. AccessKit `SpinButton` with range and step; answers `SetValue`, `Increment`, `Decrement`. The number half of a slider as its own control (the curve editor's readout). |
| Empty state | icon (24), headline (Title), one line, one primary action, optional secondary (`shell.md` section 13). |
| Step card | see 7.10 below. |

### 7.1 ParamSlider (slider with numeric entry and reset)

The most-used control in the app, so it must be perfect.

```
 ●  Width    ───────●──┆───   [ 0.62 ]  ↺
```

- **Anatomy** (row 28 pt): modified dot · label (38%, min 96) · track (min 80) · numeric field (56 × 22, mono, right-aligned, unit suffix from `x-ui.unit`) · reset button (24 × 24 hit area, space kept when hidden so the row never shifts). The track is 4 pt high with a 14 pt handle (2 pt ring). A **default tick** marks the default. The fill runs from the default for two-sided ranges, or from the left end otherwise. Decimals: ≥ 100 range → 0, ≥ 10 → 1, ≥ 1 → 2, < 1 → 3, unless `x-ui.decimals` says otherwise.
- **States:** default (value in `text.2`, no dot, no reset) · modified (value in `text`, accent dot, reset visible) · variant override (a **V** badge; hover shows the base value) · agent-set (violet fill on the track for 900 ms, then a violet tick under the label until you hover; tooltip "Set by Designer 3 s ago: 'make the face more oval'") · mixed (multi-select: "—") · locked (lock icon, tooltip names the fence owner) · driven (a live meter drawn over the track) · disabled · focused (2 pt ring) · dragging.
- **Pointer:** drag the handle; **click anywhere on the track to jump** (WCAG 2.5.7); double-click the label or click ↺ to reset; click the number to type. Shift while dragging is fine control (×0.1), Ctrl snaps to the step. Esc during a drag restores the start value. Right-click: Reset, Copy value, Paste value, Copy op call, Show in History. Paste value sends `RequestPaste` and reads the clipboard's answer a frame or two later (a Ctrl+V on the focused track reads it at once); text that is not a number is ignored. Copy op call (when the host says which op sets the value) puts an MCP call on the clipboard, `{"name":"part.set_param","arguments":{...,"value":0.62}}`; Show in History is an event for the host. Ctrl+C, Ctrl+V and Ctrl+Shift+C do the same on the focused track.
- **Keyboard:** Left and Right ±step; Shift ±10 steps; PgUp and PgDn ±10% of range; Home and End min and max; Enter or F2 starts typing (all selected); Ctrl+Backspace reset; Tab moves on. **The row is one Tab stop**, the track: the numeric field and the reset button take clicks but not focus (they are reached with Enter or F2 and Ctrl+Backspace), and the label is not a node at all (a double-click on it resets). Many sliders in a column would otherwise cost three Tab presses each. Typing accepts a unit, a percent and simple arithmetic ("0.5*1.2", "30%"), commits on Enter or blur, and reverts on Esc. Invalid input shakes nothing: it keeps the field open with an inline message ("Enter a number from 0 to 1").
- **Transactions:** a drag, a typed entry and a reset are each one transaction with a label "Set Width 0.42 → 0.62" (P3). While dragging, edits coalesce into one open transaction (contract gap, `shell.md` section 18).
- **Accessibility:** AccessKit slider role with min, max, step, and a **text value** "0.62, default 0.50, modified"; the label is a required constructor argument. The slider answers `Increment`, `Decrement` and `SetValue` (a number or text, read like typed text) whether or not it has focus, since assistive technology addresses a node directly; the reset button ("Reset Width") and the number ("Edit Width value") are named nodes that accept `Click`.
- **egui mapping:** nothing of `egui::Slider` is reused. The row is painted with the painter over one `Sense::click_and_drag` rectangle for the track (the one focusable widget), with the reset button and the resting number as `Sense::CLICK` rectangles and a `TextEdit` while typing; the default tick, the two-sided fill and the markers are painter calls. `trailing_fill` fills from the left only, so we leave it off.
- **Perf:** no allocation per frame: the formatted number is cached per `(value, decimals)`; the markers are painter calls, not child widgets.
- **Tests:** drag, click-to-jump, type with units, Esc revert, reset, multi-select mixed, snapshot in each state.

### 7.2 Toggle (switch and checkbox)

- **Switch** (34 × 18) for settings that apply immediately; **checkbox** (16) for lists and forms that need Apply, and for tri-state parents (visibility of a group).
- **Anatomy:** control then label (label click toggles). On = accent fill and a check mark inside the knob; off = `bg.4` fill with `border.control`. State is carried by position and the check, not only colour.
- **Keyboard:** Space toggles; the whole label is the hit area. AccessKit `Switch` or `CheckBox` with the toggled state.
- **egui mapping:** a custom switch (egui's own demo shows the pattern) and `Checkbox` restyled.

### 7.3 Segmented control

- For 2 to 5 mutually exclusive options. Min 56 pt per segment. The selected segment has a `bg.4` fill, a 2 pt accent underline and Medium weight. Icon-only segments need tooltip and label.
- **Keyboard:** one tab stop for the group; Left and Right move and select (radio-group semantics, roving focus). AccessKit `RadioGroup` with `RadioButton` children.
- More than 5 options or long labels → a Dropdown. Until then the group never grows past the room it is given: a label that does not fit ends in an ellipsis and an icon-only or cut segment shows its label as a tooltip.
- **egui mapping:** one allocated rectangle, with a child `Ui` that does not move the cursor (a `scope` would pull the next widget of a left-to-right row back over the group), and one `ui.interact` per segment.

### 7.4 ColourField

- **Closed:** a 28 × 20 swatch (checkerboard behind alpha), the hex in mono, a chevron. Linked colours show a chain icon and the palette slot name ("hair.root").
- **Popover** (264 pt): palette-slot row (LK-03: click a slot to **link**, so edits propagate), a saturation-value area and a hue bar, hex and numeric RGB or OKLCH fields, an alpha slider when the property has alpha, an **eyedropper** (samples the viewport or a reference), 8 recent colours, and a **contrast chip** against the neighbouring slot (`measure.palette_contrast`; "value contrast 3.2 ✓").
- **Behaviour:** live preview while dragging; one transaction on close or Enter; Esc cancels. Hex accepts `#RGB`, `#RRGGBB`, `#RRGGBBAA`, and pasted text. **Keyboard:** Enter or Space on the focused field opens the popover (the Enter that opened it is not read again as "commit", and a held key's repeats do not toggle it back); Esc cancels and Enter commits, and both hand focus back to the field.
- **Accessibility:** label "Hair root colour, #8A6FD1, violet". The area and bar are operable with arrow keys (Shift ×10); the numeric fields are the keyboard-friendly path.
- **egui mapping:** our own saturation-value mesh, hue bar and alpha bar inside an `Area`; the maths (HSV, OKLCH, hex) is ours and tested.

### 7.5 CurveEditor

Used for input response curves (PF-22), exaggeration (FE-06), expression intensity (FE-03) and, with a time axis, as the base of the Shots curve editor (SH-02).

- **Anatomy:** a plot (default 0 to 1 on both axes, axis ranges configurable), a 4 × 4 grid with labels, a diagonal reference, control points (10 pt handles, 24 pt hit area), a preset row (Linear, Ease in, Ease out, S-curve, Dead zone, Inverted, Step), and a readout row for the selected point (x, y, tangent: numeric fields).
- **Interpolation:** monotone cubic by default, so a curve can't overshoot; Bezier tangents on request.
- **Live ghost:** a vertical line at the current input with a dot riding the curve at the current output (tracker curves), and a faint copy of the original curve while editing.
- **Pointer:** drag a point (Shift constrains an axis, Ctrl snaps to the grid), double-click the curve to add a point, drag a point off the plot or press Delete to remove it. x is kept monotonic.
- **Keyboard** (WCAG 2.5.7): Tab and Shift+Tab choose a point; arrows nudge 0.01 (Shift 0.1); Insert adds a point at the cursor; Delete removes; Enter edits the readout.
- **Accessibility:** the plot is a group whose children are the points (each a pair of values), plus a text summary: "Curve with 4 points. Input 0.5 gives 0.62."
- **Perf:** the curve is sampled into a 256-entry lookup table, rebuilt only on change. The polyline has 128 segments drawn as one shape. Redraw on edit and, for the live ghost, at ≤ 60 Hz.
- **Built as:** `Curve` (the maths, no egui: points, monotone cubic, explicit tangents, presets, the lookup table) and `CurveEditor` (drawing and interaction) in `widgets/curve_editor/`. The interpolation is Fritsch and Butland's PCHIP, so a point can be dragged anywhere and the curve still never overshoots; an explicit tangent replaces the automatic slope and the table clamps to the range. The first and last points stay on the ends of the axis. Tab visits the points left to right by input value even when a rising curve climbs the screen, which the gallery's reading-order check allows through the `ordered-by-data` class (`a11y::ORDERED_BY_DATA`). Events: `Preview` while dragging, `Commit { from, cause }` once per gesture, `Rollback` on Esc. Edits that need no drag: the readout's number fields, Insert and Delete, double-click, and `Increment`, `Decrement` and `SetValue` on a point.

### 7.6 ChannelMeter

For tracking channels (PF-26), the 52-control grid (FE-01) and the life-layer view.

- **Anatomy** (a bar 10 pt high, 120 to 200 pt wide): the **processed** value as a fill (accent), the **raw** input as a thin `text` tick, the calibrated min and max as brackets, a dead-zone band as a lighter strip, a peak-hold tick that decays over 1.5 s, an **owner chip** at the end (tracker, audio, AI, life layer, hotkey; PF-23), and the name at the left. Low tracking confidence hatches the fill with diagonal lines.
- **Clipping:** the bar's end cap turns into a `warn` triangle (shape plus word "clipping" in the tooltip), never a colour change alone.
- **Value:** shown on hover and focus and in an optional column ("0.52 (raw 0.45)").
- **Update rate:** the display is smoothed and capped at 30 Hz whatever the tracking rate. Meters in a grid are drawn straight with the painter into one shape batch, with **one** hit-test rectangle for the grid and the hovered cell computed by arithmetic (no per-meter widget).
- **Keyboard and accessibility:** a grid is one tab stop with arrow-key roving between meters, announcing "Jaw open: 0.52, raw 0.45, owner tracker". Off-screen meters aren't built (virtualised, with the channel list).
- **Colour:** one hue ramp (accent), never green to red, brighter as the value rises. The ramp never drops below the theme's `meter_floor` (55% of the accent over the track in dark, 88% in high contrast, 100% in light, where the accent is only 3.4:1 on `bg.4` and the ramp is flat), so even the dimmest fill is 3:1 (7:1 in high contrast) against the track. The raw tick is `text` on a halo of the window colour, so it reads over the fill as well as over the track.

### 7.7 TreeRow

- **Anatomy:** indent guides (16 pt per level, drawn as 1 pt `line` verticals) · chevron (16 pt hit area) · type icon · name · right-aligned state icons (eye, lock, agent dot, gate warning), each with a tooltip.
- **States:** hover (`bg.3`), selected (`bg.4` plus a 2 pt accent bar on the left), selected in an unfocused panel (hollow bar), drop target (a 2 pt accent line between rows), renaming (inline text field), filtered-out (hidden) and filter match (matched text in Medium weight with an accent underline).
- **Interaction:** click selects; Ctrl and Shift extend; the chevron or Left and Right expand; double-click or F2 renames; drag reorders where allowed; context menu for the rest. Full key list in `shell.md` section 6.
- **Accessibility:** a tree item with level, expanded and selected state; the group is a tree. egui may not emit these for custom rows, so build the AccessKit nodes explicitly and test with kittest.
- **Perf:** rows come from a cached flat list and are drawn with `ScrollArea::show_rows`; per-row strings are cached by `DocVersion`; a row costs one painter pass and one interaction rect.

### 7.8 Toast

- **Anatomy** (320 pt wide, `bg.2`, 8 pt radius): status icon · message (one or two lines) · up to two actions ("Undo", "Show", "Details") · a close button. The message says what happened and who did it ("Designer changed the face profile").
- **Behaviour:** bottom right above the status bar, at most 3 visible, newest at the bottom, duplicates fold into "×3". Success and info vanish after 4 s, warnings after 8 s, errors persist until dismissed. The timer pauses on hover and on keyboard focus, and the time it was paused is given back, however long the app slept. Time is the **clock in the egui context** (`ds::clock`: egui's frame time by default, or an app-supplied `Clock`): a toast has a deadline and a frame compares the clock with it. A reactive app repaints once a second or less, so adding up frame times made a 4 s toast last minutes. A toast **never takes focus**, and nothing decision-bearing lives in one (that is an approval card). Everything is also written to the **notification log**.
- **Accessibility:** announced to screen readers through a live region where the AccessKit backend supports one; otherwise the notification log is the accessible path and has its own shortcut. Reduce motion removes the slide.

### 7.9 ApprovalCard

The unit of human-in-the-loop (MCP: "there SHOULD always be a human in the loop with the ability to deny tool invocations"). It also renders **plan approvals** and **elicitations**.

```
┌──────────────────────────────────────────────────────────────────────┐
│ ● Designer needs you                                        [render] │  actor chip · scope badge
│ Render 24 contact sheets to check expressions                        │  what, verb first
│ ~40 s · 1.2 GB GPU · $0 · reversible: yes                            │  CostHint + undo kind
│ ▸ Changes: nothing in the character                                  │  expands to the patch or diff
│ [ Approve ⏎ ] [ Approve for this session ] [ Reject… ]     6 similar ▸ │
└──────────────────────────────────────────────────────────────────────┘
```

- **Content:** who (actor chip), what in plain words, the exact scope (badge) and its meaning in a sentence on hover ("Allows rendering previews. Doesn't change the project."), the cost line, a one-line patch summary that expands to the diff with before and after thumbnails.
- **Buttons:** **Approve** (primary), **Approve for this session**, **Reject…** (opens a comment box; the comment goes back to the agent as the reason and it keeps working; a bare Reject stops the turn). A split menu adds "Always for this project" and states the grant in words.
- **Variants:** *plan* (the steps as a checklist; approving runs the in-scope ones), *elicitation* (a question with options shown as previews and Accept, Decline, Cancel), *dangerous* (`broadcast`, `publish`, `spend`, `native`: heavier `danger` border and icon, and a **two-step confirm**: the first press turns the button into "Confirm: go live as @name", which needs a second, **deliberate** press; no timeout). Deliberate means the press **starts after the confirm control appeared and a dwell of 400 ms** (a double-click lands a few frames after the first click, so it is ignored), and a **held key does not count**: egui clicks a focused button on every auto-repeat of Enter, so only the key going down, not its repeats, can confirm. The dwell is not a timer: nothing happens when it ends and an armed request waits as long as it takes. A screen reader's `Click` is a press and obeys the same dwell (`ds::confirm`).
- **Batching:** identical pending requests collapse into "6 similar" with one Approve all.
- **No timers, ever.** An approval never auto-accepts or auto-rejects. The agent shows "waiting for you".
- **Keyboard:** the card is one focus group; Enter approves; Ctrl+Enter approves for the session; Backspace opens Reject; Esc moves focus out without deciding.
- **Accessibility:** announced when it appears (live region or log); each button's label names the agent and action.

### 7.10 StepCard (History, Agent panel and Theatre)

One card per transaction. Anatomy: before and after thumbnails (76 pt; 56 compact) · label · actor chip · cause line (the prompt text, truncated) · metric deltas as chips (`likeness +0.03`) · age · actions (**Rewind**, **Explain**, **Adjust**, and Approve and Reject on a pending step). States: applied, pending review, rejected (struck through, with Restore), failed gate (a `warn` border, triangle, evidence image). Thumbnails are lazy. Each state is an icon and a word as well as a colour; the card is painted with its buttons placed by hand and costs no allocation per frame. Actions by state: applied and failed gate offer Rewind, Explain and Adjust; pending offers Approve, Reject and Explain; rejected offers Restore and Explain. Each button is named for what it does to which step ("Rewind: Rounder face profile"). A metric chip carries a check mark (better), a triangle (worse) or nothing (neutral), so the direction is never colour alone; the host says which, because a falling error is an improvement.

## 8. Mapping onto egui

Field names below were checked against the egui 0.36.2 docs (`docs.rs/egui/0.36.2`: `Style`, `Visuals`, `Spacing`, `WidgetVisuals`, `Widgets`, `Selection`). `apply(ctx, theme, scale, density)` builds the whole `Style` once, stores the theme, metrics and font ids in the egui `Context`, and calls `ctx.set_style_of`; widgets read their tokens from that context state (`ds(ctx)`), not from process globals, so two contexts can use two themes.

| Token or rule | egui field |
|---|---|
| `bg.1` | `Visuals::panel_fill` |
| `bg.2` | `Visuals::window_fill`; `faint_bg_color` (striped rows) |
| `bg.0` | `Visuals::extreme_bg_color` (scrollbar tracks); `text_edit_bg_color = Some(bg.2)` |
| `bg.3` | `Visuals::code_bg_color` |
| `line` / window border | `window_stroke` (1 pt), `widgets.noninteractive.bg_stroke` |
| `text` | `override_text_color = None`; `widgets.*.fg_stroke.color` |
| `accent.text` | `Visuals::hyperlink_color` |
| `warn`, `danger` | `Visuals::warn_fg_color`, `error_fg_color` |
| selection | `Visuals::selection.bg_fill` = accent 22% over the surface. The docs describe `selection.stroke` as the selected-text colour; set it to the accent (2 pt) and see spike item 1 for whether egui also draws the keyboard focus ring with it |
| widget states | `Visuals::widgets.{noninteractive, inactive, hovered, active, open}` each `WidgetVisuals { bg_fill, weak_bg_fill, bg_stroke, corner_radius, fg_stroke, expansion }`. Buttons use `weak_bg_fill`; checkboxes and sliders use `bg_fill`. Set `expansion = 0` on hover so rows don't jitter. |
| radii | `WidgetVisuals::corner_radius` 4; `window_corner_radius` 8; `menu_corner_radius` 6 |
| shadows | `Visuals::popup_shadow`, `window_shadow` |
| slider look | `Visuals::handle_shape` (circle), `slider_trailing_fill = false`; `Spacing::slider_rail_height = 4`, `slider_width = 160` |
| disabled | `Visuals::disabled_alpha = 0.55` |
| text cursor | `Visuals::text_cursor` (2 pt accent) |
| item gap, padding | `Spacing::item_spacing = (8, 4)`, `button_padding = (10, 4)`, `window_margin = 12`, `menu_margin = 6` |
| control height | `Spacing::interact_size = (28, 28)` (comfortable) |
| indent, icons | `Spacing::indent = 16`, `icon_width = 16`, `icon_spacing = 6` |
| widths | `Spacing::text_edit_width = 160`, `combo_width = 140`, `tooltip_width = 320` |
| text styles | `Style::text_styles` (table in section 3) |
| numbers | `Style::drag_value_text_style = Monospace`; our own `number_formatter` for decimals and units |
| tooltips | `Style::explanation_tooltips = false` (we draw our own, with op names) |
| motion | `Style::animation_time = 0.14`, `0.0` under reduce motion |
| truncation | `Style::wrap_mode = Some(Truncate)` in rows, with the full text in the tooltip |

### 8.1 Accessibility mechanics

From egui's accessibility guide (`docs/accessibility.md` in the egui repo): built-in widgets register their role and name; a custom widget that only paints must say what it is, with `response.widget_info(|| WidgetInfo::labeled(Role::Slider, enabled, "Width"))`; an input with a separate visible label connects to it with `.labelled_by(label.id)`; `Context::enable_accesskit()` is on by default under `eframe`. `ds::a11y` wraps this in one helper that every component calls, and its label parameter is not optional. Tests query the same tree with `egui_kittest` (`get_by_role_and_label`), and the egui demo can show the tree when built with the `accessibility_inspector` feature.

Rules every component follows, each enforced by a gallery-wide test (section 10):
- **One Tab stop per control**, in reading order (top to bottom, left to right: a row that is laid out right to left, like a right-aligned group of buttons, is built left to right with a measured gap instead). A segmented control and a meter grid are one stop with roving arrows; a parameter row is one stop.
- **Everything a person can operate has a name**: any node that offers Focus or Click, or is interactive by role, has a label (the lint also covers the colour popover and the inline rename field).
- **Every focusable node is visible and shows it has focus**: a 2 pt ring (or the 2 pt accent border of a focused text field) drawn around the thing that has focus.
- **A label's text is its AccessKit `value`** (egui's convention for `Role::Label`), so a node that sets a value of its own must put the message in it.

Open spike items (confirm early, record the result in the spike issue; see section 11): keyboard focus ring, AccessKit for custom widgets, multi-viewport pop-out, and tab-strip keyboard behaviour.

## 9. UI performance rules

The editor holds **60 fps** (frame ≤ 16.6 ms p95, ≤ 25 ms p99) on the mid laptop of spec §10, and an idle editor costs nothing. These are enforced by benches, not by hope.

1. **Repaint on demand.** `egui` "only repaints when there is interaction (e.g. mouse movement) or an animation, so if your app is idle, no CPU is wasted" (egui README). No panel calls `request_repaint()` unconditionally. Animations use `request_repaint_after`. The viewport asks for a frame only when the scene or camera is dirty, or while performing. **An idle editor with a project open does 0 frames per second and uses under 0.5% of an 8-core CPU.**
2. **UI budget.** Layout, tessellation and painting of the busiest workspace take ≤ 2 ms p95 and ≤ 4 ms p99 at 1920×1080 on the mid laptop. The viewport has its own budget (spec §10: ≤ 1.5 ms live, ≤ 4 ms quality). Together they leave headroom inside 16.6 ms.
3. **Virtualise.** Anything that can exceed about 50 rows (part tree, History, Library, channels, palette results, notification log) uses `ScrollArea::show_rows` ("Efficiently show only the visible part of a large number of rows"), which needs a **uniform row height**: tree rows are 26 pt, History rows 56 pt, Library tiles a fixed size, and anything that wants to expand opens a popover or the inspector instead of growing in place. The egui README warns that "having a very large UI in a scroll area (with very long scrollback) can be slow, as the content needs to be laid out each frame", and says to "only lay out the part that is in view".
4. **No per-frame allocation that scales with data.** No `format!`, `to_string` or `collect` in a panel's `ui()` for rows that didn't change. Format once, cache by `DocVersion` or by value, reuse buffers. The allocation bench compares a frame of our widgets with a bare egui frame drawing the same widgets and must show no extra allocations in steady state.
5. **View models, not document walks.** Each panel derives a view model from the document when `DocVersion` or an event says it changed, and draws from that. `Runner::get` clones a value, so it is never called per frame.
6. **Drain events with a cap.** The UI drains the `Receiver<Event>` with `try_recv`, at most 256 events per frame, and folds bursts of `OpApplied` for the same path into one display update. It never blocks and never holds a lock across painting. A lagging UI drops display updates, never ops.
7. **Textures.** Thumbnails are 96×96 pt (×2 on HiDPI) in an LRU atlas capped at 64 MB, rendered off-thread by the job manager with a placeholder; at most 4 uploads per frame; no GPU readback on the UI thread.
8. **Cache text layout** for large static text and for numbers that update at ≤ 30 Hz (hold the `Arc<Galley>`).
9. **Meters and curves** are painter shapes in one batch with one hit-test rect (7.5, 7.6), displayed at ≤ 30 Hz (60 Hz for the live ghost).
10. **Input to pixel in one frame.** A slider drag changes the viewport on the next frame; History doesn't re-layout per drag step (coalescing).
11. **Never block in `ui()`.** No file, GPU, network or lock waits. Long work goes to the job manager with a progress row.
12. **Startup.** The window is interactive within 1 s and the avatar live within 2 s (spec §10). Fonts and icons are loaded up front except CJK, which loads on demand.
13. **Memory.** The editor stays under 1.5 GB with a project open (spec §10). Every cache has an explicit cap.

### Bench scenarios (in `omuzaki-test`, p50/p95/p99 frame time, allocations, repaint count)

| # | Scenario | Pass |
|---|---|---|
| S1 | Build workspace idle, 60 s | 0 repaints; CPU < 0.5% |
| S2 | scripted slider drags on the Face inspector with 52 channels visible, 60 s | UI p95 ≤ 2 ms; no extra allocations |
| S3 | an agent applying 30 ops/s for 60 s with the Theatre open | frame p95 ≤ 16.6 ms; output frames unaffected (P10) |
| S4 | Outliner with 10,000 rows, scrolled | same cost as 40 rows ± 10% |
| S5 | History with 50,000 records, scrolled | same |
| S6 | palette open, typing into 3,000 results | each keystroke ≤ 4 ms |

Regressions above 10% fail CI (spec §34).

## 10. Testing the design system

- **Gallery.** `omuzaki --ui-gallery` shows every component in every state. Each component registers its own story (self-registering, so no shared file to conflict on).
- **Snapshots.** egui_kittest renders each story in dark, light and high contrast at 100% and 200% scale and compares images (`snapshot` and `wgpu` features; tolerances in `kittest.toml`). Reviewers look at changed snapshots.
- **Accessibility lint.** A test walks the AccessKit tree of every story and fails if a node a person can operate (interactive by role, or offering Focus or Click) has no label. kittest queries by label, so a missing label also breaks other tests.
- **Tab order and focus.** `ds::testing::tab_through` presses Tab through the whole gallery (one column, dark and high contrast); the test fails if the order is not the reading order, if a stop is unnamed or off screen, or if no focus indicator is drawn on it.
- **Contrast test.** A table of token pairs (text on each surface, control border on each surface, `on.*` on fills) is recomputed in CI against the thresholds in section 2. **Each component adds its own pairs** next to the code that picks its colours (`register_contrast!`: every button kind in every state, the key caps, the meter ramp, the switch knob, the badge, the chips), so a new colour choice is tested where it is made. Because a list can be incomplete, a second test **measures the real frame**: it reads the shapes egui produced for the gallery, composites the fills under each piece of text and checks the ratio (4.5:1, 3:1 for icons and faded text, 7:1 in high contrast), in all three themes. It found the duplicate-id overlay and a double-dimmed disabled control that the table could not.
- **Localisation tests.** The gallery runs in the pseudo-locale and a test fails on any painted or spoken string that is still plain English (section 12); scenarios for the tooltips, the error bubble, the context menu, the colour popover and the approval card's armed and reject states do the same for what a still frame does not show; a source lint reads the string literals of every component file and fails on a word written into the drawing code.
- **Keyboard test.** Each component has a test that operates it with the keyboard only.

## 11. Spike checklist (unverified egui behaviour to confirm early)

1. Does `Visuals::selection.stroke` (documented as the selected-text colour) also draw the keyboard focus ring, including for custom widgets? If not, paint the 2 pt ring ourselves from `Response::has_focus()`.
2. AccessKit nodes for custom widgets beyond what the guide shows: tree items (level, expanded), slider text values, a listbox for the palette, live regions for toasts. egui's README says AccessKit "currently implements the native accessibility APIs on Windows and macOS", while AccessKit's own README lists Linux (AT-SPI), Android and iOS adapters and says web is not yet supported. Check Linux behaviour and note the web gap.
3. Font weights as separate families, no OpenType feature control (so numerics use Mono), CJK fallback loading.
4. IME in `TextEdit` on all three desktop OSes with CJK input.
5. Multi-viewport pop-out with a shared wgpu device and per-viewport render callbacks.
6. `egui_tiles` tab-strip keyboard and focus behaviour, and layout serialisation.
7. Frame time of a dense Build workspace on the mid laptop (target 2 ms).
8. Correct 1 pt strokes and viewport sizing at 200% scale.
9. Which platforms expose a reduce-motion preference.

## 12. Localisation (SE-03)

All six languages ship at 1.0 (spec SE-03), so no component writes a user-facing word itself.

- **The caller supplies its own text.** Every label, hint, placeholder and reason is a parameter; the caller has the localised string.
- **A component's own words are messages.** What is left ("Dismiss notification" on a toast, "Reset Width" on a slider, "needs you" on an approval card, the words a screen reader hears for a status, an owner or a state icon) are `ds::Msg` values with Fluent-style ids (`ds-toast-dismiss`) and `{name}` placeholders, declared once in `ds/strings.rs` together with their English text. A widget reads them with `Ds::text(Msg::X)` and `Ds::text_with(Msg::X, &[("name", value)])`.
- **The language is a `Strings` table kept in the context beside the theme** (`ds::set_strings(ctx, table)`; English by default). Switching takes effect on the next frame and needs no restart. The trait is one method, `pattern(msg)`, so a Fluent-backed table can replace `English` without touching a component. Plurals, gender and locale-aware numbers arrive with that integration; until then the few messages that carry a count are only used for counts of two or more, which every launch language writes in the same plural form.
- **Not prose, so not translated:** key legends (`Ctrl`, `Enter`; the OS's own labels are a follow-up), format hints (`#RRGGBB`), the one-letter variant badge, list separators, op ids and scope ids (`edit:character`), and units.
- **Layouts survive longer text.** A control never grows past the room it is given (buttons, toggles and segmented controls end a long label in an ellipsis and put the whole label in the tooltip; parameter labels and tree rows already did).
- **The pseudo-locale** (`ds::Pseudo<S>`, tag `en-XA`) turns every message into `[Ápprövé·····]`: look-alike accented letters (all present in the bundled faces; a test checks), placeholders kept, 40% more length, brackets that show where the text ends. `gallery --pseudo` shows it, `--pseudo --screenshots dir` writes `pseudo-*.png`, and CI renders the gallery in it and fails on any plain English left.
- **Tests:** the gallery in the pseudo-locale, scenarios for transient UI, and a source lint over the component files (section 10).
