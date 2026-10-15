# `omuzaki-anim-api`: the animation and rig contract

**Status:** reviewed; decisions are in §15. **Crates:** `crates/omuzaki-anim-api`, `crates/omuzaki-anim-mock`.
**Spec:** v3 §8 (runtime model), §20 (rig, physics, motion), §22 (perform), §39.1 rules 1 and 6.
**Reference implementation:** the JS prototype, `arkit.js` and `life.js` (its character code) and
`js/perform/{perform,score}.js`, plus the normalised tracker frame of the bridge.

## 1. What this contract is for

Between "a tracker produced numbers" and "a posed skeleton and a set of face channels go to the renderer", ten
different features have to agree on one set of data: webcam and phone trackers, calibration, retarget, lip sync,
scores, hotkeys, the life layer, the rig, spring bones, and the face painter. This crate is that agreement.

It does three things:

1. **Fixes the real-time data.** One fixed-size channel buffer, one normalised tracker frame, one pose type. All of them
   are `Copy` or fixed arrays, so a frame allocates nothing (spec §8, §39.3 standard 2).
2. **Fixes the order.** Blending order, who owns a channel, and what the life layer may and may not touch are
   decided here once, in types, instead of being re-derived by each stage.
3. **Gives each parallel stream a trait to build against and a mock to test against** (rule 6): retarget, life layer,
   animator, rig, spring solver, calibrator, frame source.

What it deliberately does **not** do: implement the algorithms (One-Euro filtering, spring physics, IK, the full life
layer). Those are `omuzaki-anim` and `omuzaki-rig` tasks. The API has only the pure helpers that pin a meaning (range
normalisation, blend resolution, score sampling, forward kinematics, skeleton validation).

## 2. The pipeline

Spec §8 fixes `input → track → retarget → life → anim blend → IK → physics → face paint → render`. The contract's
arrows, left to right:

```text
 FrameSource ─▶ Retarget ─▶ ┐
 CompiledScore::sample ───▶ │ LayerStack::resolve ─▶ Bus ─▶ LifeLayer ─▶ (grammar) ─▶ ChannelBuf ─▶ face painter
 lip sync / triggers ─────▶ ┘  rest, score, tracker,          │
                               audio, trigger                 └▶ Animator ─▶ HumanoidPose ─▶ Rig ─▶ Pose ─▶ renderer
                                                                                              IK, SpringSolver
```

| Stage | Contract | Hot? | Owner of the implementation |
|---|---|---|---|
| input | `FrameSource::poll` | yes | `omuzaki-track-*` |
| retarget | `Retarget::apply` | yes | `omuzaki-anim` |
| score | `CompiledScore::sample` | yes (API implements it) | API |
| blend | `LayerStack::resolve` | yes (API implements it) | API |
| life | `LifeLayer::step` | yes | `omuzaki-anim` |
| grammar (FE-05) | a `Bus -> Bus` stage, same shape as `LifeLayer` | yes | `omuzaki-anim` (stream S9); not defined here |
| anim blend | `Animator::animate` | yes | `omuzaki-anim` / `omuzaki-rig` (pose library) |
| IK, physics | `Rig::solve` (uses `SpringSolver`) | yes | `omuzaki-rig` |
| calibration | `Calibrator` | no (cold) | `omuzaki-anim` |

**Hot versus cold.** Hot types are `ChannelBuf` (512 B), `ChannelMask` (16 B), `Layer` (544 B), `Bus` (608 B),
`LayerStack` (2.7 KB), `TrackFrame` (304 B), `Calibration` (448 B), `HumanoidPose` (896 B), `Tick`, `Cues`. All are
`Copy`. `Pose` is a fixed 16 KiB array (512 bones) and is passed by reference only. Cold types (configs, scores,
skeletons, spring sets) serialise to the project file, derive `JsonSchema` for ops, and may allocate. A test pins that
the project TOML (spec §7) round-trips the main cold types: score, calibration, retarget and life settings, skeleton and
spring set (`tests/at_rest_toml.rs`).

## 3. The channel table

Everything that drives a face or a head is a **channel**: an index into a table fixed in this crate. A producer writes
channels, a consumer reads them, and **an index, once assigned, never changes meaning**, which is what keeps recorded
tracks, scores and golden renders valid across versions.

| Index | What | Count |
|---|---|---|
| 0 to 51 | the 52 ARKit blendshapes, in Apple's alphabetical order (`browDownLeft` to `tongueOut`) | 52 |
| 52 to 59 | `headYaw`, `headPitch`, `headRoll`, `headShiftX`, `headShiftY`, `lean`, `bodyTurn`, `breath` | 8 |
| 60 to 72 | anime extras (FE-02): `eyeArcLeft`, `eyeArcRight`, `eyesHeart`, `eyesStar`, `eyesGloom`, `eyesShock`, `eyesSqueeze`, `eyesSpiral`, `sparkle`, `tears`, `sweatDrop`, `blush`, `fangs` | 13 |
| 73 to 111 | reserved (finger curls, more extras) | 39 |
| 112 to 127 | `custom00` to `custom15`: character-specific toggles that scores and triggers can drive | 16 |

- **Names** are camelCase with `Left`/`Right` suffixes for the character's own sides (ARKit's convention). Files and
  tools use names; only the hot path uses indices. `Channel` serialises as its name, and its JSON Schema lists the
  valid names, so agents see them.
- **Units:** weights `0..1`, angles **radians**, lengths metres. Each channel has a legal range; `ChannelBuf::sanitize`
  clamps to it and replaces NaN, as the last step before a buffer leaves the pipeline.
