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
| D3 | **Capture is off by default, turned on only by a person** (`person_only`, scope `input`, which already exists on `main`: no `omuzaki-ops-api` change), **never restored** at start, and **runs only while an indicator is shown**: starting needs an `IndicatorLease` the surface mints, and the hub stops itself if the lease lapses. | §4 |
| D4 | **Bindings and triggers share the source.** The same capture thread feeds two queues: zone events to the frame thread (motion), binding events to the binding dispatcher (PF-05 actions, PF-25 triggers, locomotion intents). Chords live in a machine-local store that `Services`, ops, MCP and agents never reach. | §5 |
| D5 | **Motion is pose layers plus IK goals in the §8 order:** after the animator, before the rig. Typing, pointer, pen and gamepad are device motions sharing one hand allocator; each hand belongs to one device at a time. A tracked bone wins (PF-23) through a `tracked` bone mask, with a 0.3 s hand-over. No `omuzaki-anim-api` change. | §6 |
| D6 | **Locomotion** is a state machine (idle, walk, run, jump, sit, emote) over a 1-D speed blend; walk and run are a procedural gait (licence-clean by construction), jump, sit and emotes are pose clips (#223). The root moves procedurally, matched to stride; foot planting (`omuzaki-rig`) removes the rest of the slide. A set is a library item; a character names one. | §7 |
| D7 | **Props** are four ST-03 prop kinds with named anchors (key plane, pad plane, tablet plane, grips). Hands use the anchors whether the prop is drawn or hidden. A desk is placed from the character's own proportions. | §8 |
| D8 | **A take stores what the video showed:** zones, coarse pointer and pen, pad state, locomotion intents and the root, all at frame resolution, as a new additive `Motion` record kind (a minor version, ADR 0052 rules). Never a key code, a chord or a sub-frame time. | §9 |
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
 OS (Raw Input, CGEventTap, XI2, evdev, XInput/GameController via gilrs)
   │  omuzaki-input: one capture thread per backend; key codes live only in `normalise`
   ▼
 normalise ──► zone table ──► MotionEvent ring (SPSC, 512) ──► frame thread: MotionFeed::drain ──► omuzaki-anim
     │                                                                    input_motion, pad_motion, locomotion
     └──► binding matcher ──► BindingEvent ring (SPSC, 64) ──► binding dispatcher: PF-05 actions, PF-25 triggers,
                                                               locomotion intents (by slot, never by chord)
 pad state ─────────────────► newest-wins PadState (triple buffer) ──► frame thread
```

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-input-api` | API (over `omuzaki-core`, `serde`, `schemars`, `thiserror`) | the event and state types of §3, the zone table, `BindingSlot`, `BindingId`, `KeyChord` (cold, binding store only), `CaptureState`, `CaptureCaps`, the traits `MotionFeed`, `BindingFeed`, `InputControl`, `Indicator` |
| `omuzaki-input-mock` | Mock | `FakeInput` (scripted events on `FakeClock`), `ReplaySource` (recorded input from text, deterministic), the conformance suite every backend passes |
| `omuzaki-input` | Implementation (`wasm = false`: OS input) | the hub, the normaliser and binding matcher, the rings, the indicator lease, the kill switch, the backends (`windows`, `macos`, `linux_x11`, `linux_evdev`, `gamepad`), the ops `input.capture.*` and `input.binding.*` |

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
pub struct PadState { pub pad: u8, pub sticks: [[i8; 2]; 2], pub triggers: [u8; 2], pub zones: PadZones, pub at: Seconds }
pub struct PadZones(u16);                     // DPad, Face, LeftStick, RightStick, LeftShoulder, RightShoulder, LeftTrigger, RightTrigger, Centre
pub struct BindingSlot(u8);                   // index into the active binding table, 0..MAX_BINDINGS (64)
pub struct BindingEvent { pub slot: BindingSlot, pub edge: Edge, pub at: Seconds }
pub enum Edge { Press, Release, Repeat }

pub trait MotionFeed: Send {                  // the frame thread's port; never blocks, never allocates
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame);   // events with at <= up_to, in order
    fn pad(&self, pad: u8) -> Option<PadState>;                    // newest wins
}
pub struct MotionFrame { /* fixed array of MOTION_FRAME_MAX (128) events plus a dropped count */ }
pub trait BindingFeed: Send { fn poll(&mut self) -> Option<BindingEvent>; }
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
   of key codes, and panics inside the capture thread are caught and reported as `CaptureState::Failed { reason }` with a
   fixed reason string (the crash-report design keeps only registered templates and sends no memory, `privacy.md` §3, §4).
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
| `input.capture.start { classes }` | `input` | `person_only` (an agent or automation gets `Denied`, never queued) | starts capture for the device classes asked, if an indicator lease is held |
| `input.capture.stop` | `perform` | anyone (stopping only removes exposure) | stops capture; idempotent |
| `input.capture.state` | `read` | anyone | `CaptureState` and `CaptureCaps`, no device names or counts of keys |
| `input.motion.*` settings keys | (settings registry) | agent `Confirm` | the motion settings of §6.5 |

