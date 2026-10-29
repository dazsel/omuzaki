---
title: "Drivers: who moves a character live, the live score, and one performer per cast member"
summary: "the driver model: four driver kinds on the face's blend layers, per-member drive maps on the cast, the live score that agents and scripts play acts on (act.*), and one performer per performing member."
area: design
---

# Drivers: who moves a character live, the live score, and one performer per cast member

**Status:** built (2026-10-23), the first slice. **Owner:** App core, with Render reviewing the stage side (#2 comment
6365689184). **Code:** `omuzaki-stage-api` `drive`, `omuzaki-stage` `perform::{drive, score, cast, host, live}`,
`ops::{act, drive}`. **Builds on:** the face's blend (`anim-api.md`, `LayerStack`, PF-23 ownership), the perform loop
(PF-24), the AI performer's intents (`ai-performer.md` §7).

## 1. What this is for

The product mixes the same few blocks in every use: a person on a webcam, an AI co-host, a person with an agent
helping, an AI-only stream, two characters on one stage. Each needs to say, for each character, **what moves it**.
Before this, a character had one performer driven by one tracker, triggers and a voice, and the only way an agent
could move it live was pressing a key a person had bound. This design adds three things and changes nothing that
worked:

1. **The live score**: anything that can run an op plays *acts* (an expression, a look, channels, a body clip) on the
   score layer, through `act.*`.
2. **The drive map**: per cast member, per group of channels, how strongly each kind of driver may move it.
3. **One performer per performing member**, each with its own drivers, reached by member id.

## 2. Drivers