- **Regions** (`Brows`, `Eyes`, `Gaze`, `Mouth`, `Head`, `Body`, `Extras`, `Custom`) are masks (`ChannelMask::EYES`,
  and so on). Ownership questions are asked per region ("does a human drive the eyes?").
- **Mirroring (PF-27):** each channel is `None`, `Swap(partner)` or `Negate` (yaw, roll, shift-x, body turn).
  `ChannelBuf::mirror` and `ChannelMask::mirrored` implement it.
- **Gaze** is the eight `eyeLook*` channels. `ChannelBuf::gaze`/`set_gaze` convert to and from the prototype's "look
  units" (`x` positive toward the character's left, `y` up, 1.0 = full-weight channel).
- **Axes and signs.** Right-handed, +Y up, a character faces +Z (VRM 1.0), so its left is +X. Head angles are YXZ
  Euler: positive yaw turns toward the character's left, positive pitch nods down, positive roll tilts toward its
  right shoulder. The prototype's signs are exactly this. A performer facing the camera maps straight through
  (yaw "nose toward image right", pitch "nose down", roll "counter-clockwise in the image"); mirrored previews flip
  yaw and roll.

**Why ARKit-native instead of the prototype's semantic params.** The prototype's bus carried the painter's own
parameters (`open`, `mSmile`, `browY`, `mWide`, ...), and the retarget ran ARKit through formulas into them
(`arkit.js` lines 140 to 190). Spec FE-01 says every character speaks ARKit-52 natively, so the contract carries the
52 channels end to end. The consequences: life, scores, lip sync and recordings all work on one vocabulary; exported VRMs
get "perfect sync" for free (FE-09); and the **formulas move to the face painter** (`omuzaki-face`: how `mouthSmile`,
`mouthFrown` and `mouthDimple` combine into a smile curve is part of a face profile, not of animation). The port map in
§11 shows where each prototype parameter went.

## 4. Frame data

`TrackFrame` is the Rust form of the bridge's "normalised frame (contract v1)". Whatever the source, the pipeline sees
the same 304-byte `Copy` value.

| Bridge field (the normalised tracker frame) | `TrackFrame` | Notes |
|---|---|---|
| `v`, `source` | `profile: DeviceProfile` | `Arkit` (iPhone and the apps that relay it) or `Webcam` (reads low); the tracker crate maps its protocol name |
| `recv`, `t` (ms) | `time: Seconds`, `seq: u64` | the receiver maps the sender's clock onto the pipeline clock and numbers the frames |
| `found` | `status: Tracking / Lost` | |
| `bs` (only channels present) | `blend: [f32; 52]` + `present: ChannelMask` | webcam models send 51; an absent channel is never "measured zero" |
| `head` (rad) | `head: Option<HeadPose>` | same sign convention as the bridge |
| `headPos` (m) | `head.position` | `x` image right, `y` up, `z` toward the camera |
| `eyes` (rad) | `eyes: Option<EyeAngles>` | for sources that send angles instead of `eyeLook*` |

Trackers convert their sender's conventions **before** building a frame (the prototype's `headFromMatrix` becomes a
tracker responsibility), so nothing downstream knows the protocol.