`CaptureState` is `Off`, `NeedsPermission { how: PermissionHow }` (macOS Input Monitoring, Linux `input` group for
evdev), `On { classes }`, `Paused { why: PauseWhy }` (indicator lapsed, protected app running) or `Failed { reason }`.
`CaptureCaps` says per class and OS what works (`pen_pressure: Unavailable`, `typing: NeedsOptIn` on Wayland), so the UI
can say so (research §2).

**Off by default, never restored.** Capture state is not a setting and is not in the session file: settings.md D9
already says session restore "never restarts capture or output". A crash or restart comes back `Off`.

**The indicator is a precondition, not a courtesy.** `InputControl::start(classes, lease: IndicatorLease)`. A lease is
minted only by an `Indicator` implementation that a surface installs: the desktop shell's status-bar badge and tray
state ("Reading keyboard and mouse: zones only. Stop"), or the CLI's persistent stderr banner. The hub calls
`Indicator::shown()` once a second on its control thread; if it reads `false` (the window was hidden by a bug, the
surface dropped the lease) the hub pauses capture within a second and reports `Paused { why: IndicatorLost }`. Windows
and X11 show the user nothing of their own (research §5), so this is the only signal the person has.

**The kill switch.** An optional list of protected programs (default empty). While capture is on and the list is not
empty, the control thread checks the running process names every 2 s and pauses capture while one runs. This is the
hub's own behaviour on the person's own list, not an op that reads processes for an actor, so it does not need the
`process` scope.

**Platform gaps** stay visible: on Wayland typing and the pointer need the evdev opt-in (the `input` group, with the
research's warning that it can read every keyboard), so the default there is gamepad and bindings (through the
GlobalShortcuts portal) only. PF-29 on Wayland is partial; that is research risk 2 and goes to the owner (§12,
question 2).

## 5. D4: bindings, triggers and locomotion intents on the same source

A **binding** is `{ id: BindingId, name, device: DeviceClass, chord: KeyChord | PadChord, action: BindingAction }`. The
action is what PF-05 and PF-25 own: an op invocation (expression, toggle, prop, scene), a trigger slot (`omuzaki-anim`
`Triggers`), or a locomotion intent (`Move(dir)`, `Run`, `Jump`, `Sit`, `Emote(n)`). This design fixes only how the
source delivers them; the action catalogue and the dispatcher belong to #469 and #478.

- **The chord store.** Chords are in `omuzaki-input`'s own machine-local file (`input-bindings.toml` in the data dir),
  read only by the hub. It is not a settings key: settings are listed to agents (`settings.list`) and their values
  appear in events, and `SecretRef`-style hiding exists only for secrets. The store's API (`ChordStore`) is handed to the
  desktop UI directly and **never inserted into `Services`**, so no op, MCP tool or agent can reach it.
- **What ops and agents see.** `input.binding.list` returns `{ id, name, device, action, conflict }`, never a chord;
  `input.binding.set_action { id, action }` (scope `perform`, agent `Confirm`) and `input.binding.remove { id }`.
  Learning a chord ("press the keys") is a UI call on `ChordStore::learn(id)`, a person's action by construction; its
  result is shown in the binding editor and written to the store, and nothing goes through the executor. A conflict
  (the same chord on two bindings) is reported, not overwritten.
