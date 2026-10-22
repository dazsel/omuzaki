---
title: "First run: the setup flow as a state machine over ops, probes and persisted state"
summary: "the setup flow as a state machine over ops, probes and persisted state"
area: design
---

# First run: the setup flow as a state machine over ops, probes and persisted state

**Status:** proposed; awaiting acceptance. **Feature:** SE-01. **Tasks that follow it:** WZ2 to WZ6.
**Spec:** v3 §2 (launch to live under 2 minutes), §6 (ops), §31 (SE-01, SE-04). **Screens:** `docs/design/ux/shell.md` §14 and `wireframes/firstrun-1366.svg` (they stay the source for what is drawn; this page only adds the machinery).
**Builds on:** `docs/design/settings.md` (D1, D4, D9, D10), ADR 0026, `docs/design/l10n-ops.md` §6, `docs/design/scenes.md` (the `main` scene), `docs/design/ops-api.md` §6 (`require_person`, `person_only`), ADR 0016 and 0017 (models), ADR 0021 (recovery).

The flow draws nothing new and owns no feature. It is a small pure machine that decides which step is showing, plus a thin layer that carries each choice out through the **same ops** the Settings, Perform and Broadcast panels call. An agent can therefore run the whole thing, under the same approvals.

## 1. Decisions at a glance

| # | Question | Decision |
|---|---|---|
| D1 | Where setup state lives | Session state, `SessionState.setup: Option<SetupProgress>` in `omuzaki-settings-api` (settings.md D1: the app writes it, so it is not a setting). |
| D2 | First-run detection | `setup` is `None` and the session has no project history. `None` with history is an existing user and counts as finished. |
| D3 | Resume | Yes, at the saved step, with probes re-run. Progress is written on every step change and synchronously on finish. |
| D4 | "Run setup again" | `setup.restart` (person only) sets progress back to Welcome. It resets no setting, project, calibration or output. |
| D5 | What the wizard writes | Only through ops (§4). No private write path. |
| D6 | Probes | Queries owned by the features; the wizard holds a `Probes` value and re-reads it on events (§5). |
| D7 | Agents | One prompt, `set_up_first_stream`, over the same ops. Device binding and the camera permission are never an agent's (§7). |
| D8 | "Under 2 minutes" | A clock passed into the pure machine (`now_ms`), per-step timers in `setup.status`, a FakeClock test, and a manual stopwatch run (§8). |

## 2. State (D1 to D4)

```rust
// omuzaki-settings-api, next to SessionState (settings.md D9). Additive: an old session.toml has no [setup] table.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SetupProgress {
    /// Stable step id: "welcome" | "character" | "input" | "calibrate" | "output" | "go_live". An unknown id resumes at "welcome".
    pub step: String,
    /// Ids of the steps the person skipped or the probes made pointless; the Go-live check lists them as "Later".
    pub skipped: Vec<String>,
    pub finished: bool,
}
// SessionState { .., #[serde(default)] pub setup: Option<SetupProgress> }
// SessionStore::set_setup(&self, progress: SetupProgress) -> Result<(), SettingError>   // synchronous write_atomic, like begin_open
```

**Why strings, and why `Option`.** The API crate must not depend on `omuzaki-setup`, so the step is an id the machine converts (`Step::id`, `Step::from_id`). The field is an `Option` rather than a plain `SetupProgress` because the loader must tell "never decided" from "decided and finished" without migrating files. That is the only deviation from the obvious shape.

**Detection** is a pure rule in `startup_plan`:

| `setup` | `last_project` or `recent` | Meaning |
|---|---|---|
| `None` | none | First run: plan is **Setup**. |
| `None` | some | An existing user from before SE-01 (or a copied-in session): nothing is shown, and the first write stores `finished: true`. |
| `Some(finished: false)` | any | Setup was started and not completed: plan is **Setup**, resumed. |
| `Some(finished: true)` | any | Normal start (Open or Welcome). |

**Writes.** `set_setup` is called with the new progress on every step change (a few writes per run) and writes through immediately. A crash in the middle therefore resumes at the step the person was on, never earlier than the last completed one. The elapsed timers (§8) are not persisted: they exist to measure one sitting.