A driver is anything that moves a character live. There are four kinds, one per layer of the face's blend, so a
driver never needs a new layer or a new `Source` (Character's `anim-api` is unchanged):

| Kind (`DriverKind`) | Layer (`Source`) | What feeds it |
|---|---|---|
| `score` | `Score` | agents over MCP, scripts, shows' automations, the AI performer (`act.*`); later a recorded take |
| `tracker` | `Tracker` | a webcam, a phone (ARKit), VMC, a replayed capture: any `FrameSource` |
| `voice` | `Audio` | the microphone or a synthesised voice, through the lip sync |
| `keys` | `Trigger` | hotkeys, the trigger strip, redeems: `trigger.press` |

The order is the blend's: `keys` over `voice` over `tracker` over `score` over the rest face, with the life layer
riding on top of whatever no person drives. PF-23 stays the rule inside each performer: where a webcam reads the face,
it wins over an agent's act. The drive map is how a person changes that.

## 3. The live score

`perform::score::LiveScore` holds up to 16 acts per performer. Each act is a target and an envelope:

- **Target.** Channels to absolute values (`Motion::Channels`: an expression baked over the rest face, a look, named
  channels) or a body clip (`Motion::Clip`, from the clip library, sampled from its start).
- **Envelope.** Fade in from the rest face, hold for `hold` seconds or until released, fade out. A release fades from
  the strength reached, never from full. Strengths are eased (smoothstep).
- **Lanes.** `face`, `look`, `channels`, `body`. A new act releases the earlier acts of its lane that move any of the
  same channels or bones, so a second expression takes over from the first (a cross-fade), while a look and an
  expression play together.

Once per performer step, before the blend, the control writes the score layer: the rest face, then every act in start
order, each lerped in by its strength over its channels (a later act wins where they overlap). The layer is an override
at full weight over the union of the acts' channels, absent when nothing plays. The clips go into the score's pose
layers, which the performer lays on the base pose **before** the animator, so the tracked head, the lean and the breath
still move a waving body.

**Over tracking** (#2409). An act made with `over_tracking: true` (`act.expression`, `act.channels`) is left out of the
score layer and written to the score's `OverLayer`: per channel, the value it pulls toward and how far, composed in start
order. The performer applies it after the blend and the life layer, before the grammar, so the act wins on its
channels over the tracker, lip sync and the triggers (eyes forced shut while the webcam runs), and fades back to them.

The life layer chases the acted face and rides on it (`omuzaki-anim` `life`): an AI character still blinks and its smile
still breathes. It relabels the channels it moves as `Life`, as it always has.

The handle bakes each act off the perform thread (`PerformHost::act`): an expression against the character's
`ExpressionSet` and rest face, a look into the eight `eyeLook*` channels plus head yaw and pitch, channels held to their
ranges, a clip from `ClipLibrary::builtin()` (borrowed for `'static`, so the perform thread frees nothing when an act
ends). The perform thread only blends: **a step allocates, locks and frees nothing** with acts playing, a gated drive
map and a second member (`tests/live_acts.rs`, `tests/live.rs`).

## 4. The drive map and one performer per member

**Stored on the cast member,** `cast/<member>.toml`:

```toml
[drive]
gaze = { tracker = 0.0 }    # the eyes look where an agent says, never the webcam
head = { tracker = 0.5 }    # the webcam moves the head half as much
```

Groups (`DriveGroup`) are the regions the blend already asks ownership questions about: `brows`, `eyes` (lids),
`gaze`, `mouth`, `head`, `body`, `extras`, `custom`. A group or a kind the map leaves out drives at full strength, so an
empty map is the behaviour before this design, exactly (`perform::drive::resolve` takes `LayerStack::resolve`'s path
when the gate is open, and a test pins the two equal). With a weight below 1, each layer mixes in group by group at its
weight times the map's, and owns a group's channels only while that still reaches the ownership threshold (0.5). So a
webcam gated off the head leaves it to an agent's `act.look`, or to the life layer when nothing else drives it.

**One performer per member.** `LiveStage` runs the subject's performer, which drives the stage, and a performer for each
member added with `add_member`: its own source (or none), triggers, voice, live score and drive map, stepped on the same
clock in the same frame. Each step's pose and channels become the member's next frame's input (`member_input`), and the
loop hands it to the stage (`FrameLoop::set_member_input`): Render's `CastStage` draws every member in one frame with
its own input, and a member's performer bakes its triggers against that member's own expressions
(`FrameLoop::trigger_expressions_of`). A single-body stage draws no other member and ignores both. `CastBook`
(`LiveCast`) names every performing member's host, the subject first, for the ops.

The studio today runs one performer and serves it as `PerformHost` with no cast; the ops treat it as the subject, so
every `act.*` works in the studio and over MCP now.

## 5. The ops

All are registered in `omuzaki-stage` and reach a performer through `LiveCast` (by `member`) or `PerformHost` (the
subject, when `member` is left out).

| Op | Kind | What it does |
|---|---|---|
| `act.expression { expression, intensity?, hold_s?, held?, fade_s?, over_tracking?, member? }` | live action | shows one of the character's expressions for a while or until released; times left out come from its `playback` (its fades, `0` a hard cut, and its `auto_stop_s` as the hold, else 2 s) |
| `act.look { at? \| x?, y?, head?, … }` | live action | points the eyes (`camera`, `left`, `right`, `up`, `down`, or `x`/`y` in -1..1, `x` toward the picture's right), the head following by `head` (default 0.35) |
| `act.channels { channels: {name: value}, over_tracking?, … }` | live action | sets channels by name (ARKit, head, body, extras), each held to its range |
| `act.clip { clip, weight?, … }` | live action | plays a body clip; a one-shot clip ends with itself, a looping one plays for `hold_s` (default 2) or until released |
| `act.release { act?, member? }` | live action | fades one act, or every act, out |
| `act.list { member? }` | query | the acts playing (with ids and envelopes), the expressions and clips, who performs |
| `drive.set { scene?, member, group, driver, weight }` | edit | changes the member's drive map in the scene, and, when it performs, its performer's at once |
| `drive.list { scene?, member? }` | query | each member's map as saved and as performing |
| `drive.sync { scene?, member, drive }` | edit, no undo | hands a map to a performing member without touching the scene: what undo and redo of `drive.set` run |

Live actions are like `trigger.press`: scope `perform`, logged, `Undo::None`, an empty patch; the act fades by itself or
on `act.release`. Names are checked before anything plays, with the nearest name in the hint; a dry run checks and plays
nothing.

`drive.set` is `Undo::Explicit`: its patch is undone like any scene edit, and its effect (`drive.sync` with the map
before) puts the live performer back, so an undo reaches both. A member that is not performing has nothing live to
undo; the saved map is what it starts from.

## 6. Contracts

- `omuzaki-stage-api::drive` (`DriveGroup`, `DriverKind`, `DriveMap`) and `CastMember::drive`: additive, Render's
  crate, reviewed by Render.
- `PerformHost` gains `act`, `release_act`, `acts`, `set_drive` and `drive`, each with a default that refuses
  (`Unsupported`), so every other implementation keeps compiling; `PerformError` gains `UnknownClip`.
- `Performer` gains `score_mut`, `set_drive`, `drive` and `base`; `PerformFrame` gains `advance`; `LiveStage` gains the
  member calls and `cast`; `FrameLoop` gains `set_member_input` and `trigger_expressions_of`, defaulted for a single
  body and implemented by `CastStage`. Nothing changes for a caller that uses none of them.

## 7. Next

- **The studio runs the cast**: its live slice builds a `CastStage` from the scene's visible cast, adds a performer per
  member and serves `CastBook`, and each member starts from its saved drive map.
- **`act.speak`** on Tracking's voice services, per member (today `performer.say` speaks for the co-host).
- **Clips from the project's library** beside the built-in ones, and each character's own hand rest for its clips.
- **Recorded takes as a score driver**, and the **conversation driver** (listen, any LLM or outside brain, speak and
  emote) built on these acts.