- **Matching.** On the capture thread, after the zone is computed, the raw event is compared with the active table (a
  fixed array of at most 64 chords, swapped in whole by the control thread when the store changes). A match pushes
  `BindingEvent { slot, edge, at }` to the binding ring; the slot indexes the same table on the consumer side. Modifier
  state is kept inside `normalise`. A non-match produces nothing on that ring.
- **Consumers.** The binding dispatcher (#469) polls `BindingFeed` on the ops side and runs the action; PF-25 triggers
  get slot presses as trigger slot presses; locomotion intents are turned into an `Intents` value per frame (§7.1).
  Gamepad bindings (`PadChord`: a button, a combination, or a stick past a threshold) match in the same place.
- **Wayland.** Bindings use the GlobalShortcuts portal: the portal sends only the shortcuts we registered, which is the
  binding table itself, so the privacy rule holds by construction there.
- **Default bindings** ship as a preset the person enables (locomotion: `W A S D`, Space, left Shift to run, `C` to
  sit; the pad's left stick and face buttons). A preset is just bindings; nothing is bound until the person applies it.

Note for #469 (filed by the lead, not here): PF-05 consumes `BindingFeed` from `omuzaki-input` and adds no second OS
capture path; OSC, MIDI and Stream Deck are other sources of the same `BindingEvent`.

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
  from a seeded `Rng` stream (seed per take), so the motion still reads as typing and carries no key information.
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
  zone press moves the thumb to that cluster; shoulder and trigger zones curl the index fingers with the trigger value.
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
# library item, kind "locomotion" (ADR 0019); characters/<id>/character.toml names one: locomotion = "default"
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

`LocomotionDef` is that file as a serde type with a JSON Schema (`omuzaki-anim`), validated on load (speeds positive,
walk below run, clip ids that exist). A set is a library item so it is shared across characters and projects; a
character only names it, and `locomotion.set { character, set }` (scope `edit:character`) changes the name.
`locomotion.state` (scope `read`) reports state, speed and root position.

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
locomotion is active, fixed little-endian layout, about 40 to 80 bytes:

```
member u8 | flags u8 (HAS_KEYS, HAS_POINTER, HAS_PEN, HAS_PAD, HAS_LOCO, HAND_ONLY) | n_keys u8 | reserved u8
keys:    n_keys × (zone u8 | down u8)                         // order of arrival within the frame; no times
pointer: dx f32 | dy f32 | wheel f32 | buttons u8 | 3 reserved // summed over the frame (motion integrates per frame)
pen:     u u8 | v u8 | pressure u8 | contact u8 | tilt i8 × 2 | 2 reserved
pad:     sticks i8 × 4 | triggers u8 × 2 | zones u16
loco:    state u8 | intents u8 | 2 reserved | root x, y, z f32 | yaw f32
```

- **Never stored:** a key code, a chord, a binding name or id, a sub-frame time, a cursor position. Binding events are
  stored only as their effect: an expression or op is already an `Event` record (T1), and a locomotion intent is in the
  `loco` bits.
- **Re-rendering.** Motion is a pure function of these records, the desk rig, the props and the seed (a `motion` live-state provider,
  `register_take_state!`, puts the seed and the motion settings in each `State`), so
  both of T6's modes re-run it; the recorded root keeps the path exact even if bounds changed. A different character
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
2. **Wayland** (§4): PF-29 typing and mouse only through the evdev opt-in. Owner to confirm P0 ships partial there
   (research risk 2).
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
acceptance. Order: IN-1 → (IN-2, MO-1, LO-1 in parallel) → IN-3 → (IN-4 to IN-8, MO-2, MO-3, LO-2, LO-3, LO-4) → ST-1 → ST-2,
ST-3 → ST-4, EV-1. MO-1 and LO-1 need only the contract and the mock, so the motion work never waits on an OS backend.

### IN-1: `omuzaki-input-api`: events, zones, bindings, capture state and the traits (new API crate)

- **Fence:** `crates/omuzaki-input-api/**` (new: `Cargo.toml`, `src/lib.rs`, `src/event.rs`, `src/zones.rs`,
  `src/binding.rs`, `src/capture.rs`, `src/feed.rs`, `tests/zones.rs`, `tests/no_identity.rs`).
- **Contract:** §3 and §5 as written: `KeyZone` and the 16-zone table by HID usage, `ZoneInfo`, `MotionEvent`,
  `PadState`, `PadZones`, `BindingSlot`, `BindingId`, `BindingEvent`, `Edge`, `KeyChord` and `PadChord` (cold),
  `CaptureState`, `CaptureCaps`, `PermissionHow`, `PauseWhy`, `MotionFrame` (`MOTION_FRAME_MAX` 128), `MAX_BINDINGS`
  64, the traits `MotionFeed`, `BindingFeed`, `InputControl`, `Indicator`, `ChordStore`, and `IndicatorLease`. Needs the
  Director's approval as a new contract.
- **Acceptance:** the zone table covers every usage of the ANSI (104), ISO (105) and JIS (109) boards, counted in a test;
  `MotionEvent` is `Copy` and at most 32 bytes (`size_of` test); serde round trip of every cold type; a test that no
  public hot type has a field of a chord type (by `JsonSchema` walk); `#![deny(missing_docs)]`; `lint-deps` and
  `lint-crate-table` accept the crate; check green.

### IN-2: `omuzaki-input-mock`: `FakeInput`, `ReplaySource` and the conformance suite (new mock)

- **Fence:** `crates/omuzaki-input-mock/**` (new).
- **Contract:** IN-1. `FakeInput` (scripted events and pad states on `FakeClock`, implements `MotionFeed`, `BindingFeed`
  and `InputControl`), `ReplaySource::from_text` (the recorded-input text format: one event per line, zone names, times
  in seconds), `conformance::run(&mut dyn MotionFeed, …)`.
- **Acceptance:** replay of a 60 s script is identical over two runs and after a text round trip; `drain` allocates 0
  bytes (counting allocator); `drain(up_to)` never returns an event after `up_to` (proptest); a dropped-event count when
  more than `MOTION_FRAME_MAX` events arrive in one drain; check green.

### IN-3: `omuzaki-input`: the hub, the normaliser, the binding matcher, the gate and the zone-equivalence test (new crate)

- **Fence:** `crates/omuzaki-input/**` except the backend folders: `Cargo.toml` (`wasm = false`, reason "OS input";
  `registers = true`), `src/lib.rs`, `src/hub.rs`, `src/normalise.rs`, `src/matcher.rs`, `src/rings.rs`,
  `src/indicator.rs`, `src/kill_switch.rs`, `src/store.rs`, and empty backend folders each with a `mod.rs` stub
  (`windows`, `macos`, `linux_x11`, `linux_evdev`, `gamepad`) that later tasks fill without touching `lib.rs`;
  `tests/privacy.rs`, `tests/gate.rs`, `benches/normalise.rs` and its `[[bench]]` entry.
- **Contract:** IN-1; ops-api `Scope::Input`; §3 (rules 1 to 5), §4 and §5. A `pub(crate)` `RawSink` the backends call.
- **Acceptance:** the zone-equivalence proptest (§3 rule 5) over every output surface the crate has; capture refuses to
  start without a lease and pauses within 1.1 s of `Indicator::shown()` turning false (`FakeClock`); the binding table
  matches chords with modifiers and reports conflicts; a full ring drops the oldest motion event and counts it, and
  never drops a key-up (a key-up displaces the oldest pointer event first); the forbidden-symbol test of §3 rule 4;
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
- **Contract:** `PadState`, `PadZones`, `PadChord` (IN-1); research §1.1, §1.2, §1.5, §4.
- **Acceptance:** a mapping test from gilrs events to `PadState` and `PadZones` with a fake gilrs event list; dead zone
  and quantisation (i8, u8) exact at the edges; hot-plug of a second pad keeps pad indices stable; the libudev link is
  dynamic (recorded under the LGPL-shared rule); check green on Linux and the Windows cross-build.

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
  §4 and §5 tables: `input.capture.start`, `input.capture.stop`, `input.capture.state`, `input.binding.list`, `input.binding.set_action`,
  `input.binding.remove`. No op takes or returns a chord.
- **Acceptance:** table tests with person, agent and automation actors (an agent's `input.capture.start` is `Denied`
  and never queued; anyone may stop; stopping is idempotent); a test that types a known string through
  `FakeInput` and scans every op result, event and journal record for any key code or character of it (none);
  registrations found by the link gate; check green.

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
  the masked bones with a 0.3 s fade; deterministic; `step` allocates 0 bytes; at most 0.02 ms per frame on `Dev`
  (bench); check green.

### MO-2: `omuzaki-anim`: mouse and pen motion

- **Fence:** `crates/omuzaki-anim/src/input_motion/{pointer.rs,pen.rs}` and their `mod` lines in `input_motion/mod.rs`, `crates/omuzaki-anim/tests/input_motion_pointer.rs`.
- **Contract:** MO-1's `DeviceMotion`; §6.3.
- **Acceptance:** the mouse hand never leaves the pad rectangle over a 60 s random-walk replay; recentre time constant
  1.5 s ± 5 %; a click curls the index finger within one frame; pen contact lowers the palm by pressure × 4 mm ± 0.5 mm;
  the allocator switches hands with the 0.15 s dwell and 0.25 s transit; deterministic, 0 allocations; check green.

### MO-3: `omuzaki-anim`: gamepad motion and stick look

- **Fence:** `crates/omuzaki-anim/src/pad_motion/**` (a new top-level folder, so `input_motion` is not edited),
  `crates/omuzaki-anim/tests/pad_motion.rs`, `crates/omuzaki-anim/library/poses/hold-pad.toml`.
- **Contract:** MO-1's `DeviceMotion`, `PadState` (IN-1), `TwoHandedProp` shape of goals (two palm goals), §6.4.
- **Acceptance:** at three statures, thumb tips within 5 mm of the stick caps at full deflection in 8 directions; full
  right-stick deflection gives 35.0 ± 0.5 degrees of look yaw after settling; with the tracker owning a head channel
  the look goal's weight is 0 after 0.3 s; deterministic, 0 allocations; check green.

### LO-1: `omuzaki-anim`: the locomotion state machine and root motion

- **Fence:** `crates/omuzaki-anim/src/locomotion/{mod.rs,machine.rs,def.rs,intents.rs}`,
  `crates/omuzaki-anim/tests/locomotion.rs`, `crates/omuzaki-anim/benches/locomotion.rs` and its `[[bench]]` entry, the
  `omuzaki-input-api` dependency if MO-1 has not added it.
- **Contract:** `PoseClip` (#223), `PoseLayer`, IN-1's `BindingEvent` and `PadState` for `Intents`; §7.1, §7.3, §7.4:
  `Intents`, `LocomotionDef`, `Locomotion::step(tick, &Intents, &DeskRig, &mut PoseLayer) -> RootStep`. The gait is a
  trait (`Gait`) with an in-place placeholder until LO-2.
- **Acceptance:** a test per allowed state pair, and every other pair refused; no bone jumps more than 2 degrees between
  30 fps frames over a scripted 120 s run; speed respects the 6 m/s² limit; turn rate at most 360 degrees a second;
  `LocomotionDef` validation errors name the field; deterministic, 0 allocations; at most 0.01 ms on `Dev`; check green.

### LO-2: `omuzaki-anim`: the procedural walk and run gait

- **Fence:** `crates/omuzaki-anim/src/locomotion/gait.rs` and its `mod` line in `locomotion/mod.rs`, `crates/omuzaki-anim/tests/locomotion_gait.rs`.
- **Contract:** LO-1's `Gait`; §7.2; foot-contact flags in the shape `FootPlanting::set_contacts` takes.
- **Acceptance:** root distance per cycle within 2 % of stride × cycles; contacts alternate with the double-support
  share given; run has a flight phase; the walk mirrored and shifted half a cycle equals itself within 1e-5; stride and
  cadence at three statures printed beside the cited reference values; check green.

### LO-3: `omuzaki-anim`: the jump, sit and emote clips

- **Fence:** `crates/omuzaki-anim/library/poses/clips/{jump,sit_down,sit_idle,stand_up,cheer,bow,dance}.toml`, the poses
  they key in `crates/omuzaki-anim/library/poses/`, `crates/omuzaki-anim/tests/locomotion_clips.rs`.
- **Contract:** `PoseClip` (#223), the pose library, §7.2 (#751's list without walk and run, which LO-2 makes).
- **Acceptance:** each clip loads, loops where marked with C1 seams, mirrors (`mirrored(mirrored(c)) == c`), and has
  contact flags; the dance loop is 8 beats at 120 bpm; licence notes cite only public biomechanics references; check
  green. Pictures come from EV-1.

### LO-4: `locomotion.*` ops and the motion settings

- **Fence:** `crates/omuzaki-anim/src/locomotion/ops.rs`, `crates/omuzaki-anim/src/input_motion/settings.rs`, their
  `mod` lines in the two folders' `mod.rs`,
  `crates/omuzaki-anim/tests/locomotion_ops.rs`.
- **Contract:** ops-api; the settings registry (`settings.md`); `locomotion.set { character, set }` (`edit:character`),
  `locomotion.state` (`read`), `locomotion.home` (`perform`), and the `input.motion.*` settings keys of §6.5 (User,
  agent `Confirm`).
- **Acceptance:** undo of `locomotion.set` is exact; a set id that does not exist is a typed error; the settings keys
  pass the registry's `tests/settings.rs` checks; check green.

### ST-1: `omuzaki-stage`: input motion in the frame, palm goals in the IK stage

- **Fence:** `crates/omuzaki-stage/src/perform/motion.rs` and its `mod` line in `src/perform/mod.rs` (hand-written),
  `crates/omuzaki-stage/src/frame/rig.rs` (palm goals and foot
  planting in the IK stage), `crates/omuzaki-stage/tests/input_frame.rs`, `crates/omuzaki-stage/benches/input_frame.rs`
  and its `[[bench]]` entry, the `omuzaki-input-api` dependency in `crates/omuzaki-stage/Cargo.toml`. Runtime-area files:
  the Runtime lead signs off.
- **Contract:** `MotionFeed` (IN-1), `InputMotion` and `MotionOut` (MO-1), `HandTarget` and `FootPlanting` (rig), §6.1.
- **Acceptance:** a 60 s replay (typing, mouse) on the mock character gives equal frame digests over two runs; the
  stage's zero-allocation frame test still passes; drain honours the A/V delay (events at `t` act at `t + D`); added
  CPU at most 0.08 ms per frame on `Dev` (drain, motion and palm IK, §10; bench, `scaled`); check green.

### ST-2: `omuzaki-stage`: locomotion, the live root and camera follow

- **Fence:** `crates/omuzaki-stage/src/perform/locomotion.rs` and its `mod` line in `src/perform/mod.rs`; the place
  where the stage builds each member's `AnchorFrame` (`omuzaki-stage-api` `resolve.rs`: `root` is the member's position
  and yaw), which takes the live root instead of the authored one; `crates/omuzaki-stage/tests/locomotion_frame.rs`. A follow-lag field on `CameraDef` is an `omuzaki-stage-api` change:
  it needs the Director's approval on the task before work starts, and lands as its own PR first if approved.
- **Contract:** `Locomotion`, `RootStep` (LO-1), `Intents` from `BindingFeed` and `PadState`, §7.3.
- **Acceptance:** added CPU at most 0.02 ms per frame on `Dev` (§10); the root stays inside the walk radius over a
  120 s random-intent run; `locomotion.home` returns within
  0.5 s ± one frame; a `Subject`-framed camera follows with the configured lag; deterministic; check green.

### ST-3: `omuzaki-stage`: the input device props

- **Fence:** `crates/omuzaki-stage/src/input_props/**` (a new top-level folder, declared by the build script),
  `crates/omuzaki-stage/tests/input_props.rs`.
- **Contract:** #705's `PropKind` and `register_prop_kind!`; §8's kinds and anchors.
- **Acceptance:** each kind builds a mesh under 2,000 triangles with its anchors; the default placement puts the key
  plane within reach (every zone centre inside 95 % of arm reach) at three statures; check green.

### ST-4: the take's `Motion` record

- **Fence:** `crates/omuzaki-take-api/src/record.rs` (the new kind and its layout, minor version bump; a contract change
  that needs the Director's approval), `crates/omuzaki-stage/src/take/motion.rs`, `crates/omuzaki-stage/tests/take_motion.rs`.
- **Contract:** takes.md T2, T3 and T5; §9.
- **Acceptance:** a take recorded during a 120 s replay contains no key code (scan for every usage of the typed string),
  re-renders to the same frames, and an older reader skips the record and counts it; check green.

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
