---
title: "0060: One OS input source for motion, bindings and triggers, with key identities confined to the capture thread (2026-10-15)"
summary: "One OS input source for motion, bindings and triggers, with key identities confined to the capture thread"
area: decisions
---

# 0060: One OS input source for motion, bindings and triggers, with key identities confined to the capture thread (2026-10-15)

**Status:** proposed (#742); accepted by the Director when merged. **Context:** PF-29 (#718), PF-30 (#764), PF-05
(#469), PF-25 (#478); `docs/design/input-motion.md`; `docs/research/2026-10-os-input-capture.md`.

## Context

PF-29 moves the avatar's hands from typing, mouse, pen and gamepad input while a game has focus; PF-30 drives
locomotion from keys and sticks; PF-05 and PF-25 bind keys and buttons to actions. All four need the same OS input,
which no crate captures today. Reading another app's keystrokes is the most sensitive thing the studio can do: on
Windows and X11 the OS tells the user nothing, and one credible "Omuzaki is a keylogger" report would cost more than
the feature. The spec forbids game hooks and injection, and gates the capture behind the `input` scope, which
`omuzaki-ops-api` already defines.

## Decision

1. **One source, three crates.** `omuzaki-input-api` (contract), `omuzaki-input-mock` (mock) and `omuzaki-input`
   (implementation, every OS backend in one crate, `wasm = false`). It is not part of `omuzaki-track-api`: trackers give
   newest-wins frames with no identity; input gives edge events across a privacy boundary and pulls OS dependencies no
   tracker consumer should compile.
2. **Key identities are confined to one crate.** The raw key type, the map from HID usage to zone, every chord and
   every per-key record are `pub(crate)` in `omuzaki-input`. What leaves it is a zone of a fixed grid (sixteen cells by
   physical position, plus two side-only values for hand-only detail) for motion, and a resolved binding hit (its
   stable `BindingId` and action) for a configured binding. No public type, op result, event, take record, log or MCP
   output holds a key code, a chord or a usage; the one stated exception is `ChordLabel`, a configured chord's display
   text that only the binding editor receives, with no serde and no `Display`. Mouse input is relative motion, never a
   screen position; pen position is coarsened to a 32 x 32 grid. A property test proves two key streams that differ
   only inside zones give byte-identical output on every surface, and capture code never puts a key value in a panic
   message.
3. **Rings, epochs and holders keep held state honest.** Backends register themselves and are known to the contract
   only by an opaque `BackendId`; each owns its rings (one producer each), consumers drain them in turn, and takes and
   replays record the drained order. Every entry carries its device's `SourceTag` and two epoch stamps (capture and
   backend); a pause, a stop or a backend exiting closes its epoch, so nothing read after it reaches a consumer, and
   consumers release what was held; one device leaving ends only that device's state. A key's release ends what its
   press started, whatever the table, chord or modifiers say by then; an action held by several sources ends only when
   the last lets go; a full ring keeps releases, and per-source snapshots reconcile after any drop. The control thread,
   which runs the indicator check and the kill switch, never waits on a consumer, a backend or a surface.
4. **Chords stay out of the executor.** Binding chords live in a machine-local store held by the desktop UI and never
   inserted into `Services`; ops, events, MCP and agents see binding ids, names and actions only. Learning a chord is a UI
   call, not an op, and reads OS input only while capture is on with its indicator shown.
5. **Capture is a person's act, shown while it runs.** `input.capture.start` needs scope `input` and is `person_only`;
   `input.capture.pause` and `input.capture.stop` are registered with `read`, so any actor can turn capture off. A
   surface installs its indicator with the hub (a hub API, never an op or a service) and writes a heartbeat the hub only
   reads; start fails with `NoIndicator` unless it is shown (an installed indicator is visible in every state, `Off` included), capture pauses when it stops
   showing, and capture is never restored at start. State is kept per backend: capture stays `On` while any backend
   runs, with the others listed as degraded, and is `Failed` only when none runs. No new scope: `input` already exists. The
   protected-program kill switch checks whether named programs run, which `Scope::Process` governs: its list is set
   only with `process` (or by the person in Settings), and the pause reason is shown only to actors with `process`.
6. **Motion is pose layers and IK goals after the animator and before the rig** (spec §8 order: humanoid, IK, node
   constraints, springs), masked by the bones a tracker owns, so a tracked arm wins with a 0.3 s hand-over. No
   `omuzaki-anim-api` change: palm orientation goes through `omuzaki-rig`'s hand targets in the stage.
7. **Takes store what the video showed:** zone and button edges, coarse pointer and pen, pad state, locomotion intents
   and the root, at frame resolution, in a new additive `Motion` record (a minor version of the take format, ADR 0052),
   with a complete motion checkpoint at each group start so a replay clip can start at any group.
8. **Locomotion sets are library items that materialise** (ADR 0019 §4): applying one writes its values and its clips'
   poses into the character with a pinned `applied/<app>` record; the runtime never reads the library.

## Consequences

- PF-05 and PF-25 consume `BindingFeed` from `omuzaki-input` and add no second capture path.
- A zone stream still carries information (eight letter groups resemble a phone keypad), so zones are personal data:
  memory only, frame resolution in takes, never in logs, events or MCP; a hand-only mode removes them.
- Wayland gets typing and pointer motion only through the evdev opt-in, and bindings through the GlobalShortcuts portal
  once its task lands; until then the Wayland default is gamepad only.
- Adding a backend is a folder in `omuzaki-input`; adding a device motion is a type implementing `DeviceMotion` in
  `omuzaki-anim`; neither edits a shared list.
- The `input.*` op namespace is shared with #301's tracker-input ops; this decision's ops are `input.capture.*` and
  `input.binding.*`, and there is no bare `input.bind`.

## Alternatives rejected

- **Global low-level hooks** (`WH_KEYBOARD_LL`, rdev): in every app's input path and the keylogger's shape (research §1.1).
- **Absolute pointer positions:** clicks at screen positions can spell keys on an on-screen keyboard.
- **Chords as a settings key:** settings values are listed to agents and appear in events.
- **A `perform`-only gate:** an agent driving the avatar must never imply reading other apps' input.
