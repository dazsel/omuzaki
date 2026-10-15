---
title: "Input-driven body motion and locomotion (PF-29, PF-30) and the OS input source PF-05 shares"
summary: "the OS input source, its privacy rules, typing, pointer, pen and gamepad motion, locomotion, props, takes, budgets and the task cut"
area: design
---

# Input-driven body motion and locomotion (PF-29, PF-30) and the OS input source PF-05 shares

**Status:** proposed (#742); the Director accepts or rejects it. **Decision record:** `docs/decisions/0060-os-input-source.md`.
**Features:** PF-29 (#718, P0: hands and arms from typing, mouse, pen and gamepad), PF-30 (#764, P1: locomotion), and
the input-source half of PF-05 (#469, bindings) and PF-25 (#478, triggers).
**Spec:** v3 §8 (frame order), §10 (budgets, tier `R`), §22 (PF-05, PF-23, PF-25, PF-27, PF-29, PF-30), §24 (ST-02,
ST-03), §26 (SH-01), §31 (SE-04).
**Builds on:** the research `docs/research/2026-10-os-input-capture.md` (APIs, crates, prompts, anti-cheat);
`anim-api.md` §6 (ownership), §6.1 (A/V delay), §8 (pose layers), §8a (IK goals); `ops-api.md` §6 (people, agents and
approvals); `settings.md` (registry, agent rules, D9 session restore); `takes.md` (T1 to T5) and ADR 0052;
`privacy.md` (crash reports never carry memory or free text); `record.md` D8; `byom.md` D4 (imported skeletons run the
same stages); `scenes.md` D7 (props). Pages named alone are in `docs/design/`.

## 0. Decisions in one screen

| # | Decision | Section |
|---|---|---|
| D1 | A new contract, **`omuzaki-input-api`**, with `omuzaki-input-mock` and one implementation crate `omuzaki-input` that holds every OS backend. Not inside `omuzaki-track-api`. | §2 |
| D2 | **Key identities exist in one module of one crate.** The capture thread turns a key into a *zone* (a cell of a fixed, coarse keyboard grid) and, only when it matches a configured binding, a *binding slot*. Neither the contract's hot types nor anything after the capture thread can hold a key code or a character. Pointer input is relative motion, never a screen position. | §3 |
| D3 | **Capture is off by default, turned on only by a person** (`person_only`, scope `input`, which already exists on `main`: no `omuzaki-ops-api` change), **never restored** at start, and **runs only while an indicator is shown**: a surface installs its indicator with the hub (not through an op), start fails with `NoIndicator` unless it is shown, and the hub pauses itself if it stops showing. Anyone may pause or stop (`read`), and every pause or stop releases whatever was held. The protected-program kill switch is set only with `process`, and its pause reason is shown only to `process`. | §4 |
| D4 | **Bindings and triggers share the source.** Each capture backend feeds its own SPSC rings: zone and pad events to the frame thread (motion), locomotion intents to the frame thread (`IntentFeed`), other bindings to the binding dispatcher (`BindingFeed`), drained in turn; the drained order is what takes and replays record. The feeds resolve every binding hit to its stable id and action inside `omuzaki-input`: generations survive table swaps, a press owns its release, releases survive overflow, and held snapshots reconcile any loss. Chords live in a machine-local store that `Services`, ops, MCP and agents never reach. | §2, §5 |
| D5 | **Motion is pose layers plus IK goals in the §8 order:** after the animator, before the rig. Typing, pointer, pen and gamepad are device motions sharing one hand allocator; each hand belongs to one device at a time. A tracked bone wins (PF-23) through a `tracked` bone mask, with a 0.3 s hand-over. No `omuzaki-anim-api` change. | §6 |
| D6 | **Locomotion** is a state machine (idle, walk, run, jump, sit, emote) over a 1-D speed blend; walk and run are a procedural gait (licence-clean by construction), jump, sit and emotes are pose clips (#223). The root moves procedurally, matched to stride; foot planting (`omuzaki-rig`) removes the rest of the slide. A set is a library item that materialises its values and clips into the character and pins itself (ADR 0019 §4). | §7 |
| D7 | **Props** are four ST-03 prop kinds with named anchors (key plane, pad plane, tablet plane, grips). Hands use the anchors whether the prop is drawn or hidden. A desk is placed from the character's own proportions. | §8 |
| D8 | **A take stores what the video showed:** zone and button edges, coarse pointer and pen, pad state, locomotion intents and the root, all at frame resolution, as a new additive `Motion` record kind (a minor version, ADR 0052 rules), with a complete motion checkpoint at every group start so a clip cut there replays exactly. Never a key code, a chord or a sub-frame time. | §9 |
| D9 | **Budget on `R`:** input plus motion at most 0.6 ms of CPU per 30 fps frame (7.5 % of `cpu.total`'s 8 ms), split as capture 0.5 ms (0.2 % of 8 cores) and frame-thread motion 0.1 ms p95. Estimates until measured. | §10 |

## 1. What this covers

| #742 asks for | Where |
|---|---|
| The input contract: where it lives, the event types, how PF-05 and PF-25 consume it | §2, §3, §5 |
| Privacy made structural; the scope question | §3, §4 |
| The motion model and the hand-over to tracking | §6 |
| Locomotion: state machine, blends, root motion, cameras, data, clips and who authors them | §7 |
| Props | §8 |
| Recording | §9 |
| Budgets | §10 |
| The re-cut of #743, #744, #748 to #751, #939 and #1168 | §13 |

**Facts about `main` this design rests on.** `Scope::Input` exists in `omuzaki-ops-api` ("capture OS keyboard, mouse,
pen or gamepad input while another app has focus") and is already `is_risky`. The rig runs humanoid, then IK, then
springs (`omuzaki-rig/src/full.rs`); PR #1650 inserts optional VRM node constraints between IK and springs, after the
Director's ruling on that PR (UniVRM's order). The stage's IK stage is `LimbsAndGaze` in
`omuzaki-stage/src/frame/rig.rs` (look-at first, then two-bone limbs). Palm-frame goals (`HandTarget`,
`TwoHandedProp`) and foot planting (`FootPlanting`) are in `omuzaki-rig/src/ik/`. `PoseClip` (#223) is not on `main`.
`omuzaki-take-api` (#1149) is not on `main`. Spec v3 §37 on `main` is "Free software"; the worlds platform #718 points
at has no section yet, so locomotion is kept world-agnostic (§7.6) and promises nothing more.

## 2. D1: the contract and the crates

```text
 OS (Raw Input, CGEventTap, XI2, evdev, GlobalShortcuts portal, XInput/GameController via gilrs)
   │  omuzaki-input: one capture thread per backend; key codes live only in `normalise`
   ▼
 per backend b:  normalise ──► zone table ──► motion ring[b] (SPSC, 512) ───────┐ drained in turn (§2)
                     │                                                          └─► frame thread: MotionFeed ──► omuzaki-anim
                     ├──► binding matcher ──► intent ring[b] (SPSC, 64) ───────┐ drained in turn
                     │                   │                                      └─► frame thread: IntentFeed ──► locomotion
                     │                   └──► binding ring[b] (SPSC, 64) ──────┐ drained in turn
                     │                                                          └─► dispatcher: BindingFeed (PF-05, PF-25)
                     └──► held snapshot[b] (atomic, one writer) ── read by the feeds for reconciliation (§5)
 gamepad backend: its own motion ring (PadEdges), rings as above for pad bindings,
                  and a newest-wins PadState (triple buffer, levels only) ──► frame thread
```

**One producer per ring.** Every capture backend runs on its own thread and owns its own rings, so each ring has
exactly one producer and one consumer and stays a plain SPSC ring with no lock. Each ring is in order; **across rings
no global order is promised.** Backends are scheduled independently, so an event stamped earlier on one backend can
reach its ring after a later event on another has already been drained, and no barrier between them is worth a stall on
the frame thread. A consumer drains the running backends' rings in turn (in the order of the backends' registered
names, which only fixes whose ring is read first), and **the live order is the order it drained**. Backends register
themselves inside `omuzaki-input` (`register_input_backend!` next to each backend, with a stable name such as
`windows.raw_input`), and the hub hands each an opaque `BackendId`; the contract has no list of backends, so adding one
is a folder and nothing else. Determinism comes from recording that order, not from re-deriving it: a take's
`Motion` records keep the drained order (§9), and `ReplaySource` keeps each event's backend and its position in the
drained order, so a replay hands consumers exactly the same sequence. Cross-device order inside one 1/120 s window is
not visible in the motion anyway (each device drives its own hand or layer).

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-input-api` | API (over `omuzaki-core`, `serde`, `schemars`, `thiserror`) | the event and state types of §3, the zone table, `BindingId`, `BindingAction`, `Intent`, `BackendId`, `PadId`, `KeyChord` (cold, binding store only), `CaptureState`, `CaptureCaps`, the traits `MotionFeed`, `IntentFeed`, `BindingFeed`, `InputControl`, `Indicator`, `IndicatorHost`, `ChordStore` |
| `omuzaki-input-mock` | Mock | `FakeInput` (scripted events on `FakeClock`), `ReplaySource` (recorded input from text, deterministic), the conformance suite every backend passes |
| `omuzaki-input` | Implementation (`wasm = false`: OS input) | the hub, the normaliser and binding matcher, the rings, the indicator host, the kill switch, the backends (`windows`, `macos`, `linux_x11`, `linux_evdev`, `linux_portal`, `gamepad`), the ops `input.capture.*` and `input.binding.*` |

**Why not inside `omuzaki-track-api`** (#1511). Its contract is a camera and a tracker that produce one newest-wins
`TrackFrame` per camera frame, behind `FrameSource`, owned by the Runtime area; it has no events, no edges, no scope gate
and no notion of identity. OS input is the opposite on each count: edge events that must not be dropped (a key-up lost
is a finger stuck down), a state stream (pads), a privacy boundary, a risky scope and OS dependencies (`windows`,
`objc2-*`, `x11rb`, `evdev`, `gilrs`) that no tracker consumer should compile. Merging them would make every crate that
reads a tracker frame link the privacy-sensitive types, and would change a contract under review in another area.
What is shared is the time base: input events carry pipeline time (`Seconds`), mapped from the OS timestamp the way
`omuzaki-track-api::clock::ClockMap` maps a camera's; `omuzaki-input` reuses that pattern (a 64-sample window and the
drift bound), not the type.

**Why one implementation crate for every backend.** The research recommends one crate with one small backend per OS
(§7). Keeping the backends in the same crate as the normaliser is also what makes D2 structural: the raw key type
(`HidUsage`) is `pub(crate)`, so no other crate can name it. A backend split into its own crate would have to pass key
codes across a public type.

**The `input.*` namespace.** #301's tracker-input ops (`input.select`, `input.bind_camera`, `input.preview`,
`input.request_camera`) already use it. This design's ops are `input.capture.*` and `input.binding.*`, and its settings keys
are `input.motion.*`; there is no bare `input.bind` (too close to `input.bind_camera`). Capture is two ops, `start` and
`stop`, not #939's `input.capture.set { enabled }`, because an op declares one scope: starting needs `input`, stopping
must not. Locomotion's ops are `locomotion.*`.

## 3. D2: the event types, and why they carry no identity

```rust
// omuzaki-input-api
pub enum DeviceClass { Keyboard, Mouse, Pen, Gamepad }
pub enum Side { Left, Right }

/// A cell of the fixed keyboard grid. An index, once assigned, never changes meaning (like `Channel`).
pub struct KeyZone(u8);                       // 0..KeyZone::COUNT (16)
impl KeyZone {
    pub const COUNT: usize = 16;
    pub fn info(self) -> ZoneInfo;            // side, finger column, row band, centre (u, v) on the key plane
}
pub struct ZoneInfo { pub side: Option<Side>, pub finger: Finger, pub band: Band, pub centre: [f32; 2] }

pub enum MotionEvent {                        // Copy, at most 32 bytes; what the frame thread sees
    Key { zone: KeyZone, down: bool, at: Seconds },
    Buttons { device: DeviceClass, pressed: u8, released: u8, at: Seconds },   // mouse and pen buttons, bit per button
    Pointer { dx: f32, dy: f32, wheel: f32, at: Seconds },                      // coalesced relative motion, metres on the pad
    Pen { u: u8, v: u8, pressure: u8, tilt: [i8; 2], contact: bool, at: Seconds }, // 32 x 32 grid, coarse (below)
}
    PadEdges { pad: PadId, pressed: PadZones, released: PadZones, at: Seconds },  // queued, so a tap between drains is kept
}
pub struct PadId(u8);                         // a connected pad, stable while it stays connected
pub struct PadState { pub pad: PadId, pub sticks: [[i8; 2]; 2], pub triggers: [u8; 2], pub zones: PadZones, pub at: Seconds }
pub struct PadZones(u16);                     // DPad, Face, LeftStick, RightStick, LeftShoulder, RightShoulder, LeftTrigger, RightTrigger, Centre
pub struct BackendId(u16);                    // opaque; the hub assigns it from the backend's registered stable name (§2)
pub struct BindingId(u32);                    // stable for the binding's life, from the binding store
pub enum Edge { Press, Release, Repeat }
pub enum Intent { Move { x: i8, y: i8 }, Run, Jump, Sit, Emote(u8) }   // Copy: the locomotion intents (§7.1)
#[non_exhaustive]
pub enum BindingAction {                      // cold; owned by the binding store, set by input.binding.set_action
    Op { op: String, args: serde_json::Value },   // run by the binding dispatcher (#469)
    Trigger(u8),                                   // a PF-25 trigger slot (#478)
    Intent(Intent),                                // routed to the frame thread's IntentFeed, never to the dispatcher
}
pub struct BindingHit<'a> { pub id: BindingId, pub action: &'a BindingAction, pub edge: Edge, pub at: Seconds }
pub struct IntentEvent { pub id: BindingId, pub intent: Intent, pub edge: Edge, pub at: Seconds }   // Copy

pub trait MotionFeed: Send {                  // the frame thread's port; never blocks, never allocates
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame);   // events with at <= up_to: each ring in its order, rings in turn (§2)
    fn pad(&self, pad: PadId) -> Option<PadState>;                 // newest wins (levels only; edges are PadEdges)
}
pub struct MotionFrame { /* fixed array of MOTION_FRAME_MAX (128) events, a dropped count, and
                             reset: DeviceMask, the classes whose held state ended this frame (§4, §5a) */ }
pub trait IntentFeed: Send {                  // the frame thread's port for locomotion intents; never blocks, never allocates
    fn drain(&mut self, up_to: Seconds, out: &mut IntentFrame);   // fixed array of IntentEvent, plus reset like MotionFrame
}
pub trait BindingFeed: Send {                 // the binding dispatcher's port (ops side): resolved, ready to run
    fn poll(&mut self) -> Option<BindingHit<'_>>;   // a Release carries the action its press started (§5)
}
```

**The zone grid.** Sixteen zones by *physical position* (USB HID usage), so AZERTY, QWERTZ and JIS hands land where the
fingers are, and the table needs no layout setting. Letter keys are grouped by touch-typing hand and finger column
into eight zones of two to six letters: left outer-upper (`1 2 3 Q W E`, Tab, Esc), left outer-lower (`A S D Z X C`,
Caps, left Shift), left inner-upper (`4 5 R T`), left inner-lower (`F G V B`), and the mirror four on the right (`6 7 Y U`,
`H J N M`, `8 9 0 - = I O P [ ] \` and Backspace, `K L ; ' , . /` with Enter and right Shift). Then Space (thumb), left
modifiers, right modifiers, the function row, the navigation block, the arrows, the numpad and "other". The table is
data in `omuzaki-input-api` (`zones.rs`), generated from one list like the channel table, and covers every usage of the
ANSI, ISO and JIS boards (a test counts them).

**What a zone stream still reveals, said plainly.** Eight letter groups are close to a phone keypad: a dictionary can
recover much ordinary prose from a long zone sequence, and a short random password keeps only about 2.4 bits a
character. The zones are therefore coarse enough to animate and to be broadcast (a 1080p stream shows the same hands),
and they are still treated as personal data: process memory only, frame resolution in takes (§9), never in logs,
events or MCP. A person who wants less turns on **hand-only** detail (§6.2): zones are replaced by their side before
they leave the capture thread, and the fingers pick seeded positions. The default is a decision for the maintainers
(§12, question 1); this design recommends zones by default with a one-key "hush" binding to hand-only.

**Pointer and pen.** The mouse gives relative motion only (Raw Input, XI2 and the macOS tap all deliver it), coalesced
on the capture thread to one event per 1/120 s and scaled to metres on the virtual pad (a 400 DPI-equivalent default).
The design never reads the cursor's screen position: clicks at screen positions can be key identities (an on-screen
keyboard), and arm motion does not need them. #743's `Pointer { x, y, monitor }` is replaced. A tablet is absolute by
nature, so pen position is quantised on the capture thread to a 32 x 32 grid of the tablet (under 5 mm on a 15 cm
pad), pressure to 8 bits and tilt to 1 degree steps; the arm cannot show finer than that.

**Times.** Every event carries pipeline time quantised to the 1/120 s coalescing period. Motion consumes events per
frame; takes keep the frame only (§9). Dwell and flight times at 8 ms still carry some typing rhythm, which is why the
hot types stay in memory.

**How the boundary is enforced, not hoped for.**
1. **Types.** No public type of `omuzaki-input-api` that crosses a thread or a crate has a field derived from a key
   code; `KeyChord` (a binding's chord) appears only in the binding store's API (§5), which `Services` never holds.
2. **One module.** In `omuzaki-input`, `HidUsage` and the raw event are `pub(crate)` in `normalise`; the backends hand
   raw events straight to it through a `pub(crate)` sink, on the capture thread's stack, overwritten by the next event.
3. **No text path.** The crate has no `Display` or `Debug` for raw events (a manual `Debug` prints `<key>`), no logging
   of key codes, and panics inside a capture thread are caught and reported as that backend's `BackendState::Failed { reason }`
   with a fixed reason string (the crash-report design keeps only registered templates and sends no memory, `privacy.md` §3, §4).
4. **No injection or hooks.** No `SendInput`, `CGEventPost`, uinput, `SetWindowsHookEx` or DLL in the crate. A test
   in `omuzaki-input` fails if the crate's dependency tree in `Cargo.lock` holds `rdev`, `device_query`, `inputbot` or
   `willhook`, or if its `src/` names any of those symbols.
5. **The zone-equivalence test** (the acceptance of the hub task, §13 IN-3): two key streams that differ only in keys
   inside the same zones produce **byte-identical** output on every surface: the motion frames, the binding feed with
   no binding configured, `CaptureState`, op results and events, take `Motion` records and the hub's own diagnostics.
   It is the structural claim stated as a property test (proptest over random streams).

## 4. D3: the gate, the scope and the indicator

**Scope: `input`, which `main` already has.** No new scope, so no `omuzaki-ops-api` change. `perform` is not enough:
an agent with `perform` drives the avatar, which must never imply reading another app's keystrokes.

| Op | Scope | Who | Effect |
|---|---|---|---|
| `input.capture.start { classes }` | `input` | `person_only` (an agent or automation gets `Denied`, never queued) | starts or resumes capture for the device classes asked; fails with `NoIndicator` unless an installed indicator reports shown |
| `input.capture.pause` | `read` | anyone | pauses capture (`Paused`), keeping the classes; only a person's `start` resumes; idempotent |
| `input.capture.stop` | `read` | anyone | stops capture (`Off`); idempotent |
| `input.capture.state` | `read` | anyone | `CaptureState` and `CaptureCaps`, no device names or counts of keys; the pause reason only with `process` (below) |
| `input.capture.protect { programs }` | `process` | agent `Confirm` | sets the kill-switch list (below) |

`input.capture.pause` exists so that the person, an automation or an agent can step capture aside (a password prompt, a
private call) without losing the classes chosen; the hub's own pauses (indicator, protected program) use the same path.
| `input.motion.*` settings keys | (settings registry) | agent `Confirm` | the motion settings of §6.5 |

**State is per backend, summarised once.** Each running backend has a `BackendState`: `Running`,
`NeedsPermission { how: PermissionHow }` (macOS Input Monitoring, Linux `input` group for evdev) or
`Failed { reason }` (a fixed string). The global `CaptureState` is derived from them, never set on its own:
- `Off` before start and after stop; `Paused { why: Option<PauseWhy> }` (`IndicatorLost`, `ProtectedApp`) while paused;
- `On { classes, degraded }` while **any** backend runs: `classes` are the device classes actually being read, and
  `degraded` lists the classes asked for whose backend is not running (failed, missing permission, not built for this
  OS), so the person and the indicator see both;
- `Failed { reason }` only when capture was started and **no** backend runs.

A backend that fails while others run takes only its own epoch (below), releasing what it held; the rest keep running
and the state stays `On` with that class in `degraded`. `input.capture.state` returns the summary and the per-backend
list (`[{ name, classes, state }]`, no device names). Who sees `why` is limited by the kill-switch rule below.
`CaptureCaps` says per class and OS what works (`pen_pressure: Unavailable`, `typing: NeedsOptIn` on Wayland), so the UI
can say so (research §2).

**Off by default, never restored.** Capture state is not a setting and is not in the session file: settings.md D9
already says session restore "never restarts capture or output". A crash or restart comes back `Off`.

**Stopping and pausing are open to everyone.** Both only remove exposure, so they are registered with `read`, the
scope every actor holds (the runner requires `read` plus the op's own scope, so `read` adds nothing). A read-only agent,
an automation or a script can always turn capture off.

**The indicator is a precondition, not a courtesy.** An op request cannot carry anything a surface holds, so the
indicator is installed with the hub, not passed through an op. When a surface starts, it calls
`IndicatorHost::install(Box<dyn Indicator>)` on the hub: a hub API that, like `ChordStore`, is handed to the surface
directly and never inserted into `Services`, so no op, MCP tool or agent can install or replace an indicator. **An installed indicator is visible in every state, `Off` included**: it shows "Input capture: off" (with the Start
control, which only the person can use) before capture starts, and "Reading keyboard and mouse: zones only. Stop"
while it runs, so `shown()` describes the indicator, not the capture. The two surfaces that install one are the desktop
shell (its status-bar badge and tray state; task UI-1) and the CLI (a stderr status line; task CLI-1). `input.capture.start` then calls
`InputControl::start(classes)`, which fails with the typed `InputError::NoIndicator` unless an indicator is installed
and `Indicator::shown()` reads `true` at that moment; a headless process that installed none can never start capture. The hub calls
`shown()` once a second on its control thread; if it reads `false` (the window was hidden by a bug, the surface
uninstalled or dropped it) the hub pauses capture within a second and reports `Paused { why: IndicatorLost }`. Windows
and X11 show the user nothing of their own (research §5), so this is the only signal the person has.

**Every pause and stop ends what is held, without breaking single ownership.** A key held when capture pauses or
stops would otherwise never deliver its release. The control thread must not write into a backend's ring or snapshot
(each has exactly one writer, §2), so the release travels as an **epoch**, not as injected events:
1. Every transition out of `On` (an explicit `pause` or `stop`, a lost indicator, a protected program) bumps one atomic
   `CaptureEpoch` and then tells each backend thread to stop reading.
2. Each backend thread, on its own thread, discards what it has not pushed, clears its own held snapshot, and parks.
3. Each feed (`BindingFeed`, `IntentFeed`, `MotionFeed`) reads the epoch before every poll or drain. On a change it
   first ends everything it holds: the binding and intent feeds hand out a `Release` for every held-action record
   (§5), motion releases every zone, button and pad zone it holds (fingers lift, locomotion intents fall to zero), and
   then it discards any ring entry stamped with an older epoch (every pushed event carries the epoch it was read in).
   `CaptureState` is published only after both consumers have acknowledged the new epoch, or after 100 ms if one is
   stalled, in which case it applies the epoch on its next poll before anything else.
4. **Resuming starts with nothing held:** a key still physically down when capture resumes does nothing until it is
   released and pressed again (its release, matching no held-action record and no held zone, is ignored).

The same epoch path covers a **single backend** leaving while others keep running (a backend failure, a gamepad or
keyboard unplugged, the portal session closed): that backend gets its own epoch bump (`BackendEpoch[b]`), and the
consumers release what that backend's events were holding, and nothing else.

**The kill switch.** An optional list of protected programs (default empty). While capture is on and the list is not
empty, the control thread checks the running process names every 2 s and pauses capture while one runs. That check is
"whether a named program is running", which `Scope::Process` governs, and an actor who could both set the list and read
the pause reason could probe any program name. So, the safer of the two fixes:
- **Setting the list** is `input.capture.protect { programs }`, scope `process` (agent `Confirm`), or the person's own
  Settings page, which calls the hub directly. An actor without `process` can neither set nor read the list.
- **The pause reason never names a program**, and is shown only to actors that hold `process`: `input.capture.state`
  returns `Paused { why: Some(_) }` (`ProtectedApp` or `IndicatorLost`) to them and plain `Paused { why: None }` to
  everyone else. Every reason is hidden, not only `ProtectedApp`, because showing `IndicatorLost` alone would let a
  reader infer the other by elimination. The person's own indicator shows the full reason and the program, since it is
  their list.

**Platform gaps** stay visible: on Wayland typing and the pointer need the evdev opt-in (the `input` group, with the
research's warning that it can read every keyboard). Bindings there need the GlobalShortcuts portal backend (task IN-9,
which waits on the owner's answer to §12 question 2). **Until IN-9 lands, the Wayland default is gamepad only**:
`CaptureCaps` reports keyboard bindings and typing as `NeedsOptIn` and the UI says why. PF-29 on Wayland is partial;
that is research risk 2.

## 5. D4: bindings, triggers and locomotion intents on the same source

A **binding** is `{ id: BindingId, name, device: DeviceClass, chord: KeyChord | PadChord, action: BindingAction }`. The
action is what PF-05 and PF-25 own: an op invocation (expression, toggle, prop, scene), a trigger slot (`omuzaki-anim`
`Triggers`), or a locomotion intent (`Move(dir)`, `Run`, `Jump`, `Sit`, `Emote(n)`). This design fixes only how the
source delivers them; the action catalogue and the dispatcher belong to #469 and #478.

- **The chord store.** Chords are in `omuzaki-input`'s own machine-local file (`input-bindings.toml` in the data dir),
  read only by the hub. It is not a settings key: settings are listed to agents (`settings.list`) and their values
  appear in events, and `SecretRef`-style hiding exists only for secrets. The store's API (`ChordStore`) is handed to the
  desktop UI directly and **never inserted into `Services`**, so no op, MCP tool or agent can reach it.
- **What ops and agents see.** `input.binding.list` (scope `read`) returns `{ id, name, device, action, conflict }`,
  never a chord; `input.binding.set_action { id, action }` and `input.binding.remove { id }` (scope `perform`, agent
  `Confirm`). An action is itself an op request, so `set_action` also checks that the caller holds every scope the
  action's op needs: a binding cannot become a way for an actor to make the person's key press run an op the actor
  could not run itself. The dispatcher runs a binding's action as the person, with `cause: binding` in the journal.
  Learning a chord ("press the keys") is a UI call on `ChordStore::learn(id)`, a person's action by construction; its
  result is shown in the binding editor and written to the store, and nothing goes through the executor. A conflict
  (the same chord on two bindings) is reported, not overwritten.
- **Matching.** On the capture thread, after the zone is computed, the raw event is compared with the active table (a
  fixed array of at most `MAX_BINDINGS` (64) chords, swapped in whole by the control thread when the store changes). A
  match pushes an internal entry `(gen, slot, edge, at)` to that backend's ring for the binding's consumer: **intent
  bindings** go to the backend's intent ring (read by the frame thread through `IntentFeed`), **every other binding**
  to its binding ring (read by the dispatcher through `BindingFeed`). Each ring has one producer and one consumer.
  Modifier state is kept inside `normalise`. A non-match produces nothing.
- **Resolution is the feed's job, not the consumer's.** The generation tables, slots, held snapshots and held-action
  records are `pub(crate)` in `omuzaki-input`; the feeds hand consumers resolved values only: `BindingHit { id, action,
  edge, at }` (the stable `BindingId` and a borrow of the `BindingAction`) and `IntentEvent { id, intent, edge, at }`.
  So #469's dispatcher runs `hit.action` and #478's triggers press `Trigger(n)`, and neither needs to know a table
  exists. The rest of this list is how the feeds keep that resolution right.
- **Identity across table swaps.** Every swap bumps the table's generation, and every entry carries the generation it
  was matched against. The feed resolves `(gen, slot)` against **that** generation's table, never the current one: it
  keeps each retired table (a fixed pool of 4 generations, recycled off the hot path) until the last queued entry of
  that generation has drained, so a queued slot can never index another binding. If 4 generations are pending (edits
  faster than the consumers drain), the control thread waits to swap instead of recycling a table that still has
  queued entries. An entry from a retired generation whose binding (by its `BindingId`) is gone from the current table
  is dropped and counted; so that this never strands a held action, the feed hands out a `Release` for every action
  held for a binding the new table removes when it takes the swap.
- **A press owns its release.** A key can be held across a swap: pressed under the old table, released under the new
  one. So each feed keeps a held-action record per `BindingId` (a fixed array of `MAX_BINDINGS`), written at the press:
  the generation and the action the press started. A `Release` resolves to **the action in that record**, whatever its
  own generation or the current table says, and clears it; a `Repeat` repeats the recorded action. If the binding's
  action was edited while it was held, the old action ends at the release and the new action never starts from that
  press; the next press starts the new one. A `Release` with no record (its press was dropped, or happened before a
  resume) is not handed out.
- **A lost release is never permanent.** Inside a run of capture, entries can be dropped only when a ring is full (its
  consumer is stalled); a pause, a stop or a backend leaving releases everything held (§4). A full ring keeps releases,
  as the motion ring keeps key-ups: it drops its oldest `Repeat`, then its oldest complete tap (a `Press` with its
  `Release` queued behind it, both together), then its oldest lone `Press`, and drops a `Release` only if the ring holds
  nothing else. Each backend also publishes a held snapshot (the slots it holds down now, with its generation). Whenever
  a ring has dropped anything, the feed reconciles: every held-action record whose slot the snapshot shows as up gets a
  synthesised `Release`; a slot held in the snapshot with no record is left alone (a missed press is not replayed,
  since its moment has passed). So a stalled consumer can delay a release but cannot leave movement or a held trigger
  stuck.
- **Consumers.** The binding dispatcher (#469) polls `BindingFeed` on the ops side and runs each hit's action (an op,
  or a PF-25 trigger press); the frame thread drains `IntentFeed` and folds the intents into an `Intents` value per
  frame (§7.1).
  Gamepad bindings (`PadChord`: a button, a combination, or a stick past a threshold) match in the same place.
- **Wayland.** Bindings use the GlobalShortcuts portal: the portal sends only the shortcuts we registered, which is the
  binding table itself, so the privacy rule holds by construction there.
- **Default bindings** ship as a preset the person enables (locomotion: `W A S D`, Space, left Shift to run, `C` to
  sit; the pad's left stick and face buttons). A preset is just bindings; nothing is bound until the person applies it.

Note for #469 (filed by the lead, not here): PF-05 consumes `BindingFeed` from `omuzaki-input` and adds no second OS
capture path; OSC, MIDI and Stream Deck are other sources of the same `BindingHit`.

## 5a. Invariants: held state, ordering and scopes

The review rounds found three classes of problem more than once. This section states each as one rule and lists every
place in the design it applies, so the tasks test the class, not one instance.

**1. Nothing stays held when its cause goes away.** Every piece of held state, what ends it, and the task that tests it:

| Held state | Ended by | Test |
|---|---|---|
| A binding's action (move, run, a held trigger) | its release, through the press's held-action record, even across a table swap (§5); a removed binding at the swap (§5); reconciliation after a ring drop (§5); a capture epoch (pause, stop, indicator, protected program; §4); its backend's own epoch (failure, unplug, portal session closed; §4) | IN-3 |
| A zone, mouse or pen button held in motion | its release edge; `MotionFrame.reset` for its class on any capture or backend epoch; a change of `input.motion.detail` or `hand` (motion resets the keyboard and mouse classes at the switch, since zones and sides no longer match) | IN-3 (epochs), MO-1, MO-2 |
| Pad zones and sticks | the next `PadState`; on unplug the gamepad backend publishes a neutral `PadState` and its epoch resets the gamepad class | IN-5, MO-3 |
| Locomotion intents (move, run) | their binding's release, or a reset of the class that drove them; with no intent the machine decays to `Idle` | LO-1 |
| A locomotion state mid-flight (jump, sit, emote) when `locomotion.set` changes the def | the state finishes under the def it started with (the old def is kept until the machine is next in `Idle` or `Move`), then the new def applies | LO-1, LO-4 |
| A palm goal, posture or stick look | the hand allocator's rest timers, and the 0.3 s fade when a tracked bone takes over | MO-1, MO-3 |
| Held state inside a take | the checkpoint holds it (§9); a reset or settings change is recorded in the frame it happened (`reset`, `HAS_SETTINGS`), so replay ends the same things at the same frame | ST-4 |

**2. Order is promised only where one writer makes it.** Each ring is in its producer's order; across rings and
threads nothing is promised, and determinism comes from recording the drained order (§2). Every other order in the
design is single-threaded and fixed: the frame stages (§6.1), the pose layers (`PoseLayers` lowest first), the IK stage
(look-at, palms, limbs, feet), the take's chunk records (takes.md T2). Each piece of shared state has exactly one
writer: a ring and a held snapshot are written only by their backend's thread; the binding table and the epochs
only by the control thread; consumers only read them. No step anywhere writes into another thread's ring.

**3. Every op names its scope, and none widens what its caller can do.**

| Op or key | Scope | Agent | Why |
|---|---|---|---|
| `input.capture.start` | `input` | never (`person_only`) | reads other apps' input |
| `input.capture.pause`, `input.capture.stop` | `read` | free | only removes exposure |
| `input.capture.state` | `read` | free | the pause reason only with `process` (§4) |
| `input.capture.protect` | `process` | `Confirm` | checks whether named programs run |
| `input.binding.list` | `read` | free | ids, names, actions; never a chord |
| `input.binding.set_action`, `input.binding.remove` | `perform`, plus every scope of the action's op | `Confirm` | a binding cannot launder an op its setter could not run (§5) |
| `input.binding.apply_preset` | `perform`, plus every scope of the preset's actions | `Confirm` | the preset's chords are data in `omuzaki-input`; the op names the preset only |
| chord learning, indicator install | none: hub APIs handed to the surface, never ops or services | not reachable | identities and the indicator stay out of the executor |
| `input.motion.*` settings keys | settings registry, `User` | `Confirm` | capture-adjacent, so never `Free` (settings.md rule) |
| `locomotion.set` | `edit:character` | as the registry allows | writes document values (§7.4) |
| `locomotion.home` | `perform` | free | moves the live root only |
| `locomotion.state` | `read` | free | state, speed, root |

## 6. D5: the motion model

### 6.1 Where it enters the frame (spec §8)

```text
input ─► track ─► retarget ─► life ─► anim blend ──────────────────────────────► IK ───────► constraints ─► springs
 drain        (Performer: LayerStack, LifeLayer, Animator)    │                    ▲  look-at, palms,   (PR #1650)
 MotionFrame                                                  ▼                    │  limbs, feet
                                       PoseLayers: [locomotion, device posture, fingers]  (masked by !tracked)
                                       IkGoals + palm goals + look-at  ────────────┘
```

1. **input**: the stage drains `MotionFeed` up to `tick.time − D` (the A/V delay of anim-api §6.1; the ring holds
   500 ms at the worst event rate, so any `D` the ramp allows fits).
2. **anim blend**: the `Performer`'s animator produces the `HumanoidPose` as today (bind pose, head split, lean,
   breath, clips). Input motion then resolves its pose layers onto it with `PoseLayers::resolve`, lowest first:
   `locomotion` (Override, whole body), `posture` (Override, spine, shoulders and arms: leaning to the desk, holding a
   pad), `fingers` (Override, finger bones). Each layer's mask is ANDed with `!tracked`.
3. **IK**: the stage's IK stage gains palm goals: `LimbsAndGaze` becomes look-at, then **palms** (`HandTarget::solve`
   for a hand with a palm goal), then two-bone limbs (hands without a palm goal, and the feet), then foot planting while
   locomotion runs. Then node constraints and springs as on `main` and in #1650: twist bones follow the IK-placed
   forearm, hair and sleeves see the typing arm.

No `omuzaki-anim-api` change: `PoseLayer`, `BoneMask`, `IkGoals` and `Limb` already carry everything except a palm's
orientation, which goes through `omuzaki-rig`'s `HandTarget` in the stage (an App, which may depend on both). The
motion output is an `omuzaki-anim` type:

```rust
// omuzaki-anim: input_motion
pub struct MotionOut {
    pub layers: [PoseLayer; 3],          // locomotion, posture, fingers (weight 0 when idle)
    pub palms: [Option<PalmGoal>; 2],     // left, right; root space
    pub look: Option<LookAt>,             // stick look (§6.4); merged into IkGoals.look_at by the stage
    pub root: RootStep,                   // locomotion's root delta (§7.3)
}
pub struct PalmGoal { pub frame: Transform, pub pole: Vec3, pub weight: f32 }
```

### 6.2 Typing

- **Desk rig.** `DeskRig` is computed once per character (cold) from its skeleton: shoulder width, upper and lower arm
  length, chest height, and the prop anchors (§8). The key plane's (u, v) map to root space through the keyboard
  anchor. Works the same on imported avatars (byom D4: the rest is the T-pose, the humanoid map gives the bones).
- **Key down in zone z**: the zone's side picks the hand (Space goes to the hand not on the mouse), its finger column
  picks the finger. The palm goal moves toward the zone's centre, offset back by that finger's reach so the fingertip,
  not the palm, lands; the move is a critically damped spring (ω 30 rad/s: 90 % in about 80 ms), so a burst of keys at
  10 a second reads as a busy hand, not a teleport. The finger layer plays a strike on that finger (down 40 ms, up
  80 ms, the hand-shape library's curl angles), and the wrist dips 3 mm.
- **Idle.** 0.4 s without a key on a hand: the hand drifts to its home-row zone. 1.5 s: the hand eases off the board to
  the character's rest (0.5 s blend, palm goal weight to 0). These are the numbers of #748, kept.
- **Hand-only detail.** Zones arrive as their side only; the finger and the target cell within that side's half come
  from counter-based draws (`Rng::new(mix(seed, frame, hand))`, seed per take, §9), so the motion still reads as typing and carries no key information.
- **Mirroring (PF-27).** A mirrored preview mirrors the pose and swaps the hands as anim-api does for everything else;
  zones are physical and never mirrored themselves.

### 6.3 Mouse and pen

- **Mouse.** The hand (right by default, `hand` setting) follows a virtual mouse on the pad plane: deltas integrate into
  a position clamped to the pad rectangle (default 0.22 x 0.18 m) with a slow recentre (time constant 1.5 s), so a long
  drag never walks off the pad and nothing depends on the cursor's real position. The palm goal is the mouse's grip
  frame; a button press curls the index or middle finger (30 ms), the wheel flicks the middle finger. Smoothing is the
  spring above, not a One-Euro filter: the input is already coalesced and the spring is cheaper and deterministic.
- **Pen.** The hand holds the pen (a grip hand shape and the pen prop's grip anchor); the palm goal follows the coarse
  pen position on the tablet plane; contact lowers the hand by up to 4 mm with pressure; tilt turns the wrist (clamped
  to ±40 degrees). Hover (in range, no contact) lifts 1 cm. Where the backend cannot give pen data (research §8 risk 3,
  until the hardware spike), the pen hand falls back to the mouse model.

### 6.4 Gamepad

- Both hands hold the pad (`TwoHandedProp` on the pad's grip anchors) at chest height; the posture layer is a pose from
  the pose library (`hold-pad`), scaled by the desk rig.
- Thumbs: the finger layer bends each thumb so its tip follows its stick cap within the stick's travel; a face or d-pad
  zone press (a queued `PadEdges` event, so a tap shorter than a frame still moves the thumb) moves the thumb to that
  cluster; shoulder and trigger zones curl the index fingers with the trigger value.
- **Stick look** (opt-in, #749's numbers kept: ±35° yaw, ±20° pitch, smoothing 0.1 s, return in 0.3 s) is a `LookAt`
  goal with `eye_share` 0.3, **not** a channel write. #749 proposed the `Trigger` layer, but that layer sits above the
  tracker in `LayerStack` and would override a tracked head. The stage gives the look-at goal weight 0 whenever the bus
  says the tracker owns any head channel (`bus.owners.mask_of(Source::Tracker)` intersects `ChannelMask::HEAD`), fading
  with the same 0.3 s hand-over.

### 6.5 Who owns a hand, and the hand-over to tracking (PF-23)

- **The hand allocator.** Each hand is in one state: `Rest`, `Keyboard`, `Mouse`, `Pen` or `Pad`. The most recent
  device active for that side wins after a 0.15 s dwell (so one stray key while mousing does not yank the hand); a
  switch is a transit of 0.25 s on the palm goal. A gamepad active (stick past its dead zone or a button in the last
  1 s) takes both hands. Device motions implement one trait so the gamepad module plugs in without editing the others:

  ```rust
  pub trait DeviceMotion: Send {
      fn class(&self) -> DeviceClass;
      fn feed(&mut self, frame: &MotionFrame, pad: Option<&PadState>);
      fn activity(&self) -> Activity;                        // which hands it wants, and when it last saw input
      fn step(&mut self, tick: Tick, desk: &DeskRig, hands: Hands, out: &mut MotionOut);
  }
  ```

- **Tracked bones win.** The stage passes `tracked: BoneMask`, the bones a tracker source (VMC body, PF-04 and PF-07;
  hand tracking, PF-06) currently drives at weight at least `OWN_THRESHOLD`. Empty today: no body tracker is wired on
  `main`. Layers are masked with `!tracked`; a hand whose arm is tracked drops its palm goal; fingers tracked by hand
  tracking drop the finger layer for that hand. Each change fades the affected weight over 0.3 s with `Fade::toward`,
  so ownership flips half-way, exactly as channels do (anim-api §6).
- **Settings** (`input.motion.*`, User scope, agent `Confirm`): which devices animate, `hand` (left or right mouse
  hand), `detail` (`zones` or `hand_only`), stick look on or off and which stick. Taste and setup, not project data.

## 7. D6: locomotion (PF-30)

### 7.1 Intents and the state machine

`Intents` is built per frame from binding events (key bindings) and pad state (left stick), never from zones:
`{ move: Vec2 (−1..1, camera-relative), run: bool, jump: Edge, sit: Edge, emote: Option<u8> }`.

| State | Enters from | On | Goes to | Blend |
|---|---|---|---|---|
| `Idle` | any ground state | `move` > 0.1 | `Move` | 0.25 s |
| `Move` (1-D blend by speed: idle 0, walk 1.4, run 3.5 m/s at average stature) | `Idle` | `move` ≤ 0.1 and speed < 0.05 m/s | `Idle` | 0.25 s |
| `Jump` (anticipate 0.15 s, air, land 0.2 s) | `Idle`, `Move` | `jump` press | `Land` → previous ground state | 0.1 s in, 0.2 s out |
| `Sit` (down clip, seated loop, up clip) | `Idle` | `sit` press; again or `move` to stand | `Idle` after the up clip | 0.2 s |
| `Emote(n)` | `Idle`, `Sit` | `emote` | the state it came from, when the clip ends or on `move` | in 0.2 s, out 0.3 s |

Speed follows `move` with an acceleration limit (6 m/s²) and is scaled by stature (leg length over the average
template's). Turning faces the move direction at up to 360 degrees a second; above 90 degrees off, the character turns
in place first. Jump and emote presses during a transition are queued for one frame, not dropped. The table is the
test: every allowed pair has a test, every other pair is refused.

### 7.2 Gait and clips

- **Walk and run are procedural** (a parametric gait: stride, cadence, double-support share, arm swing, pelvis sway and
  bob, flight phase for run), phase-locked so the blend between them never crosses feet. Why procedural: licence-clean
  by construction (no motion data to clear), it scales to any proportion (chibi to tall, spec §32: big heads need slower,
  larger arm swings) from the same numbers, and the stride is known exactly, which root motion needs. The numbers start
  from published gait norms cited in the task, never copied from a product.
- **Jump, sit and emotes are pose clips** (#223's `PoseClip`, keys of pose-library poses), authored by Character-lane
  tasks as TOML in `crates/omuzaki-anim/library/poses/clips/`. #223's walk cycle stays as a pose-library example and
  a fallback; locomotion uses the gait.
- Clips carry foot-contact flags per key; `FootPlanting::set_contacts` takes them, the gait gives its own.
- Users bring clips later through motion import (RG-08, VRMA) into a set; nothing here blocks that.

### 7.3 Root motion, the stage and cameras

- **Root.** Clips and the gait are in place. The root moves by the integrated velocity, and the gait's cadence follows
  speed so that stride × cadence = speed; foot planting (`FootPlanting`, with `set_root` given the moving root) removes
  what is left of the slide. `RootStep { translation: Vec3, yaw: f32 }` is output by locomotion; the stage applies it.
- **Where the root lives.** The member's live placement in the stage (not the document: walking is a performance, like
  a head turn, not an edit). It is clamped to a walk radius around the authored placement (default 4 m, a
  `LocomotionDef` field; a per-scene floor area belongs to the worlds work), and `locomotion.home` returns it to the authored
  placement over 0.5 s. A take records it (§9).
- **Cameras (ST-02).** Framings already follow the `Subject` anchor through its `AnchorFrame` (position and yaw of the
  member); ST-2 builds that frame from the live root, so a framed camera follows the walker. Two additions, both owned by the stage area: a follow lag (critically damped,
  default 0.4 s) so a jump does not shake the frame, and `auto_full` (switch to the `Full` framing while moving, back
  after 1 s idle; off by default). Camera rotation never follows the character's yaw by default (a streamer's shot
  stays put); "behind" framing is left to the worlds work.

### 7.4 Data format, and who owns it

```toml
# the values materialised at characters/<id>/locomotion (ADR 0019 §4); the seed item omuzaki/locomotion/default
# carries the same values in its recipe
id = "default"
walk_speed = 1.4            # m/s at average stature
run_speed = 3.5
accel = 6.0
turn_rate_deg = 360.0
[gait]
stride = 0.72               # m at walk, average stature
arm_swing_deg = 18.0
bob = 0.025
[blend]
idle_move = 0.25
jump_in = 0.1
jump_out = 0.2
emote_in = 0.2
emote_out = 0.3
[clips]
jump = "jump"
sit_down = "sit_down"
sit_idle = "sit_idle"
stand_up = "stand_up"
emotes = ["cheer", "bow", "dance"]
```

`LocomotionDef` is that table as a serde type with a JSON Schema (`omuzaki-anim`), validated when written (speeds
positive, walk below run, every clip it names present beside it).

**A locomotion set is a library item, and applying it materialises it** (ADR 0019 §4). The item is of kind `setting`,
and its recipe is one step, `locomotion.set { character: $character, def, clips }`, which carries the values, never a
name to look up. `library.apply` therefore writes into the document:
- `characters/<id>/locomotion`: the whole `LocomotionDef` (every number above);
- `characters/<id>/locomotion/clips/<name>`: each clip the set uses, as its keys with their poses written out as
  sparse `HumanoidPose` values (not pose-library ids), contact flags and loop flag;
- `characters/<id>/applied/<app>`: ADR 0019's record, with the pin (`item`, `version`, `hash`), the values written and
  the item's rights.

The runtime reads only the document, so removing, replacing or updating an installed item never changes a project:
an update is ADR 0019 §5's three-way merge, one undo step, and `library.lock` verifies the pin. `locomotion.set`
(scope `edit:character`) is also the op a person or agent uses to edit the values directly. The first-party seed item,
`omuzaki/locomotion/default`, lives in `crates/omuzaki-anim/library/seed/locomotion/default.toml` and registers through
the crate's existing `register_library_source!` (ADR 0019 §8). `locomotion.state` (scope `read`) reports state, speed
and root position.

### 7.5 Who authors the clip set

The Character lane: the gait task (code and its default numbers, with citations) and the clip task (TOML key poses),
signed off by a maintainer from CI evidence renders (§11). No third-party motion data.

### 7.6 Worlds

Nothing here assumes a stage or a camera layout: `Intents` in, a pose layer and a `RootStep` out. A worlds platform
can drive the same machine with its own intents and apply `RootStep` to its own physics.

## 8. D7: props (ST-03)

Four prop kinds on #705's `PropKind` registry, each a low-poly mesh built in code (no asset files), with named anchors
that motion reads whether the prop is visible or not:

| Kind | Anchors | Default placement |
|---|---|---|
| `input.keyboard` | `key_plane` (origin at the home-row centre, X right, Z away from the character, Y up; size in metres), `zone_cells` (from the zone table) | on a desk plane at 0.1 m below the elbow height of the desk rig, 0.28 m in front of the chest |
| `input.mouse` | `pad_plane`, `grip` | right of the keyboard (left if `hand` is left) |
| `input.tablet` and `input.pen` | `tablet_plane`; the pen attached to the hand, `grip` | in place of the mouse |
| `input.gamepad` | `grip_left`, `grip_right`, `stick_left`, `stick_right`, `face`, `dpad` | attached to the hands, at chest height |

The desk rig computes the default placement from the character's proportions, so a chibi and a tall character both
reach their keyboard. A person can move a prop (`stage.set_prop`); motion reads the anchors each frame. A prop set
"desk" toggles all four. The props are in the scene document; the motion is not.

## 9. D8: what a take stores (SH-01)

One new record kind in `omuzaki-take-api`, `Motion = 12` (after `Watermark`), added in a minor version as T3 allows
(an older reader skips it by `len` and counts it). One record per cast member per frame period while motion or
locomotion is active, fixed little-endian layout, about 40 to 80 bytes (plus the checkpoint at group starts):

```
member u8 | flags u8 (HAS_KEYS, HAS_BUTTONS, HAS_POINTER, HAS_PEN, HAS_PAD, HAS_LOCO, HAND_ONLY, CHECKPOINT)
  | reset u8 (DeviceMask: classes whose held state ended before this frame's edges, §5a) | aux u8 (bit 0 HAS_SETTINGS)
keys:     n u8 | n × (zone u8 | down u8)                      // edges, in drained order within the frame (§2); no times
buttons:  n u8 | n × (source u8 | button u8 | down u8)        // edges: source 0 mouse, 1 pen
pad_edges: n u8 | n × (pad u8 | zone u8 | down u8)            // from MotionEvent::PadEdges, one entry per zone bit
pointer:  dx f32 | dy f32 | wheel f32                         // summed over the frame (motion integrates per frame)
pen:      u u8 | v u8 | pressure u8 | contact u8 | tilt i8 × 2 | 2 reserved   // the frame's last sample
pad:      pad u8 | sticks i8 × 4 | triggers u8 × 2 | reserved u8               // levels of the active pad; zones are pad_edges
loco:     state u8 | flags u8 (RUN, JUMP, SIT) | emote u8 | reserved u8 | move i8 × 2 | 2 reserved | root x, y, z f32 | yaw f32
settings: len u16 | the motion settings (§6.5) as written, when they changed this frame (HAS_SETTINGS)
checkpoint (group-start chunks only): len u16 | MotionCheckpoint bytes (below)
```

**Edges, not bitmaps.** Keys, mouse and pen buttons and pad zones (with their pad) are recorded as the edges that arrived in the frame,
in drained order, so a click or a tap shorter than one frame (down and up in the same 33 ms) replays as the strike it was. A
per-frame bitmap of what is held would lose it. Levels (pointer deltas, pen position, sticks, triggers, the move vector)
are values, since motion samples them once per frame.

**Checkpoints at group boundaries** (so a clip that starts mid-motion re-renders exactly). A replay clip can start at
any group start (takes.md T2), but motion carries state across frames. So the group-start chunk's `Motion` record for
each member carries `CHECKPOINT` and the member's whole motion state, `MotionCheckpoint` (`Copy`, fixed layout, about
300 bytes; carried in the record rather than in `State` so the 16 KiB `State` cap can never omit it):
- **randomness:** none to store. Motion's randomness (hand-only fingers, strike jitter) is counter-based, a fresh
  `Rng::new(mix(seed, frame, hand))` per draw, so the "rng position" is the chunk's frame number and the seed is in
  `State` (the `motion` live-state provider, `register_take_state!`, with the motion settings);
- **hands:** per hand the allocator state, the dwell and transit timers, the palm spring's position and velocity, each
  finger's strike phase, the keys held per zone, the tracked-bone fade weights;
- **pointer and pen:** the virtual mouse position and its recentre state, the buttons held, the last pen sample;
- **pad:** the last `PadState`, the stick-look smoothing value and its fade;
- **locomotion:** the state and sub-phase (jump anticipate, air, land; sit down, seated, up), the transition in flight
  (from, to, progress), speed, facing, the gait phase, the clip time, a queued intent, and the root.

The rule that makes this complete is structural: the motion types keep their mutable state **only** in their
`MotionCheckpoint` field, and `restore` replaces it whole, so nothing can be left out of the checkpoint without being
left out of the motion too.

- **Never stored:** a key code, a chord, a binding name or id, a sub-frame time, a cursor position. Binding events are
  stored only as their effect: an expression or op is already an `Event` record (T1), and a locomotion intent is in the
  `loco` bits.
- **Re-rendering.** From a checkpoint, motion is a pure function of these records, the desk rig, the props and the
  seed, so both of T6's modes re-run it; the recorded root keeps the path exact even if bounds changed. A different character
  re-solves the same zones with its own arms.
- **Hand-only** frames carry `HAND_ONLY` and zone values reduced to their side.
- The record is personal data under T5: off with the input track (`record.input`), deleted with the take, and never
  picked up by publish or upload ops unless the person names the file.

## 10. D9: budgets on `R`

`cpu.total` on `R` is 8 ms of CPU per 30 fps frame (spec §10). This design asks for at most **0.6 ms (7.5 %)**:

| Part | Thread | Budget | Basis (estimate until measured) |
|---|---|---|---|
| Capture, normalise, match, coalesce | capture | 0.2 % of 8 cores (0.5 ms of CPU per frame), with a keyboard, a 1000 Hz mouse and a pad polled at 120 Hz | the research's proposed `input.capture` row; mouse coalescing to 120 Hz caps the work per frame |
| Delivery, OS event to the queue | capture | 1 ms p95 | research §6 proposal; a spec row is a maintainers' decision (§12, question 3) |
| Drain, allocator, device motions, three `PoseLayers::resolve` | frame | 0.05 ms p95 | anim-api §13: a 55-bone lerp is about 1 µs on a busy machine; three layers plus springs for two hands and ten fingers are tens of µs |
| Palm IK (two `HandTarget` solves) and foot planting | frame | 0.03 ms p95 | two-bone solves are µs each |
| Locomotion: gait, one clip sample, blend | frame | 0.02 ms p95 | two pose samples and one blend |

Measured like `physics.step`: `Dev` pinned to an E-core, criterion benches as regression detectors
(`omuzaki-anim/benches/input_motion.rs`, `benches/locomotion.rs`, `omuzaki-stage/benches/input_frame.rs`), printed as
`scaled`. Nothing on the frame thread allocates or locks: the rings are SPSC, `MotionFrame` is a fixed array, pad state
is a triple buffer, the binding table is swapped whole by an atomic pointer exchange off the frame thread. The
scheduler's degrade step for this system is to drop finger strikes (keep palm goals) before anything else.

## 11. Evidence and tests

- **Numbers** in tests: fingertip within 1 cm of the zone centre at the next frame, no fingertip below the key plane by
  more than 2 mm over a 60 s replay, pointer hand inside the pad, thumb tips within 5 mm of the stick caps in 8
  directions, 35.0 ± 0.5 degrees of stick look, no bone jumping more than 2 degrees between 30 fps frames over a 120 s
  locomotion run, root distance per cycle within 2 % of stride × cycles (the #748 to #750 numbers, kept).
- **Pictures** are CI evidence cases, not local contact sheets. `omuzaki-anim` cannot build or render a body, so the
  cases live in **`crates/omuzaki-test-char/evidence.toml`** (new file), the Character area's test crate, which may
  depend on `omuzaki-char`, `omuzaki-rig`, `omuzaki-anim` and the mocks: a test builds a template character, drives the
  replay through the motion and the rig, and renders front and three-quarter frames through the mock toon renderer
  (`omuzaki-render-mock`, as `omuzaki-char`'s `head-part` case does) at three statures. Pull requests that change motion
  carry the `visual` label.
- **Determinism** (decision 0009): seeded streams only, ticks only, equal digests over two runs and after a JSON round
  trip of the recorded input.

## 12. Open questions for the maintainers and the owner

1. **Zone detail by default** (§3): `zones` (recommended; what users of the incumbents expect) or `hand_only`.
2. **Wayland** (§4): PF-29 typing and mouse only through the evdev opt-in, and bindings only once IN-9 (the
   GlobalShortcuts portal) lands; until then gamepad only. Owner to confirm P0 ships partial there (research risk 2),
   and that IN-9 is wanted.
3. **Budget rows** (§10): add `input.capture` (1 ms p95 delivery, 0.2 % CPU) and an `input.motion` frame row to §10, or
   keep them as design targets inside `cpu.total`.
4. **Anti-cheat** (research risk 1): the vendor questions of research §7 spike 5 stay with the maintainers; the design
   needs no exemption from anyone.

## 13. Tasks

The re-cut of #743, #744, #748 to #751, #939 and #1168. Each is about 400 changed lines or less. Rules every task
inherits: module lists are generated (`omuzaki-anim` from `src` by its build script, `omuzaki-stage` from `src` and
`src/ops`, ADR 0054 §1), so no task adds a `mod` line there; a task that adds a top-level folder to `omuzaki-anim` edits
nothing else to declare it. Inside a folder a task creates (`input_motion/`, `locomotion/`, `perform/` in the stage) the
`mod.rs` is hand-written, so a later task's fence includes its one `mod` line there; those tasks are ordered, so the
line never conflicts. The first of MO-1 and LO-1 to merge adds the `omuzaki-input-api` dependency to
`omuzaki-anim/Cargo.toml`; the other merges `main` and drops its copy. A new bench adds its `[[bench]]` entry to the crate's `Cargo.toml` (inside the fence). A new
crate describes itself in its own `Cargo.toml` (`[package.metadata.omuzaki]` `kind` and `summary`), never in
`crates/README.md`. A new dependency adds its row to `docs/licensing/register.md` and a file in
`docs/licensing/deps/`. `Cargo.lock` changes cargo makes for an allowed dependency are inside every fence.

| ID | Replaces | Crate | Model | Blocked by |
|---|---|---|---|---|
| IN-1 | #743 (contract half) | `omuzaki-input-api` | sonnet | this design |
| IN-2 | #743 (mock half) | `omuzaki-input-mock` | haiku | IN-1 |
| IN-3 | #744 (core half) | `omuzaki-input` | sonnet | IN-1, IN-2 |
| IN-4 | #744 (Windows half) | `omuzaki-input` | sonnet | IN-3 |
| IN-5 | new | `omuzaki-input` (gamepads) | sonnet | IN-3 |
| IN-6 | new | `omuzaki-input` (Linux) | sonnet | IN-3 |
| IN-7 | new | `omuzaki-input` (macOS) | sonnet, local lane | IN-3 |
| IN-8 | #939 (input half) | `omuzaki-input` (ops) | haiku | IN-3 |
| IN-9 | new (Wayland bindings) | `omuzaki-input` (portal) | sonnet | IN-3; the owner's answer to §12 question 2 |
| UI-1 | new (indicator) | `omuzaki-ui` | sonnet | IN-1 |
| CLI-1 | new (indicator) | `omuzaki-cli` | haiku | IN-1 |
| UI-2 | new (binding editor, kill-switch list) | `omuzaki-ui` | sonnet | IN-3, IN-8 |
| WIRE-1 | new (App wiring) | `omuzaki-cli` | sonnet | IN-3, IN-8, CLI-1, ST-1 |
| MO-1 | #748 (typing half) | `omuzaki-anim` | sonnet | IN-1 |
| MO-2 | #748 (pointer half) | `omuzaki-anim` | sonnet | MO-1 |
| MO-3 | #749 | `omuzaki-anim` | sonnet | MO-1 |
| LO-1 | #750 | `omuzaki-anim` | sonnet | IN-1, #223 |
| LO-2 | new (gait) | `omuzaki-anim` | sonnet | LO-1 |
| LO-3 | #751 | `omuzaki-anim` (data) | sonnet | LO-1, #223 |
| LO-4 | #939 (locomotion half) | `omuzaki-anim` (ops) | haiku | LO-1 |
| ST-1 | #1168 (motion half) | `omuzaki-stage` | sonnet | IN-3, MO-1 |
| ST-2 | #1168 (locomotion half) | `omuzaki-stage` | sonnet | ST-1, LO-2 |
| ST-3 | new (props) | `omuzaki-stage` | sonnet | #705, MO-1 |
| ST-4 | #1168 (take half) | `omuzaki-take-api`, `omuzaki-stage` | haiku | ST-2, #1149, #1357 |
| EV-1 | the contact sheets of #748 to #751 | `omuzaki-test-char` | sonnet | MO-3, LO-3 |
| SP-1 | research spikes 1 to 4 | none (a report) | local lane | IN-4 |

The table is each task's model and `Blocked by` line; the sections below give the title, crate, fence, contract and
acceptance. Order: IN-1 → (IN-2, MO-1, LO-1, UI-1, CLI-1 in parallel) → IN-3 → (IN-4 to IN-9, UI-2, MO-2, MO-3, LO-2, LO-3, LO-4) → ST-1 → WIRE-1, ST-2,
ST-3 → ST-4, EV-1. MO-1 and LO-1 need only the contract and the mock, so the motion work never waits on an OS backend.

### Who owns each boundary

Every public type and handle this design adds, who produces it, who consumes it, and the task that connects them. A
row without both ends inside a task would be an interface nobody owns; there is none.

| Boundary | Producer (task) | Consumer (task) |
|---|---|---|
| `InputHub` construction, `Services` entry `dyn InputControl` | `omuzaki-input` (IN-3) | the App (WIRE-1); ops (IN-8) |
| `IndicatorHost`, `Indicator` | hub (IN-3) | desktop shell (UI-1), CLI (CLI-1), handed over by WIRE-1 |
| `ChordStore` | hub (IN-3) | desktop shell's binding editor (UI-2), handed over by WIRE-1 |
| `MotionFeed` (`MotionEvent`, `PadEdges`, `MotionFrame.reset`) | hub and backends (IN-3 to IN-7, IN-5 for pads) | stage frame (ST-1) → `InputMotion` (MO-1 to MO-3); set by WIRE-1 |
| `PadState` levels | gamepad backend (IN-5) | `pad_motion` (MO-3), locomotion `move` (ST-2) |
| `IntentFeed` (`IntentEvent`) | hub (IN-3), portal (IN-9) | stage locomotion (ST-2) → `Locomotion` (LO-1); set by WIRE-1 |
| `BindingFeed` (`BindingHit`) | hub (IN-3), portal (IN-9) | the binding dispatcher of #469 (actions) and #478 (trigger presses); WIRE-1 hands it over |
| `BindingAction` values | `input.binding.set_action`, `apply_preset` (IN-8) | the feeds (IN-3) |
| `CaptureState`, `BackendState` | hub (IN-3) | `input.capture.state` (IN-8), indicators (UI-1, CLI-1) |
| `input.motion.*` settings keys | registered by LO-4 | stage (ST-1), by settings subscription |
| `MotionOut`, `PalmGoal`, `DeskRig` | `omuzaki-anim` (MO-1) | stage IK stage (ST-1) |
| `InputPropAnchors` | stage props (ST-3) | desk rig input in the stage (ST-1) |
| `Intents`, `RootStep`, `LocomotionDef` | `omuzaki-anim` (LO-1, LO-2) | stage (ST-2); `locomotion.set` (LO-4) |
| `locomotion.home`, `locomotion.state` | stage (ST-2) | ops callers |
| `MotionCheckpoint`, the `motion` take provider, the `Motion` record | MO-1, LO-1 (checkpoint), ST-4 (record, provider) | the take reader (#1149 family), re-render |
| `register_input_backend!` | each backend folder (IN-4 to IN-7, IN-9) | the hub (IN-3) |

### IN-1: `omuzaki-input-api`: events, zones, bindings, capture state and the traits (new API crate)

- **Fence:** `crates/omuzaki-input-api/**` (new: `Cargo.toml`, `src/lib.rs`, `src/event.rs`, `src/zones.rs`,
  `src/binding.rs`, `src/capture.rs`, `src/feed.rs`, `tests/zones.rs`, `tests/no_identity.rs`).
- **Contract:** §3 and §5 as written: `KeyZone` and the 16-zone table by HID usage, `ZoneInfo`, `MotionEvent`,
  `PadState`, `PadZones`, `PadId`, `BackendId`, `BindingId`, `BindingAction`, `Intent`, `BindingHit`, `IntentEvent`, `Edge`,
  `KeyChord` and `PadChord` (cold), `CaptureState`, `BackendState`, `CaptureCaps`, `PermissionHow`, `PauseWhy`,
  `DeviceMask`, `MotionFrame` (`MOTION_FRAME_MAX` 128), `IntentFrame`, `MAX_BINDINGS` 64, `InputError` (with
  `NoIndicator`), the traits `MotionFeed`, `IntentFeed`, `BindingFeed`, `InputControl`, `Indicator`, `IndicatorHost`,
  `ChordStore`, and `InputHub` (the one constructor contract an App calls: it returns the `Services` entry, the two
  surface handles and the three feeds, §13 "Who owns each boundary"). Needs the
  Director's approval as a new contract.
- **Acceptance:** the zone table covers every usage of the ANSI (104), ISO (105) and JIS (109) boards, counted in a test;
  `MotionEvent` is `Copy` and at most 32 bytes (`size_of` test); serde round trip of every cold type; a test that no
  public hot type has a field of a chord type (by `JsonSchema` walk); `#![deny(missing_docs)]`; `lint-deps` and
  `lint-crate-table` accept the crate; check green.

### IN-2: `omuzaki-input-mock`: `FakeInput`, `ReplaySource` and the conformance suite (new mock)

- **Fence:** `crates/omuzaki-input-mock/**` (new).
- **Contract:** IN-1. `FakeInput` (scripted events, pad states and binding hits on `FakeClock`; implements `InputHub`,
  `MotionFeed`, `IntentFeed`, `BindingFeed`, `InputControl`, `IndicatorHost` and `ChordStore`, so every consumer task
  tests against the mock before IN-3 lands), `ReplaySource::from_text` (the recorded-input text format: one event per line, zone names, times
  in seconds and the backend, in drained order), `conformance::run(&mut dyn MotionFeed, …)`.
- **Acceptance:** replay of a 60 s script is identical over two runs and after a text round trip; `drain` allocates 0
  bytes (counting allocator); `drain(up_to)` never returns an event after `up_to` (proptest); a dropped-event count when
  more than `MOTION_FRAME_MAX` events arrive in one drain; a replay of events from several backends hands them out in
  exactly the recorded drained order, on every run; check green.

### IN-3: `omuzaki-input`: the hub, the normaliser, the binding matcher, the gate and the zone-equivalence test (new crate)

- **Fence:** `crates/omuzaki-input/**` except the backend folders: `Cargo.toml` (`wasm = false`, reason "OS input";
  `registers = true`; `generated-mods = ["src"]` with the `omuzaki-embed` build script, so each backend task adds its
  own folder, gated inside by `#![cfg(target_os = …)]`, and nobody edits `lib.rs` again), `build.rs`, `src/lib.rs`,
  `src/hub.rs`, `src/normalise.rs`, `src/matcher.rs`, `src/rings.rs`, `src/dispatch.rs` (draining the rings, the
  generation pool, held actions and reconciliation), `src/indicator.rs`, `src/kill_switch.rs`, `src/store.rs`; `tests/privacy.rs`,
  `tests/gate.rs`, `tests/bindings.rs`, `benches/normalise.rs` and its `[[bench]]` entry.
- **Contract:** IN-1; ops-api `Scope::Input`; §3 (rules 1 to 5), §4 and §5. Implements `InputHub` and the three feeds
  (`MotionFeed`, `IntentFeed`, `BindingFeed`), `InputControl`, `IndicatorHost` and `ChordStore`; `register_input_backend!`
  for the backend folders; a `pub(crate)` `RawSink` the backends call.
- **Acceptance:** the zone-equivalence proptest (§3 rule 5) over every output surface the crate has; `start` fails with
  `NoIndicator` when no indicator is installed and when the installed one reports hidden, and capture pauses within
  1.1 s of `Indicator::shown()` turning false (`FakeClock`); **held across a pause:** hold a movement binding and a key,
  then pause (once each by `pause`, `stop`, a lost indicator and a protected program): the movement action ends and the
  next `MotionFrame` carries `reset` for the keyboard class before the state is published, and after resuming the
  still-held key does nothing until pressed again; **one backend leaves:** with keyboard and gamepad both capturing,
  fail the gamepad backend while a pad binding and a key are held: the pad binding's action ends and the gamepad class
  resets, while the key stays held; **single writers:** a debug-build check on every ring and held snapshot records its
  writer thread and fails if a second thread writes; **a press owns its release:** press a binding, swap in a table that changes its action, release:
  the old action ends and the new one never starts, and the next press starts the new one; the binding table
  matches chords with modifiers and reports conflicts; a full ring drops the oldest motion event and counts it, and
  never drops a key-up (a key-up displaces the oldest pointer event first); **table swaps:** an edit that swaps the
  table while events are queued runs no wrong action (queue presses on slots 0 to 3, swap in a table that removes
  slot 1's binding and gives slot 2's chord another action, drain: slot 1's held action ends at the swap and its queued
  release is dropped and counted, slot 2's press and release both run its old action, and no event runs an action of
  another binding); **a lost release:** hold a movement binding,
  overflow its backend's intent ring with repeats from a stalled consumer, release the key, resume the consumer: the
  movement intent ends (by the queued release or by reconciliation against the held snapshot) within one drain, and the
  same for an op binding through `BindingFeed`;
  the forbidden-symbol test of §3 rule 4;
  `normalise` plus `match` at most 1 µs per event on `Dev` (bench); check green.

### IN-4: `omuzaki-input`: the Windows keyboard, mouse and pen backend (Raw Input, no hooks)

- **Fence:** `crates/omuzaki-input/src/windows/**`, `tests/windows.rs`, the `windows` dependency in
  `crates/omuzaki-input/Cargo.toml` with its register row and `docs/licensing/deps/` file.
- **Contract:** `RawSink` (IN-3); research §1.1 and §7: Raw Input with `RIDEV_INPUTSINK` on the crate's own hidden window
  and thread, buffered reads, no hooks; pen through the HID digitiser only if SP-1 confirms, otherwise
  `CaptureCaps::pen = Unavailable`.
- **Acceptance (Windows-only tests, skipped elsewhere with a message):** `SendInput` into a test window gives the
  expected zones and pointer deltas; delivery at most 2 ms p95 from `SendInput` to `drain`; capture-thread CPU at most
  0.05 % of 8 cores idle over 60 s on `Dev` (#744's numbers); the `x86_64-pc-windows-gnu` cross-build and check green.
  (`SendInput` is used only by the test binary, never by the crate.)

### IN-5: `omuzaki-input`: gamepads on every OS (gilrs, XInput on Windows)

- **Fence:** `crates/omuzaki-input/src/gamepad/**`, `tests/gamepad.rs`, the `gilrs` dependency
  (`default-features = false`, `xinput` on Windows) with register row and deps file.
- **Contract:** `PadState`, `PadZones`, `PadId`, `MotionEvent::PadEdges`, `PadChord` (IN-1); a `register_input_backend!`
  named `gamepad.gilrs`; research §1.1, §1.2, §1.5, §4.
- **Acceptance:** a mapping test from gilrs events to `PadState`, queued `PadEdges` and `PadZones` with a fake gilrs
  event list (a press and release between two drains arrive as two edges, with the pad id); dead zone
  and quantisation (i8, u8) exact at the edges; hot-plug of a second pad keeps pad indices stable; the libudev link is
  dynamic (recorded under the LGPL-shared rule); unplugging a pad with a stick deflected and a button held publishes a
  neutral `PadState` and the backend's epoch, so nothing stays deflected or pressed; check green on Linux and the
  Windows cross-build.

### IN-6: `omuzaki-input`: Linux X11 (XI2 raw events) and the evdev opt-in

- **Fence:** `crates/omuzaki-input/src/linux_x11/**`, `src/linux_evdev/**`, `tests/linux.rs`, the `x11rb` (`xinput`)
  and `evdev` dependencies with register rows and deps files.
- **Contract:** `RawSink`; research §1.3, §1.4; `CaptureState::NeedsPermission { how: InputGroup }` when evdev is chosen
  without access.
- **Acceptance:** an Xvfb test (skipped with a message where the runner has no Xvfb) sends XTest events to a client window and reads zones and deltas; evdev reads a
  `uinput` test device where the runner allows it, else skips with a message; `XI_RawMotion` is unselected while no
  pointer motion is enabled; check green.

### IN-7: `omuzaki-input`: macOS (listen-only event tap, GameController background events)

- **Fence:** `crates/omuzaki-input/src/macos/**`, `tests/macos.rs`, the `objc2`, `objc2-core-graphics` and
  `objc2-game-controller` dependencies with register rows and deps files.
- **Contract:** `RawSink`; research §1.2; `NeedsPermission { how: InputMonitoring }`; re-enable a tap disabled by
  timeout; `shouldMonitorBackgroundEvents = true`.
- **Acceptance:** a `cargo check --target aarch64-apple-darwin` in CI (the weekly macOS job runs the tests); a manual
  checklist on a Mac (local lane) in the PR body: prompt shown once, tap recovers, secure input mode yields no keys.

### IN-8: `input.capture.*` and `input.binding.*` ops

- **Fence:** `crates/omuzaki-input/src/ops/**` (one file per op), `crates/omuzaki-input/tests/ops.rs`.
- **Contract:** ops-api (`register_op!`, `Scope::Input`, `person_only`, `require_person`), `InputControl` as a service;
  §4 and §5 tables: `input.capture.start`, `input.capture.stop`, `input.capture.state`, `input.capture.protect`,
  `input.capture.pause`, `input.binding.list`, `input.binding.set_action`, `input.binding.remove`,
  `input.binding.apply_preset { preset }` (scope `perform`, agent `Confirm`; the preset's chords are data inside
  `omuzaki-input`, so the op carries only the preset name). No op takes or returns a chord or an indicator.
- **Acceptance:** table tests with person, agent and automation actors (an agent's `input.capture.start` is `Denied`
  and never queued; a person's `start` with no indicator installed fails with `NoIndicator`; **a read-only actor**
  (grants `read` only) stops and pauses capture; stopping and pausing are idempotent; `input.binding.set_action` with an
  action whose op needs a scope the caller lacks is `Denied`); a test that types a known string through
  `FakeInput` and scans every op result, event and journal record for any key code or character of it (none);
  **the protected-program list:** `input.capture.protect` is refused without `process` (an actor with `input` and
  `perform` only gets `Denied`); with capture paused by a protected program, `input.capture.state` returns
  `Paused { why: None }` to an actor without `process`, and the same for a pause by a lost indicator, so the two cannot
  be told apart; with `process` it returns `Paused { why: Some(ProtectedApp) }`; no result, event or journal record
  names a program; registrations found by the link gate; check green.

### IN-9: `omuzaki-input`: Wayland bindings through the GlobalShortcuts portal

- **Waits on** the owner's answer to §12 question 2 (what PF-29 ships on Wayland). Until it lands, the Wayland default is
  gamepad only (§4).
- **Fence:** `crates/omuzaki-input/src/linux_portal/**`, `tests/portal.rs`, the `ashpd` dependency (its
  `global_shortcuts` feature) in `crates/omuzaki-input/Cargo.toml` with its row in `docs/licensing/register.md` and its
  file in `docs/licensing/deps/`.
- **Contract:** `BindingFeed`, `IntentFeed` (IN-1), the internal ring, generation and held-snapshot machinery (IN-3),
  a `register_input_backend!` named `linux.portal`; §5 ("Wayland"): the binding table is registered
  as the portal's shortcut list (one shortcut per binding, described by its name, never by a chord we invent), and every
  table swap re-registers it; `Activated` and `Deactivated` signals become `Press` and `Release` on the portal backend's
  own binding ring; the portal gives no zones, so this backend produces no motion events. `CaptureCaps` reports
  `bindings: Portal` and `typing: NeedsOptIn`.
- **Acceptance:** against a fake portal on a private D-Bus session bus (`dbus-daemon --session` started by the test,
  skipped with a message where the runner has none): registering a table of three bindings sends three shortcuts with
  their names and no key codes; activation and deactivation signals arrive as `Press` and `Release` with the right
  `(gen, slot)`; a table swap re-registers and stamps the new generation; a portal that refuses or is missing gives
  the portal backend `BackendState::Failed` with a fixed reason, leaves the other backends running and the global state
  `On` with keyboard bindings in `degraded` (and `Failed` only when the portal was the only backend); check green.

### UI-1: `omuzaki-ui`: the capture indicator in the desktop shell

- **Platform area** (the Platform lead signs off the shell files).
- **Fence:** `crates/omuzaki-ui/src/shell/input_indicator.rs` and its `mod` line in `src/shell/mod.rs`,
  `crates/omuzaki-ui/tests/input_indicator.rs`, the `omuzaki-input-api` dependency in `crates/omuzaki-ui/Cargo.toml`.
- **Contract:** `Indicator`, `IndicatorHost`, `CaptureState`, `BackendState` (IN-1); §4: a status-bar badge and tray
  state **in every capture state, `Off` included** ("Input capture: off" with a Start control for the person; while on,
  the classes read, the degraded classes, the detail, and a one-click Stop that calls `input.capture.stop`; while
  paused, the full reason and program for the person); installed with the hub at shell start (the handle comes from
  WIRE-1); `shown()` is true only while the badge is actually drawn in a visible window.
- **Acceptance:** with the shell's test harness, the badge is drawn at `Off` and `shown()` is true, so a person's
  `input.capture.start` succeeds; the badge changes within one frame of `On`; `shown()` turns false when the window is
  hidden or the badge is removed, and capture then pauses; Stop ends capture; strings go through the localisation
  pipeline; check green.

### CLI-1: `omuzaki-cli`: the capture banner

- **Platform area.**
- **Fence:** `crates/omuzaki-cli/src/input_banner.rs` and its `mod` line in `src/lib.rs`,
  `crates/omuzaki-cli/tests/input_banner.rs`, the `omuzaki-input-api` dependency in `crates/omuzaki-cli/Cargo.toml`.
- **Contract:** `Indicator`, `IndicatorHost` (IN-1); §4: a stderr status line printed as soon as the indicator is
  installed ("input capture: off") and kept current in every state, reprinted after other output; installed only when
  stderr is a terminal (`shown()` is false otherwise, so a piped or headless run can never start capture). The handle
  comes from WIRE-1.
- **Acceptance:** with stderr a pseudo-terminal the line prints at `Off`, `shown()` is true there and `start` succeeds,
  and the line changes on `On`, `Paused` and `Off`; with stderr redirected `start` fails with `NoIndicator`; check
  green.

### UI-2: `omuzaki-ui`: the binding editor and the protected-program list

- **Platform area.**
- **Fence:** `crates/omuzaki-ui/src/shell/input_bindings.rs` and its `mod` line in `src/shell/mod.rs`,
  `crates/omuzaki-ui/tests/input_bindings.rs`.
- **Contract:** `ChordStore` (learn, set, remove, conflicts; IN-1, IN-3), the ops `input.binding.*` and
  `input.capture.protect` (IN-8), §4 and §5: the only place chords are learnt and shown, and the person's own editor for
  the kill-switch list (it calls the hub through the handle WIRE-1 gives the shell).
- **Acceptance:** learning a chord through the harness stores it and shows it only in this panel; the panel's actions
  that go through ops carry no chord (checked on the recorded requests); a conflicting chord is shown, not overwritten;
  check green.

### WIRE-1: wiring `omuzaki-input` into the Apps

- **Owner:** the App that builds `Services` (today `omuzaki-cli`'s session, `crates/omuzaki-cli/src/session.rs`; the
  desktop App's startup path does not exist on `main` yet, so its half is a follow-up in the same shape once it does).
- **Fence:** `crates/omuzaki-cli/src/session.rs`, `crates/omuzaki-cli/Cargo.toml` (the `omuzaki-input` dependency),
  the CLI's link gate `crates/omuzaki-cli/tests/registrations.rs` expectations (the one-line `link()` call is inside
  the fence by standing ruling), `crates/omuzaki-cli/tests/input_wiring.rs`.
- **Contract:** `InputHub` (IN-1, built in IN-3): construct the hub once per process; insert its `dyn InputControl`
  into `Services`; hand `IndicatorHost` to the surface (CLI-1 here, UI-1 in the desktop App) and `ChordStore` only to
  the desktop shell (UI-2); give the stage its `MotionFeed` and `IntentFeed` (through ST-1's and ST-2's setters) and the
  binding dispatcher its `BindingFeed` (#469); call `omuzaki_input::link()`. `omuzaki-mcp`'s standalone server is
  deliberately not wired: there `input.*` ops answer `Unavailable`, which a test pins.
- **Acceptance:** a CLI session with a terminal stderr lists the `input.*` ops, `input.capture.state` answers `Off`,
  and a person's `start` reaches the hub; the link gate passes; `omuzaki mcp` answers `Unavailable` for
  `input.capture.start`; check green.

### MO-1: `omuzaki-anim`: input motion, the desk rig, the hand allocator and typing

- **Fence:** `crates/omuzaki-anim/src/input_motion/{mod.rs,desk.rs,hands.rs,typing.rs}` (a new folder, declared by the
  build script), `crates/omuzaki-anim/tests/input_motion_typing.rs`, `crates/omuzaki-anim/benches/input_motion.rs` and
  its `[[bench]]` entry, the `omuzaki-input-api` dependency (and `omuzaki-input-mock` as a dev-dependency) in
  `crates/omuzaki-anim/Cargo.toml`.
- **Contract:** IN-1 and IN-2; `PoseLayer`, `BoneMask`, `LookAt`, `Transform` (anim-api); the hand-shape library
  (`hands/`); §6.1, §6.2, §6.5: `MotionOut`, `PalmGoal`, `DeskRig`, `DeviceMotion`, `Hands`, `InputMotion`.
- **Acceptance (replay on mock humanoids at three statures):** the striking fingertip within 1 cm of its zone centre at
  the next frame; no fingertip more than 2 mm below the key plane over 60 s; home row after 0.4 s and rest after 1.5 s
  without keys (± one frame); `hand_only` output identical for two streams with the same sides; a `tracked` mask drops
  the masked bones with a 0.3 s fade; `MotionFrame.reset` for the keyboard class lifts every held finger in that
  frame, and so does switching `detail` or `hand` with keys held; `InputMotion::checkpoint` and `restore` (§9): restoring the checkpoint taken at
  any frame of a 60 s replay and stepping on gives bit-identical `MotionOut` to the uninterrupted run; the motion types
  hold their mutable state only in `MotionCheckpoint`; deterministic; `step` allocates 0 bytes; at most 0.02 ms per frame
  on `Dev` (bench); check green.

### MO-2: `omuzaki-anim`: mouse and pen motion

- **Fence:** `crates/omuzaki-anim/src/input_motion/{pointer.rs,pen.rs}` and their `mod` lines in `input_motion/mod.rs`, `crates/omuzaki-anim/tests/input_motion_pointer.rs`.
- **Contract:** MO-1's `DeviceMotion`; §6.3.
- **Acceptance:** the mouse hand never leaves the pad rectangle over a 60 s random-walk replay; recentre time constant
  1.5 s ± 5 %; a click curls the index finger within one frame; pen contact lowers the palm by pressure × 4 mm ± 0.5 mm;
  the allocator switches hands with the 0.15 s dwell and 0.25 s transit; deterministic, 0 allocations; check green.

### MO-3: `omuzaki-anim`: gamepad motion and stick look

- **Fence:** `crates/omuzaki-anim/src/pad_motion/**` (a new top-level folder, so `input_motion` is not edited),
  `crates/omuzaki-anim/tests/pad_motion.rs`, `crates/omuzaki-anim/library/poses/hold-pad.toml`.
- **Contract:** MO-1's `DeviceMotion`, `PadState` and `PadEdges` (IN-1), `TwoHandedProp` shape of goals (two palm
  goals), §6.4.
- **Acceptance:** at three statures, thumb tips within 5 mm of the stick caps at full deflection in 8 directions; full
  right-stick deflection gives 35.0 ± 0.5 degrees of look yaw after settling; with the tracker owning a head channel
  the look goal's weight is 0 after 0.3 s; a face-button tap whose press and release fall between two frames still
  moves the thumb; deterministic, 0 allocations; check green.

### LO-1: `omuzaki-anim`: the locomotion state machine and root motion

- **Fence:** `crates/omuzaki-anim/src/locomotion/{mod.rs,machine.rs,def.rs,intents.rs}`,
  `crates/omuzaki-anim/tests/locomotion.rs`, `crates/omuzaki-anim/benches/locomotion.rs` and its `[[bench]]` entry, the
  `omuzaki-input-api` dependency if MO-1 has not added it.
- **Contract:** `PoseClip` (#223), `PoseLayer`, IN-1's `IntentEvent` (from `IntentFeed`) and `PadState` for `Intents`; §7.1, §7.3, §7.4:
  `Intents`, `LocomotionDef`, `Locomotion::step(tick, &Intents, &DeskRig, &mut PoseLayer) -> RootStep`. The gait is a
  trait (`Gait`) with an in-place placeholder until LO-2.
- **Acceptance:** a test per allowed state pair, and every other pair refused; no bone jumps more than 2 degrees between
  30 fps frames over a scripted 120 s run; speed respects the 6 m/s² limit; turn rate at most 360 degrees a second;
  `LocomotionDef` validation errors name the field; a def change during a jump finishes the jump under the old def and
  applies the new one from the next `Idle` or `Move`; a reset of the class driving `move` brings the machine to `Idle`;
  checkpoint and restore at any frame of the 120 s run (including
  mid-transition and mid-jump) continue bit-identically (§9); deterministic, 0 allocations; at most 0.01 ms on `Dev`;
  check green.

### LO-2: `omuzaki-anim`: the procedural walk and run gait

- **Fence:** `crates/omuzaki-anim/src/locomotion/gait.rs` and its `mod` line in `locomotion/mod.rs`, `crates/omuzaki-anim/tests/locomotion_gait.rs`.
- **Contract:** LO-1's `Gait`; §7.2; foot-contact flags in the shape `FootPlanting::set_contacts` takes.
- **Acceptance:** root distance per cycle within 2 % of stride × cycles; contacts alternate with the double-support
  share given; run has a flight phase; the walk mirrored and shifted half a cycle equals itself within 1e-5; stride and
  cadence at three statures printed beside the cited reference values; check green.

### LO-3: `omuzaki-anim`: the jump, sit and emote clips

- **Fence:** `crates/omuzaki-anim/library/poses/clips/{jump,sit_down,sit_idle,stand_up,cheer,bow,dance}.toml`, the poses
  they key in `crates/omuzaki-anim/library/poses/`, `crates/omuzaki-anim/tests/locomotion_clips.rs`.
- **Contract:** `PoseClip` (#223), the pose library, §7.2 (#751's list without walk and run, which LO-2 makes). These
  are the authoring sources; a project never reads them at run time: LO-4's seed item writes their keys out as pose
  values when it is applied (§7.4).
- **Acceptance:** each clip loads, loops where marked with C1 seams, mirrors (`mirrored(mirrored(c)) == c`), and has
  contact flags; the dance loop is 8 beats at 120 bpm; licence notes cite only public biomechanics references; check
  green. Pictures come from EV-1.

### LO-4: `locomotion.*` ops and the motion settings

- **Fence:** `crates/omuzaki-anim/src/locomotion/ops.rs`, `crates/omuzaki-anim/src/input_motion/settings.rs`, their
  `mod` lines in the two folders' `mod.rs`, the seed item `crates/omuzaki-anim/library/seed/locomotion/default.toml`
  (registered by the crate's existing `register_library_source!`), `crates/omuzaki-anim/tests/locomotion_ops.rs`.
- **Contract:** ops-api; ADR 0019 (§4 materialise and pin, §5 update, §8 seed); the settings registry
  (`settings.md`); `locomotion.set { character, def, clips }` (`edit:character`, values only, never a name) and the
  `input.motion.*` settings keys of §6.5 (User, agent `Confirm`). `locomotion.home` and `locomotion.state` act on the
  stage's live root, so they are ST-2's.
- **Acceptance:** undo of `locomotion.set` is exact; `library.apply` of the seed item writes the whole def and every
  clip's keys as pose values under `characters/<id>/locomotion` plus an `applied/<app>` record with the pin, and
  `library.lock` verifies it; after the item is uninstalled or replaced by another version, the project's locomotion
  plays bit-identically (the runtime reads only the document); a def naming a clip that is not beside it is a typed
  error that names it; the settings keys pass the registry's `tests/settings.rs` checks; the seed passes the crate's
  seed test; check green.

### ST-1: `omuzaki-stage`: input motion in the frame, palm goals in the IK stage

- **Fence:** `crates/omuzaki-stage/src/perform/motion.rs` and its `mod` line in `src/perform/mod.rs` (hand-written),
  `crates/omuzaki-stage/src/frame/rig.rs` (palm goals and foot
  planting in the IK stage), `crates/omuzaki-stage/tests/input_frame.rs`, `crates/omuzaki-stage/benches/input_frame.rs`
  and its `[[bench]]` entry, the `omuzaki-input-api` dependency in `crates/omuzaki-stage/Cargo.toml`. Runtime-area files:
  the Runtime lead signs off.
- **Contract:** `MotionFeed` (IN-1), handed in through a new `Stage::set_motion_feed` that WIRE-1 calls; `InputMotion`
  and `MotionOut` (MO-1); `HandTarget` and `FootPlanting` (rig); the `input.motion.*` settings (LO-4), read through a
  `SettingsEvent` subscription and copied on change, never per frame; the device-prop anchors from ST-3 when present,
  else the desk rig's defaults; §6.1.
- **Acceptance:** a 60 s replay (typing, mouse) on the mock character gives equal frame digests over two runs; the
  stage's zero-allocation frame test still passes; drain honours the A/V delay (events at `t` act at `t + D`); added
  CPU at most 0.08 ms per frame on `Dev` (drain, motion and palm IK, §10; bench, `scaled`); check green.

### ST-2: `omuzaki-stage`: locomotion, the live root and camera follow

- **Fence:** `crates/omuzaki-stage/src/perform/locomotion.rs` and its `mod` line in `src/perform/mod.rs`; the ops
  `locomotion.home` and `locomotion.state` in `crates/omuzaki-stage/src/ops/locomotion/` (they act on the live root,
  which only the stage holds; the `src/ops` list is generated); the place
  where the stage builds each member's `AnchorFrame` (`omuzaki-stage-api` `resolve.rs`: `root` is the member's position
  and yaw), which takes the live root instead of the authored one; `crates/omuzaki-stage/tests/locomotion_frame.rs`. A follow-lag field on `CameraDef` is an `omuzaki-stage-api` change:
  it needs the Director's approval on the task before work starts, and lands as its own PR first if approved.
- **Contract:** `Locomotion`, `RootStep` (LO-1); `Intents` from `IntentFeed` (handed in through a new
  `Stage::set_intent_feed` that WIRE-1 calls) and `PadState`; §7.3; `locomotion.home` (`perform`) and
  `locomotion.state` (`read`).
- **Acceptance:** added CPU at most 0.02 ms per frame on `Dev` (§10); the root stays inside the walk radius over a
  120 s random-intent run; `locomotion.home` returns within
  0.5 s ± one frame; a `Subject`-framed camera follows with the configured lag; deterministic; check green.

### ST-3: `omuzaki-stage`: the input device props

- **Fence:** `crates/omuzaki-stage/src/input_props/**` (a new top-level folder, declared by the build script),
  `crates/omuzaki-stage/tests/input_props.rs`.
- **Contract:** #705's `PropKind` and `register_prop_kind!`; §8's kinds and anchors. #705's `PropOutput` carries no
  anchors, so this task does not change it: the anchors are computed in `input_props` from each prop's params and
  placement (`InputPropAnchors`, a stage-internal type) and handed to ST-1's desk rig each frame.
- **Acceptance:** each kind builds a mesh under 2,000 triangles; its anchors move with the prop when `stage.set_prop`
  moves it, and ST-1's hands follow; the default placement puts the key plane within reach (every zone centre inside 95 % of arm reach) at three statures; check green.

### ST-4: the take's `Motion` record

- **Fence:** `crates/omuzaki-take-api/src/record.rs` (the new kind and its layout, minor version bump; a contract change
  that needs the Director's approval), `crates/omuzaki-stage/src/take/motion.rs`, `crates/omuzaki-stage/tests/take_motion.rs`.
- **Contract:** takes.md T2, T3 and T5; §9; the `motion` live-state provider (`register_take_state!`: the seed and the
  motion settings) and the per-frame data ST-1 and ST-2 already hold.
- **Acceptance:** a take recorded during a 120 s replay contains no key code (scan for every usage of the typed string),
  re-renders to the same frames, and an older reader skips the record and counts it; **replay cuts:** for every group
  boundary of that take, a clip cut there and re-rendered from its checkpoint gives frame digests equal to the uncut
  run's from that frame on (the cuts include one inside a jump, one mid-transition and one during a typing burst);
  **short edges:** a click and a key tap whose down and up fall in the same frame replay as a strike; **resets and
  settings:** a pause with keys held, and a `detail` switch, recorded mid-take replay with the same fingers lifting in
  the same frame; check green.

### EV-1: the evidence cases for typing, pointer, pad and locomotion

- **Fence:** `crates/omuzaki-test-char/evidence.toml` (new), `crates/omuzaki-test-char/tests/input_motion_render.rs`,
  `omuzaki-input-mock` and `omuzaki-render-mock` as dev-dependencies in `crates/omuzaki-test-char/Cargo.toml`.
- **Contract:** MO-1 to MO-3, LO-1 to LO-3, `tools/ci/evidence.py`'s case format.
- **Acceptance:** four cases (typing, mouse and pen, pad, locomotion strip), each front and three-quarter at three
  statures, fixed camera and seed; they render identically twice; the PR carries the `visual` label.

### SP-1: the hardware spikes (local lane)

- **Fence:** a dated research note under `docs/research/`.
- **Scope:** research §7 spikes 1 to 4: pen data unfocused on each OS, XInput with an unfocused game, UIPI with an
  elevated game, macOS prompts, Wayland evdev, and the `input.capture` numbers on `Dev` and, when one exists, `R`.
- **Acceptance:** each question answered yes, no or untested, with the machine named; IN-4 and IN-7 update
  `CaptureCaps` from it.

**Issues this makes unnecessary:** none closes outright; each old issue maps to the tasks above and is re-cut by the
lead. #223's walk clip stays (pose-library example); locomotion uses LO-2's gait.