**Run setup again** (Help menu, `setup.restart`) sets `Some(SetupProgress { step: "welcome", skipped: [], finished: false })`. Nothing else changes: the language, theme, input, calibration, outputs and the open project stay. Every step then shows the **current** choice preselected (Input shows the current source, Output the current output) and Continue accepts it with no write. The Character step gains a first card, **Keep "<name>"**, when a project is open, so a rerun never creates a second project by accident. The restart is `person_only` for agents (the menu is the person's).

## 3. The machine (contract of `omuzaki-setup`)

Pure and deterministic: no I/O, no clock reads, no UI. Names follow with the additions the cases below need (marked `+`).

```rust
pub enum Step { Welcome, Character, Input, Calibrate, Output, GoLive }          // Step::ALL, id(), from_id(), budget_s()
pub const TOTAL_BUDGET_S: u32 = 120;                                             // the budgets sum to 110

pub struct Probes {
    pub cameras: Cameras,                 // None | One(Permission) | Several(u8)
    pub tracker: TrackerModel,            // the webcam model (`face_tracker@1`): Ready | Downloading { percent } | Unavailable; irrelevant to the iPhone source
    pub iphone: IPhone,                   // + NotPaired | Paired                (so Input can say "iPhone connected")
    pub recommended_output: Option<OutputKind>, // None when the OS offers nothing: the Output step is left out
    pub assistant: bool,
    pub language: LanguageOffer,
    pub project_open: bool,               // + true on a rerun: Character offers Keep
    pub privacy_claim_true: bool,         // + SE-04 says camera frames stay local and telemetry is off (§9)
}

pub enum Event {
    Probed(Probes), Next, Back, Skip, SkipSetup,
    ChooseCharacter(CharacterChoice),     // Sample | FromSheetOrIdea | ImportVrm | Keep(+)
    ChooseInput(InputChoice), CalibrationDone, ChooseOutput(OutputKind), Finish,
}
pub enum Effect {
    Show(Step), RequestCameraPermission, StartCalibration, ShowTestPattern(OutputKind),
    EnsureProject,                        // + create the sample project and `main` scene if none is open (Skip setup)
    Persist(Progress), Finished { skipped: bool, total_ms: u64 },
}
impl SetupFlow {
    pub fn new(probes: Probes, resume: Progress) -> Self;
    pub fn handle(&mut self, event: Event, now_ms: u64) -> Vec<Effect>;        // + now_ms: the host's monotonic clock
    pub fn view(&self) -> View;           // step, position, shown, can_back, camera_picker, notes, + elapsed_ms by step
    pub fn timers(&self) -> &Timers;      // + per step: active_ms and waited_ms
}
```

`SetupStore` and `MemoryStore` are as in the contract above. The settings-backed implementation (`SessionSetupStore`, converting `Progress` to `SetupProgress`) lives in `omuzaki-setup`'s `ops` module, so the pure modules keep depending on `omuzaki-core` and `serde` only and stay wasm-clean.

### 3.1 Which steps are shown, and what each event does

`shown` is computed from `Probes` and the choices so far, and Back and Next move over steps not in it.

| Step | In `shown` when | Next / choice | Skip | Back |
|---|---|---|---|---|
| Welcome | always | Start = `Next` | none (only Skip setup) | disabled |
| Character | always | each card continues at once (`Keep` when a project is open) | none | Welcome |
| Input | always | `ChooseInput` then Next | "Skip for now" = `LifeLayerOnly`; adds Input to `skipped` | Character |
| Calibrate | the chosen source is Webcam and the webcam model is `Ready`, or the source is a paired iPhone (its blendshapes need no webcam model) | `CalibrationDone` | "Use defaults": adds Calibrate to `skipped` | Input |
| Output | `recommended_output` is `Some` | `ChooseOutput` then Next | "Later": adds Output to `skipped` | previous shown step |
| GoLive | always | `Finish` | none | previous shown step |

- **P6 (a probe can answer, so skip the question).** `Cameras::One(_)` sets `camera_picker: false`; `Several(n)` shows a picker; `None` removes Webcam from the Input choices.
- **Tracker not ready.** Readiness is per source. `Downloading` or `Unavailable` affects only the Webcam source: Input continues with the choice recorded and the life layer showing, Calibrate is left out of `shown` and added to `skipped` ("Later: Calibrate, waiting for the tracker model"). A paired iPhone keeps Calibrate in `shown` whatever the webcam model is (test `iphone_calibrates_while_webcam_model_unavailable`). A later `Probed(Ready)` while the person is still on Input or Output puts Calibrate back in `shown` as an optional step; it never auto-advances.
- **Esc** is mapped by the panel to the innermost cancel: the calibration overlay first (it keeps the previous calibration, shell.md §12), a dialog second, and only then `SkipSetup`. The machine only ever sees `SkipSetup`.
- **Every state handles every event.** An event that makes no sense is ignored with no effects. `Finish` is honoured only on GoLive. `Next` on Input without a choice is ignored. `Probed` re-computes `shown`; if the current step has just become pointless (the camera was unplugged on Calibrate), the step stays, a `Note` explains, and the person chooses (no jump under their hands).
- **`SkipSetup` from any step** emits `EnsureProject`, `Persist(finished: true)` then `Finished { skipped: true }`, in that order. Anything the person already chose stays.

### 3.2 Failure paths

| Case | Probe | What the person sees | Flow |
|---|---|---|---|
| No camera | `Cameras::None` | `Note::NoCamera`; choices: Pair an iPhone, Use the life layer only | Calibrate skipped |
| Permission denied | `One(Denied)` | `Note::PermissionDenied` with "Open system camera settings" (the empty state in shell.md §13) | Next stays possible with iPhone or life layer; a `Probed(Granted)` after the person fixes it in the OS updates in place |
| Permission undecided | `One(Undecided)` | Nothing is asked on Welcome or Character; choosing Webcam on Input emits `RequestCameraPermission`, with the reason on the same screen | stays on Input until the OS answers |
| Tracker downloading | `Downloading { percent }` | Progress, "You can continue; your avatar idles until it is ready" | see above; the download is hash-verified and resumes (ADR 0017, spec §10) |
| Tracker unavailable (offline, hash mismatch) | `Unavailable` | `Note::TrackerUnavailable`, "Try again" (a `model.select`-style retry op, not a wizard path) | as no camera |
| Calibration cancelled or skipped | `CalibrationDone` never sent | the previous calibration, else defaults | `skipped` gains Calibrate; GoLive lists it |
| App dies mid-flow | none | Next start resumes (D3); recovery wins (§6) | the saved step |
| Output test pattern fails to open | host error | an `OpError` rendered as in shell.md §15 with the op's hint | stays on Output |

**What "Skip setup" leaves behind:** an open sample project with the `main` scene, the life layer on (its default, nothing written), default tracking settings, no input started, no output started, language and theme as chosen or as the OS gave them, `finished: true`. Help › Run setup again is always there.

## 4. What the wizard writes: one op per choice (D5)

| Step | Choice | Op the host calls | Agent rule | Owner |
|---|---|---|---|---|
| Welcome | Language (only if the person changes it) | `settings.set {key: "ui.language"}` | Confirm (settings.md D4) | settings |
| Welcome | Theme (only if changed) | `settings.set {key: "ui.theme"}` | Free | settings |
| Character | Sample | `project.create {name, template: "sample"}` then `stage.create_scene {id: "main"}` (scenes.md) then `stage.place_character` | Confirm (writes outside the project roots) for `project.create`; the rest Free | **new task** (`project.create` and the sample template, T-A) |
| Character | Create from a sheet or idea | the guided character flow (CB-90): `character.create` | Free | CB-90 tasks |
| Character | Import a VRM | `io.import` into the new project | Confirm (file roots, ADR 0013) | EX-12 tasks |
| Input | Source: Webcam / iPhone / life layer | `input.select {source}` (Machine setting `input.source` underneath; picks a kind, binds no device) | Confirm |
| Input | Which camera (device binding) | `input.bind_camera {camera}` | **Never** (`person_only`, like `audio.bindings`) | **new task** (T-B), with PF-01 and PF-02 |
| Input | Camera permission | `input.request_camera` | Never | PF-01 |
| Input | Start and stop the live mirror (the capture the blink check and Calibrate reuse) | `input.preview {on}` | Confirm (starts capture) | **PF-01 (T-B)** |
| Calibrate | Start, cancel | `calibration.start`, `calibration.cancel`; the result is the PF-20 store | Confirm (starts capture) | PF-20 |
| Output | Add the recommended output | `broadcast.outputs.add {kind}` (a show output, project state) | Confirm | BR-01, BR-02 |
| Output | Test pattern | `broadcast.test_pattern {output, on}` | Confirm (it emits frames to an egress) | BR-01, BR-02 |
| Output | Clean output | `broadcast.clean_output {on}` | Free | BR-01 |
| GoLive | Open Perform, Customise, Close | UI navigation only (no op) | n/a | WZ4 |
| any | Skip, finish, restart | `setup.skip`, `setup.finish`, `setup.restart` | require_person, require_person, person_only | WZ3 |

Nothing in the table starts a camera, microphone, stream or recording without a person's click on that step (settings.md D9, SE-04). Opening the app after setup starts nothing either.

The ops are named from the designs where a design names them; the camera, input, calibration and broadcast ops are **provisional names** owned by features that are not yet split into tasks. The wizard's tasks follow whatever names those features register; §10 lists the tasks to cut.

## 5. Probes (D6)

Each probe is a query op of the owning feature (scope `read`, allowed to agents, no side effects), so the wizard needs no service of its own. The host reads them at start and again when a `devices.changed` or `model` event arrives (hot plug, download progress) and sends `Event::Probed`.

| Probe | Query | Owner | What the wizard does with the answer |
|---|---|---|---|
| Cameras and permission | `input.cameras` → `[{id, name, permission}]` | PF-01 | None / One / Several; permission drives the notes (§3.2). Listing names does not open a camera. |
| Tracker model readiness | `model.status {slot: "face_tracker@1"}` → `{state: ready \| downloading{percent} \| unavailable}`; `model.list` (ADR 0017) names the slot but carries no progress, so this query is new and reads the percent from `FileSource::get`'s progress callback (ai-api.md §8) | ML-01 (T-D) | Ready, Downloading with a percent, or Unavailable |
| iPhone pairing | `input.iphone.pairing` → `{address, code, paired}` | PF-02 | shows the QR or address; `paired` flips the step chip |
| Recommended output | `output.recommend` → `{kind?, others[], obs, obs_hint, unavailable[]}` | BR-01, BR-02 | one card; none recommended leaves Output out |
| Test pattern | an action, not a probe: `broadcast.test_pattern` (§4) | BR-01, BR-02 | `Effect::ShowTestPattern` |
| Assistant configured | `agent.connection` → `{configured}` | agent-sessions tasks | Welcome shows "Let an assistant set this up" or "Connect an agent" |
| Language offer | the host's `negotiate()` result (l10n-ops §6), not an op | SE-03 | `SimplifiedChineseOnTraditionalSystem` shows the one-click 简体中文 picker; everything else follows the OS silently |
| Privacy claim | `privacy.status` → `{camera_egress: false, telemetry: false}` | SE-04 (PV1, PV3 as named) | §9 |

A probe that fails to answer (a driver hangs) is treated as its most cautious value (`Cameras::None`, `Unavailable`, no recommendation) after 2 s, with a "Check again" action; the flow never waits on a probe.

## 6. The startup plan, crash recovery and "What's new" (D3)

- `StartupPlan` (settings.md D9) gains `Setup { resume_project: Option<PathBuf> }`. The order becomes: **Recover** first (recovery wins, as in D9 and VC-08); then **Setup** when §2 says so (it ignores `startup.reopen`, which has no meaning before a first project exists); then **Open**, then **Welcome**.
- Setup creates a real project on leaving Character (needed so Output can add a show output and the avatar is a real scene). So a crash during setup is an ordinary unclean shutdown of an ordinary project and the journal covers it: the next start offers Recover, and after the choice the plan returns to Setup at the saved step. This replaces the wireframe's "Sample project · not saved yet" line with "Sample project · saved in Documents/Omuzaki" (§11, disagreement 1).
- `opening` is set and cleared by the usual `begin_open` when Setup reopens the project; a crash while opening gives the D9 notice, never a loop.
- **"What's new"** (D10): at the first run `last_seen_version` is recorded when the plan is Setup, so nothing shows over the wizard or after it. If setup resumes after an update, the notes are still waiting for the Welcome screen or the next project open, never over the wizard. Skipping setup does not release them early.

## 7. The agent-assisted variant (D7)

"Let an assistant set this up" starts an agent session (Theatre) with the built-in recipe `set_up_first_stream` in `omuzaki-mcp`'s `prompts.rs` (spec §11). `PromptDef.needs` lists every op the template names, so the recipe is offered only when they all exist in the session's catalogue (the crate never recommends a missing tool).

Reads (no approval): `setup.status`, `settings.get`, `session.get`, `project.describe`, the probes of §5.
Writes the agent may do: `setup.advance`, `setup.choose`, `settings.set` for taste keys, `stage.create_scene`, `stage.place_character`, `character.create`.
Confirm (queued for the person through `require_person`): `settings.set ui.language`, `project.create`, `io.import`, `input.select`, `calibration.start`, `broadcast.outputs.add`, `broadcast.test_pattern`, `setup.skip`, `setup.finish`.
Never (`person_only`, never queued): `input.bind_camera` (which physical camera), `input.request_camera` (the OS dialog is the person's), the audio and device bindings (`audio.bindings`), `setup.restart`.

The recipe's text says: do the steps in the order of `setup.status`, wait for the person to pick the camera and answer the OS dialog on the panel (it polls `setup.status` and `input.cameras`; it never binds a device), ask them to be in front of the camera before `calibration.start`, **stop at the Go-live check** (an agent never starts a stream or recording, it says what to press), and finish with `setup.finish`, which waits for the person. **One shared flow, one bridge.** The host owns the single `SetupFlow`. The panel and the ops both feed it through `SetupHost::send(Actor, Event)`, so there is no second progression path. Two ops carry the events: `setup.advance {to?: step}` sends `Next` (or `Back` when `to` is earlier) and `setup.choose {choice}` sends `ChooseCharacter`, `ChooseInput` or `ChooseOutput`; both are `edit:settings`, Free for agents because every capture or file write behind a choice is its own Confirm op in §4. They refuse an event the machine ignores with an `OpError` naming the current step. **`CalibrationDone` is not a caller's choice**: no op or panel button sends it. The host sends it only after `calibration.status` (PF-20) reports a completed, stored result for this sitting; a cancelled, failed or never-started run sends nothing, so the step stays and "Use defaults" (`Skip`, recorded in `skipped`) is the only other way past. Tests: `premature_calibration_done_is_refused`, `cancelled_calibration_leaves_progress_unchanged`, `agent_cannot_advance_calibrate_without_a_result`. The panel renders `setup.status` after each event, so it follows the agent live, and Back and Skip setup still work for the person at any moment. Acceptance (T-F): an agent actor walking Welcome to Go-live through these ops reaches the same `View` sequence and `Persist` effects as the panel's events (`agent_and_panel_agree`), and a queued Confirm leaves the step unchanged until the person approves. A first run never requires an API key (shell.md §14).

## 8. Measuring "under 2 minutes" (D8)

- `handle` takes `now_ms`; the host passes its monotonic clock, tests pass a `FakeClock`. Active time per step is recorded between `Show(step)` and the next `Show`; time spent waiting on the OS camera dialog or a model download is also tracked in `waited_ms` (the host marks it with the `RequestCameraPermission` effect and the `Downloading` probe). `setup.status` returns both, so the stopwatch run can tell "slow step" from "slow computer".
- `setup.status` → `{ step, position, finished, skipped_steps, budget_s_by_step, elapsed_ms_by_step, waited_ms_by_step, total_ms }`: numbers and ids, no prose (the shape, plus `waited_ms_by_step` and `total_ms`).
- **The pass mark is wall clock**: the manual stopwatch run (WZ6) counts everything from launch to the Go-live check, waits included. Waits are reported only to say where the time went.
- **Automated tests** (`tests/flow.rs`), all with fake probes and a FakeClock:
  - `budgets_add_up`: 5, 10, 20, 45, 20, 10 sum to 110 and `TOTAL_BUDGET_S` is at least that (a `const` assertion).
  - `every_probe_combination_fits`: for every combination of the probe values, the sum of `budget_s` over `shown` is at most `TOTAL_BUDGET_S`.
  - `happy_path_on_the_clock`: a scripted run in which each step takes exactly its budget ends at 110 s with `Finished { skipped: false, total_ms: 110_000 }`; a second run that takes 3 times the budget on Calibrate shows `total_ms` over 120 s, which is what the status reports (the test checks the report, not a pass).
  - `elapsed_is_per_sitting`: Back and re-entering a step adds to its time; a restart resets all timers.

## 9. Truthful copy, privacy, keyboard and motion

- **Step 0's line is a claim, so it is checked.** The Welcome card shows "Your camera never leaves this computer. Telemetry is off." only when `Probes.privacy_claim_true` (from `privacy.status`, SE-04 PV1 and PV3 as named). When it is not true (a build with telemetry switched on by policy, or a connector routing the camera), the card shows the true line from the same op instead, never the default text. Until SE-04 lands, the probe is `false` and the card says only what is true today ("Nothing is sent anywhere unless you connect it"); the Welcome panel (WZ4) waits for it as a copy gate, not a code dependency (§10).
- **The camera is asked for once, on Input, with its reason on screen** ("Omuzaki needs your camera to move your avatar. Frames stay on this computer."). Welcome and Character never touch a device. Calibration reuses the stream `input.preview` started; the host stops it with `input.preview {on: false}` when setup ends, is skipped or the app closes, so no capture outlives the flow.
- **Keyboard first.** Tab order is visual order; Enter is Continue; Esc is the innermost cancel (§3.1); Back is Alt+Left; every control has an AccessKit name and the step list announces "Step 2 of 5, Input". Focus moves to the new step's heading on each `Show`.
- **Reduce motion** (`ui.reduce_motion`): no sliding between steps, no animated progress bars (the percent is text), the avatar's life layer keeps its own rule. The first run does not skip the person's OS reduce-motion preference: it is read as the default.
- **Language and theme follow the OS** and are written only if the person changes them, so a later OS change still applies.

## 10. Task cut (re-cut of WZ2 to WZ6)

| Task | Change from the first plan |
|---|---|
| **WZ2** `omuzaki-setup` flow | Adds `now_ms` and `Timers`, `Probes.{iphone, project_open, privacy_claim_true}`, `CharacterChoice::Keep`, `Effect::EnsureProject`, `Finished.total_ms`, `Step::id/from_id`, and the tests of §8. Size grows to about 330 lines: split it into **flow + probes** and **timers + budgets tests** if it exceeds that. |
| **WZ3** `setup.*` ops | adds `setup.advance` and `setup.choose` (the event bridge of §7), an `agent_and_panel_agree` test and the three calibration-guard tests of §7; `setup.status` gains `waited_ms_by_step` and `total_ms`; the settings-backed store takes `Option<SetupProgress>`. Blocked by the new API task (T-C) instead of the loose "the API crate that WZ1 chooses". |
| **WZ4** panel, Welcome, Character, Go-live | Welcome copy comes from `privacy.status` (T-E); the project line reads "saved in Documents/Omuzaki"; adds the Keep card. |
| **WZ5** Input, Calibrate, Output | Calls the provisional ops of §4; its `Blocked by` becomes the probe tasks T-B and T-D. |
| **WZ6** stopwatch | Unchanged; reads `setup.status` for the per-step numbers. |
| **New** | T-A `project.create` with a sample template; T-B `input.select`, `input.bind_camera` (person only) and `input.preview {on}`; T-C `SessionState.setup` and `set_setup` in `omuzaki-settings-api` plus the `startup_plan` `Setup` variant ; T-D the probe queries of §5 (`input.cameras`, `model.status`, `broadcast.outputs.recommend`, `agent.connection`) as thin ops over their features; T-E `privacy.status` (SE-04); T-F the `set_up_first_stream` recipe in `prompts.rs`. Each becomes its own task. |

## 11. Where documents disagree, and what I chose

1. **"Sample project · not saved yet" (wireframe) against a crash-safe, resumable flow.** The wireframe implies the project exists only in memory until the end. But Output must add a show output (project state) and resume (D3) needs somewhere to resume to. Chose: create the project when leaving Character; change the wireframe line to "saved in Documents/Omuzaki". The SVG is not edited here; the panel task (WZ4) updates it.
2. **settings.md D10 ("first run: record the version, show nothing") and "What's new never opens over a wizard"** agree with this design; the only addition is that the version is recorded when the plan is Setup, so a first run cannot be followed by a "What's new" for the version it started on.
3. **shell.md §14 lists "Go live check" as step 5 of 5 while the machine counts six `Step`s (Welcome plus five).** Kept: Welcome shows no number ("None on Welcome"), the others show "n of 5".
4. **The first sketch of `SetupProgress` was not an `Option`.** Changed (§2) for first-run detection without migrating files.