`FrameSource::poll(now)` is the input port: the newest frame since the last call, or `None`; it never blocks and never
queues (spec §8: "newest frame wins"). The lock-free mailbox behind it (a triple buffer) is the tracker crate's job;
this crate's part is making the payload plain data. At rest, a frame serialises as `{seq, time, profile, status,
blend: {name: value}, head, eyes}` with only present channels, readable by agents and diffable in `takes/<n>.track`.

## 5. Retarget (PF-20 to PF-22, FE-06)

**One mechanism instead of three.** The prototype had a per-channel neutral map, a hand-tuned gain table (`GAIN`) and
per-user ranges (`rangeFromSamples`), applied in two different code paths. The contract keeps only the range: every ARKit
channel has `ChannelRange { lo, hi }` mapping raw tracker values to `0..1`. `lo` is the performer's rest value. A webcam's
missing amplitude is a smaller `hi - lo` (the old "gain 2.0" is `hi = 0.5`). An uncalibrated user gets
`Calibration::from_gains(profile, ..)`; the guided routine measures both ends. "Calibrate neutral" is
`Calibration::rest_at(frame)`, which moves `lo` and keeps the span, as the prototype did.

Per channel, in this order (documented in `retarget.rs`):

1. mirror (`RetargetConfig::mirror`)
2. range normalise (`ChannelRange::normalize`, span floored at `MIN_SPAN = 0.05`)
3. `Response`: dead zone, saturation, gamma, invert (PF-22); blinks have a built-in dead zone, as in the prototype: 0.28 to 0.8
   while the channel is still on its default (gain-derived) range, 0.25 to 0.85 once it has a fitted one
4. exaggeration on the `EXAGGERATED` channels (22 of them, the prototype's list): `v^(1/amount)` (FE-06)
5. One-Euro smoothing, `OneEuroParams::for_smoothing(dial, fast)` reproducing the prototype's cutoffs; `FAST` (11
   channels: blinks, gaze, jaw open) get the fast family (PF-21)

Then head: `(frame - calibration.head_rest) * head_gain` into `headYaw/Pitch/Roll`, shift into `headShiftX/Y`, and lean
(`lean_per_metre` = 90 degrees per metre, the prototype's 0.9 degrees per cm) from movement toward the camera. Each head
channel is clamped to its range in the channel table (the prototype's were unbounded; a yaw of 3 rad at gain 1.4 stays 1.75).

**Losing the face (PF-23).** `LostPolicy { grace_s: 0.25, fade_s: 0.5 }`: lost frames inside the grace period change
nothing; after it, values ease to rest and the layer weight fades to 0, which hands ownership to the life layer; it
fades back in when the face returns. The prototype did the easing through its filters; the contract states the
behaviour and leaves the mechanism to the implementation.

**Calibration (PF-20).** `CalibrationPrompt::ROUTINE` is the 13 prompts (38 seconds). `Calibrator::push/finish` fits
`lo` = median at rest, `hi` = 95th percentile per prompt then the largest over prompts (a channel fires only in its own
prompt), `hi >= lo + MIN_FIT_SPAN (0.15)`; the first half second of each prompt is dropped; prompts with fewer than 5
tracked frames are reported as skipped. `omuzaki-anim-mock` implements it, as a reference for the real one.

## 6. Blending order and ownership (PF-23)

`LayerStack` is a struct, not an array, so the order is part of the type, lowest first:

| Layer | Source | Default mode | Typical content |
|---|---|---|---|
| `rest` | (base) | | the character's resting expression |
| `score` | `Source::Score` | Override | a timeline, or AI intents compiled into cues |
| `tracker` | `Source::Tracker` | Override | the retargeted human performance |
| `audio` | `Source::Audio` | **Max** | lip sync: opens the mouth if either the camera or the audio says so (FE-08) |
| `trigger` | `Source::Trigger` | Override | hotkeys and timed expressions: a held "smug" face wins |

Each `Layer` is `{values, mask, weight, mode}`: the mask says which channels the source has an opinion on, so "what does
this source drive right now?" is a single value. `LayerStack::resolve(&self, &mut Bus)` is **stateless and pure**:
`Override` lerps toward the value by weight, `Add` adds `weight * value`, `Max` lerps toward `max`.

**Ownership is derived, not configured.** A layer *owns* its channels when it is `Override`, its weight is at least
`OWN_THRESHOLD` (0.5) and the mask is non-empty; the highest owning layer wins. `Max` and `Add` layers refine but never
own. `Ownership` holds one mask per source (always a partition of the table), so "does a human drive the eyes?" is
`owners.human_owns_any(ChannelMask::EYES)`. `Source::is_human` is `Tracker | Trigger`.

**Hand-off is done by weights**, not by state in the blender. A source that is going away fades its own weight
(`Fade::toward`), so `resolve` stays a pure function that can be tested, scrubbed and benchmarked on its own; ownership
changes at the half-way point of the fade.

`Source::Life` exists for the health meter and the UI ("who owns this channel"): the life layer relabels channels it
changes. It is not a layer; life edits the bus in place.

### 6.1 The A/V delay: `PlayoutDelay` and `DelayRamp` (PF-12)

A voice changer makes the heard voice late, so the avatar's performance channels are held back by the same amount
(ADR 0018 decision 7, `docs/design/audio-api.md` section 5.2). `omuzaki-stage` keeps one `PlayoutDelay<64>` per layer
source and samples each at `now - D` **before** `LayerStack::resolve`; `D` is **one number for the whole stream**.

- **`PlayoutDelay<N>`** is a ring of `(stamp, Layer)`, all `Copy`, no allocation (64 slots hold 500 ms at 120 Hz;
  about 35 KB, so keep it in a field, not on a small stack). `push` refuses a stamp that is not finite or not after the
  newest. A full ring drops its oldest sample.
- **`sample(now, delay)`** is `None` before the first stamp. At a stamp it returns that sample exactly. Between two it
  interpolates the channels both drive, and the weight, linearly; the mask and mode are the earlier sample's. After the
  newest it holds the newest. A negative or NaN delay counts as zero, so a sample never comes from after `now - delay`.
- **`DelayRamp`** moves the applied delay to a new target at a constant rate, the playout running at
  most 10% faster or slower than real time, so a change of Δ takes `Δ / 0.1` s with no minimum (20 ms takes 0.2 s, 200 ms
  takes 2 s; `with_slew(.., 0.4)` makes 200 ms take 0.5 s). The playout position never jumps and never reverses.
- **What differs per source is the stamp:** tracker frames carry capture time; hotkeys and triggers the time they
  fired; audio-derived layers `perf_time`; AI-driven layers `program_time - D`, so they land on time with the AI voice
  after the one delay. UI feedback and everything outside the avatar (scene, OBS, overlays) is never delayed.
- **Cost** (release, `cargo bench -p omuzaki-anim-api`, full 89-channel layers): `push` into a full ring 12 to 17 ns;
  `sample` at a stamp about 23 ns; `sample` between stamps 28 to 38 ns (a whole-buffer `ChannelBuf::lerp_all`; 138 ns
  with the per-channel `lerp_masked` it replaced).

## 7. The life layer (PF-24)

`LifeLayer::step(tick, cues, &mut bus)`. The contract is about determinism and ownership:

1. **No time, no change.** A zero, negative or non-finite `dt` leaves the bus untouched; `dt` above 0.25 s counts as
   0.25 s.
2. **The incoming bus is the target.** Springs chase the values already in the bus; blinks, gaze and noise ride on top.
   The result is `incoming + amount * (processed - incoming)`.
3. **Humans are never overridden.** A channel in `Ownership::human_mask` is not written. Behaviours that need a whole
   region free are skipped while a human drives any channel of it, and the matching springs snap to the incoming values
   so hand-back has no jump:

   | Region free | Behaviours that need it |
   |---|---|
   | `EYES` | blinks, lid springs, laugh squint and ^^ arcs |
   | `GAZE` | micro-saccades, glances, fixation drift, eyes leading a head turn |
   | `BROWS` | brow kicks on stressed syllables, brow noise |
   | `MOUTH` | mouth springs, mouth-corner noise, smile blush |
   | `HEAD` | head noise, speech nods, laugh bob, lean |

   (The prototype lumped brows with eyes; per-channel ownership lets a tracker that supplies only some channels leave
   the rest to life.)
4. **Seeded.** `LifeStreams::new(seed)` forks four independent `Rng` streams (blink, gaze, head, traits) in a fixed
   order, so adding a draw to one never shifts another; `LifeTraits::draw` gives the seed its fixed quirks (which side
   leads, which lid lags). Stream ids are constants in `life::stream` and are part of the determinism contract.
5. **Measurable.** Every number is a field of `LifeParams`, defaults converted from `life.js` (radians, seconds):
   blink gaps (floor 0.8 s, Erlang-3 scale 1.6 s, cap 6 s, 12% doubles, close 60 to 80 ms, open 120 to 180 ms),
   saccades (0.25 to 1.2 s apart, 0.03 to 0.08 look units), glances (5 to 12 s apart), head noise (1.6, 1.2, 1.3
   degrees), spring feel per family (eye 12 Hz/0.9, mouth 8/0.75, brow 5/0.7, head 2.5/0.55), and so on. `LifeSystems`
   switches behaviours on and off (FE-05: each rule can be toggled).
6. **Personalities** (energetic, calm, shy, sleepy, plus `Natural`) are `LifeParams::for_personality`: multipliers on five
   shared knobs plus one or two per preset. They are first guesses to be tuned against the liveliness metrics (spec §15); tuning them is open work.

`Cues { loudness, onset, laugh }` is what audio hands the layer (PF-08).

## 8. Skeleton and rig (RG-01, RG-06)

**Two levels, on purpose.**

- `HumanoidPose` is what **animation produces**: one rotation per VRM humanoid bone plus a hips offset, in the
  *normalised space of VRM Animation* (parent-local, identity = the VRM T-pose). A pose in that space means the same
  thing on every character, which is what makes clips, mirrored poses (`HumanoidPose::mirrored`), motion import (RG-08)
  and the AI's intents portable. Blending is `HumanoidPose::lerp` (slerp; exact at weight 0 and 1).
- `Pose` is what **the rig solves on one skeleton**: a local transform for every bone, including hair, cloth and
  accessory bones that animation never touches. `Skeleton::world_pose` is the one forward-kinematics pass (parents come
  first), shared by spring bones, IK and skinning; `Skeleton::inverse_bind` and `Skeleton::skin_matrices` turn it into the
  joint matrices the renderer takes (§12).

**Rest and bind (decision 0014).** A skeleton rests in the VRM T-pose, and its mesh may have been sculpted in another pose
(an A-pose), the *bind*. The inverse bind matrices are therefore data, not a function of the rest:
`Skeleton::inverse_bind_for(&Pose)` gives them for any local bind pose (cold), and `Skeleton::inverse_bind()` is
`inverse_bind_for(&rest_pose())`, bit for bit what it has always been. In `omuzaki-rig`, `RigSkeleton::new` binds at rest and
`RigSkeleton::with_inverse_bind(skeleton, matrices)` takes the matrices as given (one per bone, all finite, else
`InverseBindError`), so posing the bones at the bind reproduces the mesh and posing them at rest turns the arms to the T. A
character's bind pose is a `HumanoidPose` (`CharacterRig::bind`, `char-face-api.md`); `HumanoidPose` serialises sparsely, only
the bones it turns, with `{}` the T-pose.

`HumanoidBone` has all 55 VRM 1.0 bones with names, parents, required flags (15) and mirror partners, generated from one
table. `Skeleton::new` validates: at most `MAX_BONES` (512), parents before children, unique names, finite unit-rotation
rest transforms, unique humanoid roles, the 15 required bones, and the VRM hierarchy (a humanoid bone's nearest humanoid
ancestor must be the one the spec prescribes, skipping absent optional bones). `Skeleton::humanoid(bones, rest)` builds
the standard hierarchy for a chosen bone set, which is where the template rig (RG-01) and tests start.

**Humanoid or generic (CB-05).** Non-human templates (a quadruped, a winged or tailed creature, a blob) have no
VRM humanoid, so humanoid-ness is a property of a skeleton that code asks about, never one it assumes:

- `Skeleton::new` and `Skeleton::humanoid` are unchanged: a skeleton built with them is a humanoid and has a
  `HumanoidMap` (the role table; holding one proves the 15 required bones and the VRM hierarchy).
- `Skeleton::generic(bones)` keeps every rule that is not about VRM (1 to `MAX_BONES` bones, parents first, unique
  names, finite unit-rotation rests) and has **no humanoid roles at all**. A bone tagged with a role is refused
  (`SkeletonError::UnexpectedHumanoidRole`): roles are all or nothing, so `humanoid_bone(role)` is `None` for every role
  on a generic skeleton and a half-tagged skeleton cannot exist. Relaxing that later (tag the upper body of a centaur)
  is additive; tightening it would not be.
- `Skeleton::humanoid_map() -> Option<&HumanoidMap>`, `is_humanoid()` and `require_humanoid() -> Result<_, NotHumanoid>`
  are the query. `TryFrom<Vec<BoneDef>>` (and so serde, which stays a plain list of bones) picks the constructor from the
  bones: any role means `Skeleton::new`, none means `Skeleton::generic`.

What each consumer does with a generic skeleton:

| Consumer | On a generic skeleton |
|---|---|
| forward kinematics, `inverse_bind`, `skin_matrices`, `SpringSet::validate`, the spring solver | work as on any skeleton (they never look at roles) |
| `HumanoidRig::new` (`omuzaki-rig`) | explicit fallback: no bindings, every solve gives the rest pose, nothing is read back; springs still move the soft parts |
| `HumanoidRig::try_new` | `Err(NotHumanoid)` |
| VRM export, motion retargeting (RG-08), the pose library (RG-06) | start with `skeleton.require_humanoid()?` and wrap `NotHumanoid` in their own error (none of the three exists yet) |
| `JointRef::Humanoid(bone)` (`omuzaki-char-api`) | resolves to `None` (the existing "no such bone" path of `PoseSource::skin_matrix`), so a runtime skips it; whether a *build* with such a mesh binding fails is the character builder's call |
| `JointRef::Named(name)` | works: it is a name lookup (`Skeleton::find`) |
| `Rig::solve` / `Animator` | a rig for a generic skeleton ignores the `HumanoidPose`; posing the body parts of a creature is bone drivers and the spring solver, not the humanoid pose |

Thumbs: VRM 1.0 renamed them Metacarpal, Proximal, Distal (0.x: Proximal, Intermediate, Distal). The crate uses the 1.0
names; the 0.x importer shifts them.

Two traits:

- `Rig::solve(tick, &HumanoidPose, &mut Pose)`: humanoid rotations onto the skeleton's rest, then IK, then springs.
- `Animator::animate(tick, &Bus, &[Cue], &mut HumanoidPose)`: the bus (head, lean, breath, body turn) and the cues that
  fired this frame, layered onto `out`, which arrives holding the **base pose**, the character's `bind` (decision 0014
  §4). Bones the animator does not drive stay as given. `PoseLayers::resolve` (`pose_layer.rs`) pins the layering, as
  `LayerStack::resolve` does for channels: a `PoseLayer { pose, mask: BoneMask, weight, mode }` is `Override` (slerp from
  the incoming pose by weight; clips, authored in T space) or `Add` (a delta in the parent's T space; the life layer). Reference behaviour: the head angles split 35% neck and 65% head (the prototype's
  split), lean bends the spine, breath lifts the chest, clips play over the procedural idle. Clip data (the pose library,
  RG-06) arrives with that feature; this is the interface it plugs into.

## 8a. IK and look-at goals (RG-05, FE-07)

Additive to everything above: no existing type or signature changes, and `Rig::solve` behaves as before until a rig runs an
`IkSolver`. Code: `ik.rs`; stand-in: `MockIk` in `omuzaki-anim-mock`.

- `IkGoals` (`Copy`, fixed size, no heap): `look_at: Option<LookAt>` and `left_hand`, `right_hand`, `left_foot`,
  `right_foot: Option<Limb>`. `IkGoals::NONE` asks for nothing; `is_idle()` is true when every goal is absent or weighs 0.
- `LookAt { target, weight, eye_share }`: `eye_share` `0` turns neck and head only, `1` turns only the eyes, between the eyes
  finish what the head leaves.
- `Limb { target, pole, weight }`: where the hand or foot goes, and a point the elbow or knee bends toward (it picks the side
  of the root-to-target line, so the bend cannot flip).
- `IkSolver::solve(&mut self, &Skeleton, &IkGoals, &mut Pose)` and `reset()`. It runs after the humanoid pose is applied
  and before springs (spec §8). It edits only the **rotations** of the chain bones (arm and leg bones; neck, head, eyes),
  so lengths never change; an idle `IkGoals` leaves the pose bit-identical; a generic skeleton is left alone.

**Space.** Targets and poles are in the skeleton's root space (what `Skeleton::world_pose` gives); the caller undoes the
character's placement in the scene. **Weights** are `0..=1` and a goal out of reach is approached with the limb straight,
along the line to it.

**Eye channels.** Without a look-at goal the `eyeLook*` channels are offsets from straight ahead (§3). With one, the solver
aims the eyes at the target and the channels become offsets **from the target direction**: the pose keeps the aim, the
channels keep saccades and micro-movement (the prototype's `lookAdd` on top of "look at the camera").

What the contract leaves to the real solver (`omuzaki-rig`): joint limits, smoothing and speed limits, foot planting, hand
orientation, fingers. Adding them is additive (new fields with defaults, or a config passed at construction).

## 9. Spring bones (RG-03)

`SpringParams` uses VRM 1.0's names, units and defaults (`hit_radius 0`, `stiffness 1`, `gravity_power 0`,
`gravity_dir (0,-1,0)`, `drag_force 0.5`), verified against the `VRMC_springBone` schema, so a character's springs export
and import without translation. Differences from VRM, both for people and agents editing the data:

- colliders live **inside** their `ColliderGroup`, and chains name their groups (strings), not indices;
- `SpringSet::validate(&Skeleton)` returns an error that says what to fix: names unique and non-empty, at least 2 joints,
  every joint a descendant of the one before, `center` the first joint or one of its ancestors (a VRM rule), groups that
  exist, parameters in range (`drag_force` in `0..=1`);
- two rules span chains (VRM 1.0): a bone is a joint of **at most one** chain (`SharedJoint`), and a chain's `center` is
  neither a joint of another chain nor below one of its joints (`CenterInOtherChain`). A centre above another chain is
  fine.

`SpringChain::tapered(name, bones, root, tip)` blends parameters along a chain, which is what hair generation (CB-33)
needs. `SpringSolver` is the boundary with the rig: fixed 120 Hz sub-steps (`PHYSICS_HZ`), at most 8 per frame by
default, the remainder carried over so results do not depend on the frame rate, `warm_up` (hair settled before frame
0), `reset`, `set_max_substeps` (the scheduler's way to degrade gracefully) and `stats` (sub-steps run, time dropped).

Not included: VRM's extended colliders (plane, inside-out shapes) and cloth (RG-04, P0). Both are additive to this contract.

## 10. Scores (spec §7 `performances/<id>/score.toml`, AP-04)

A `Score` is cold data: `tracks: [{channel, initial, keys}]` and `cues`. Playing it means `Score::compile()` once, then
`CompiledScore::sample(t, &mut Layer)`, a pure function of time: scrubbing, rendering frame 4000 first and playing live
give the same numbers.

**Keys are transitions, not waypoints.** A key says "from this time, ease from wherever the channel is now to this
value over `over` seconds". That is how the prototype's score was transcribed (a change starts when you see it start),
and it makes mid-flight changes continuous: compiling precomputes each key's start value from the previous transition
(which may still be in flight), so a new key never jumps. `over = 0` snaps; `ease` is `linear`, `smooth`, `out_cubic`
(default, "faces snap and settle") or `in_out_cubic`. Editors convert to waypoint curves for display.

**Cues** are things that happen: `Clip { clip, weight, fade_s }`, `Expression { expression, intensity, hold_s, fade_s }`,
`Marker { name }` (non-exhaustive, so more kinds are additive). `CompiledScore::fired(after, up_to)` returns the cues in
`(after, up_to]` as a borrowed slice, so a player collects them each frame without allocating. AI intents (AP-04:
`emote`, `gesture`, `play_clip`) compile to cues; camera moves are not channels (they belong to the shot timeline, §26).

```toml
duration = 4.0

