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
| D2 | **Key identities exist in one module of one crate.** The capture thread turns a key into a *zone* (a cell of a fixed, coarse keyboard grid) and, only when it matches a configured binding, a resolved binding hit. No public type, op, event, take record, log or MCP output can hold a key code, a chord or a usage-to-zone mapping; the one stated exception is `ChordLabel`, a configured chord's display text that only the binding editor receives. Pointer input is relative motion, never a screen position. | §3 |
| D3 | **Capture is off by default, turned on only by a person** (`person_only`, scope `input`, which already exists on `main`: no `omuzaki-ops-api` change), **never restored** at start, and **runs only while an indicator is shown**: a surface installs its indicator with the hub (not through an op), start fails with `NoIndicator` unless it is shown, and the hub pauses itself if it stops showing. Anyone may pause or stop (`read`), and every pause or stop releases whatever was held. The protected-program kill switch is set only with `process`, and its pause reason is shown only to `process`. | §4 |
| D4 | **Bindings and triggers share the source.** Each capture backend feeds its own SPSC rings: zone and pad events to the frame thread (motion), locomotion intents to the frame thread (`IntentFeed`), other bindings to the binding dispatcher (`BindingFeed`), drained in turn; the drained order is what takes and replays record. The feeds resolve every binding hit to its stable id and action inside `omuzaki-input`: generations survive table swaps, a press owns its release, releases survive overflow, and held snapshots reconcile any loss. Chords live in a machine-local store that `Services`, ops, MCP and agents never reach. | §2, §5 |
| D5 | **Motion is pose layers plus IK goals in the §8 order:** after the animator, before the rig. Typing, pointer, pen and gamepad are device motions sharing one hand allocator; each hand belongs to one device at a time. A tracked bone wins (PF-23) through a `tracked` bone mask, with a 0.3 s hand-over. No `omuzaki-anim-api` change. | §6 |
| D6 | **Locomotion** is a state machine (idle, walk, run, jump, sit, emote) over a 1-D speed blend; walk and run are a procedural gait (licence-clean by construction), jump, sit and emotes are pose clips (#223). The root moves procedurally, matched to stride; foot planting (`omuzaki-rig`) removes the rest of the slide. A set is a library item that materialises its values and clips into the character and pins itself (ADR 0019 §4). | §7 |
| D7 | **Props** are five ST-03 prop kinds with named anchors (key plane, pad plane, tablet plane, grips). Hands use the anchors whether the prop is drawn or hidden. A desk is placed from the character's own proportions. | §8 |
| D8 | **A take stores what the video showed:** zone and button edges, coarse pointer and pen, pad state, locomotion intents and the root, all at frame resolution, as a new additive `Motion` record kind (a minor version, ADR 0052 rules), with a complete motion checkpoint at every group start so a clip cut there replays exactly. Never a key code, a chord or a sub-frame time; zone edges never leave the machine unless the person opts in for that take (exports carry the resolved motion instead). | §9 |
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
                     └──► held snapshots[b], per source (atomic, one writer) ── read by the feeds (§5)
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
| `omuzaki-input-api` | API (over `omuzaki-core`, `serde`, `serde_json`, `schemars`, `thiserror`) | the event and state types of §3, `KeyZone` with its sixteen `ZoneInfo` entries and two side-only values (never the usage-to-zone map, which is `omuzaki-input`'s), `SourceTag`, `PadId`, `BackendId`, `BindingId`, `BindingAction`, `Intent`, `ChordLabel` (the one stated exception to D2, §3 rule 1), `CaptureState`, `BackendState`, `CaptureCaps`, the traits `InputHub`, `MotionFeed`, `IntentFeed`, `BindingFeed`, `InputControl`, `IndicatorHost`, `ChordStore`, `ProtectList` |
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
are `input.motion.*`; there is no bare `input.bind` (too close to `input.bind_camera`). Starting and stopping are
separate ops (`start`; `pause` and `stop`), not #939's `input.capture.set { enabled }`, because an op declares one scope:
starting needs `input`, stopping must not. §4 lists every capture op. Locomotion's ops are `locomotion.*`.

## 3. D2: the event types, and why they carry no identity

```rust
// omuzaki-input-api
pub enum DeviceClass { Keyboard, Mouse, Pen, Gamepad }
pub enum Side { Left, Right }

/// A cell of the fixed keyboard grid. An index, once assigned, never changes meaning (like `Channel`).
pub struct KeyZone(u8);                       // 0..KeyZone::COUNT (16), plus the two side-only values below
impl KeyZone {
    pub const COUNT: usize = 16;
    pub const LEFT_ONLY: KeyZone;             // hand-only detail (§6.2): a key of the left hand, no zone
    pub const RIGHT_ONLY: KeyZone;            // the same for the right hand; info() gives the side and no finger
    pub fn info(self) -> ZoneInfo;            // side, finger column, row band, centre (u, v) on the key plane
}
pub struct ZoneInfo { pub side: Option<Side>, pub finger: Finger, pub band: Band, pub centre: [f32; 2] }

pub enum MotionEvent {                        // Copy, at most 32 bytes; what the frame thread sees
    Key { src: SourceTag, zone: KeyZone, down: bool, at: Seconds },
    Buttons { src: SourceTag, device: DeviceClass, pressed: u8, released: u8, at: Seconds },  // mouse and pen buttons, bit per button
    Pointer { src: SourceTag, dx: f32, dy: f32, wheel: f32, at: Seconds },    // coalesced relative motion, metres on the pad
    Pen { src: SourceTag, u: u8, v: u8, pressure: u8, tilt: [i8; 2], in_range: bool, contact: bool, at: Seconds }, // 32 x 32 grid
    PadEdges { pad: PadId, pressed: PadZones, released: PadZones, at: Seconds },  // queued, so a tap between drains is kept
    SourceUp { src: SourceTag, class: DeviceClass, pad: Option<PadId>, at: Seconds },  // a device connected (pads: their id)
    SourceGone { src: SourceTag, at: Seconds },   // that one device left: consumers drop what it held, and nothing else
}
pub struct SourceTag(u8);                     // opaque per connected device (a keyboard, a mouse, a pad); reused only after SourceGone
pub struct PadId(u8);                         // a connected pad, stable while it stays connected (a pad's SourceTag maps to it)
pub const MAX_PADS: usize = 4;
pub struct PadSet { /* fixed array of up to MAX_PADS PadIds, with a length */ }
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
    fn pads(&self, out: &mut PadSet);                              // the connected pads, fixed array of MAX_PADS, no allocation
}
pub struct MotionFrame { /* fixed array of MOTION_FRAME_MAX (128) events, and reset: SourceMask, the sources whose
                             held state ended this frame (one bit per SourceTag, §4, §5a). Never drops: due events past
                             128 stay queued, in order, for the next drain (pointer motion is coalesced, so this is rare) */ }
pub trait IntentFeed: Send {                  // the frame thread's port for locomotion intents; never blocks, never allocates
    fn drain(&mut self, up_to: Seconds, out: &mut IntentFrame);   // fixed array of IntentEvent, plus reset; never drops (as MotionFrame)
}
pub trait BindingFeed: Send {                 // the binding dispatcher's port (ops side): resolved, ready to run
    fn poll(&mut self) -> Option<BindingHit<'_>>;   // a Release carries the action its press started (§5); the borrow is
}                                                   // of the held record's own copy, never of a recyclable table
pub struct ChordLabel(/* private String */);  // display text; no serde, no Display, a redacting Debug (§3 rule 1)
pub trait ChordStore: Send + Sync {           // handed to the binding editor only; never in Services (§5)
    fn learn(&self, id: BindingId) -> Result<ChordLabel, InputError>;   // needs capture On and a shown indicator (§5)
    fn label(&self, id: BindingId) -> Option<ChordLabel>;
    fn clear(&self, id: BindingId);
    fn conflicts(&self, id: BindingId, out: &mut Vec<BindingId>);       // cold
}
pub trait ProtectList: Send + Sync {          // the person's kill-switch editor; never in Services (§4)
    fn set(&self, programs: &[String]);
    fn get(&self) -> Vec<String>;
    fn matching(&self) -> Option<String>;     // the protected program now running, for the person's own indicator
}
pub trait IndicatorHost: Send + Sync {        // the surface installs its indicator here; never in Services (§4)
    fn install(&self) -> IndicatorHandle;     // the surface writes heartbeats to it and reads the view from it
}
pub trait InputHub {                          // what an App calls once (WIRE-1); returns everything it hands out
    fn control(&self) -> Arc<dyn InputControl>;                           // the Services entry
    fn indicator_host(&self) -> Arc<dyn IndicatorHost>;
    fn chord_store(&self) -> Arc<dyn ChordStore>;
    fn protect_list(&self) -> Arc<dyn ProtectList>;
    fn take_feeds(&self) -> Option<(Box<dyn MotionFeed>, Box<dyn IntentFeed>, Box<dyn BindingFeed>)>;  // once
    fn set_detail(&self, detail: Detail);                                 // from the input.motion.detail setting (§6.2)
}
```

**The zone grid.** Sixteen zones by *physical position* (USB HID usage), so AZERTY, QWERTZ and JIS hands land where the
fingers are, and the table needs no layout setting. Letter keys are grouped by touch-typing hand and finger column
into eight zones of two to six letters: left outer-upper (`1 2 3 Q W E`, Tab, Esc), left outer-lower (`A S D Z X C`,
Caps, left Shift), left inner-upper (`4 5 R T`), left inner-lower (`F G V B`), and the mirror four on the right (`6 7 Y U`,
`H J N M`, `8 9 0 - = I O P [ ] \` and Backspace, `K L ; ' , . /` with Enter and right Shift). Then Space (thumb), left
modifiers, right modifiers, the function row, the navigation block, the arrows, the numpad and "other". The contract
holds only the zones themselves: `KeyZone` and its sixteen `ZoneInfo` entries (side, finger, band, centre), generated
from one list like the channel table. **The map from HID usage to zone is not in the contract**: it is a key identity
table, so it lives in `omuzaki-input`'s `normalise` module with D2's other `pub(crate)` types, and its test that every
usage of the ANSI, ISO and JIS boards maps to a zone runs there.

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
1. **Types.** No public type of `omuzaki-input-api` holds a key code, a chord or a usage-to-zone mapping. The checked
   list (the sweep of round 4): the zones carry position only; `MotionEvent`, `PadState`, `PadEdges` and `IntentEvent`
   carry zones, pad zones, levels and intents; `BindingHit` carries a `BindingId` and an action; `BindingAction` holds
   op requests, trigger slots and intents, which the person or an agent wrote, never a chord. **The one stated
   exception is `ChordLabel`**: a configured binding's chord rendered as display text, because the person must see what
   they bound. It is confined three ways: only `ChordStore` returns it, and `ChordStore` is handed to the binding editor
   alone (never `Services`, never an op, never MCP); it has no serde, no `Display` and a `Debug` that prints `<chord>`,
   so it cannot be serialised, logged or put in an event by accident; and IN-1 pins all three with a test. `CaptureState`, `BackendState` and `CaptureCaps`
   name classes and backends, never devices or keys. Chords, `SourceId`, slots, generations and held snapshots are all
   `pub(crate)` in `omuzaki-input`.
2. **One module.** In `omuzaki-input`, `HidUsage` and the raw event are `pub(crate)` in `normalise`; the backends hand
   raw events straight to it through a `pub(crate)` sink, on the capture thread's stack, overwritten by the next event.
3. **No text path.** The crate has no `Display` or `Debug` for raw events (a manual `Debug` prints `<key>`), no logging
   of key codes, and panics inside a capture thread are caught and reported as that backend's `BackendState::Failed { reason }`
   with a fixed reason string. A panic hook runs before the catch and prints the message, so the capture code must
   never put a key value into a panic message: `normalise` and the matcher index nothing by a raw code (they use `get`
   and return fixed-reason errors; `clippy::indexing_slicing` is denied in those modules), and IN-3B tests that a forced
   panic on a capture thread writes no key value to stderr (the crash-report design keeps only registered templates and sends no memory, `privacy.md` §3, §4).
4. **No injection or hooks.** No `SendInput`, `CGEventPost`, uinput, `SetWindowsHookEx` or DLL in the crate. A test
   in `omuzaki-input` fails if the crate's dependency tree in `Cargo.lock` holds `rdev`, `device_query`, `inputbot` or
   `willhook`, or if its `src/` names any of those symbols.
5. **The zone-equivalence test** (the acceptance of the normaliser task, §13 IN-3B): two key streams that differ only in keys
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
| `input.motion.*` settings keys | (settings registry) | agent `Confirm` | the motion settings of §6.5 |

`input.capture.pause` exists so that the person, an automation or an agent can step capture aside (a password prompt, a
private call) without losing the classes chosen. The hub's own pauses (indicator, protected program) close the epoch
the same way, and undo themselves when their cause clears (below); a `pause` asked through the op does not.

**State is per backend, summarised once.** Each running backend has a `BackendState`: `Running`,
`NeedsPermission { how: PermissionHow }` (macOS Input Monitoring, Linux `input` group for evdev) or
`Failed { reason }` (a fixed string). The global `CaptureState` is derived from them, never set on its own:
- `Off` before start and after stop; `Paused { why: Option<PauseWhy> }` (`IndicatorLost`, `ProtectedApp`) while paused;
- `On { classes, degraded }` while **any** backend runs: `classes` are the device classes actually being read, and
  `degraded` lists the classes asked for that a backend should serve but is not serving (`Keyboard` while the portal
  that carries Wayland's keyboard bindings is down, for example), each because its backend is not running (failed, missing permission, not built for this
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
`IndicatorHost::install()` and keeps the `IndicatorHandle` it returns: `IndicatorHost` is a hub API that, like
`ChordStore`, is handed to the surface directly and never inserted into `Services`, so no op, MCP tool or agent can
install or replace an indicator. **An installed indicator is visible in every state, `Off` included**: it shows "Input
capture: off" (with the Start control, which only the person can use) before capture starts, and "Reading keyboard
and mouse: zones only. Stop" while it runs. The two surfaces that install one are the desktop shell (its status-bar
badge and tray state; task UI-1) and the CLI (a stderr status line; task CLI-1).

**The hub never calls into the surface.** Asking a UI whether it is drawn could block on the UI thread, and the
control thread that would ask also runs the kill switch. So the handle is two atomics: the surface writes a
**heartbeat** (a timestamp) each time it has actually drawn the indicator in a visible window, and the hub writes the
**view** the indicator shows (the `CaptureState`, and for the person's own indicator the full pause reason and the
protected program from `ProtectList::matching`). The hub only reads the heartbeat: shown means a heartbeat newer than
1.5 s. `input.capture.start` calls `InputControl::start(classes)`, which fails with the typed `InputError::NoIndicator`
unless an indicator is installed and shown; a headless process that installed none can never start capture. The control
thread checks the heartbeat once a second; when it goes stale (the window was hidden, the surface hung or dropped the
handle) the hub pauses capture and reports `Paused { why: IndicatorLost }`. Windows and X11 show the user nothing of
their own (research §5), so this is the only signal the person has.

**Pauses the hub makes undo themselves; pauses an actor makes do not.** A pause for a lost indicator or a protected
program resumes on its own (a new open epoch) when the heartbeat returns or the program exits, because the person
started capture and nothing they chose has changed. A `pause` or `stop` requested through an op stays until a person
starts capture again.

**Every pause and stop ends what is held, without breaking single ownership.** A key held when capture pauses or
stops would otherwise never deliver its release. The control thread must not write into a backend's ring or snapshot
(each has exactly one writer, §2), so the release travels as an **epoch**, not as injected events:
1. **Epochs open and close.** `CaptureEpoch` and each backend's `BackendEpoch[b]` are atomic counters whose low bit says
   whether the epoch is open (capture running) or closed. Every transition out of `On` (an explicit `pause` or `stop`,
   a lost indicator, a protected program) bumps `CaptureEpoch` to a **closed** epoch and then tells each backend thread
   to stop reading; a resume or a start bumps it to a new **open** epoch. Every ring entry carries **both** stamps: the
   global epoch and its backend's epoch it was read in.
2. **A feed accepts only entries whose two stamps are both current and open.** It reads both counters before every
   poll or drain and drops anything else. This closes the race between the bump and a backend's stop: an event a
   backend reads after the bump carries a closed epoch, and one it read just before carries an epoch that is no longer
   current; both are dropped, so a press read anywhere in that window never reaches a consumer, and an entry from an
   earlier run of a backend that has since come back is dropped too. A backend never has to be acknowledged first, so
   a stalled backend cannot hold a pause up.
3. Each backend thread, on its own thread, discards what it has not pushed, clears its own held snapshot, clears its
   **modifier state**, marks **every key as unknown**, and parks. After a resume, a down or an auto-repeat for an
   unknown key is ignored until that key's up has been seen, so a key held across the pause (and the OS's repeats of it)
   cannot start anything; a modifier released while paused is not still "down" in a chord.
4. On seeing a new epoch, each feed (`BindingFeed`, `IntentFeed`, `MotionFeed`) first ends everything it holds: the
   binding and intent feeds hand out a `Release` for every held-action record (§5); motion releases every zone, button
   and pad zone it holds and treats every **level** as neutral (sticks and triggers centred, the pen out of range):
   `MotionFeed::pad` returns `None` for a pad whose backend epoch is closed, so a deflected stick cannot keep the
   character walking. The control thread publishes the new `CaptureState` right after the bump and never waits for a
   feed: from the bump on, no entry can reach a consumer (step 2), and a stalled feed applies the epoch on its next
   poll before anything else.
5. **Resuming starts with nothing held:** a key still physically down when capture resumes does nothing until it is
   released and pressed again (step 3), and its release, matching no held-action record and no holder, is ignored.

The same open-and-close epoch covers a **single backend** exiting while others keep running (a backend failure, the
portal session closed): that backend's `BackendEpoch[b]` closes, the feeds drop its entries and release what its
sources were holding, and nothing else; it opens again if the backend comes back.

**One device leaving is narrower still.** A keyboard, mouse or pad unplugged while its backend keeps running (and
other devices of the same backend keep working) ends only that device's state. Every event carries its device's
`SourceTag`, and every held count and holder set in the feeds and in motion is kept per source (§5, §5a). The backend
pushes `SourceGone { src }` after the device's last event, on its own ring, so it is ordered after everything that
device sent: on it the binding and intent feeds clear that source's holder bits (ending an action only if it empties
the set), and motion clears that source's held zones and buttons, and for a pad its `PadState` and pad zones. Nothing
of the other devices changes. A new device announces itself with `SourceUp`.

**A `detail` change touches motion only.** `input.motion.detail` reaches the hub through `InputHub::set_detail` (WIRE-1
subscribes to the setting) and is applied on the capture threads (§6.2). It bumps a **detail generation** that every
key entry on a motion ring carries; `MotionFeed` drops key entries of an older detail generation and, in the frame it
sees the change, sets `reset` for every keyboard source, so no zone resolved under the old detail meets a holder kept
under the new one. Bindings and intents are untouched: a held W keeps walking through a `detail` switch. A change of
`hand` is applied on the frame thread inside one frame, so it needs neither (motion resets the keyboard and mouse
sources at the switch, §5a).

**The kill switch.** An optional list of protected programs (default empty). While capture is on and the list is not
empty, the control thread checks the running process names every 2 s and pauses capture while one runs. That check is
"whether a named program is running", which `Scope::Process` governs, and an actor who could both set the list and read
the pause reason could probe any program name. So, the safer of the two fixes:
- **Setting the list** is `input.capture.protect { programs }`, scope `process` (agent `Confirm`), or the person's own
  Settings page through the `ProtectList` handle (§3), which WIRE-1 hands to the shell and never puts in `Services`.
  An actor without `process` can neither set nor read the list.
- **The pause reason never names a program**, and is shown only to actors that hold `process`: `input.capture.state`
  returns `Paused { why: Some(_) }` (`ProtectedApp` or `IndicatorLost`) to them and plain `Paused { why: None }` to
  everyone else. Every reason is hidden, not only `ProtectedApp`, because showing `IndicatorLost` alone would let a
  reader infer the other by elimination. The person's own indicator shows the full reason and the program (the hub writes them
  into the indicator's view from `ProtectList::matching`), since it is their list.

**Platform gaps** stay visible: on Wayland typing and the pointer need the evdev opt-in (the `input` group, with the
research's warning that it can read every keyboard). Bindings there need the GlobalShortcuts portal backend (task IN-9,
which waits on the owner's answer to §12 question 2). **Until IN-9 lands, the Wayland default is gamepad only**:
`CaptureCaps` reports keyboard bindings and typing as `NeedsOptIn` and the UI says why. PF-29 on Wayland is partial;
that is research risk 2.

## 5. D4: bindings, triggers and locomotion intents on the same source

A **binding** is `{ id: BindingId, name, device: DeviceClass, chord, action: BindingAction }`; the chord is a private
`omuzaki-input` type (a key or pad chord), and nothing outside the crate ever holds one (§3, rule 1). The
action is what PF-05 and PF-25 own: an op invocation (expression, toggle, prop, scene), a trigger slot (`omuzaki-anim`
`Triggers`), or a locomotion intent (`Move(dir)`, `Run`, `Jump`, `Sit`, `Emote(n)`). This design fixes only how the
source delivers them; the action catalogue and the dispatcher belong to #469 and #478.

- **The chord store.** Chords are in `omuzaki-input`'s own machine-local file (`input-bindings.toml` in the data dir),
  read only by the hub. It is not a settings key: settings are listed to agents (`settings.list`) and their values
  appear in events, and `SecretRef`-style hiding exists only for secrets. The store's API (`ChordStore`) is handed to the
  desktop UI directly and **never inserted into `Services`**, so no op, MCP tool or agent can reach it. Even that API
  carries no chord: `learn(id)` captures the next chord inside the hub and returns a `ChordLabel` (localised display
  text such as "Ctrl+Shift+F9", for the editor to show), `label(id)` returns it again, `clear(id)` forgets it, and
  `conflicts(id)` lists the bindings sharing its chord. The chord itself never leaves `omuzaki-input`. **Learning reads
  OS input, so it is gated like capture:** `learn` fails with `InputError::NotCapturing` unless capture is `On` with a
  shown indicator, and it reads one chord, then stops.
- **What ops and agents see.** `input.binding.list` (scope `read`) returns `{ id, name, device, action, conflict }`,
  never a chord; `input.binding.set_action { id, action }` (scope `perform`, agent `Confirm`) and
  `input.binding.remove { id }` (scope `perform`, agent `Confirm`; removing can only take a capability away). An action
  is itself an op request, so `set_action` also checks that the caller holds every scope the action's op needs: a binding cannot become a way for an actor to make the person's key press run an op the actor
  could not run itself. The dispatcher runs a binding's action as the person, with `cause: binding` in the journal.
  Learning a chord ("press the keys") is a UI call on `ChordStore::learn(id)`, a person's action by construction; its
  label is shown in the binding editor and the chord is written to the store, and nothing goes through the executor. A conflict
  (the same chord on two bindings) is reported, not overwritten.
- **Matching.** On the capture thread, after the zone is computed, the raw event is compared with the active table (a
  fixed array of at most `MAX_BINDINGS` (64) chords, swapped in whole by the control thread when the store changes).
  Each capture thread publishes the generation it is matching against, and the control thread recycles a retired
  table only when no capture thread still publishes it, so a match never reads a table being overwritten. A match pushes
  an internal entry `(gen, slot, source, edge, at)`, with the device's `SourceTag`, to that backend's ring for the
  binding's consumer: **intent
  bindings** go to the backend's intent ring (read by the frame thread through `IntentFeed`), **every other binding**
  to its binding ring (read by the dispatcher through `BindingFeed`). Each ring has one producer and one consumer.
  Modifier state is kept inside `normalise`. A non-match produces nothing.
- **Resolution is the feed's job, not the consumer's.** The generation tables, slots, held snapshots and held-action
  records are `pub(crate)` in `omuzaki-input`; the feeds hand consumers resolved values only: `BindingHit { id, action,
  edge, at }` (the stable `BindingId` and a borrow of the `BindingAction`) and `IntentEvent { id, intent, edge, at }`.
  So #469's dispatcher runs `hit.action` and #478's triggers press `Trigger(n)`, and neither needs to know a table
  exists. The rest of this list is how the feeds keep that resolution right.
- **A key's release ends what its press started.** The matcher keeps a `pub(crate)` record per physical key that is
  down: the `(gen, slot)` and the ring its press went to. The key's up is matched against that record first, never
  against the current table or the current modifiers. So letting go of Ctrl before F9 still ends Ctrl+F9; a chord
  relearned, cleared or replaced by a preset while held still ends its old action when the old key comes up; and an
  action edited from an intent to an op (or back) still sends its release to the consumer that got the press. The new
  chord or action applies from the next press.
- **Identity across table swaps.** Every swap bumps the table's generation, and every entry carries the generation it
  was matched against. The feed resolves `(gen, slot)` against **that** generation's table, never the current one: it
  keeps each retired table (a fixed pool of 4 generations, recycled off the hot path) until the last queued entry of
  that generation has drained, so a queued slot can never index another binding. A held record does not pin a table:
  it keeps **its own copy** of the action it started (made once at the press, off the frame thread), and
  `BindingHit` borrows from that copy, so a key held across any number of swaps still ends its own action. The control thread never waits for
  a feed: if 4 generations are still pending (edits faster than the consumers drain), a further edit is written to the
  store and becomes the **pending table**, a later edit replaces it (edits coalesce), and it is swapped in when a
  generation retires. So the indicator check and the kill switch, which run on the same control thread, keep their
  one-second and two-second cadence even with every consumer stalled. An entry from a retired generation whose binding (by its `BindingId`) is gone from the current table
  is dropped and counted; so that this never strands a held action, the feed hands out a `Release` for every action
  held for a binding the new table removes when it takes the swap.
- **A press owns its release.** A key can be held across a swap: pressed under the old table, released under the new
  one. So each feed keeps a held-action record per `BindingId` (a fixed array of `MAX_BINDINGS`), written at the first
  press: the generation and the action that press started. A `Release` resolves to **the action in that record**,
  whatever its own generation or the current table says; a `Repeat` repeats the recorded action. If the binding's
  action was edited while it was held, the old action ends at the release and the new action never starts from that
  press; the next press starts the new one. A `Release` with no record (its press was dropped, or happened before a
  resume) is not handed out.
- **Several sources can hold one binding.** Two pads, two keyboards, or a pad and a keyboard can be bound to the same
  binding and held at once. Each held-action record therefore keeps a set of holders, one bit per source (the
  `SourceTag` every entry carries: a device of a backend, at most 64), not a flag. A press from a new holder adds its bit; the
  action starts only when the set goes from empty to one holder, so consumers see one `Press`. A release removes its
  holder's bit, and the action ends only when the set is empty again, so consumers see one `Release`. A backend
  leaving clears all of its sources' bits, a device leaving (`SourceGone`) clears its own (each ends the action only if
  that empties the set), and reconciliation (below) clears a bit whose source's snapshot shows the slot up.
- **A lost release is never permanent.** Inside a run of capture, entries can be dropped only when a ring is full (its
  consumer is stalled); a pause, a stop or a backend leaving releases everything held (§4). A full ring keeps releases,
  as the motion ring keeps key-ups: it drops its oldest `Repeat`, then its oldest complete tap (a `Press` with its
  `Release` queued behind it, both together), then its oldest lone `Press`, and drops a `Release` only if the ring holds
  nothing else. Each backend also publishes a held snapshot **per source** (for each of its devices, the slots that device holds
  down now, with the generation). Whenever
  a ring has dropped anything, the feed reconciles: every held-action record whose slot the snapshot shows as up gets a
  synthesised `Release`; a slot held in the snapshot with no record is left alone (a missed press is not replayed,
  since its moment has passed). So a stalled consumer can delay a release but cannot leave movement or a held trigger
  stuck.
- **Consumers.** The binding dispatcher (#469) polls `BindingFeed` on the ops side and runs each hit's action (an op,
  or a PF-25 trigger press); the frame thread drains `IntentFeed` and folds the intents into an `Intents` value per
  frame (§7.1).
  Gamepad bindings (a pad chord: a button, a combination, or a stick past a threshold) match in the same place.
- **Wayland.** Bindings use the GlobalShortcuts portal: the portal sends only the shortcuts we registered, which is the
  binding table itself, so the privacy rule holds by construction there. The portal's replies also carry each
  shortcut's `trigger_description` (the key text the person chose in the portal dialog); the portal backend drops it
  on receipt and never stores, logs or forwards it (IN-9).
- **Default bindings** ship as a preset the person enables (locomotion: `W A S D`, Space, left Shift to run, `C` to
  sit; the pad's left stick and face buttons). A preset is just bindings; nothing is bound until the person applies it.

Note for #469 (filed by the lead, not here): PF-05 consumes `BindingFeed` from `omuzaki-input` and adds no second OS
capture path; OSC, MIDI and Stream Deck are other sources of the same `BindingHit`.

## 5a. Invariants: held state, ordering and scopes

The review rounds found the same classes of problem more than once. This section states each as one rule and lists every
place in the design it applies, so the tasks test the class, not one instance.

**1. Nothing stays held when its cause goes away.** Every piece of held state, what ends it, and the task that tests
it. Every held thing is a **count or a set of holders, never a flag**, so two sources holding the same thing (two pads,
two keyboards, a pad and a keyboard on one binding, two keyboards pressing keys in one zone) end it only when the last
one lets go:

| Held state | Ended by | Test |
|---|---|---|
| A binding's action (an op held down, a held trigger) | the last holder's release (a holder set per binding, §5), through the press's held-action record, even across a table swap (§5); a removed binding at the swap (§5); reconciliation after a ring drop (§5); a capture epoch (pause, stop, indicator, protected program; §4); its backend's own epoch (failure, unplug, portal session closed; §4) | IN-3 |
| A zone, mouse or pen button held in motion (per zone and per button, a holder set by `SourceTag`) | its source's release edge, which clears that source's bit (a release for a bit not set is ignored, never an underflow); `MotionFrame.reset` for exactly the sources a capture, backend or device epoch ended; a `detail` change (the keyboard sources) or a `hand` change (keyboard and mouse sources), since zones and sides no longer match | IN-3 (epochs), MO-1, MO-2 |
| Levels: sticks, triggers, pen position and contact | the next sample of that source; on any epoch that closes its backend, `MotionFeed::pad` returns `None` and motion treats the pen as out of range, so a deflected stick or a pen down cannot outlast a pause | IN-3, IN-5, MO-2, MO-3 |
| Pad zones and sticks (per `PadId`, so two pads never share) | `PadEdges` releases and the next `PadState`; on unplug `SourceGone` for that pad only clears its zones, sticks and holders | IN-5, MO-3 |
| Locomotion intents (move, run) | the intent feed's holder set (as for bindings, §5) emptying, or a reset of the sources that drove them; with no intent the machine decays to `Idle`; the `move` vector is the sum of the held move intents and the pad stick, clamped | LO-1, IN-3 |
| A locomotion state mid-flight (jump, sit, emote) when `locomotion.set` changes the def | the state finishes under the def it started with (the old def is kept until the machine is next in `Idle` or `Move`), then the new def applies; the checkpoint names the def in force (§9) | LO-1, LO-4, ST-4 |
| A palm goal, posture or stick look | the hand allocator's rest timers, and the 0.3 s fade when a tracked bone takes over | MO-1, MO-3 |
| Held state inside a take | the checkpoint holds it (§9); a reset or settings change is recorded in the frame it happened (`reset`, `HAS_SETTINGS`), so replay ends the same things at the same frame | ST-4 |

**Every transition, and what closes its window.** A read or drain can straddle any change of state; each one has
a rule that closes the window:

| Transition | The straddling case | What closes it |
|---|---|---|
| pause, stop, indicator lost, protected program | a backend reads a press after the bump, or reads before it and pushes after | closed epochs: feeds accept only entries of the current open epoch (§4) |
| a backend leaving (failure, portal closed) | the same, for that backend; entries from its earlier run after it comes back | its own `BackendEpoch` closes the same way; entries carry both stamps (§4) |
| resume or start | a key held across the gap releases after resume, or auto-repeats | a new open epoch; keys are unknown until their up is seen, modifiers cleared; a release with no holder is ignored (§4) |
| a binding-table swap | an entry matched under the old table drains after the swap; a press before, a release after | generation-stamped entries and the held-action record of the first press (§5) |
| a change of `detail` | an event resolved on a capture thread under the old detail meets a holder kept under the new one | a detail generation on key entries, and a motion-only reset of the keyboard sources; bindings untouched (§4) |
| one device unplugged (a pad, a keyboard) | its last events and its departure | `SourceGone` on that device's own ring, after its last event; only its source's state ends (§4) |
| a chord changed while held (`learn`, a preset), a modifier released first, an action moved between intent and op | the press matched the old chord or ring, the release comes under the new table or modifiers | the per-key record of the press: its up ends what the press started, on the ring the press used (§5) |
| a held key across many swaps | the press's generation is recycled before the release | the held record keeps its own copy of the action (§5) |
| a binding-table swap while a capture thread is mid-match | the table is recycled under it | capture threads publish the generation they read; only unread tables are recycled (§5) |
| a motion ring full of key edges | no pointer event to displace | the ring drops the oldest complete down and up pair of one source, else the oldest lone down (its source's later up is then ignored); never an up (§3) |
| a `MotionFrame` or `IntentFrame` past its capacity | due events that do not fit, releases among them | frames never drop: the rest stay queued, in order, for the next drain (§3) |
| a change of `hand` | none across threads: applied on the frame thread inside one frame | motion resets the keyboard and mouse sources at the switch |
| a `locomotion.set` during a jump | the machine mid-state under the old def | the old def is kept until the machine is next in `Idle` or `Move` |
| a group start in a take | motion state carried across the cut, including through idle stretches | a checkpoint at every group start for every member with motion state, naming the locomotion def in force (§9) |

**2. Order is promised only where one writer makes it.** Each ring is in its producer's order; across rings and
threads nothing is promised, and determinism comes from recording the drained order (§2). Every other order in the
design is single-threaded and fixed: the frame stages (§6.1), the pose layers (`PoseLayers` lowest first), the IK stage
(look-at, palms, limbs, feet), the take's chunk records (takes.md T2). Each piece of shared state has exactly one
writer: a ring and a held snapshot are written only by their backend's thread; the binding table, the epochs and an
indicator's view only by the control thread; an indicator's heartbeat only by its surface; consumers only read them.
**The safety loop never waits on anything:** the control thread's indicator and kill-switch checks read atomics only
(the heartbeat, the process list) and never call a surface, wait for a feed, or block on a table swap (§4, §5). No step anywhere writes into another thread's ring.

**3. Every op names its scope, and none widens what its caller can do.**

| Op or key | Scope | Agent | Why |
|---|---|---|---|
| `input.capture.start` | `input` | never (`person_only`) | reads other apps' input |
| `input.capture.pause`, `input.capture.stop` | `read` | free | only removes exposure |
| `input.capture.state` | `read` | free | the pause reason only with `process` (§4) |
| `input.capture.protect` | `process` | `Confirm` | checks whether named programs run |
| `input.binding.list` | `read` | free | ids, names, actions; never a chord |
| `input.binding.set_action` | `perform`, plus every scope of the action's op | `Confirm` | a binding cannot launder an op its setter could not run (§5) |
| `input.binding.remove` | `perform` | `Confirm` | removing only takes a capability away |
| `input.binding.apply_preset` | `perform`, plus every scope of the preset's actions | `Confirm` | the preset's chords are data in `omuzaki-input`; the op names the preset only |
| chord learning, indicator install, the protected-program editor | none: hub APIs (`ChordStore`, `IndicatorHost`, `ProtectList`) handed to the surface, never ops or services | not reachable | identities, the indicator and the program list stay out of the executor; `learn` also needs capture `On` (§5) |
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
  not the palm, lands; the move is a critically damped spring (ω 49 rad/s: 90 % of the way in about 80 ms, since 90 % takes ωt ≈ 3.9), so a burst of keys at
  10 a second reads as a busy hand, not a teleport. The finger layer plays a strike on that finger (down 40 ms, up
  80 ms, the hand-shape library's curl angles), and the wrist dips 3 mm.
- **Idle.** 0.4 s without a key on a hand: the hand drifts to its home-row zone. 1.5 s: the hand eases off the board to
  the character's rest (0.5 s blend, palm goal weight to 0). These are the numbers of #748, kept.
- **Hand-only detail.** Zones arrive as their side only (`KeyZone::LEFT_ONLY` or `RIGHT_ONLY`); the finger and the target cell within that side's half come
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
      fn feed(&mut self, frame: &MotionFrame, feed: &dyn MotionFeed);   // events, plus pads() and pad() for levels
      fn activity(&self) -> Activity;                        // which hands it wants, and when it last saw input
      fn step(&mut self, tick: Tick, desk: &DeskRig, hands: Hands, ctx: &StepCtx, out: &mut MotionOut);
      fn save(&self, slot: &mut [u8; 64]);                   // its part of MotionCheckpoint (§9)
      fn restore(&mut self, slot: &[u8; 64]);
  }
  // StepCtx carries what the stage knows and a device motion cannot: head_tracked (the tracker owns a head channel).
  ```

  Each device motion registers itself next to its code (`register_device_motion!`, inventory, in `omuzaki-anim`), and
  `InputMotion::new()` builds every registered one in a fixed order (by name). So MO-2 and MO-3 add their folders and
  edit neither `input_motion/mod.rs`'s list nor the checkpoint: each owns one 64-byte slot of it.

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
| `Move` (1-D blend by speed: idle 0, walk 1.4, run 3.5 m/s at average stature) | `Idle`, `Land` (from a jump that started in `Move`) | `move` ≤ 0.1 and speed < 0.05 m/s | `Idle` | 0.25 s |
| `Jump` (anticipate 0.15 s, air, land 0.2 s) | `Idle`, `Move` | `jump` press | `Land` → previous ground state | 0.1 s in, 0.2 s out |
| `Sit` (down clip, seated loop, up clip) | `Idle` | `sit` press; again or `move` to stand | `Idle` after the up clip | `sit_in` 0.2 s |
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
walk_radius = 4.0           # m around the authored placement (§7.3)
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
sit_in = 0.2
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

Five prop kinds on #705's `PropKind` registry, each a low-poly mesh built in code (no asset files), with named anchors
that motion reads whether the prop is visible or not:

| Kind | Anchors | Default placement |
|---|---|---|
| `input.keyboard` | `key_plane` (origin at the home-row centre, X right, Z away from the character, Y up; size in metres), `zone_cells` (from the zone table) | on a desk plane at 0.1 m below the elbow height of the desk rig, 0.28 m in front of the chest |
| `input.mouse` | `pad_plane`, `grip` | right of the keyboard (left if `hand` is left) |
| `input.tablet` and `input.pen` | `tablet_plane`; the pen attached to the hand, `grip` | in place of the mouse |
| `input.gamepad` | `grip_left`, `grip_right`, `stick_left`, `stick_right`, `face`, `dpad` | attached to the hands, at chest height |

The desk rig computes the default placement from the character's proportions, so a chibi and a tall character both
reach their keyboard. A person can move a prop (`stage.set_prop`); motion reads the anchors each frame. A prop set
"desk" toggles all five. The props are in the scene document; the motion is not.

## 9. D8: what a take stores (SH-01)

One new record kind in `omuzaki-take-api`, `Motion = 12` (after `Watermark`), added in a minor version as T3 allows
(an older reader skips it by `len` and counts it). One record per cast member per frame period while motion or
locomotion is active, **and at every group start for every member that has motion state at all** (the walked root,
the virtual mouse and the hand states persist through idle stretches), fixed little-endian layout, about 40 to 80 bytes
(plus the checkpoint at group starts):

```
member u8 | flags u8 (HAS_KEYS, HAS_BUTTONS, HAS_POINTER, HAS_PEN, HAS_PAD, HAS_LOCO, HAND_ONLY, CHECKPOINT)
  | aux u8 (bit 0 HAS_SETTINGS, bit 1 HAS_RESET, bit 2 HAS_SOURCES, bit 3 RESOLVED) | reserved u8
reset:    u64 (SourceMask: the sources whose held state ended before this frame's edges, §5a; HAS_RESET)
sources:  n u8 | n × (src u8 | class u8 | up u8)              // SourceUp and SourceGone in this frame (HAS_SOURCES)
keys:     n u8 | n × (src u8 | zone u8 | down u8)             // edges, in drained order within the frame (§2); no times
buttons:  n u8 | n × (src u8 | device u8 | button u8 | down u8)   // edges: device 0 mouse, 1 pen
pad_edges: n u8 | n × (pad u8 | zone u8 | down u8)            // from MotionEvent::PadEdges, one entry per zone bit
pointer:  dx f32 | dy f32 | wheel f32                         // summed over the frame (motion integrates per frame)
pen:      u u8 | v u8 | pressure u8 | flags u8 (IN_RANGE, CONTACT) | tilt i8 × 2 | 2 reserved   // the frame's last sample
pad:      pad u8 | sticks i8 × 4 | triggers u8 × 2 | reserved u8               // levels of the active pad; zones are pad_edges
loco:     state u8 | flags u8 (RUN, JUMP, SIT) | emote u8 | reserved u8 | move i8 × 2 | 2 reserved | root x, y, z f32 | yaw f32
settings: len u16 | the motion settings (§6.5) as written, when they changed this frame (HAS_SETTINGS)
resolved: len u16 | the frame's motion result: layer poses and palm and look goals (RESOLVED; only in exported takes)
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
- **sources:** the connected `SourceTag`s with their class and `PadId`, and per zone and per button the holder set;
- **hands:** per hand the allocator state, the dwell and transit timers, the palm spring's position and velocity, each
  finger's strike phase, the keys held per zone, the tracked-bone fade weights;
- **devices:** one fixed slot (64 bytes) per registered device motion (§6.5): each device motion writes and reads its
  own slot (the virtual mouse position and recentre state, the last pen sample; for pads the last `PadState`, the
  active pad, the stick-look smoothing value and its fade), so a device motion added later brings its own state without
  editing the checkpoint's other parts;
- **locomotion:** the state and sub-phase (jump anticipate, air, land; sit down, seated, up), the transition in flight
  (from, to, progress), speed, facing, the gait phase, the clip time, a queued intent, the root, and **the def in
  force**: the journal position (`def_lsn`) of the `locomotion.set` whose values the running state uses, so a cut
  inside a jump that began before a later `locomotion.set` restores under the old def (the reader resolves it through
  takes.md's `DocAt`).

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
- **Zone edges stay on this machine** (the Director's condition on ADR 0060 decision 6). A take that leaves the
  machine (an export, a collaborator send, a share link) has its `Motion` records rewritten on the way out: the `keys`
  and `pad_edges` entries and the checkpoint's held-zone sets are dropped, and a `RESOLVED` block takes their place
  per frame, the motion result (the three layer poses and the palm and look goals, as sparse rotations and
  transforms), so the receiver re-renders the same body without ever seeing which zones were pressed. The person can
  opt in to keep zone edges for one take (`take.share { keep_zone_edges: true }`, a person-only choice made at the
  share step, never a setting). Why: a zone stream resembles keypad input (§3), and a frame-rate record of zone edges
  is enough for a dictionary attack on what was typed, which is more than the video shows.

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
5. **The root manifest** (§13 DEP-0): the backends' external dependencies are pre-declared in one change to the root
   `Cargo.toml` `[workspace.dependencies]`, a workspace-wide hub file; the Director rules on it before DEP-0 starts.

## 13. Tasks

The re-cut of #743, #744, #748 to #751, #939 and #1168. Each is about 400 changed lines or less. Rules every task
inherits: module lists are generated (`omuzaki-anim` from `src` by its build script, `omuzaki-stage` from `src` and
`src/ops`, ADR 0054 §1), so no task adds a `mod` line there; a task that adds a top-level folder to `omuzaki-anim` edits
nothing else to declare it. Inside a folder a task creates (`input_motion/`, `locomotion/`, `perform/` in the stage) the
`mod.rs` is hand-written, so a later task's fence includes its one `mod` line there; those tasks are ordered, so the
line never conflicts. The first of MO-1 and LO-1 to merge adds the `omuzaki-input-api` dependency to
`omuzaki-anim/Cargo.toml`; the other merges `main` and drops its copy. A new bench adds its `[[bench]]` entry to the crate's `Cargo.toml` (inside the fence). A new
crate describes itself in its own `Cargo.toml` (`[package.metadata.omuzaki]` `kind` and `summary`), never in
`crates/README.md`. The repository conventions every fence and acceptance below follows (swept in round 4):
- **External dependencies are pre-declared once** in the root `Cargo.toml` `[workspace.dependencies]`, so parallel
  tasks never collide on that hub file. DEP-0 declares every external crate this design needs in one change; every
  other task uses `<crate>.workspace = true` (with its own `features` where it needs more) and never edits the root
  manifest.
- **The licence register is generated.** A dependency's licence is one file, `docs/licensing/deps/<name>.toml`, in the
  task that first declares it (DEP-0); `docs/licensing/register.md` is printed by `cargo xtask lint-licences --print` and
  never hand-edited, and acceptance runs `cargo xtask lint-licences`.
- **Other generated files** (the docs index, the crate table, the generated module lists, the CLI help and MCP
  schemas) are never hand-edited; a new crate describes itself in its own manifest.
- **Registrations and `link()`.** A crate that registers anything (ops, backends, prop kinds, library sources) sets
  `registers = true` and defines `link()` with `omuzaki_core::define_link!()` (`omuzaki-input` in IN-3; `omuzaki-anim`
  and `omuzaki-stage` already do). The one-line `link()` call in an App is inside the App task's fence by standing
  ruling (WIRE-1), and each App's link gate (`tests/registrations.rs`) passes.
- **Release notes.** A task a person can see once it is merged (new ops, UI, CLI output, motion in the frame) adds one
  `.changes/<branch-with-dashes>.md` in the format of the existing files, and that file is part of its fence. These are
  IN-8, UI-1, UI-2, CLI-1, WIRE-1, WIRE-2, LO-4, ST-1, ST-2 and ST-3. Contract, mock, backend (IN-4 to IN-7, IN-9,
  IN-10: no person sees a backend until WIRE-1 wires it), motion-library, data and test tasks add none.
- **One crate manifest, several backend tasks.** IN-4 to IN-7, IN-9 and IN-10 each add only their own dependency lines to
  `crates/omuzaki-input/Cargo.toml`, each in its own `[target.'cfg(...)'.dependencies]` table; the second to merge merges
  `main` first, as for MO-1 and LO-1 below.
- **Crate-private tests.** `tests/` and `benches/` are separate crates and cannot name `pub(crate)` items. Tests that
  need the raw path (the zone-equivalence proptest, the epoch-window hook, the panic-output check, the privacy scans)
  are `#[cfg(test)]` unit tests in `src/`, or go through a `#[doc(hidden)]` `test-hooks` cargo feature of
  `omuzaki-input` that exposes a raw-event injector to its own tests and benches only (IN-3 adds the feature; no other
  crate enables it, which a test in IN-3 checks with `cargo metadata`).
- `Cargo.lock` changes cargo makes for an allowed dependency are inside every fence.

| ID | Replaces | Crate | Model | Blocked by |
|---|---|---|---|---|
| DEP-0 | new (pre-declared dependencies) | root `Cargo.toml` | haiku | this design; the Director's ruling on §12 question 5 |
| IN-1 | #743 (contract half) | `omuzaki-input-api` | sonnet | this design |
| IN-2 | #743 (mock half) | `omuzaki-input-mock` | haiku | IN-1 |
| IN-3 | #744 (core: rings, epochs, gate, indicator, kill switch) | `omuzaki-input` | sonnet | IN-1, IN-2 |
| IN-3B | #744 (core: normaliser, zone map, matcher, generations, holders, chord store) | `omuzaki-input` | sonnet | IN-3 |
| IN-4 | #744 (Windows keyboard and mouse) | `omuzaki-input` | sonnet | IN-3B, DEP-0 |
| IN-5 | new (gamepads) | `omuzaki-input` | sonnet | IN-3B, DEP-0 |
| IN-6 | new (Linux) | `omuzaki-input` | sonnet | IN-3B, DEP-0 |
| IN-7 | new (macOS keyboard and mouse) | `omuzaki-input` | sonnet (run in the local lane: needs a Mac) | IN-3B, DEP-0 |
| IN-8 | #939 (input half) | `omuzaki-input` (ops) | haiku | IN-3B |
| IN-9 | new (Wayland bindings) | `omuzaki-input` (portal) | sonnet | IN-3B, DEP-0; the owner's answer to §12 question 2 |
| IN-10 | new (pen, per OS) | `omuzaki-input` | sonnet | SP-1 |
| UI-1 | new (indicator) | `omuzaki-ui` | sonnet | IN-1 |
| CLI-1 | new (indicator) | `omuzaki-cli` | haiku | IN-1 |
| UI-2 | new (binding editor, kill-switch list) | `omuzaki-ui` | sonnet | IN-3B, IN-8 |
| WIRE-1 | new (CLI wiring) | `omuzaki-cli` | sonnet | IN-3B, IN-8, CLI-1, ST-1, ST-2, LO-4 |
| WIRE-2 | new (desktop App wiring) | the desktop App | sonnet | WIRE-1, UI-1, UI-2, and the desktop App's startup path (not on `main` yet) |
| MO-1 | #748 (typing half) | `omuzaki-anim` | sonnet | IN-1, IN-2 |
| MO-2 | #748 (pointer half) | `omuzaki-anim` | sonnet | MO-1 |
| MO-3 | #749 | `omuzaki-anim` | sonnet | MO-1 |
| LO-1 | #750 | `omuzaki-anim` | sonnet | IN-1, IN-2, #223 |
| LO-2 | new (gait) | `omuzaki-anim` | sonnet | LO-1 |
| LO-3 | #751 | `omuzaki-anim` (data) | sonnet | LO-1, #223 |
| LO-4 | #939 (locomotion half) | `omuzaki-anim` (ops) | haiku | LO-1 |
| ST-1 | #1168 (motion half) | `omuzaki-stage` | sonnet | IN-3B, MO-1, LO-4 |
| ST-2A | new (camera follow lag, a contract change) | `omuzaki-stage-api` | haiku | the Director's approval on the task |
| ST-2 | #1168 (locomotion half) | `omuzaki-stage` | sonnet | ST-1, LO-2, ST-2A (only for the follow lag) |
| ST-3 | new (props) | `omuzaki-stage` | sonnet | #705, ST-1 |
| ST-4A | new (the `Motion` record kind, a contract change) | `omuzaki-take-api` | haiku | #1149; the Director's approval on the task |
| ST-4 | #1168 (take half) | `omuzaki-stage` | haiku | ST-2, ST-4A, #1357 |
| EV-1 | the contact sheets of #748 to #751 | `omuzaki-test-char` | sonnet | MO-2, MO-3, LO-2, LO-3 |
| SP-1 | research spikes 1 to 4 | none (a report) | none: a maintainer on real hardware (local lane) | IN-4 |

The table is each task's model and `Blocked by` line; the sections below give the title, crate, fence, contract and
acceptance. Order: DEP-0 and IN-1 → (IN-2, UI-1, CLI-1) → (MO-1, LO-1, IN-3) → IN-3B → (IN-4 to IN-9, UI-2, MO-2,
MO-3, LO-2, LO-3, LO-4) → ST-1 → (ST-2, ST-3, SP-1) → (WIRE-1, ST-4, EV-1, IN-10) → WIRE-2. MO-1 and LO-1 need only the
contract and the mock, so the motion work never waits on an OS backend. #469's binding dispatcher takes `BindingFeed`
from the hub that WIRE-1 builds (`InputHub::take_feeds`); until it lands WIRE-1 holds the feed and nothing dispatches
op bindings, which is #469's task, not this design's.

### Who owns each boundary

Every public type and handle this design adds, who produces it, who consumes it, and the task that connects them. A
row without both ends inside a task would be an interface nobody owns; there is none.

| Boundary | Producer (task) | Consumer (task) |
|---|---|---|
| `InputHub` construction, `Services` entry `dyn InputControl`, `set_detail` | `omuzaki-input` (IN-3) | the CLI App (WIRE-1), the desktop App (WIRE-2); ops (IN-8) |
| `IndicatorHost`, `IndicatorHandle` (heartbeat and view) | hub (IN-3) | desktop shell (UI-1, handed over by WIRE-2), CLI (CLI-1, handed over by WIRE-1) |
| `ChordStore` (`ChordLabel` text only), `ProtectList` | hub (IN-3B, IN-3) | desktop shell's binding editor (UI-2), handed over by WIRE-2 |
| `MotionFeed` (`MotionEvent`, `PadEdges`, `SourceUp`, `SourceGone`, `pads`, `pad`, `MotionFrame.reset`) | hub (IN-3, IN-3B) and backends (IN-4 to IN-7, IN-5 for pads, IN-10 for pens) | stage frame (ST-1) → `InputMotion` (MO-1 to MO-3); set by WIRE-1 |
| `PadState` levels, the connected-pad list | gamepad backend (IN-5) | `pad_motion` (MO-3), locomotion `move` (ST-2) |
| `IntentFeed` (`IntentEvent`) | hub (IN-3B), portal (IN-9) | stage locomotion (ST-2) → `Locomotion` (LO-1); set by WIRE-1 |
| `BindingFeed` (`BindingHit`) | hub (IN-3B), portal (IN-9) | the binding dispatcher of #469 (actions) and #478 (trigger presses), which take it from the hub WIRE-1 builds |
| `BindingAction` values | `input.binding.set_action`, `apply_preset` (IN-8) | the feeds (IN-3B) |
| `CaptureState`, `BackendState` | hub (IN-3) | `input.capture.state` (IN-8), indicators through their view (UI-1, CLI-1) |
| `input.motion.*` settings keys | registered by LO-4 | stage (ST-1), by settings subscription; the hub's `set_detail`, through WIRE-1's subscription |
| `MotionOut`, `PalmGoal`, `DeskRig`, `StepCtx`, `register_device_motion!` | `omuzaki-anim` (MO-1) | stage IK stage (ST-1); device motions (MO-2, MO-3) |
| `InputPropAnchors` | stage props (ST-3) | the desk rig input in the stage, at the call site ST-1 leaves for it and ST-3 fills |
| `Intents`, `RootStep`, `LocomotionDef` | `omuzaki-anim` (LO-1, LO-2) | stage (ST-2); `locomotion.set` (LO-4) |
| `locomotion.home`, `locomotion.state` | stage (ST-2) | ops callers |
| the camera follow lag on `CameraDef` | `omuzaki-stage-api` (ST-2A) | stage camera (ST-2) |
| `MotionCheckpoint` (with device slots), the `motion` take provider | MO-1, LO-1 (checkpoint), MO-2, MO-3 (their slots), ST-4 (provider) | the take reader (#1149 family), re-render |
| the `Motion` record kind | `omuzaki-take-api` (ST-4A) | the stage producer (ST-4), the take reader |
| `register_input_backend!` | each backend folder (IN-4 to IN-7, IN-9, IN-10) | the hub (IN-3) |
| the usage-to-zone map, chords, per-key press records, slots, generations, held snapshots | `omuzaki-input` only (IN-3B) | nobody outside the crate (`pub(crate)`); its own tests through the `test-hooks` feature |
| `[workspace.dependencies]` entries for the backends, their licence files | DEP-0 | IN-4 to IN-7, IN-9, IN-10 (`workspace = true`) |

### DEP-0: pre-declare the input backends' dependencies

- **Hub file:** the root `Cargo.toml` is a workspace-wide hub, so this task needs the Director's ruling (§12
  question 5) before it starts.
- **Fence:** the root `Cargo.toml` `[workspace.dependencies]` only, and one licence file each in
  `docs/licensing/deps/`: `gilrs.toml` (recording that `gilrs` links the system `libudev` dynamically on Linux, under
  the policy's LGPL-shared rule), `x11rb.toml`, `evdev.toml`, `objc2.toml` (with `objc2-core-graphics`,
  `objc2-game-controller` and `objc2-io-kit` in its `crates` list, as the research's register row groups them),
  `ashpd.toml`, `sdl-gamecontrollerdb.toml`, and an update of the existing `windows-sys` entry's features (Raw Input,
  HID, XInput) and its licence file. `Cargo.lock` as cargo writes it.
- **Contract:** research §4 and §9: versions and features (`gilrs` with `default-features = false` and `xinput`;
  `x11rb` with `xinput`; `ashpd` with `global_shortcuts`). The research's `windows` crate is replaced by the
  `windows-sys` the workspace already declares, so no second Windows binding is added; `gilrs-core`'s own `windows`
  stays within the `<= 0.62` range the research names.
- **Acceptance:** `cargo xtask lint-licences` passes and `--print` shows the new rows; `cargo deny check licenses bans
  sources` passes; `cargo tree -d` shows no Windows binding duplicated beyond what `gilrs` brings; no crate uses the
  entries yet (unused entries cost nothing); check green.

### IN-1: `omuzaki-input-api`: events, zones, bindings, capture state and the traits (new API crate)

- **Fence:** `crates/omuzaki-input-api/**` (new: `Cargo.toml`, `src/lib.rs`, `src/event.rs`, `src/zones.rs` (the
  sixteen zones only), `src/binding.rs`, `src/capture.rs`, `src/feed.rs`, `tests/zones.rs`, `tests/no_identity.rs`).
- **Contract:** §3 and §5 as written: `KeyZone` with its sixteen `ZoneInfo` entries and the two side-only values (never
  the usage-to-zone map), `MotionEvent`,
  `PadState`, `PadZones`, `PadId`, `PadSet`, `MAX_PADS`, `SourceTag`, `BackendId`, `BindingId`, `BindingAction`, `Intent`, `BindingHit`, `IntentEvent`, `Edge`,
  `ChordLabel`, `CaptureState`, `BackendState`, `CaptureCaps`, `PermissionHow`, `PauseWhy`,
  `DeviceMask`, `SourceMask`, `Detail`, `MotionFrame` (`MOTION_FRAME_MAX` 128), `IntentFrame`, `MAX_BINDINGS` 64,
  `InputError` (with `NoIndicator` and `NotCapturing`), the traits `MotionFeed`, `IntentFeed`, `BindingFeed`,
  `InputControl`, `IndicatorHost` (and its `IndicatorHandle`), `ChordStore`, `ProtectList`, and `InputHub` (the one
  constructor contract an App calls: it returns the `Services` entry, the three surface handles and the three feeds,
  and takes `set_detail`; §13 "Who owns each boundary"). Dependencies: `omuzaki-core`, `serde`, `serde_json`,
  `schemars`, `thiserror`. Needs the
  Director's approval as a new contract.
- **Acceptance:** the sixteen `ZoneInfo` entries are complete and their centres lie on the key plane (a test);
  `MotionEvent` is `Copy` and at most 32 bytes (`size_of` test); serde round trip of every cold type; a test that no
  public type of the crate holds a key code, a chord or a usage (a `JsonSchema` walk of every type, plus a test that the
  crate's source names no HID usage constant); **`ChordLabel` stays inside the editor:** a compile-time check that it
  implements neither `Serialize` nor `Display`, and a test that its `Debug` prints `<chord>`; `#![deny(missing_docs)]`; `lint-deps` and
  `lint-crate-table` accept the crate; check green.

### IN-2: `omuzaki-input-mock`: `FakeInput`, `ReplaySource` and the conformance suite (new mock)

- **Fence:** `crates/omuzaki-input-mock/**` (new).
- **Contract:** IN-1. `FakeInput` (scripted events, pad states and binding hits on `FakeClock`; implements `InputHub`,
  `MotionFeed`, `IntentFeed`, `BindingFeed`, `InputControl`, `IndicatorHost`, `ChordStore` and `ProtectList`, with
  scripted `SourceUp`, `SourceGone` and connected pads, so every consumer task tests against the mock before IN-3
  lands), `ReplaySource::from_text` (the recorded-input text format: one event per line, zone names, times
  in seconds and the backend, in drained order), `conformance::run(&mut dyn MotionFeed, …)`.
- **Acceptance:** replay of a 60 s script is identical over two runs and after a text round trip; `drain` allocates 0
  bytes (counting allocator); `drain(up_to)` never returns an event after `up_to` (proptest); when more than
  `MOTION_FRAME_MAX` events are due in one drain, the rest arrive, in order, in the next drain, and no release is ever
  lost (a stress test with 1,000 due events); a replay of events from several backends hands them out in
  exactly the recorded drained order, on every run; check green.

### IN-3: `omuzaki-input`: the hub, the rings, the epochs, the gate, the indicator and the kill switch (new crate)

- **Fence:** `crates/omuzaki-input/**` except the backend folders and IN-3B's files: `Cargo.toml` (`wasm = false`,
  reason "OS input"; `registers = true`; `generated-mods = ["src"]` with the `omuzaki-embed` build script, so each
  backend task adds its own folder, gated inside by `#![cfg(target_os = …)]`, and nobody edits `lib.rs` again; the
  `test-hooks` feature), `build.rs`, `src/lib.rs` (with `omuzaki_core::define_link!()`), `src/hub.rs`,
  `src/rings.rs`, `src/epoch.rs`, `src/indicator.rs`, `src/kill_switch.rs`, `src/protect.rs`, `src/test_hooks.rs`;
  `#[cfg(test)]` unit tests beside them; `tests/gate.rs` (through `test-hooks`).
- **Contract:** IN-1; ops-api `Scope::Input`; §3 rules 2 to 4, §4. Implements `InputHub` (its `set_detail` forwards to
  IN-3B's normaliser through a `pub(crate)` setter that IN-3 defines as a no-op until IN-3B), `InputControl`,
  `IndicatorHost`, `ProtectList`, the rings with their two epoch stamps, `register_input_backend!` and a `pub(crate)`
  `RawSink` the backends call.
- **Acceptance:**
  - **The gate and the indicator:** `start` fails with `NoIndicator` when no indicator is installed and when its
    heartbeat is stale; capture pauses within 1.1 s of the heartbeat going stale and resumes on its own when it
    returns; a pause asked through `pause` does not resume on its own.
  - **The safety loop never waits:** with a test surface that never returns from drawing and every feed stalled, the
    indicator check and the protected-program check still pause capture within 1.1 s and 2.1 s (the control thread
    reads atomics only).
  - **The epoch window:** a backend that reads a press after the pause bump, and one that read it just before and
    pushes it after, never gets it to a feed (a `test-hooks` pause between read and push, both ways round); an entry
    stamped by an earlier run of a backend that has come back is dropped.
  - **Held across a pause:** hold a movement binding, a key, a deflected stick and a pen in contact, then pause (once
    each by `pause`, `stop`, a lost indicator and a protected program): the movement action ends, the next
    `MotionFrame` carries `reset` for the keyboard source, `pad()` returns `None` and the pen reads out of range, and
    the new state is published without waiting for any feed; **after the resume** the still-held key, its OS
    auto-repeats and a modifier released while paused start nothing until the key is released and pressed again.
  - **One backend leaves:** with keyboard and gamepad capturing, fail the gamepad backend while a pad binding and a
    key are held: the pad binding's action ends and the pad sources reset, while the key stays held.
  - **Single writers:** a debug-build check on every ring and held snapshot records its writer thread and fails if a
    second thread writes.
  - **Frames never drop:** with 1,000 due events in one drain the rest arrive in order in later drains, and no release
    is lost.
  - **`test-hooks` stays private:** `cargo metadata` shows no other crate enabling the feature.
  - The forbidden-symbol test of §3 rule 4; check green.

### IN-3B: `omuzaki-input`: the normaliser, the zone map, the matcher, generations, holders and the chord store

- **Fence:** `crates/omuzaki-input/src/normalise.rs` and `src/normalise/zone_map.rs`, `src/matcher.rs`,
  `src/dispatch.rs` (draining, the generation pool, per-key press records, holder sets and reconciliation),
  `src/store.rs` (the chord store and its file); `#[cfg(test)]` unit tests beside them; `tests/bindings.rs` and
  `tests/privacy.rs` (through `test-hooks`); `benches/normalise.rs` and its `[[bench]]` entry (through `test-hooks`).
- **Contract:** IN-1, IN-3; §3 rules 1, 3 and 5, §5, the detail rule of §4. Implements `MotionFeed`, `IntentFeed`,
  `BindingFeed` and `ChordStore`.
- **Acceptance:**
  - The usage-to-zone map covers every usage of the ANSI (104), ISO (105) and JIS (109) boards (counted).
  - **Zone equivalence:** the proptest of §3 rule 5, through the `test-hooks` raw injector, over every output surface
    the crate has.
  - **No key value in a panic:** a forced panic on a capture thread writes no key value to stderr.
  - **A key's release ends what its press started:** (a) press Ctrl+F9, release Ctrl first, then F9: the action
    ends; (b) hold a binding, learn a new chord for it, release the old keys: the action ends, and the new chord
    starts it only on its own press; (c) hold an `Intent(Move)` binding, change its action to an op, release: the
    intent ends on `IntentFeed` and nothing reaches `BindingFeed`; (d) press a binding, swap in a table that changes
    its action, release: the old action ends and the new one never starts.
  - **Held across many swaps:** hold a binding across 6 table swaps, release: its own action ends (the record's own
    copy), never another binding's.
  - **Recycling is safe:** a capture thread paused mid-match keeps its table from being recycled.
  - **Table swaps with queued entries:** queue presses on slots 0 to 3, swap in a table that removes slot 1's binding
    and gives slot 2's chord another action, drain: slot 1's held action ends at the swap and its queued release is
    dropped and counted, slot 2's press and release both run its old action, and no entry runs another binding's action.
  - **Two holders:** two keyboard sources hold one binding: the first release does not end the action and the second
    does; the same with two pads and with a pad and a keyboard; a release from a source that holds nothing is ignored.
  - **One keyboard of two unplugged** with keys held on both: only its zones and its binding holders end.
  - **A lost release:** hold a movement binding, overflow its source's intent ring with repeats from a stalled
    consumer, release, resume: the intent ends (by the queued release or by reconciliation against that source's held
    snapshot) within one drain; the same for an op binding through `BindingFeed`.
  - **Full motion ring:** a key-up displaces the oldest pointer event; with none left, the oldest complete down and up
    pair of one source goes, else the oldest lone down, whose source's later up is then ignored; never an up.
  - **A `detail` switch** with a key and a movement binding held: the key's zone holder is reset and no old-detail key
    entry reaches motion, while the movement keeps going.
  - **Learning is gated:** `ChordStore::learn` fails with `NotCapturing` while capture is not `On`.
  - The matcher matches chords with modifiers and reports conflicts; `normalise` plus `match` at most 1 µs per event on
    `Dev` (bench); check green.

### IN-4: `omuzaki-input`: the Windows keyboard and mouse backend (Raw Input, no hooks)

- **Fence:** `crates/omuzaki-input/src/windows/**`, `tests/windows.rs`, and `windows-sys.workspace = true` in
  `crates/omuzaki-input/Cargo.toml` (the features it needs are declared by DEP-0).
- **Contract:** `RawSink` (IN-3); research §1.1 and §7: Raw Input with `RIDEV_INPUTSINK` on the crate's own hidden window
  and thread, buffered reads, no hooks; one `SourceTag` per Raw Input device handle, with `SourceUp` and `SourceGone`
  from `WM_INPUT_DEVICE_CHANGE`. The pen is not in this task: `CaptureCaps::pen = Unavailable` until IN-10, which waits
  on SP-1's answer (so IN-4 and SP-1 do not wait on each other).
- **Acceptance (Windows-only tests, skipped elsewhere with a message):** `SendInput` into a test window gives the
  expected zones and pointer deltas; delivery at most 1 ms p95 from the OS event (the `SendInput` call) to the entry pushed on the ring, the §10 row's
  measure (the time an entry then waits for the frame's `drain` is the frame period, not delivery); capture-thread CPU at most
  0.05 % of 8 cores **idle** over 60 s on `Dev` (#744's number; §10's 0.2 % is the loaded budget, measured by SP-1);
  two keyboards are two sources (unplugging one in a test sends `SourceGone` for it only); the `x86_64-pc-windows-gnu` cross-build and check green.
  (`SendInput` is used only by the test binary, never by the crate.)

### IN-5: `omuzaki-input`: gamepads on every OS (gilrs, XInput on Windows)

- **Fence:** `crates/omuzaki-input/src/gamepad/**`, `tests/gamepad.rs`, `gilrs.workspace = true` and (macOS)
  `objc2-game-controller.workspace = true` in `crates/omuzaki-input/Cargo.toml` (declared, with their licence files,
  by DEP-0).
- **Contract:** `PadState`, `PadZones`, `PadId`, `PadSet`, `MotionEvent::PadEdges`, `SourceUp`, `SourceGone` (IN-1),
  the hub's pad chords (IN-3B); research §1.1, §1.2, §1.5, §4. **This backend alone owns pads on every OS**, so a pad
  is never read twice and `PadId`s come from one allocator in the hub: `gilrs` (with `xinput`) on Windows and Linux,
  registered as `gamepad.gilrs`; on macOS `GameController` with `shouldMonitorBackgroundEvents = true`
  (`objc2-game-controller`, research §1.2), registered as `gamepad.gc`, with gilrs's IOKit path as its fallback. The
  macOS keyboard task (IN-7) reads no pads.
- **Acceptance:** a mapping test from gilrs events to `PadState`, queued `PadEdges` and `PadZones` with a fake gilrs
  event list (a press and release between two drains arrive as two edges, with the pad id); dead zone
  and quantisation (i8, u8) exact at the edges; hot-plug of a second pad keeps pad indices stable; two pads holding
  the same pad binding end its action only when the second releases; the libudev link is
  dynamic (as DEP-0's `gilrs.toml` records); `pads()` lists a pad whose first activity is a stick move; unplugging one of two pads with a stick deflected and a button held on each pushes `SourceGone` for that pad only: its
  sticks and buttons and the actions it held end, and the other pad's are untouched; check green on Linux and the
  Windows cross-build.

### IN-6: `omuzaki-input`: Linux X11 (XI2 raw events) and the evdev opt-in

- **Fence:** `crates/omuzaki-input/src/linux_x11/**`, `src/linux_evdev/**`, `tests/linux.rs`, the `x11rb` (`xinput`)
  and `evdev` dependencies as `workspace = true` (declared by DEP-0).
- **Contract:** `RawSink`; research §1.3, §1.4; the evdev backend's `BackendState::NeedsPermission { how: InputGroup }`
  when evdev is chosen without access; one `SourceTag` per device node, with `SourceUp` and `SourceGone`.
- **Acceptance:** an Xvfb test (skipped with a message where the runner has no Xvfb) sends XTest events to a client window and reads zones and deltas; evdev reads a
  `uinput` test device where the runner allows it, else skips with a message; `XI_RawMotion` is unselected while no
  pointer motion is enabled; check green.

### IN-7: `omuzaki-input`: macOS keyboard and mouse (listen-only event tap)

- **Fence:** `crates/omuzaki-input/src/macos/**`, `tests/macos.rs`, the `objc2`, `objc2-core-graphics` and
  `objc2-io-kit` dependencies as `workspace = true` (declared by DEP-0; `objc2-io-kit` for the Input Monitoring check).
- **Contract:** `RawSink`; research §1.2; the tap backend's `BackendState::NeedsPermission { how: InputMonitoring }`
  (checked with `CGPreflightListenEventAccess` and IOKit's access check); re-enable a tap disabled by timeout. Pads are
  IN-5's on macOS too.
- **Acceptance:** a `cargo check --target aarch64-apple-darwin` in CI (the weekly macOS job runs the tests); a manual
  checklist on a Mac (local lane) in the PR body: prompt shown once, tap recovers, secure input mode yields no keys.

### IN-10: `omuzaki-input`: the pen, per OS (after SP-1)

- **Fence:** a `pen` module inside each backend folder SP-1 confirms (`src/windows/pen.rs`, `src/linux_x11/pen.rs`,
  `src/macos/pen.rs`) with its `mod` line in that folder's `mod.rs`, `tests/pen.rs`.
- **Contract:** `MotionEvent::Pen` with `in_range` and `contact` (IN-1); SP-1's verdict per OS; `CaptureCaps::pen`.
- **Acceptance:** on each OS SP-1 confirmed, a recorded or injected pen stream gives the coarse position, pressure,
  tilt, in-range and contact; an OS SP-1 did not confirm keeps `pen = Unavailable`; check green.

### IN-8: `input.capture.*` and `input.binding.*` ops

- **Fence:** `crates/omuzaki-input/src/ops/**` (one file per op), `crates/omuzaki-input/tests/ops.rs` (through the
  `test-hooks` feature where it needs raw input), its `.changes/` note.
- **Contract:** ops-api (`register_op!`, `Scope::Input`, `person_only`, `require_person`), `InputControl` as a service;
  §4 and §5 tables: `input.capture.start`, `input.capture.stop`, `input.capture.state`, `input.capture.protect`,
  `input.capture.pause`, `input.binding.list`, `input.binding.set_action`, `input.binding.remove`,
  `input.binding.apply_preset { preset }` (scope `perform`, agent `Confirm`; the preset's chords are data inside
  `omuzaki-input`, so the op carries only the preset name). No op takes or returns a chord or an indicator.
- **Acceptance:** table tests with person, agent and automation actors (an agent's `input.capture.start` is `Denied`
  and never queued; a person's `start` with no indicator installed fails with `NoIndicator`; **a read-only actor**
  (grants `read` only) stops and pauses capture; stopping and pausing are idempotent; `input.binding.set_action` with an
  action whose op needs a scope the caller lacks is `Denied`; `input.binding.apply_preset` that changes the chord of a
  held binding: the held action ends when the old chord is released, as in IN-3B); a test that types a known string through
  the real normaliser (the `test-hooks` raw injector, so the string's key codes really enter the hub) and scans every
  op result, event and journal record for any of its key codes or characters (none; the scan is shown to find them
  when a test-only leak is planted, so it is not vacuous);
  **the protected-program list:** `input.capture.protect` is refused without `process` (an actor with `input` and
  `perform` only gets `Denied`); with capture paused by a protected program, `input.capture.state` returns
  `Paused { why: None }` to an actor without `process`, and the same for a pause by a lost indicator, so the two cannot
  be told apart; with `process` it returns `Paused { why: Some(ProtectedApp) }`; no result, event or journal record
  names a program; registrations found by the link gate; check green.

### IN-9: `omuzaki-input`: Wayland bindings through the GlobalShortcuts portal

- **Waits on** the owner's answer to §12 question 2 (what PF-29 ships on Wayland). Until it lands, the Wayland default is
  gamepad only (§4).
- **Fence:** `crates/omuzaki-input/src/linux_portal/**`, `tests/portal.rs`, `ashpd.workspace = true` in
  `crates/omuzaki-input/Cargo.toml` (declared, with `global_shortcuts` and its licence file, by DEP-0).
- **Contract:** `BindingFeed`, `IntentFeed` (IN-1), the internal ring, generation and held-snapshot machinery (IN-3B),
  a `register_input_backend!` named `linux.portal`; §5 ("Wayland"): the binding table is registered
  as the portal's shortcut list (one shortcut per binding, described by its name, never by a chord we invent), and every
  table swap re-registers it; `Activated` and `Deactivated` signals become `Press` and `Release` on the portal backend's
  own binding ring; the portal gives no zones, so this backend produces no motion events. The portal's
  `trigger_description` strings are dropped on receipt: never stored, logged, put in a state or shown. `CaptureCaps` reports
  `bindings: Portal` and `typing: NeedsOptIn`.
- **Acceptance:** against a fake portal on a private D-Bus session bus (`dbus-daemon --session` started by the test,
  skipped with a message where the runner has none): registering a table of three bindings sends three shortcuts with
  their names and no key codes; activation and deactivation signals arrive as `Press` and `Release` with the right
  `(gen, slot)`; a table swap re-registers and stamps the new generation; a reply carrying a `trigger_description`
  leaves no trace of it in any state, log line or feed output; a portal that refuses or is missing gives
  the portal backend `BackendState::Failed` with a fixed reason, leaves the other backends running and the global state
  `On` with `Keyboard` in `degraded` (and `Failed` only when the portal was the only backend); check green.

### UI-1: `omuzaki-ui`: the capture indicator in the desktop shell

- **Platform area** (the Platform lead signs off the shell files).
- **Fence:** `crates/omuzaki-ui/src/shell/input_indicator.rs` and its `mod` line in `src/shell/mod.rs`,
  `crates/omuzaki-ui/tests/input_indicator.rs`, the `omuzaki-input-api` dependency in `crates/omuzaki-ui/Cargo.toml`
  (the first of UI-1 and UI-2 to merge adds it), the generated localisation output for its strings regenerated with
  the repository's tooling (strings use derived ids, l10n-ops.md; nothing hand-written in the generated files), its
  `.changes/` note.
- **Contract:** `IndicatorHost`, `IndicatorHandle`, `CaptureState`, `BackendState` (IN-1); §4: a status-bar badge and
  tray state **in every capture state, `Off` included** ("Input capture: off" with a Start control for the person; while
  on, the classes read, the degraded classes, the detail, and a one-click Stop that calls `input.capture.stop`; while
  paused, the full reason and program for the person, from the handle's view); the badge writes the heartbeat each time
  it is actually drawn in a visible window, and never blocks on the hub. Until WIRE-2 wires the desktop App, it is
  exercised through the shell's test harness with a hub from `omuzaki-input-mock`.
- **Acceptance:** with the shell's test harness, the badge is drawn at `Off` and its heartbeat is fresh, so a person's
  `input.capture.start` succeeds; the badge changes within one frame of `On`; when the window is hidden or the badge
  removed the heartbeat stops, and capture pauses within 1.5 s; Stop ends capture; check green.

### CLI-1: `omuzaki-cli`: the capture banner

- **Platform area.**
- **Fence:** `crates/omuzaki-cli/src/input_banner.rs` and its `mod` line in `src/lib.rs`,
  `crates/omuzaki-cli/tests/input_banner.rs`, the `omuzaki-input-api` dependency in `crates/omuzaki-cli/Cargo.toml`, its
  `.changes/` note.
- **Contract:** `IndicatorHost`, `IndicatorHandle` (IN-1); §4: a stderr status line printed as soon as the indicator is
  installed ("input capture: off") and kept current in every state from the handle's view, reprinted after other
  output; the line's printer writes the heartbeat; installed only when stderr is a terminal (no heartbeat otherwise, so
  a piped or headless run can never start capture). The handle comes from WIRE-1.
- **Acceptance:** with stderr a pseudo-terminal the line prints at `Off`, the heartbeat is fresh and `start` succeeds,
  and the line changes on `On`, `Paused` and `Off`; with stderr redirected `start` fails with `NoIndicator`; check
  green.

### UI-2: `omuzaki-ui`: the binding editor and the protected-program list

- **Platform area.**
- **Fence:** `crates/omuzaki-ui/src/shell/input_bindings.rs` and its `mod` line in `src/shell/mod.rs`,
  `crates/omuzaki-ui/tests/input_bindings.rs`, the `omuzaki-input-api` dependency in `crates/omuzaki-ui/Cargo.toml` if
  UI-1 has not added it, its localisation output as in UI-1, its `.changes/` note.
- **Contract:** `ChordStore` (`learn`, `label`, `clear`, `conflicts`; IN-1, IN-3B), `ProtectList` (IN-1, IN-3), the ops
  `input.binding.*` and `input.capture.protect` (IN-8); §4 and §5: the only place chords are learnt and shown (learning
  needs capture `On`; the editor says so and offers Start), and the person's own editor for the kill-switch list. The
  handles come from WIRE-2; until then the harness uses the mock.
- **Acceptance:** learning a chord through the harness stores it and shows it only in this panel, and with capture off
  the editor shows the `NotCapturing` message instead; the panel's actions that go through ops carry no chord (checked
  on the recorded requests); a conflicting chord is shown, not overwritten; the protected-program list round-trips
  through `ProtectList`; check green.

### WIRE-1: wiring `omuzaki-input` into the CLI App

- **Owner:** the App that builds `Services` today, `omuzaki-cli`'s session (`crates/omuzaki-cli/src/session.rs`).
- **Fence:** `crates/omuzaki-cli/src/session.rs`, `crates/omuzaki-cli/Cargo.toml` (the `omuzaki-input` dependency),
  the CLI's link gate `crates/omuzaki-cli/tests/registrations.rs` expectations (the one-line `link()` call is inside
  the fence by standing ruling), `crates/omuzaki-cli/tests/input_wiring.rs`, its `.changes/` note.
- **Contract:** `InputHub` (IN-1, built in IN-3 and IN-3B): construct the hub once per process; insert its
  `dyn InputControl` into `Services`; hand `IndicatorHost` to CLI-1; take the feeds once and give the stage its
  `MotionFeed` and `IntentFeed` (through ST-1's and ST-2's setters); keep `BindingFeed` for #469's dispatcher, which
  takes it from the session when it lands; subscribe to `input.motion.detail` and call `InputHub::set_detail` on
  change; call `omuzaki_input::link()`. `omuzaki-mcp`'s standalone server is deliberately not wired: there `input.*`
  ops answer `Unavailable`, which a test pins.
- **Acceptance:** a CLI session with a terminal stderr lists the `input.*` ops, `input.capture.state` answers `Off`,
  and a person's `start` reaches the hub; changing `input.motion.detail` reaches the hub; the link gate passes;
  `omuzaki mcp` answers `Unavailable` for `input.capture.start`; check green.

### WIRE-2: wiring `omuzaki-input` into the desktop App

- **Blocked** until the desktop App's startup path exists on `main` (today `omuzaki-ui` has the shell library and a
  gallery binary, no App start-up); the lead files it when that lands.
- **Fence:** the desktop App's start-up module and its manifest, its link gate, `tests/input_wiring.rs` in that crate,
  its `.changes/` note.
- **Contract:** as WIRE-1, plus `IndicatorHost` to UI-1's badge and `ChordStore` and `ProtectList` to UI-2's editor.
- **Acceptance:** the shell's badge shows `Off` at start and a person's Start from it begins capture; the binding
  editor learns a chord; the link gate passes; check green.

### MO-1: `omuzaki-anim`: input motion, the desk rig, the hand allocator and typing

- **Fence:** `crates/omuzaki-anim/src/input_motion/{mod.rs,desk.rs,hands.rs,typing.rs}` (a new folder, declared by the
  build script), `crates/omuzaki-anim/tests/input_motion_typing.rs`, `crates/omuzaki-anim/benches/input_motion.rs` and
  its `[[bench]]` entry, the `omuzaki-input-api` dependency (and `omuzaki-input-mock` as a dev-dependency) in
  `crates/omuzaki-anim/Cargo.toml`.
- **Contract:** IN-1 and IN-2; `PoseLayer`, `BoneMask`, `LookAt`, `Transform` (anim-api); the hand-shape library
  (`hands/`); §6.1, §6.2, §6.5: `MotionOut`, `PalmGoal`, `DeskRig`, `DeviceMotion` (with its 64-byte checkpoint slot),
  `StepCtx`, `register_device_motion!`, `Hands`, `InputMotion`; the typing device motion is the first registered one.
- **Acceptance (replay on mock humanoids at three statures):** the striking fingertip within 1 cm of its zone centre at
  the next frame; no fingertip more than 2 mm below the key plane over 60 s; home row after 0.4 s and rest after 1.5 s
  without keys (± one frame); `hand_only` output identical for two streams with the same sides; a `tracked` mask drops
  the masked bones with a 0.3 s fade; `MotionFrame.reset` for a keyboard source lifts that source's held fingers in
  that frame and leaves another keyboard's held fingers down, and switching `detail` or `hand` with keys held lifts
  them all; `SourceGone` does the same as a reset for its source; a key-up from a source that holds nothing changes
  nothing; a test device motion registered from the test (with `register_device_motion!`) is built by
  `InputMotion::new()` and its slot round-trips through the checkpoint; `InputMotion::checkpoint` and `restore` (§9): restoring the checkpoint taken at
  any frame of a 60 s replay and stepping on gives bit-identical `MotionOut` to the uninterrupted run; the motion types
  hold their mutable state only in `MotionCheckpoint`; deterministic; `step` allocates 0 bytes; at most 0.02 ms per frame
  on `Dev` (bench); check green.

### MO-2: `omuzaki-anim`: mouse and pen motion

- **Fence:** `crates/omuzaki-anim/src/input_motion/{pointer.rs,pen.rs}` and their `mod` lines in `input_motion/mod.rs`, `crates/omuzaki-anim/tests/input_motion_pointer.rs`.
- **Contract:** MO-1's `DeviceMotion` and `register_device_motion!` (each registers itself and keeps its state in its
  own checkpoint slot); `MotionEvent::Pointer` and `Pen` with `in_range` (IN-1); §6.3.
- **Acceptance:** the mouse hand never leaves the pad rectangle over a 60 s random-walk replay; recentre time constant
  1.5 s ± 5 %; a click curls the index finger within one frame; pen contact lowers the palm by pressure × 4 mm ± 0.5 mm;
  the allocator switches hands with the 0.15 s dwell and 0.25 s transit; hover (in range, no contact) lifts the hand
  1 cm and out of range releases the pen hand; a closed backend epoch reads as out of range; restore from a checkpoint
  continues bit-identically; deterministic, 0 allocations; check green.

### MO-3: `omuzaki-anim`: gamepad motion and stick look

- **Fence:** `crates/omuzaki-anim/src/pad_motion/**` (a new top-level folder, so `input_motion` is not edited),
  `crates/omuzaki-anim/tests/pad_motion.rs`, `crates/omuzaki-anim/library/poses/hold-pad.toml`.
- **Contract:** MO-1's `DeviceMotion`, `StepCtx` (its `head_tracked` flag, which the stage fills from the bus) and
  `register_device_motion!`; `PadState`, `PadEdges`, `PadSet` (IN-1), `TwoHandedProp` shape of goals (two palm goals),
  §6.4. The pads come from `MotionFeed::pads` (and `SourceUp` and `SourceGone`), so a pad whose first activity is
  a stick move is found; the hands hold the **active pad**, the connected pad with the latest stick move past its dead
  zone or the latest pad edge.
- **Acceptance:** at three statures, thumb tips within 5 mm of the stick caps at full deflection in 8 directions; full
  right-stick deflection gives 35.0 ± 0.5 degrees of look yaw after settling; with `StepCtx::head_tracked` set the
  look goal's weight is 0 after 0.3 s (ST-1 tests that the stage sets it from the bus); a pad listed by `pads()` whose
  only activity is a stick move drives the hands; unplugging one of two pads (`SourceGone`) moves the hands to the
  other only if it is active; `pad()` returning `None` (a closed epoch) centres the thumbs; a face-button tap whose press and release fall between two frames still
  moves the thumb; deterministic, 0 allocations; check green.

### LO-1: `omuzaki-anim`: the locomotion state machine and root motion

- **Fence:** `crates/omuzaki-anim/src/locomotion/{mod.rs,machine.rs,def.rs,intents.rs}`,
  `crates/omuzaki-anim/tests/locomotion.rs`, `crates/omuzaki-anim/benches/locomotion.rs` and its `[[bench]]` entry, the
  `omuzaki-input-api` dependency and the `omuzaki-input-mock` dev-dependency if MO-1 has not added them.
- **Contract:** `PoseClip` (#223), `PoseLayer`, IN-1's `IntentEvent` (from `IntentFeed`) and `PadState` for `Intents`; §7.1, §7.3, §7.4:
  `Intents`, `LocomotionDef`, `Locomotion::step(tick, &Intents, &DeskRig, &mut PoseLayer) -> RootStep`. The gait is a
  trait (`Gait`) with an in-place placeholder until LO-2.
- **Acceptance:** a test per allowed state pair, and every other pair refused; no bone jumps more than 2 degrees between
  30 fps frames over a scripted 120 s run; speed respects the 6 m/s² limit; turn rate at most 360 degrees a second;
  `LocomotionDef` validation errors name the field; a def change during a jump finishes the jump under the old def and
  applies the new one from the next `Idle` or `Move`; a reset of the sources driving `move` (and a `None` pad) brings the machine to `Idle`; the checkpoint
  names the def in force (`def_lsn`), so restoring mid-jump after a later `locomotion.set` continues under the old def;
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
  `crates/omuzaki-stage/src/frame/mod.rs` (`Stage::set_motion_feed` and the drain call in the frame sequence),
  `crates/omuzaki-stage/src/frame/rig.rs` (palm goals and foot planting in the IK stage),
  `crates/omuzaki-stage/tests/input_frame.rs`, `crates/omuzaki-stage/benches/input_frame.rs` and its `[[bench]]` entry,
  the `omuzaki-input-api` dependency in `crates/omuzaki-stage/Cargo.toml`, its `.changes/` note. Runtime-area files: the
  Runtime lead signs off.
- **Contract:** `MotionFeed` (IN-1), handed in through `Stage::set_motion_feed`, which WIRE-1 calls; `InputMotion`,
  `MotionOut` and `StepCtx` (MO-1), with `head_tracked` set from the bus (the tracker owns any head channel);
  `HandTarget` and `FootPlanting` (rig); the `input.motion.*` settings (LO-4), read through a `SettingsEvent`
  subscription and copied on change, never per frame; the desk rig's default anchors, read through one call site
  (`desk_anchors()` in `perform/motion.rs`) that ST-3 later fills from the props; §6.1.
- **Acceptance:** a 60 s replay (typing, mouse) on the mock character gives equal frame digests over two runs; the
  stage's zero-allocation frame test still passes; drain honours the A/V delay (events at `t` act at `t + D`); with a
  tracker layer owning a head channel `StepCtx::head_tracked` is set and the stick look fades out; added CPU at most
  0.08 ms per frame on `Dev` (drain, motion and palm IK, §10; bench, `scaled`); check green.

### ST-2A: `omuzaki-stage-api`: the camera follow lag (contract change)

- **Fence:** `crates/omuzaki-stage-api/src/camera.rs` (one optional field on `CameraDef`, `follow_lag_s`, defaulting to
  no lag), its tests, an ADR only if the Director rules the change breaking.
- **Contract:** contracts change first and alone (`CLAUDE.md`): this lands before ST-2 uses it, and needs the
  Director's approval on the task.
- **Acceptance:** a camera file without the field loads with no lag; serde round trip; `lint-deps`; check green.

### ST-2: `omuzaki-stage`: locomotion, the live root and camera follow

- **Fence:** `crates/omuzaki-stage/src/perform/locomotion.rs` and its `mod` line in `src/perform/mod.rs`;
  `crates/omuzaki-stage/src/frame/mod.rs` (`Stage::set_intent_feed` and the locomotion step in the frame sequence); the
  ops `locomotion.home` and `locomotion.state` in `crates/omuzaki-stage/src/ops/locomotion/` (they act on the live
  root, which only the stage holds; the `src/ops` list is generated); the stage's own code that builds each member's
  `AnchorFrame` (the `omuzaki-stage-api` type is used as it is: `root` is the member's position and yaw, and the stage
  now passes the live root instead of the authored one); the camera's use of ST-2A's follow lag;
  `crates/omuzaki-stage/tests/locomotion_frame.rs`; its `.changes/` note. No `omuzaki-stage-api` file.
- **Contract:** `Locomotion`, `RootStep` (LO-1); `Intents` from `IntentFeed` (handed in through
  `Stage::set_intent_feed`, which WIRE-1 calls) and the active pad's `PadState` (found through `MotionFeed::pads`, as
  in MO-3); `CameraDef::follow_lag_s` (ST-2A); §7.3; `locomotion.home` (`perform`) and `locomotion.state` (`read`).
- **Acceptance:** added CPU at most 0.02 ms per frame on `Dev` (§10); the root stays inside the walk radius over a
  120 s random-intent run; `locomotion.home` returns within 0.5 s ± one frame; a `Subject`-framed camera follows with
  the configured lag; a pad whose only activity is a stick move walks the character, and a closed epoch (`pad()` is
  `None`) stops it; deterministic; check green.

### ST-3: `omuzaki-stage`: the input device props

- **Fence:** `crates/omuzaki-stage/src/input_props/**` (a new top-level folder, declared by the build script), the body
  of ST-1's `desk_anchors()` call site in `crates/omuzaki-stage/src/perform/motion.rs`,
  `crates/omuzaki-stage/tests/input_props.rs`, its `.changes/` note.
- **Contract:** #705's `PropKind` and `register_prop_kind!`; §8's kinds and anchors. #705's `PropOutput` carries no
  anchors, so this task does not change it: the anchors are computed in `input_props` from each prop's params and
  placement (`InputPropAnchors`, a stage-internal type) and returned by `desk_anchors()` each frame.
- **Acceptance:** each of the five kinds builds a mesh under 2,000 triangles; its anchors move with the prop when
  `stage.set_prop` moves it, and the hands follow; the default placement puts the key plane within reach (every zone
  centre inside 95 % of arm reach) at three statures; check green.

### ST-4A: `omuzaki-take-api`: the `Motion` record kind (contract change)

- **Fence:** `crates/omuzaki-take-api/src/record.rs` (the kind `Motion = 12`, its layout of §9 and the checkpoint
  framing, a minor version bump), its tests.
- **Contract:** takes.md T2 and T3 (a minor version is additive); contracts change first and alone, with the
  Director's approval on the task.
- **Acceptance:** the record encodes and decodes every field of §9 bit-exactly; an older reader (the previous minor)
  skips it by `len` and counts it; check green.

### ST-4: `omuzaki-stage`: the take's `Motion` records and the motion provider

- **Fence:** `crates/omuzaki-stage/src/take/motion.rs` and its `mod` line in `src/take/mod.rs` (#1357 creates that
  folder), `crates/omuzaki-stage/tests/take_motion.rs`; the export rewrite of `Motion` records (drop zone edges, write
  `RESOLVED`) where the take leaves the machine, `crates/omuzaki-take/src/export_motion.rs` with its `mod` line (the
  `omuzaki-take` lib of #1318 and #1355; Runtime area, its lead signs off), and the `keep_zone_edges` choice on the
  share or export op that #1381 and #1382 own.
- **Contract:** ST-4A; takes.md T2, T3 and T5; §9; the `motion` live-state provider (`register_take_state!`: the seed
  and the motion settings) and the per-frame data ST-1 and ST-2 already hold.
- **Acceptance:** the records hold only the fields of §9's layout (a decoder check over the whole take: every byte is
  accounted for by a known field, so no other data can ride along); the take re-renders to the same frames;
  **replay cuts:** for every group boundary of a 120 s take, a clip cut there and re-rendered from its checkpoint gives
  frame digests equal to the uncut run's from that frame on (the cuts include one inside a jump that began before a
  later `locomotion.set`, one mid-transition, one during a typing burst, and one in an idle stretch after walking);
  **short edges:** a click, a key tap and a pad tap whose down and up fall in the same frame replay as strikes;
  **resets and settings:** a pause with keys held, a device unplugged, and a `detail` switch, recorded mid-take, replay
  with the same fingers lifting in the same frame; **zone edges stay local:** exporting or sharing the 120 s take
  without the opt-in writes no `keys` or `pad_edges` entry and no held-zone set (checked by decoding every record of the
  exported file), and the exported take re-renders to the same frames from its `RESOLVED` blocks; with the opt-in for
  that take the zone edges are kept; check green.

### EV-1: the evidence cases for typing, pointer, pad and locomotion

- **Fence:** `crates/omuzaki-test-char/evidence.toml` (new), `crates/omuzaki-test-char/tests/input_motion_render.rs`,
  `omuzaki-input-mock` and `omuzaki-render-mock` as dev-dependencies in `crates/omuzaki-test-char/Cargo.toml`.
- **Contract:** MO-1 to MO-3, LO-1 to LO-3, `tools/ci/evidence.py`'s case format.
- **Acceptance:** four cases (typing, mouse and pen, pad, locomotion strip), each front and three-quarter at three
  statures, fixed camera and seed; they render identically twice; the PR carries the `visual` label.

### SP-1: the hardware spikes (local lane)

- **Fence:** a dated research note under `docs/research/` (with its front matter).
- **Scope:** research §7 spikes 1 to 4: pen data unfocused on each OS, XInput with an unfocused game, UIPI with an
  elevated game, macOS prompts, Wayland evdev, and the `input.capture` numbers on `Dev` and, when one exists, `R`.
- **Acceptance:** each question answered yes, no or untested, with the machine named; the pen answers are what IN-10
  builds from, and the loaded `input.capture` CPU number is checked against §10's 0.2 %.

**Issues this makes unnecessary:** none closes outright; each old issue maps to the tasks above and is re-cut by the
lead. #223's walk clip stays (pose-library example); locomotion uses LO-2's gait.