[[tracks]]
channel = "mouthSmileLeft"
[[tracks.keys]]
time = 0.5
value = 0.75
over = 0.25

[[cues]]
time = 1.0
type = "clip"
clip = "wave"
```

Live AI intents append one score fragment each to the member's live score; `look_at` and `move_to` go to IK goals and placement, not cues (`ai-performer.md` §7.2). The performer's big moves (head, lean, body turn, gaze) fill the score layer's channels that no key drives, and its mouth is a `Source::Audio` layer (`ai-performer-voice-face.md` §4.2, §5.1, ADR 0041).

## 11. Port map from the prototype

| Prototype (`face.js` param / bus field) | Rust channel(s) |
|---|---|
| `winkL`, `winkR` | `eyeBlinkLeft`, `eyeBlinkRight` |
| `smileEye`, `winkArc` | `eyeArcLeft`, `eyeArcRight` (how much a closed lid draws as ^) |
| `open`, `openL/R` | `eyeWide` minus `eyeSquint` per side (the base eye size is a face-profile property) |
| `lowLid`, `lowLidL/R` | `cheekSquint` and `eyeSquint` per side |
| `browY`, `browYL/R`, `browTilt*` | `browInnerUp`, `browOuterUp*`, `browDown*`, `noseSneer*` (net height and angle are derived by the painter) |
| `mOpen` | `jawOpen` (with `mouthClose`) |
| `mSmile`, `mSmileL/R` | `mouthSmile*` against `mouthFrown*` (plus `mouthDimple*`) |
| `mWide`, `mRound`, `pucker`, `mPress` | `mouthStretch*`, `mouthFunnel`, `mouthPucker`, `mouthPress*` with `mouthRoll*` |
| `mX` | `mouthLeft/Right`, `jawLeft/Right` |
| `teeth`, `teethLow`, `tongue` | `mouthUpperUp*`, `mouthLowerDown*` (painter), `tongueOut` |
| `blush`, `heart`, `fang`, `spark`, `tear` | `blush`, `eyesHeart`, `fangs`, `sparkle`, `tears` |
| `look {x,y}`, `lookAdd` | the eight `eyeLook*` channels (`ChannelBuf::gaze`/`set_gaze`) |
| `head {yaw,pitch,roll}` (degrees), `lean`, `shift` | `headYaw/Pitch/Roll`, `lean`, `headShiftX/Y` (radians, metres) |
| `breath`, `turn` | `breath`, `bodyTurn` |
| `wave` | a `Clip` cue |
| `dist`, `camX`, `camY` | not channels: the shot's camera (§26) |

| Prototype constant | Where it went |
|---|---|
| `GAIN` table | `Calibration::from_gains` data, owned by `omuzaki-anim` (tuning data) |
| `rangeFromSamples`, `calibrate` | `Calibrator` (fit), `Calibration::rest_at` |
| `EXAGGERATE` regex | `retarget::EXAGGERATED` mask |
| One-Euro `fc = 1.2 + (1-s)*4.5`, fast `x2.6` | `OneEuroParams::for_smoothing` |
| blink window (`0.28..0.8`, ranged `0.25..0.85`) | `Response::built_in` is the ranged window; the retarget uses `0.28..0.8` for a blink still on its default range (the span is the tell, since the contract has no "fitted" flag) |
| `leanPerCm 0.9` | `RetargetConfig::lean_per_metre` (90 degrees per metre) |
| `winkArc` hysteresis (0.42 / 0.32) | the grammar stage (FE-05), not defined here |
| `life.js` `DEF`, `DYN`, blink/gaze/head constants | `LifeParams` and its sub-structs |
| `mulberry32` streams | `omuzaki_core::rng::Rng` forks via `LifeStreams` |
| `perform.js` neck 35% / head 65% | `Animator` reference behaviour |

## 12. Interfaces with the other contracts

The render, char/face and io contracts are designed in parallel (the render contract is merged; the others are open), and the track contract
is not yet designed. This crate depends on none of them; face-api depends on this one. What each should expect from this one, and what this one expects:

**`omuzaki-render-api` (merged).** Agreed and implemented: the renderer takes final skinning matrices in
`Scene::joints` and knows no skeleton or bone names. This crate provides the producer side: `Skeleton::inverse_bind()`
(once per skeleton) and `Skeleton::skin_matrices(local, inverse_bind, scratch, out)` (every frame,
`world * inverse_bind`, bind space to model space), so each rig does not re-derive them. `MAX_BONES` (512) is under the
renderer's 2048-joint limit. The face painter implements render-api's `Painter`; it reads a `&ChannelBuf` (128 floats,
sanitised and in range; `as_slice()` goes straight to a GPU buffer).

**`omuzaki-char-api` and `omuzaki-face-api`.**
- *Channel table.* **Decided: this crate owns it** (§15). `omuzaki-face-api` depends on `omuzaki-anim-api` for `Channel`,
  `ChannelBuf` and `ChannelMask`. If the face contract needs extra channels, it lists them and they are added to
  `channel.rs` in the reserved range (indices 73 to 111) by an API change here; an index, once assigned, never moves.
- *Painter input.* The face painter reads a `&ChannelBuf` and owns the mapping from ARKit channels to its shapes (the
  formulas in `arkit.js`, face profiles, the exaggeration of painted shapes). Anim promises values are clamped to each
  channel's range and finite.
- *Hair and outfit parts* (CB-30 to CB-43) emit bones and `SpringChain`s. Bone names are `hair_<part>_<i>`; chains use
  `SpringChain::tapered`; the base is `Skeleton::humanoid` (or `Skeleton::generic` for a non-human template). Stored in
  `characters/<id>/rig.toml` (spec §7).
- *Expressions (FE-03):* a named expression is a sparse `ChannelBuf` (serialises as `{name: value}`) plus an intensity
  curve. `ExpressionId` and `ClipId` are declared here (slug ids) because cues reference them; they move if face-api
  owns the library.
- *Grammar rules (FE-05)* are a `Bus -> Bus` stage after life, owned by stream S9; same shape as `LifeLayer`.

**`omuzaki-io-api`.**
- *VRM import* builds a `Skeleton` (`BoneDef` per node, humanoid roles, 0.x thumb shift) and a `SpringSet` from
  `VRMC_springBone` (groups get synthesised names `group_<i>`; colliders move into their groups). VRMA import yields a
  `Score` or clips later (RG-08).
- *Export (FE-09):* the 52 ARKit names are the channel names, so "perfect sync" baking is the identity mapping from
  channels to morph targets; the extras and `customNN` bake to custom expressions.
- Rest pose must be the VRM T-pose, and `HumanoidPose` is in VRM normalised space, so no conversion is needed at the
  boundary.

**`omuzaki-track-api` (not designed yet).** It needs `TrackFrame`, `FrameSource`, `DeviceProfile`, `CalibrationPrompt` and
`Calibrator`. Proposal: it depends on this crate for them instead of defining twins. It adds the `Tracker` device trait
(start, stop, latency, device list), protocol parsing, the matrix to `HeadPose` conversion, clock mapping and the
lock-free mailbox.

**`omuzaki-audio-api`.** Lip sync fills `LayerStack::audio` (mouth channels, `Max` mode by default, `Override` when no
tracker is present) and produces `Cues`.

**`omuzaki-ops-api` and `omuzaki-doc-api`.** Configs are plain serde structs, so ops edit them as documents: the
`RetargetConfig`, `LifeConfig` and `Calibration` of a performer, `SpringSet` and `Skeleton` in `rig.toml`, `Score` in
`score.toml`. Proposed op names, all registered by their streams: `perform.set_retarget`, `perform.calibrate`,
`life.set_personality`, `life.set_system`, `score.set_key`, `rig.set_spring_params`.

**Frame-budget scheduler (spec §8).** Each trait call is a unit the caller times; `SpringSolver::set_max_substeps` and
`stats` are the knobs and meters this crate adds. A shared `Stage` enum was left out: it belongs with the scheduler.

## 13. Performance and determinism

Criterion benchmarks (`benches/hot_path.rs`), release build, two runs on a shared machine that was busy (load about 38),
so each row is the range seen. Spec §10 budget: the whole CPU side under 3% of 8 cores at 1080p30, so over 1 ms per
frame is available and the whole table sums to about 5 us:

| Operation | Time |
|---|---|
| `LayerStack::resolve`, 4 layers, all 89 channels | 0.5 to 0.7 us |
| `ChannelBuf::sanitize` | 0.14 to 0.16 us |
| `ChannelBuf::mirror` | 0.09 us |
| `CompiledScore::sample`, 40 tracks of 200 keys | 0.8 us |
| `HumanoidPose::lerp` (55 slerps) | 0.75 to 1.2 us |
| `Skeleton::world_pose`, 55 bones | 0.4 to 1.4 us |
| `Skeleton::skin_matrices`, 55 bones | 0.95 to 2.1 us |

Nothing here allocates, and two tests prove it with a counting allocator (the pattern from `omuzaki-render-api`):
`omuzaki-anim-api/tests/hot_loop.rs` runs the contract's own hot operations for 1000 frames, and
`omuzaki-anim-mock/tests/hot_loop.rs` runs whole mock-pipeline frames (replay poll, retarget, score, blend, life,
animator) across a lost-face span and a loop wrap, both with zero allocations after warm-up (mutation-checked: one
`Vec::with_capacity` in the loop fails the test). The real implementations are held to the same rule and can reuse the
mock's test.

**Determinism follows decision 0009** (`docs/decisions/0009-determinism-policy.md`). On one machine and toolchain,
everything here is bit-identical: all randomness is `omuzaki_core::rng::Rng`, all time is `Tick`, nothing reads a clock,
and `ChannelBuf::checksum` and `HumanoidPose::checksum` digest results so tests can assert byte equality. The mock
pipeline replays a 20 s synthetic track identically, including after saving and reloading it as JSON (rule 3: the
workspace's `serde_json` has `float_roundtrip`). Across machines and toolchains the policy gives a stated
tolerance, so the one test that compares against numbers recorded elsewhere
(`the_performance_matches_recorded_values_within_the_cross_platform_tolerance`) allows 1e-5 per channel, the policy's
default. The mocks also avoid `sin`, `exp` and `ln`, so they are exact in practice; the real life layer and retarget
will use libm (`ln` in `BlinkParams::gap`, `powf` in exaggeration) and fall under the tolerance, not byte equality.

## 14. Decisions and alternatives

| Decision | Alternative rejected | Why |
|---|---|---|
| ARKit-native bus | keep the prototype's painter-specific params | FE-01; one vocabulary for tracker, audio, score, recording and VRM export; the formulas move to the painter |
| One range per channel | neutral map plus gain table plus ranges | the three were one idea; fewer code paths and one thing to calibrate |
| Ownership derived from layers | an explicit ownership table users configure | fewer states to get inconsistent; matches the prototype's `owns` flags without a second source of truth |
| Stateless `resolve`, weights do hand-off | a stateful `Blender` trait | testable, scrubbable, trivially deterministic; sources already know when they are going away |
| Keys are transitions | waypoint keys | matches the transcribed prototype score and makes mid-flight changes continuous |
| `HumanoidPose` in normalised VRM space | per-skeleton rotations | portable clips, mirror, motion import |
| Colliders inside groups, groups by name | VRM's index lists | reordering cannot silently rewire a chain |
| Channels as a fixed table with named constants | dynamic registration | hot-path indexing, stable recordings; extension via the 16 custom slots and the reserved range |
| `IkGoals` and an `IkSolver` trait, added later (§8a) | an extra argument group on `Rig::solve` | `Rig::solve` and every implementor stay as they are; IK is a separate stage with its own mock, as springs are |
| Generic skeletons have no roles at all; `Skeleton::new` untouched | optional roles on one constructor; a `Skeleton` enum | a humanoid keeps its guarantees (15 bones, hierarchy) with no `Option` at the use site; "is it a humanoid?" is one query; partial roles can be added later without breaking anyone |
| `NotHumanoid` from `Skeleton::require_humanoid`, consumers wrap it | one shared error for every humanoid-only feature | each feature says what it was doing; the gate is one line |

## 15. Decisions taken on review, and what is still open

**Decisions taken on review** (review):

| # | Question | Decision | What changed in this PR |
|---|---|---|---|
| 1 | Who owns the channel table? | **`omuzaki-anim-api`.** `omuzaki-face-api` depends on it; extra channels are requested by listing them and added here. | §12 updated. The reserved range (73 to 111) is where they go. |
| 2 | ARKit-native bus? | **Confirmed.** The smile, brow and mouth formulas go to the face painter. | none (§3 already says so) |
| 3 | `serde_json` `float_roundtrip` workspace-wide | **Done on main** (done). | the mock no longer overrides the feature; `RecordedTrack::from_json` docs cite decision 0009 |
| 4 | Cross-platform float determinism | **Decision 0009:** bit-identical on the same machine and toolchain; stated tolerances across platforms (channels 1e-5, joint transforms 1e-5 m and rad). | replay tests follow it (§13): bit equality for same-machine replays and JSON reloads, a 1e-5 tolerance for recorded numbers; the pinned digest is gone |
| 5 | `MAX_BONES = 512` | **Fine; document why.** | rationale on the constant (`skeleton.rs`): about 10 times the prototype's rig, `Pose` 16 KiB fits in L1, well under the renderer's 2048 joints, a one-constant change later |

**Still open:**

1. ~~**IK and look-at goals**~~ Landed in §8a. `Rig` does not run an `IkSolver` itself yet; wiring it in is the rig
   streams' work (RG-05).
2. **`ExpressionId` and `ClipId`** live here for now (cues reference them); they should move if face-api owns the
   library.
3. **Dev-dependencies:** `criterion` (benches) and `toml` (the at-rest test), both pre-declared and registered.
   `cargo deny` passes.
4. **The grammar stage** (FE-05) is left to stream S9 with the same shape as `LifeLayer`. Confirm it should not be an
   API trait here.

## 16. Follow-ups

Open items: the IK and look-at goal types (RG-05, FE-07, landed in §8a) and personality tuning (PF-24). The
`float_roundtrip` and float-determinism questions are closed by decision 0009.
Each stream's first pieces of work (retarget, life, rig, springs, pose library, calibration UI) can start once this
merges; the mocks make them independent.
