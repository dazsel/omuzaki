---
title: "`omuzaki-audio-api`: the audio engine contract (devices, mixer, voice effects, taps, latency)"
summary: "the audio engine contract (devices, mixer, voice effects, taps, latency)"
area: design
---

# `omuzaki-audio-api`: the audio engine contract (devices, mixer, voice effects, taps, latency)

**Status:** accepted (2026-10-12; review answers in section 11). **Decision record:**
`docs/decisions/0018-audio-engine.md`.
**Spec:** v3 §8 (audio on a real-time thread), §9, §10 (`voice.fx`, `voice.fx.pitch`, `voice.fx.cpu`), §39.1; rows PF-08,
PF-12, PF-23, PF-26, BR-13, BR-18, BR-21, BR-22, BR-24, AP-02, AP-03, AP-05, ML-03, ML-05, ML-14, SE-04, SE-05.
**Builds on:** `docs/research/2026-10-voice-changers.md`, ADR 0016 (user-supplied models), ADR 0017 and `omuzaki-ai-api`
(ML-01: streaming model slots, `BlockSink` / `BlockSource`), the speech host (`crates/omuzaki-audio`) and its CPU share
(`docs/research/2026-10-speech-host-cpu.md`).

## In one minute

One real-time **audio graph** runs at **48 kHz** on the audio thread of one *clock device*. Device inputs come in
through bridges; buses sum their sources, run an effect chain, apply gain, mute and ducking, and send to other buses;
the program bus goes out to devices. **Taps** copy a signal out of the graph into lock-free rings for the things that
listen (lip sync, captions, the AI performer, recording); **feeds** carry audio in from things that speak (TTS, the
soundboard, music). Every node reports a fixed latency, so the engine can say how late every route is, and the
performance pipeline can hold the face back by exactly that much (the PF-12 A/V delay, which lives with the
performance channels, not here).

```text
 device threads            audio thread (clock device callback, 48 kHz, no alloc, no locks)              consumers
 ──────────────            ─────────────────────────────────────────────────────────────────            ─────────
 mic (raw)  ─▶ bridge ─┐   ┌──────────── bus "mic" (mono) ─────────────────────────┐
 changer    ─▶ bridge ─┼──▶│ [aec] ▶ chain: gate▶eq▶comp▶pitch/formant▶model.vc ▶  │ fader ▶ sends ─┐
              (ring +  │   │   ▲ tap: raw           ▲ tap: heard (post-fader) ▲   │                │
               ASRC)   │   └───┼────────────────────┼─────────────────────────────┘                │
 TTS feed ─────────────┼──▶ bus "ai_voice" ─────────┼──── tap: ai_voice ──────────────────────────┤
 soundboard player ────┼──▶ bus "soundboard" ── ducks "mic"/"game" ───────────────────────────────┤
 music player / line ──┴──▶ bus "music" ─────────────────────────────────────────────────────────┤
                            bus "program" (stereo) ◀───────────────────────────────────────────────┘
                              │ chain: fx.limiter (true peak, −1 dBTP, on by default)
                              ├─▶ output bridge ─▶ program_out device (virtual cable to OBS)
                              └─▶ tap: program, per-bus taps ─▶ recording (BR-18), direct stream (BR-05)
   taps ─▶ SPSC rings (drop policy per consumer) ─▶ speech host (captions, AI listening), lip sync and cues,
                                                    clap test and drift monitor, recorder
   control thread: compile GraphSpec ─▶ swap in at a block boundary; parameter messages; device watcher
```

| Crate | Kind | What |
|---|---|---|
| `omuzaki-audio-api` | API | the contract: devices and roles, `GraphSpec`, the `Node` / `NodeKind` contract and its self-registration, parameters, taps and feeds (with the SPSC ring), the engine clock, latency and health reports, the `AudioEngine` service, the `AudioBackend` seam, presets. Pure data and traits, wasm-clean. |
| `omuzaki-audio-mock` | Mock | the reference **offline engine** (single-threaded, obviously correct), a scripted fake backend (devices that vanish, return, reorder and drift), seeded test signals (synthetic voice, claps, sweeps), and the **node conformance suite** every node kind passes. |
| `omuzaki-audio` | Implementation | the engine: the cpal backend, device manager, bridges with clock alignment and drift resampling, the graph compiler and real-time executor, buses, ducking, meters, taps and feeds, the model node (ML-14), the clap test and drift monitor, and the `audio.*` ops. |
| `omuzaki-audio-fx` | Implementation | the BR-24 effect nodes, one folder each (gate, EQ, compressor, robot, radio and telephone, pitch and formant), plus the program limiter and the echo canceller, self-registered, and the `audio.fx.*` preset ops. |
| `omuzaki-voice` | Implementation | the PF-08 and FE-08 consumers of the voice taps: cues and lip sync, stamped with performance time. |
| `omuzaki-speech` | Implementation | the speech host, unchanged except its name: it reads a voice tap (section 4.3). Later it also holds the TTS host (ML-04), which writes the `ai_voice` feed. |

## 1. What the contract covers

**In:** device enumeration, binding and loss; the mixer graph (buses, chains, gain, mute, ducking, sends, outputs); the
node contract and effect parameters; taps and feeds; the engine clock; latency, drift and health numbers; effect
presets as data; offline rendering for tests; the ops that drive all of it.

**Out, and who owns it:**

| Thing | Owner | What it gets from here |
|---|---|---|
| The A/V delay line (PF-12) | the performance pipeline: `omuzaki-anim-api` (`PlayoutDelay`) wired by `omuzaki-stage` (section 5.2) | latency numbers and performance-time stamps |
| Lip sync and voice cues (FE-08, PF-08) | `omuzaki-voice` (new), producing `Cues` and an audio `Layer` | the `mouth` and `cues` voice taps |
| Speech recognition and VAD (ML-03) | `omuzaki-speech` on ML-01 slots | the `captions` and `listen` voice taps |
| Voice to face (ML-05) | `omuzaki-ai` slots, driven from `omuzaki-voice` | the `mouth` and `ai_voice` taps |
| TTS (ML-04, AP-02) | `omuzaki-speech` (the TTS host on the ML-04 slot) | a feed into `ai_voice` |
| Model slots, streaming deadlines, fallback models | `omuzaki-ai-api` (ADR 0017) | nothing: the model node *uses* `BlockSink` / `BlockSource` |
| Encoding, muxing, recording files | `omuzaki-stream` | per-bus and program taps |
| Soundboard library, provenance, licences (BR-21) | the BR-21 tasks (library items) | a player source and duck gates |

## 2. Devices (PF-12, BR-22)

### 2.1 Backend: cpal behind our own seam

**cpal 0.18** (Apache-2.0, already in the licence register as `conditions`: the `asio` feature is banned). Checked on
2026-10-12 against its source and changelog:
- **Stable ids exist:** `DeviceTrait::id` and `HostTrait::device_by_id` since 0.17; since 0.18 a `DeviceId` is
  `(HostId, string)`. The string is the WASAPI endpoint id, the CoreAudio device UID, the ALSA or PipeWire name.
- **Loss is reported:** `DeviceNotAvailable` on disconnect (ALSA, JACK, CoreAudio; PipeWire unreleased) and
  `DeviceChanged` on WASAPI.
- **WASAPI is shared mode only.** cpal hard-codes `AUDCLNT_SHAREMODE_SHARED`, does not use `IAudioClient3` low-latency
  periods, and `BufferSize::Fixed` changes only its ring size: the callback period stays the engine period (10 ms on
  most drivers).

The engine talks to devices only through `AudioBackend` (API crate): enumerate, open an input or output stream with a
callback, report errors. `omuzaki-audio` implements it with cpal; the mock implements it with scripted fake devices. A
native WASAPI backend (exclusive mode, or `IAudioClient3` 2.67 ms shared periods) can be added later as a sibling crate
without touching the graph. We build it only if the Dev loopback test (section 9.4) shows shared mode costs more than
the A/V delay and the voice budgets can absorb. The `voice.fx` budgets exclude the device by definition.

**Licences of the options:**
- WASAPI is an OS API; the `windows` crate a native backend would use is MIT OR Apache-2.0.
- **ASIO stays banned.** Steinberg reportedly relicensed the ASIO SDK in October 2025 as GPLv3 *or* a proprietary
  agreement (press coverage; Steinberg's text not read: unverified). The GPLv3 route links GPL code into the product,
  which the policy bans. The proprietary route needs the maintainers to sign Steinberg's agreement and legal review to read it.
- ASIO is not needed. Changers run their own I/O, and Omuzaki reads their virtual output, so their ASIO latency is
  theirs. ASIO stays banned (review answer 5 in section 11). Supporting ASIO-only interfaces would need the maintainers to
  take the proprietary route.

### 2.2 Ids, roles and bindings

- **The project names roles, never devices.** Roles are slugs: `voice` (the mic, or the changer's output when that is
  all there is), `raw_voice` (the raw mic when a changer is used), `program_out`, `monitor_out`, and any user role
  (`game_in`, `music_in`). A project opened on another machine asks for a device per role.
- **The machine profile binds roles to devices.** It is person-level settings, outside the project: the same store that
  keeps ML-13's per-hash acknowledgements. A `ProfileStore` trait stands in until that store lands.

```rust
pub struct DeviceId { pub host: String, pub id: String }        // cpal's (HostId, id), as strings
pub enum Binding {
    Fixed { id: DeviceId, label: String, channels: ChannelMap }, // the default; `label` is the name at bind time
    SystemDefault { channels: ChannelMap },                      // explicit opt-in to "follow the OS default"
}
pub struct DeviceInfo { pub id: DeviceId, pub label: String, pub direction: Direction, pub is_default: bool,
                        pub rates: Vec<u32>, pub channels: u16, pub latency_hint: Option<Seconds> }
```

- **Never an index.** Windows reorders devices, so an index is never stored or compared.
- **`ChannelMap`** picks device channels for the source: input 2 of a USB interface, or the left channel of a stereo
  virtual cable.
- **Several inputs at once** is the normal case for PF-12: `voice` bound to the changer's virtual output and `raw_voice`
  to the raw mic. Each input is its own stream and bridge (section 3.3); the same device may serve two roles.
- **Outputs** are bound per bus (`program_out`, `monitor_out`, any user role). Binding an output is a person's act
  (section 7).

### 2.3 Loss and reconnect

Each binding has a state, shown on the health meter and sent as an `AudioEvent`:

```text
 Unbound ──bind──▶ Opening ──ok──▶ Running ──error / vanished──▶ Lost ──same id reappears──▶ Opening
                      └──fail──▶ Failed(reason) ◀──────── (retry on the next watch tick, backing off to 5 s)
```

- **Detection.** A stream error callback (`DeviceNotAvailable`, `DeviceChanged`) or the watcher, which re-enumerates
  once a second on the control thread. Enumeration is cheap; there is no portable hotplug API.
- **While lost:**
  - an input bridge feeds silence and its taps flag the gap, so the speech host never splices two utterances;
  - an output bridge discards;
  - the graph keeps running.
  - If the lost device was the clock device, an internal timer clock takes over, so taps, lip sync and recording keep
    their timeline. The clock device comes back when the device does.
- **Never substitute.** A `Fixed` binding never falls back to another device, the OS default included, so a changer
  restarting or Windows reordering reconnects the same chain or shows "lost". It never silently swaps inputs (PF-12).
- **Same name, new id** (a driver reinstall or a reinstalled virtual cable): the engine does not rebind. It raises
  `AudioEvent::PossibleMatch { role, candidate }`, and the person confirms.
- **On return:** reopen, re-prime the bridge, fade in over 5 ms, and send `Reconnected { role, gap }`.

## 3. The real-time graph (BR-22, BR-24)

### 3.1 Shape: a mixer, not a free graph

The graph is **sources → buses → sends → outputs**, the shape of a hardware mixer and of VoiceMeeter. It is not a
general node graph, because:
- it is what BR-22 describes ("a small mixer");
- it keeps the UI a mixer strip;
- it makes "which captured audio reaches which output" a cheap closure check (section 7.2).

Effects are ordered chains of nodes inside a bus.

```rust
pub struct GraphSpec { pub buses: Vec<BusSpec>, pub ducks: Vec<DuckRule> }          // cold: serde + JsonSchema
pub struct BusSpec {
    pub id: BusId, pub channels: Channels,                // Mono | Stereo
    pub sources: Vec<SourceRef>,                          // Device{role} | Feed(id) | Player(id)
    pub chain: Vec<NodeSpec>,                             // in order
    pub gain_db: f32, pub muted: bool,
    pub sends: Vec<SendSpec>,                             // { to: BusId, gain_db, pre_fader }
    pub outputs: Vec<DeviceRole>,
}
pub struct NodeSpec { pub id: NodeId, pub kind: NodeKindId, pub config: NodeConfig, pub params: Params, pub enabled: bool }
pub struct DuckRule { pub trigger: DuckTrigger /* Bus(id) | Gate(id) */, pub target: BusId,
                      pub depth_db: f32, pub threshold_db: f32, pub attack_ms: f32, pub hold_ms: f32, pub release_ms: f32 }
```

**Default buses** (a new project; users add more, such as `game` and `monitor`):

| Bus | Channels | Sources | Chain | Sends | Notes |
|---|---|---|---|---|---|
| `mic` | mono | `voice` | `fx.aec` when speakers are in use (section 4.6), then the BR-24 preset of the active character | `program` | the voice taps sit here (section 4.2) |
| `ai_voice` | mono | the TTS feed | none | `program` | reaches its lip-sync tap digitally, never through a speaker, so there is no echo (BR-22) |
| `soundboard` | stereo | the player | none | `program` | its gates duck `mic` and `game` (BR-21) |
| `music` | stereo | a player or `music_in` | none | `program` | ducked by `ai_voice` and `mic` when the person sets it |
| `program` | stereo | sends | `fx.limiter`: a transparent true-peak safety limiter at −1 dBTP, **on by default**, with an off switch; its gain reduction is shown on the program meter (section 3.5) | none | out to `program_out`; tapped for recording and streaming |

The compiler sorts buses by their sends and **refuses cycles**. A mono bus sends to a stereo bus centred
(equal-power, −3 dB per side).

### 3.2 Rate and block policy

- **The graph runs at 48 kHz, always:** the broadcast rate, OBS's default, and the rate every budget is gated at. A
  device at another rate is resampled at its bridge, and the status says so. One rate means one set of tests and
  coefficients.
- **The block is whatever the clock device's callback delivers**, split into chunks of at most `MAX_BLOCK` = 1024
  frames.
  - There is no fixed internal quantum, because one would add up to a quantum of latency.
  - Nodes must therefore be **block-size invariant**: the same input gives bit-identical output however it is split
    into blocks. The conformance suite enforces this, and it is what makes variable blocks safe and offline tests
    exact.
  - A node that works in frames (the pitch shifter) keeps its own frame buffer and declares it as latency.
- **Requested device buffer:** 256 frames (5.3 ms), the size the §10 gates use, configurable from 64 to 1024 where the
  backend honours it. On WASAPI shared mode the period is the engine's (section 2.1); the status shows the real one.
- **The clock device** is the device bound to `program_out`. If none is bound (the common "OBS takes the mic, Omuzaki
  only listens" setup), it is the `voice` input. If neither is bound, an internal timer clock drives the graph.
- **Samples are planar `f32`.** Denormals are flushed to zero on the audio thread (FTZ/DAZ set at thread start on x86;
  flush-to-zero on Arm). Any node with feedback also guards against them itself.

### 3.3 Threads and the real-time rules

| Thread | Runs | May |
|---|---|---|
| **audio** (the clock device's callback) | the whole graph: bridges' reads, sources, chains, faders, ducking, sends, taps' writes, outputs' writes, meters | nothing that allocates, frees, locks, blocks, logs or makes a syscall. The one exception: a tap sink may wake its consumer once per block (a non-blocking futex wake, as `AudioInput` does). |
| **device** (every other device's callback) | copies samples into its bridge ring with their capture timestamps | the same rules |
| **control** | compiles `GraphSpec`, creates nodes, opens streams, watches devices, takes back old graphs and frees them | anything |
| **analysis** | clap test, drift monitor | anything; reads taps |
| **consumers** | speech host, lip sync, recorder, model workers | anything; read taps, write feeds |

**Bridges.** Every device except the clock device has a bridge into the graph:
- an SPSC ring, plus an adaptive resampler (`rubato` 5.0, MIT OR Apache-2.0, through its non-allocating
  `process_into_buffer`);
- a PI controller holds the ring at its target fill (one clock block plus one device period), so two crystals that
  disagree by 100 ppm never underrun or grow latency;
- samples are placed on the engine timeline by their **capture timestamps** (cpal's `capture` instant), not by when they
  arrived, so two inputs line up to within a millisecond. The clap test (section 5.3) depends on this.

Each bridge reports its buffering latency, its rate ratio in ppm, and its underruns and overruns.

**Three kinds of change, three paths:**
1. **Parameters** (gain, mute, an effect knob, a preset that keeps the chain's shape):
   - small `Copy` messages `(node slot, param index, value)` on an SPSC command ring, applied at the next block start;
   - nodes smooth their parameters per sample (gain over 5 ms by default), so nothing zips and block-size invariance
     holds.
   - Enabling or disabling a node is a parameter too: a 5 ms crossfade, with the node's latency unchanged (rule 3 in
     section 3.4).
2. **Structure** (a node added, removed or reordered; a bus, send, tap, feed or output added):
   - The control thread compiles a new `CompiledGraph` with all its buffers, and sends it in a `Box` over the command
     ring.
   - At the block boundary the audio thread **moves** every surviving node (same bus, same node id, same kind and
     config) out of the old graph into the new one: a pointer swap, so filter and shifter state carry over. It then
     swaps the graphs and sends the old one back on a return ring for the control thread to free.
   - A bus whose chain changed fades out and in over 128 samples (2.7 ms). Untouched buses don't glitch.
   - If the return ring is full, the old graph waits in one spare slot and the swap is retried on the next block.
     Nothing is ever freed on the audio thread.
3. **Devices** never swap the graph: a bridge changes state (section 2.3).

### 3.4 Nodes and latency

```rust
pub trait Node: Send {
    /// Samples at 48 kHz by which this node delays its input. Constant while the node is in the graph.
    fn latency(&self) -> u32;
    /// Applies a parameter at a block start (audio thread). Smooth inside; never allocate.
    fn set_param(&mut self, param: ParamIndex, value: f32);
    /// Processes one block in place: planar channels, `frames <= MAX_BLOCK`, `first_sample` on the graph clock.
    fn process(&mut self, block: &mut BlockMut<'_>);
    /// Forgets its history (a new stream). No allocation.
    fn reset(&mut self);
    /// Numbers for the meters after `process` (gain reduction in dB); empty by default. `Copy`, read on the audio thread.
    fn report(&self) -> NodeReport { NodeReport::default() }
}
pub trait NodeKind: Send + Sync {
    fn info(&self) -> NodeKindInfo;   // id ("fx.eq"), summary, channels (Mono | Any), params: [ParamSpec], latency range,
                                      // sidechain: bool (fx.aec takes a reference bus)
    /// Control thread: builds a node for a bus of `channels`, with every buffer it will ever need.
    fn create(&self, config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError>;
}
register_node_kind!(Eq);   // self-registration (§39 rule 2), with `define_link!` (ADR 0015)
pub struct ParamSpec { pub name: &'static str, pub unit: Unit, pub min: f32, pub max: f32, pub default: f32, pub smooth_ms: f32 }
```

**Sidechain.** A kind that needs a second signal declares `sidechain`, and its `NodeSpec` names the bus. `BlockMut`
then carries that bus's output from the **previous** block, so no sidechain ever makes a cycle. `fx.aec` uses it for
its speaker reference; the ducking rules don't need it, because they live in the fader (section 3.5).

Rules:
1. **Latency is declared and honest.** The conformance suite measures it (an impulse for linear nodes, a click train for
   the rest) and fails a node whose output is not aligned with its declared latency to within one sample (one
   millisecond for the pitch shifter).
2. **Path latency is computed, not measured live.** The compiler sums node latencies along every route (source → bus →
   sends → output or tap) and reports them in the `GraphReport` (section 5.5).
   - There is no global delay compensation between different sources: the soundboard is not delayed to match the
     effected voice, because live audio wants each source as early as it can be.
   - Parallel paths of *one* signal (a dry/wet mix) are aligned inside the node.
3. **A disabled node keeps its latency.** A disabled pitch shifter passes the dry signal delayed by its latency. So a
   preset switch that only toggles effects never moves the A/V delay. A preset that *adds or removes* a node with
   latency is a structural change: the new chain latency goes out as `AudioEvent::LatencyChanged`, and the A/V delay
   ramps to it (section 5.2).
4. **`voice.fx` is checked against declarations.** `voice.fx` (≤ 10 ms) and `voice.fx.pitch` (≤ 25 ms) are, per §10,
   the chain's declared latency plus one 256-frame block. A unit test asserts them for the default chains, and the Dev
   loopback test confirms them.

### 3.5 Gain, mute, ducking, meters

- **Fader:** gain in dB (−∞ to +12), smoothed over 5 ms. Mute is a 5 ms ramp to zero, never a hard cut. The exact
  arithmetic is in section 3.6.
- **Ducking:**
  - A `DuckRule` lowers a target bus by `depth_db` while its trigger is active.
  - A trigger is either a bus's level (an envelope over `threshold_db`) or a **gate** a source raises. The soundboard
    raises one for each sound marked "ducks" (BR-21, per sound).
  - The duck gain moves in dB at fixed rates set by `attack_ms` and `release_ms`, after `hold_ms`.
  - A rule reads its trigger **1,024 samples (21.3 ms) late**, which is `MAX_BLOCK`. Every sample it needs was
    computed in an earlier block, so bus order never matters and the result is the same whatever the block size. The
    exact envelope is in section 3.6.
- **Meters:** per bus and per device, peak and RMS (300 ms). On `program` they add a short-term loudness (EBU R128,
  3 s), its true peak, and the limiter's gain reduction (so a person can see when the safety limiter is working). The
  audio thread writes them as `f32` bits into atomics (latest wins), and the UI reads them at its frame rate. Meters
  are numbers, so agents may read them (section 7).
- **The program limiter** (`fx.limiter`, accepted review answer 6):
  - on by default, with an off switch (`audio.bus.limiter`);
  - true-peak detection by 4x oversampling (ITU-R BS.1770-4, Annex 2), a ceiling of −1 dBTP, and a look-ahead of
    1.5 ms, so it is transparent: no clipping and no audible pumping on speech;
  - its latency (look-ahead plus the oversampling filter, about 2 ms) is declared like any node's and counted in the
    routes through `program` (section 5.1). It is not part of `voice.fx`, which is the `mic` chain only.

### 3.6 Pinned semantics: what the mock and the engine must match to 1e-6

Decided with the `omuzaki-audio-api` implementation (2026-10-12).
- **The recurrences are shared code.** Ramps, envelopes and the dB conversion carry state from sample to sample, so a
  difference in rounding grows. They live once, in `omuzaki_audio_api::semantics`, and both engines call them.
- **The rest is reimplemented in the mock.** Routing, ordering, sends and taps are written independently, so the mock
  stays an oracle. Products of a few factors agree to 1e-6 in any order.
- All state is `f32`. A constant that needs `exp` or `powf` is computed once in `f64` and narrowed.

**Gain and mute.**
- `db_to_gain(db) = 10^(db / 20)`, evaluated in `f64` and narrowed. Minus infinity and NaN give exactly `0.0`.
- A bus has **two** `FaderRamp`s: `gain` (target `db_to_gain(gain_db)`) and `mute` (target 0 when muted, 1 when not).
  A send has one (target `db_to_gain(send.gain_db)`).
- **`FaderRamp`:** sample `k` (1 to 240) after a new target is `start + (target − start) · (k / 240)`. From sample 240
  on it is exactly `target`. A new target restarts the ramp from the current value, and re-sending the same target
  changes nothing.
- **Per sample on a bus:**
  - `pre` = the sum of the sources, through the chain;
  - `post = pre · gain[n] · mute[n] · duck[n]`;
  - a pre-fader send carries `pre · mute[n] · send[n]` (mute cuts it, gain doesn't);
  - a post-fader send carries `post · send[n]`;
  - outputs and post-fader taps read `post`.
- **Channel conversion at a send:** mono to stereo puts `x · 1/√2` on each side; stereo to mono is `(L + R) · 1/√2`;
  the same channel count passes through. `1/√2` is `FRAC_1_SQRT_2` as `f32`. A centred signal keeps its level both
  ways.
- **A sidechain** reads the referenced bus's `post` from the previous block. A stereo reference is averaged:
  `(L + R) / 2`. It is a reference for analysis, so its level doesn't matter.
  - **This is the one place the result depends on the block plan:** the reference's delay is the length of the
    previous block.
  - **Why not a fixed delay** like ducking's: an echo canceller needs its reference *before* the echo. The echo's
    round trip (output buffer, air, input buffer) is at least about two blocks, while a fixed 1,024-sample delay could
    arrive after it on a fast device.
  - **So:** sidechained graphs are compared under the same `BlockPlan`, and the block-plan invariance property leaves
    sidechained buses out.
- **Node ids** are unique within a bus, not across the graph. Surviving nodes are moved by `(bus, node id)`.

**Ducking, exactly** (`semantics::{LevelFollower, DuckEnvelope, TRIGGER_DELAY}`):
1. **Trigger level** (bus triggers), per sample on the trigger bus's `post`:
   - `x = max over channels of |post_c[n]|`;
   - `e = x if x > e, else e · R`, with `R = exp(−1 / (0.050 · 48000))`: instant attack and a 50 ms release;
   - `e` starts at 0.
2. **Trigger signal:**
   - for a bus trigger, `t[n] = e[n] > db_to_gain(threshold_db)`;
   - for a gate trigger, `t[n]` is the gate's state at sample `n`.
   - Each trigger keeps a history of its last `TRIGGER_DELAY = 1024` samples: `e` values for buses (so rules with
     different thresholds share one), booleans for gates.
3. **A rule reads `a = t[n − 1024]`.** Before sample 1024 of the stream, `a` is false.
4. **Hold**, per sample, in this order, with `H = round(hold_ms · 48)`:
   ```
   if a { hold = H }
   ducked = a || hold > 0
   if !a && hold > 0 { hold -= 1 }
   ```
   This holds the duck for exactly `H` samples after the trigger ends.
5. **Gain in dB, moved in a whole number of steps.** `g` starts at 0 dB and its target is `−depth_db` when `ducked`,
   else 0. When the target changes, the ramp starts from the current `g`:
   - the step count is an integer, `n = max(1, round(attack_ms · 48))` for a fall (the new target is below `g`) and
     `n = max(1, round(release_ms · 48))` for a rise, computed whenever a parameter changes;
   - each step moves `g` by `(target − start) / n`: step `k` puts it at `start + (target − start) · (k / n)` in `f32`,
     so rounding does not accumulate;
   - step `n` sets `g` to the target exactly (snap).

   A full-depth fall therefore takes exactly `n` samples, which is `attack_ms` to within half a sample, a full rise
   exactly the release's `n`, and `g` lands exactly on its target. A change of target mid-ramp restarts from the
   current `g` with the new `n`.
6. **The bus's duck factor** is `duck[n] = db_to_gain(Σ g_r[n])`, summed over the rules that target it in the order
   `GraphSpec::ducks` lists them. Rules multiply, so their dBs add. With every `g_r` at 0 it is exactly 1.0.
   An engine may cache the factor while every `g_r` is unchanged; the result is identical.
7. **Validation:**

   | Field | Range |
   |---|---|
   | `depth_db` | 0 to 60 |
   | `threshold_db` | −80 to 0 |
   | `attack_ms` | 1 to 1,000 |
   | `hold_ms` | 0 to 5,000 |
   | `release_ms` | 1 to 5,000 |

   A rule is identified by `(trigger, target)`, which must be unique in a graph.
8. **Rule state carries across graph swaps,** by that identity. A rule that a swap removes keeps running, with `a`
   forced false and no hold, until its `g` is back at 0. A rule whose parameters change continues from its current
   `g`. Nothing jumps.

### 3.7 The engine service

The method set of `AudioEngine`, decided. Every method runs off the audio thread, and none blocks it.

```rust
pub trait AudioEngine: Send + Sync {
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError>;
    fn bind(&self, role: &DeviceRole, binding: Binding) -> Result<(), AudioError>;  // also confirms a PossibleMatch
    fn unbind(&self, role: &DeviceRole) -> Result<(), AudioError>;
    fn apply(&self, graph: &GraphSpec) -> Result<(), AudioError>;     // diffs against the running graph (below)
    fn graph(&self) -> GraphSpec;                                     // the running spec, live setters included
    fn set_param(&self, bus: &BusId, node: &NodeId, param: ParamIndex, value: f32) -> Result<(), AudioError>;
    fn set_bus_gain_db(&self, bus: &BusId, db: f32) -> Result<(), AudioError>;
    fn set_bus_muted(&self, bus: &BusId, muted: bool) -> Result<(), AudioError>;
    fn mic_mode(&self) -> MicMode;
    fn set_mic_mode(&self, mode: MicMode) -> Result<MicMode, AudioError>;  // returns where it landed (Live from CohostOnly lands Muted)
    fn tap(&self, req: &TapRequest, sink: Box<dyn TapSink>) -> Result<(TapHandle, TapInfo), AudioError>;
    fn tap_ring(&self, req: &TapRequest) -> Result<(TapReader, TapInfo), AudioError>;
    fn feed(&self, req: &FeedRequest) -> Result<FeedWriter, AudioError>;
    fn set_changer_latency(&self, measured: Option<LatencyEstimate>) -> Result<(), AudioError>;
    fn clock(&self) -> Arc<EngineClock>;
    fn status(&self) -> AudioStatus;
    fn report(&self) -> GraphReport;
    fn meters(&self, at: &MeterPoint) -> Option<Meters>;              // MeterPoint::Bus(id) | Input(role)
    fn subscribe(&self) -> AudioEvents;                               // one bounded queue per subscriber
}
```

- **`apply` diffs** the new spec against the running one:
  - parameter-only differences (node parameters, gains, mutes, send gains, duck-rule numbers, voice-tap choices that
    resolve to the same points) become parameter messages, with no swap;
  - everything else is a structural swap (section 3.3).
- **Live setters** (`set_param`, `set_bus_gain_db`, `set_bus_muted`, `set_mic_mode`) also update the running spec, so
  `graph()` is always true. The document stays the source of truth: the app writes it, then calls `apply` or a setter.
- **Lifetimes:**
  - dropping a `TapHandle` or a `TapReader` closes its tap;
  - dropping a `FeedWriter` closes its feed, and its source plays silence;
  - a closing feed fades out over 2.5 ms; `FeedWriter::closer()` raises the close flag from another thread, and `FeedWriter::playhead()` is a bounded log (about 2 s) of per-block records saying which feed samples entered the bus at which engine sample, plus the engine sample where a closed feed went silent (`cut_at_sample`, the authority on what played) (`ai-performer-voice-face.md` §3.2, §3.3, ADR 0041);
  - closing sets a flag the audio thread reads at the next block and never waits for it. The slot is reclaimed at the
    next compile.
- **Events:** each `subscribe()` gets its own bounded queue of 256 that drops the oldest event and counts drops, so
  the UI, the health meter and the ops never steal each other's events.
- **There is no clap test on the engine.** The clap op opens two `DropNewest` taps with `tap_ring`, runs the
  estimator (`omuzaki-audio`), and stores a validated result with `set_changer_latency`. The stored result lives in the
  machine profile and feeds the changer's `capture_offset` (section 5.1) and `D = auto` (section 5.2). It doesn't touch
  the A/V delay setting itself.
- **Additions to the types:**
  - `GraphSpec::voice_taps: VoiceTaps` (serde default: the section 4.2 table);
  - `GraphSpec::aec: AecMode { Auto, On, Off }`, with `Auto` as the default (section 4.6);
  - an output `Binding` carries `speakers: bool` (the machine profile, set by the person; default false), which
    `AecMode::Auto` reads;
  - `TapRequest::leaves_device: bool` (section 7.2);
  - `TapSink::flags(&mut self, flags: TapFlags) {}`, a provided no-op, called on the audio thread before the first push
    after the `muted` or `private` flags change. A sink consumer can then carry `private` onward (the speech host marks
    utterances heard in `CohostOnly`).
- **`AudioBackend`** callbacks take interleaved `f32`. `CallbackTime { callback, device }` is in the pipeline clock's
  timebase, and the backend converts its own instants (cpal's `StreamInstant`) by sampling both clocks when the stream
  starts.
- **The conformance suite is not part of the contract.** It lives in `omuzaki-audio-mock`, where the design put
  it, so its changes don't need API-crate review.

## 4. Taps and feeds (PF-12, BR-18, BR-22)

### 4.1 Taps

A tap copies one point of the graph into a ring the consumer reads on its own thread.

```rust
pub enum TapPoint { Input(DeviceRole), BusPreChain(BusId), BusPostChain(BusId), BusPostFader(BusId), Voice(VoiceUse) }
pub struct TapFormat { pub rate: TapRate /* Graph48k | Speech16k */, pub mono: bool }
pub enum DropPolicy {
    DropNewest,   // contiguous stream: when full, refuse what doesn't fit and count it (ASR, recording, clap test)
    Overwrite,    // latest window: the writer never waits and the reader skips ahead (lip sync, cues, meters)
}
pub struct TapRequest { pub point: TapPoint, pub format: TapFormat, pub capacity: Seconds, pub policy: DropPolicy, pub consumer: String }
pub trait TapSink: Send {           // called on the audio thread: wait-free, allocation-free, at most one wake per block
    fn push(&mut self, samples: &[f32]) -> usize;
}
pub struct TapInfo {
    pub first_sample: u64,          // engine-clock index of the tap's first sample, at the tap's rate
    pub rate: u32,
    pub capture_offset: Seconds,    // how much later than the raw voice this point hears the same sound (section 5.1)
    pub honours_mute: bool,
}
```

- **The engine ships the ring.** `AudioEngine::tap_ring(req)` returns a `TapReader`. It is the SPSC ring
  (`crates/omuzaki-audio/src/ring.rs`: `AtomicU32` slots, safe Rust, release/acquire counters, a dropped counter),
  generalised with the `Overwrite` policy and moved into the API crate, so consumers in any crate can read it.
  `TapReader::read` returns the samples, their `start_sample`, and any **gap** (samples dropped or never captured)
  before them, so a consumer never splices across a loss without knowing.
- **A consumer that already owns a ring** passes a `TapSink` instead (`AudioEngine::tap`).
- **Formats.** `Speech16k` is mono 16 kHz from a fixed 3:1 polyphase decimator, which is the only format Silero VAD and
  Whisper take: linear phase, about 100 taps, flat to 7 kHz, at least 70 dB down above 9 kHz. Its group delay (about
  1 ms) is in `capture_offset`. Every other tap is 48 kHz.
- **Default capacities:** 10 s for recognition (the speech host's default `ring_seconds`), 2 s for recording, 0.5 s
  for lip sync and cues.
- Taps are counted on the health meter: samples dropped per consumer.

### 4.2 The voice taps of PF-12

Each consumer picks a **voice signal**, and the engine resolves it to a tap point from what is bound:

| Signal | Two inputs (raw mic + changer output) | One input, our own chain (BR-24 or ML-14) | One input that is already changed |
|---|---|---|---|
| `raw` | `Input(raw_voice)` | `BusPreChain(mic)` | not available: falls back to `heard`, and the UI says "recognition uses the changed voice" |
| `heard` | `BusPostFader(mic)` (fed by the changer) | `BusPostFader(mic)` | `BusPostFader(mic)` |

| Consumer (`VoiceUse`) | Default | Reads | Policy |
|---|---|---|---|
| `mouth`: lip sync (FE-08, ML-05) | `heard` | 48 kHz mono | `Overwrite` |
| `cues`: loudness, onsets, laughter (PF-08) | `heard` | 48 kHz mono | `Overwrite` |
| `captions`: ASR (BR-13, ML-03) | `raw` | 16 kHz mono | `DropNewest` |
| `listen`: the AI performer's listening (AP-03) | `raw` | 16 kHz mono | `DropNewest` |

These defaults are the research's and the spec's. The recognition gap on changed voices is measured in tests (the
speech host on public-domain clips, dry against shifted), not assumed.

**Mute is privacy, and means nobody hears, the AI included** (accepted review answer 2). People mute to say something
private. When the person mutes `mic`:
- every voice tap of that voice goes silent and is flagged as muted, including `raw` taps that sit before the fader and
  the `listen` tap;
- if viewers can't hear it, there are no captions of it and the mouth doesn't move.

**"Talk to co-host only"** is a separate, explicit mode for speaking privately to the AI performer. The `mic` bus has
three states: `Live`, `Muted` and `CohostOnly`.
- **In `CohostOnly`:**
  - `mic` is muted to `program`, and `mouth`, `cues` and `captions` are silent, as with `Muted`;
  - only the `listen` tap is open;
  - the AI performer's replies in this mode go to `monitor_out` only (its `ai_voice` send to `program` is muted while
    the mode lasts), so the whole exchange stays private.
- **It needs a monitor output.** With no `monitor_out` bound, the mode can't be entered and the UI says why.
- **It is shown, never hidden.** A clear indicator stays on the stream deck, the mixer strip and the health meter
  ("Private: co-host only"), and the engine reports the state in `AudioStatus` and as an `AudioEvent`.
- **Leaving it returns to `Muted`, never straight to `Live`,** so ending a private talk can't put the mic on stream by
  accident.
- **It is never the default, and agents can't turn it on** (section 7.2). An agent may turn it *off*, which only makes
  things more private.
- **Barge-in** cuts the AI voice by closing its `ai_voice` feed, and the `listen` tap stays open while the AI speaks; a private turn applies no intents (`ai-performer.md` §5.4, §5.5).

### 4.3 How the speech host gets its input

The speech host already takes 16 kHz mono through `AudioInput::push`: an SPSC ring that drops the newest samples
when full, never allocates, and wakes its VAD thread. That *is* a `TapSink`, so it is wired without being rewritten:
- `omuzaki-speech` adds `impl TapSink for AudioInput` (a forwarding method);
- the app opens `TapRequest { point: Voice(Captions), format: Speech16k, policy: DropNewest, .. }` with that sink.

One follow-up keeps it DRY: once the API crate carries the ring, the speech host uses it and deletes its own copy.

### 4.4 Feeds

`AudioEngine::feed(req) -> FeedWriter` is the mirror of a tap: an SPSC ring *into* a bus, written by a producer thread
(TTS, a music decoder). An underrun plays silence and is counted. A feed declares its rate, and the engine resamples it
at the edge. TTS writes the AI voice ahead of time; the soundboard and music use the player source (BR-21), whose
decoded samples are shared buffers made off-thread.
The AI performer's TTS host writes two alternating feeds into `ai_voice` and drives its mouth from the PCM ahead of playback, not from the bus tap (`ai-performer-voice-face.md` §3, §4.1, ADR 0041). Its private turns go to a third feed on an `ai_voice_private` bus whose only output is `monitor_out`, so they never depend on how long `CohostOnly` lasts (§3.2 there).

### 4.5 Streaming model slots in the graph (ML-14)

The model node (`model.stream`, in `omuzaki-audio`) plugs an ML-01 streaming slot into a chain. It uses `omuzaki-ai-api`'s
`BlockSink` / `BlockSource` exactly as ADR 0017 defines them and adds only the audio side:
- **Format:** it resamples the bus to the model's `StreamSpec` rate, cuts model blocks, pushes block `k` and takes
  block `k − queue` in the same call (the deadline is the take, counted in blocks, so tests are deterministic), then
  resamples back.
- **Latency:** fixed while in the graph: one model block (accumulation), plus `StreamSpec::added_latency(queue)`, plus
  both resamplers' delays, padded to a whole number of samples.
- **Fallback:** on `Block::Missed` it plays the **time-aligned dry voice**, delayed by the same latency through its own
  delay line, with a 2.5 ms crossfade at each change. While `StreamStats::stalled`, it stays dry. The health meter shows
  missed blocks and "voice conversion stalled: dry voice". Fallback models are the slot's business, not the node's.
- **User models:** a model that is not acknowledged (ADR 0016), or one that imitates a real person without a consent
  record (SE-05), is refused when the node is created, with the reason. The node never loads models itself; it asks
  the registry for a slot session.

Streaming models *outside* the graph (streaming ASR, ML-05 voice to face) are ordinary tap consumers. They push to their
`BlockSink` from their own thread.

### 4.6 Echo cancellation for speaker users (accepted review answer 3)

The co-host conversation must not hear its own voice. BR-22's "without echo" covers our digital routing. A person on
**speakers**, though, feeds the AI voice back into the mic. Without echo cancellation:
- the `listen` tap hears the AI itself;
- barge-in (AP-03, under 200 ms) false-triggers;
- the human's mouth moves with the AI's voice;
- viewers hear the AI twice.

So an acoustic echo canceller is part of the engine:
- **What it is:** a node kind, `fx.aec`, first on the `mic` bus, and on the `raw_voice` input when one is bound. It sits
  before every voice tap, so `raw`, `heard`, `listen` and `captions` all read the cancelled signal.
- **Its reference** is the signal Omuzaki sends to the speaker device: the bus bound to `monitor_out`, or `program` when
  `program_out` is the speaker. When the speakers are fed outside Omuzaki (OBS monitoring), the reference is `program`,
  and the canceller's own delay estimator must find the lag. The research checks its range.
- **When it is on:** when the person says they use speakers, or when a bound output is a speaker device. It is off with
  headphones, where it would only cost CPU and latency.
- **Its latency** (the canceller's frame, about 10 ms for AEC3-class designs) is declared like any node's. It counts in
  the routes and the A/V delay but not in `voice.fx`, which is the BR-24 chain.
- **The implementation** is picked by research: permissive only (WebRTC AEC3 under BSD-3, speexdsp under BSD, or
  our own), with CPU against `cpu.total`.
- **A limit:** a changer outside Omuzaki transforms the echo non-linearly (pitch, conversion), so it can't be cancelled on
  the changer's output. In the two-input setup the `raw` path is cancelled (enough for `listen` and `captions`), and
  the UI recommends headphones for the `heard` path.
- **A cheaper mitigation** stays as a fallback when the canceller can't converge: the `listen` tap is gated while the
  AI speaks.

## 5. Time, the A/V delay, the clap test and drift (PF-12, PF-26)

### 5.1 One clock and performance time

- **`EngineClock`** counts samples at 48 kHz on the clock device. It maps a sample index to the pipeline's monotonic
  time (`omuzaki_core::Clock`, the timebase of `TrackFrame::time`) through a delay-locked loop fed with callback
  timestamps, the way JACK does. Readers get a lock-free snapshot: `time_of(sample) -> Seconds` and
  `sample_at(Seconds)`. Recording uses it to put audio and video on one timeline. When the clock device changes, the
  mapping stays continuous.
- **Performance time** is when the person actually made a sound. Every tap reports `capture_offset`, how much later
  than the raw mic its point hears the same sound:

  | Tap point | `capture_offset` |
  |---|---|
  | raw mic | 0 |
  | post-chain | the chain's declared latency |
  | a changer's output | the measured changer latency (section 5.3) plus the chain |

  `fx.aec`, when on, adds its latency to every row.

  A consumer stamps what it makes (cues, visemes) with `perf_time = time_of(sample) − capture_offset`. Tracker frames
  are already stamped with capture time. **Everything about the human performance shares one timeline.**

### 5.2 The A/V delay belongs to the performance pipeline

PF-12's delay "holds the performance channels back", so it lives where those channels are mixed:
- `omuzaki-anim-api` gains a fixed-capacity, `Copy`-item `PlayoutDelay` (a ring of timestamped layer values per source,
  sampled at `now − D` with interpolation between neighbours).
- **One delay for the whole performance stream** (accepted review answer 5). `omuzaki-stage` runs it before
  `LayerStack::resolve`, for **every** layer source, so tracking, hotkey expressions, automation triggers on the avatar
  and audio-derived layers stay coherent with the voice viewers hear.
- **The stamp is what differs per source, not the delay:**
  - tracker frames carry their capture time;
  - hotkeys and triggers carry the time they fired;
  - audio-derived layers carry `perf_time` (section 5.1);
  - AI-driven layers, whose timing belongs to the AI voice and not to the human's, are stamped `program_time − D`, so
    after the one delay they land on time with the AI voice.
- **Not delayed:**
  - UI feedback (a hotkey button lighting up, the trigger list, meters);
  - anything outside the avatar: scene switches, OBS actions, overlays and alerts.
- The life layer and physics run in real time after the blend. Nothing renders twice, so this is cheaper than delaying
  video frames, as PF-12 says.

The audio engine owns none of it. It supplies the number:
- **`D = auto`:** the clap-test measurement of the external changer, plus the declared latency on the `heard` path to
  the program output. That path includes our own chain (`fx.aec`, BR-24, ML-14) and the `program` limiter.
- **Presets** (when the clap test is skipped): our effects 20 ms, a Beatrice-class model 60 ms, an RVC-class model
  200 ms (research §5).
- **Manual:** 0 to 500 ms.
- When `D` changes (a preset adds the shifter, a new measurement arrives), the playout **ramps** to it at
  most 10% faster or slower, so the face never jumps: a change of Δ takes Δ / 0.1 with no minimum, so 20 ms takes 0.2 s and 200 ms takes 2 s. The setting is a document field owned by the perform settings
  and set by `perform.av_delay.set` (section 7).

### 5.3 The clap test: onset-envelope correlation

A changed voice has a different waveform, pitch and formants from the raw one, so waveform cross-correlation
(including GCC-PHAT) finds nothing. **The timing of onsets survives:** a clap, a plosive or a syllable starts at the
same moment in both. The estimator correlates onset envelopes, not samples:
1. **Capture** 6 s of `raw` and `heard` (both `DropNewest` 48 kHz taps; their samples are on one engine timeline,
   section 3.3) while the UI asks the person to clap five times. "Say 'ta' five times" is offered too, because some
   changers mangle a clap but keep a syllable.
2. **Onset strength per signal:**
   - three bands (150–600 Hz, 600–2,400 Hz, 2,400–8,000 Hz, second-order band-passes);
   - log energy per band in 4 ms windows at a 1 ms hop;
   - the first difference, half-wave rectified, summed over bands;
   - minus a 100 ms moving median, clamped at zero.

   Bands keep it robust when formant shifting moves energy between regions.
3. **Cross-correlate** the two envelopes (normalised: zero mean, unit variance) for lags from −50 ms to +600 ms. Take
   the peak and refine it by parabolic interpolation to about 0.2 ms.
4. **Validate before trusting it:**
   - the peak's normalised correlation is at least 0.4;
   - the next-best peak more than 10 ms away is at most 0.7 of it;
   - after shifting, at least four onsets pair up within ±3 ms;
   - the per-onset lags have a spread (MAD) of at most 5 ms.

   Otherwise the test reports why ("only 2 claps heard in the changed voice") and changes nothing.
5. **Report** `LatencyEstimate { lag, confidence, jitter, onsets_matched, rejected_reason }`.
   - A validated estimate is stored with `AudioEngine::set_changer_latency` (section 3.7), so `capture_offset` and
     `D = auto` use it.
   - Applying it as the delay is a separate `perform.av_delay.set { source: measured }`, so one writer owns the delay.

The estimator is a pure function in `omuzaki-audio` (`latency/onset.rs`). It is tested on synthetic pairs: claps and
plosive bursts through a known delay plus our pitch shifter, a formant shift, a random timbre filter and noise. The
acceptance is ±2 ms for delays from 0 to 500 ms.

### 5.4 Drift monitoring (PF-26)

With both `raw` and `heard` bound, the analysis thread re-runs the estimator in the background:
- every 10 s, on the last 4 s, only when both taps carry speech (an energy gate; the speech host's VAD events when it
  runs);
- it keeps the median of the last five estimates that pass validation.

The health meter shows **the delay in use, the latest measured latency and the difference**. It warns when the
difference stays over 20 ms (about two thirds of a 30 fps frame) for three estimates in a row, with "re-run the clap
test" or "use the measured value" (one click, a person's or an agent's `perform.av_delay.set`). With only a changed
input there is nothing to compare, and the meter says "changer latency: not measured".

It also shows each bridge's clock drift (ppm), underruns and overruns, tap drops per consumer, model-node missed
blocks, and device state.

### 5.5 Route latency (BR-22)

`GraphReport` and `audio.latency.report` give, for every route (source → output or tap):
- declared graph latency;
- bridge buffering in and out;
- the device's own latency (cpal's timestamps: capture to callback, callback to playback);
- the measured latency, when a loopback or clap measurement exists.

The soundboard's line is BR-21's "latency shown". The `voice.fx` figures are the chain's declared latency plus one
block.

## 6. Voice effects (BR-24)

Node kinds in `omuzaki-audio-fx`. All are ours. The BR-24 effects are mono (the `mic` bus), with no look-ahead except
the shifter. The last two rows are the non-BR-24 kinds the default buses use (section 3):

| Kind | What | Latency |
|---|---|---|
| `fx.gate` | noise gate: hysteresis (open and close thresholds), hold, attack and release, side-chain high-pass at 100 Hz | 0 |
| `fx.eq` | up to 8 RBJ biquads (high-pass, low-pass, shelves, peak), transposed direct form II, `f64` state | 0 |
| `fx.compressor` | feed-forward, peak or RMS detector, soft knee, attack and release, make-up gain | 0 |
| `fx.robot` | ring modulation (30–200 Hz) plus a short feedback comb (2–12 ms) for the metallic ring | 0 |
| `fx.radio` | telephone (300–3,400 Hz) and radio (AM-style band, mid boost) presets: band-pass, soft saturation, optional bit and rate crush, optional seeded noise bed | 0 |
| `fx.pitch` | pitch and formant shift, our own low-latency design (below); a monotone mode (fixed target F0) for the classic robot voice | ≤ 912 samples (19 ms) |
| `model.stream` | an ML-14 slot (section 4.5) | from the model |
| `fx.limiter` | the `program` safety limiter: true peak (4x oversampling), −1 dBTP, 1.5 ms look-ahead, gain reduction to the meter (section 3.5) | about 2 ms, on `program`, not in `voice.fx` |
| `fx.aec` | acoustic echo canceller, first on `mic` and `raw_voice` when speakers are in use (section 4.6); implementation picked | its frame (about 10 ms), not in `voice.fx` |

**The canonical chain** is `gate → eq → compressor → pitch → robot | radio → model.stream`. A preset may leave any of them
out.

**Budgets:**
- `voice.fx` = 0 + one block = 5.3 ms ≤ 10 ms.
- `voice.fx.pitch` = 19 + 5.3 = 24.3 ms ≤ 25 ms.
- `voice.fx.cpu` (≤ 0.5% of 8 cores, about 830 ns per sample on an `R` core) is benched on a Dev E-core. The shifter's
  share is about 60% of it.

### 6.1 The pitch and formant shifter (own design, `voice.fx.pitch`)

Phase vocoders need about one FFT window of latency (21 ms for `pitch_shift`, 100+ ms for Signalsmith). A
**pitch-synchronous** design needs only about one pitch period of look-ahead:
1. **F0 tracking** on the past signal plus the look-ahead: a YIN-style cumulative mean normalised difference, with an
   F0 floor of 70 Hz (a 14.3 ms period), a 0.15 threshold, parabolic refinement, continuity against octave jumps, and
   a voiced/unvoiced decision.
2. **Pitch marks** at glottal closures: peaks of the LPC residual, searched within each predicted period.
3. **Source-filter split per grain:** LPC on pre-emphasised, two-period Hann grains (order chosen by measurement, about
   24–36 at 48 kHz, or on a decimated analysis path). Inverse filtering gives the residual (the excitation).
4. **Pitch: TD-PSOLA on the residual.** Synthesis marks are spaced `T0 / α`, and each takes the nearest analysis
   grain. Unvoiced stretches use a 5 ms pseudo-period with seeded ±10% jitter and no pitch change.
5. **Formants: warp the envelope independently.** Convert LPC to line spectral frequencies, scale them by the formant
   factor β (with clamping and minimum spacing), convert back, and interpolate LSFs between grains so the filter never
   clicks. Then refilter the shifted excitation and de-emphasise. Pitch moves without the chipmunk sound, because the
   envelope stays put unless β moves it.
6. **Latency** = one maximum period plus the mark-confirmation margin: ≤ 19 ms, declared exactly.

The task issue carries the measured acceptance: latency, F0 accuracy, formant accuracy, glides without octave errors,
no clicks, block-size invariance, no allocation, and CPU.

### 6.2 Presets per character

A preset is data: a name and the chain (`NodeSpec`s with parameters).
- **Storage:** with the character (`characters/<id>/voice.toml`), so a character's voice travels with it (the TTS voice assignment is a separate `voice-assignment.toml`, `ai-performer-voice-face.md` §3.4). The active
  preset is a perform setting.
- **Switching:** by hotkey, scene or op. Each is a call to `audio.fx.apply`, which is a parameter change when the
  chain's shape matches and a structural one otherwise (section 3.3).
- **A/B for agents:**
  - `audio.fx.render` renders a **reference clip** (public domain, shipped) or a synthetic voice through a preset
    offline, and returns the audio as a resource plus measurements: median F0, formant peaks, loudness (LUFS),
    spectral centroid, latency.
  - `audio.fx.compare` returns two presets side by side with the differences.
  - Agents never receive the person's own voice unless the person sends it (section 7).

## 7. Ops and the agent surface

### 7.1 The ops

Registered by `omuzaki-audio` (`audio.*`), `omuzaki-audio-fx` (`audio.fx.*`) and the perform settings (`perform.av_delay.*`).

**Persistence:**
- The live engine follows the document: bus, tap, ducking and preset settings are document edits (`Undo::Auto`). The
  app recompiles the graph when they change.
- Device bindings live in the machine profile (`Undo::Explicit`).
- Live actions (meters, the clap test, renders) call the `AudioEngine` service (ops-api `Services`).

| Op | Kind | Scope | Agent may? | Undo |
|---|---|---|---|---|
| `audio.device.list`, `audio.device.status` | query | read | yes | none |
| `audio.device.bind`, `audio.device.unbind` | edit (profile) | perform | **person only** | explicit |
| `audio.bus.list`, `audio.tap.list`, `audio.meters`, `audio.latency.report` | query | read | yes (numbers only) | none |
| `audio.bus.create`, `audio.bus.delete` | edit | perform | yes, unless egress grows (7.2) | auto |
| `audio.bus.set_gain` | edit | perform | yes, up to the bus's person-set ceiling (default 0 dB) | auto |
| `audio.bus.mute` | edit | perform | mute: yes. Unmute of a bus carrying captured audio to an egress: **person only** | auto |
| `audio.bus.route` (sources, sends, outputs) | edit | perform | yes, unless egress grows | auto |
| `audio.bus.duck` (set or remove a rule) | edit | perform | yes | auto |
| `audio.bus.limiter { on, ceiling_dbtp }` (the `program` safety limiter) | edit | perform | yes | auto |
| `audio.mic.mode { live \| muted \| cohost_only }` | edit | perform | `muted` from any state: yes. `cohost_only` and `live`: **person only** (section 4.2) | auto |
| `audio.aec.set { auto \| on \| off }` (speaker use) | edit | perform | yes | auto |
| `audio.voice_tap.set { use, signal }` | edit | perform | yes (local consumers) | auto |
| `audio.clap_test.run { raw, heard }` | job | perform | may start it; the person claps; it returns numbers | none |
| `perform.av_delay.set { auto \| preset \| ms \| measured }`, `.get` | edit / query | perform | yes | auto |
| `audio.fx.kinds`, `audio.fx.preset.list`, `audio.fx.preset.get` | query | read | yes | none |
| `audio.fx.preset.save`, `.set_param`, `.rename`, `.delete` | edit | edit:character | yes | auto |
| `audio.fx.apply { preset \| dry }` | edit | perform | yes, except a `model.stream` node with an unacknowledged model or a real person's voice (ADR 0016, SE-05) | auto |
| `audio.fx.render`, `audio.fx.compare` | job | render | yes on reference clips. On the person's recordings: **person only** | none |

The soundboard's ops (`audio.sound.*`: add, set, remove, play, with cooldowns and provenance) are BR-21's. Playing a
sound is `perform`, and agents (the AI performer included) may do it within cooldowns: it is content, not captured
audio.

### 7.2 What needs a person (SE-04 for audio)

The rule: **audio never leaves the device unless a person routes it**. Concretely:
1. **Captured audio** is anything from an input device: mic, changer, line in, game loopback, named as
   `CapturedSource(DeviceRole)`. An **egress** is an output device, a network sink (direct stream, collab, a hosted
   connector), or an agent.
   - **Output devices are graph edges.** `egress_pairs(&GraphSpec, &Bindings)` covers them as
     `Egress::Output { role }` (`#[non_exhaustive]`). `Bindings` holds the bound input roles, the bound output roles
     and the `MicMode`.
   - **Its cuts:**
     - a muted bus cuts every edge leaving it, pre-fader sends included;
     - a gain of minus infinity cuts the post-fader sends and the outputs;
     - a send at minus infinity cuts itself;
     - `Muted` and `CohostOnly` cut `mic` entirely;
     - in `CohostOnly`, a path through `ai_voice` may end only at `monitor_out`;
     - ducking never cuts.
   - **Network sinks are not edges.** They take audio through taps marked `leaves_device`, and those are opened only by
     features a person enabled: going live needs the `broadcast` grant a person gives, and a hosted connector carries
     the person's grant. `AudioStatus` lists every open `leaves_device` tap, and the health meter shows "audio leaving
     the device: …".
   - **Agents** never receive captured audio (rule 4).
2. **An agent can never grow the set of (captured source → egress) pairs.** Every op that edits the graph computes that
   set over the `GraphSpec` before and after, with mute and gain −∞ cutting the edge. If it grows and the actor is not
   `Actor::Person`, the op is `Denied` with "needs a person: this would send <mic> to <program_out>", and is queued for
   approval once approvals land in ops-api. Making things quieter or more private (mute, unroute, lower) is always allowed.
3. **Device binding and unbinding are person-only**, inputs included: an agent never opens a microphone.
4. **Captured audio is never returned to an agent** as audio. Meters, latency and drift are numbers and are fine.
   Transcripts reach the AI performer only through the listening the person turned on (AP-03), and audio reaches a
   hosted connector only through an egress the person granted.
5. **A person's mute wins:** taps honour it, `listen` included (section 4.2). An agent's undo of its own mute restores
   the person's earlier state, so it is allowed.
6. **Only a person enters "talk to co-host only"** (`audio.mic.mode cohost_only`): it opens the AI's ears while the
   person believes they are private. An agent may end it, and ending it lands on `Muted`.

These checks live in the ops (plain `ctx.actor` tests) until ops-api grows a declared "person only" rule. That is
noted, not invented here.

## 8. Crates

```text
 omuzaki-core ◀── omuzaki-audio-api ◀──┬── omuzaki-audio-mock
                    ▲   ▲          ├── omuzaki-audio      (engine; also depends on omuzaki-ai-api, omuzaki-ops-api, omuzaki-doc-api)
 omuzaki-ai-api ──────┘   │          ├── omuzaki-audio-fx   (effect nodes; omuzaki-ops-api, omuzaki-doc-api)
                        │          ├── omuzaki-speech     (renamed; omuzaki-ai-api after the ML-01 migration)
                        │          └── omuzaki-voice      (PF-08 / FE-08 consumers; omuzaki-anim-api)
                        └── omuzaki-stream, omuzaki-stage, omuzaki-ui (consumers / wiring)
```

- **The API crate is wasm-clean.** `omuzaki-audio` (cpal, native threads) opts out with `wasm = false`, and so does
  `omuzaki-speech` (whisper.cpp). The web player uses WebAudio directly and can reuse `omuzaki-audio-fx` nodes, which are
  plain DSP.
- **The speech host is its own crate, `omuzaki-speech`, unchanged except its name.** It was built as `crates/omuzaki-audio`, as
  the speech host and nothing else (asr, vad, segmenter, host, ring, catalogue).
  - **Different dependencies:** ort and whisper.cpp, against cpal and DSP.
  - **Different thread class:** model workers, against the audio callback.
  - **Different owner next:** the ML-01 design moves its VAD and ASR onto registry slots.
  - **The fences stay apart,** so engine tasks and speech tasks never edit the same `lib.rs` or `Cargo.toml`.

  The rename is mechanical (package name, README row, `use omuzaki_audio` in its tests and example) and needs no review
  of the logic. Done as soon as the speech host merges, it frees `omuzaki-audio` for the engine that §9 and §39.1 name.
- **The mixer is `omuzaki-audio`'s, not `omuzaki-stream`'s.** §9 lists "the audio mixer" under `omuzaki-stream`, but there is
  one real-time graph and one clock. `omuzaki-stream` encodes what the program and per-bus taps give it. The §9 rows were
  amended with ADR 0018's acceptance, in this PR.

## 9. Testing

### 9.1 Offline rendering, deterministic

- **`OfflineRender`** (API trait): given a `GraphSpec`, input buffers per source and a **block plan** (fixed sizes or a
  seeded random split), it returns the output per bus, output role and tap. No devices, no threads, no clock.
- **Implemented twice:** by the mock (the reference) and by `omuzaki-audio`'s executor in offline mode. A property test
  renders random graphs (seeded) through both, with random block plans, and requires them to agree to 1e-6. Each must
  also be bit-identical across runs and across block plans.
- **Test signals are generated in code with a seeded RNG:** a synthetic voice (an LF glottal pulse train through formant
  resonators, with F0 and vowel trajectories), claps, plosive bursts, sweeps and noise.
- **Real speech** comes from public-domain LibriVox clips that are not committed, found through an env var (the
  pattern). Private test clips (any recording of a real person) never enter fixtures, goldens or PR evidence.

### 9.2 Node conformance (every registered kind, in `omuzaki-audio-mock`, never in the contract)

Each check runs at three parameter settings (default, min, max):
- **Latency honesty:** the impulse or click-train onset lands at the declared latency, within one sample (1 ms for
  `fx.pitch`).
- **Block-size invariance:** blocks of 1, 7, 64, 256, 1024 and random sizes give bit-identical output.
- **No allocation in `process` and `set_param`:** a counting global allocator, as in `tests/no_alloc.rs`.
- **Finite and stable:** no NaN or Inf over a minute of white noise, and silence in decays to below −120 dBFS.
- **Reset:** after `reset`, output equals a fresh node's.
- **Parameter sweeps:** sweeping every parameter across its range mid-signal produces no step bigger than the
  smoothing allows (a click detector on the second difference).

### 9.3 Engine tests

- **Swap:** structural edits mid-stream (random add, remove, reorder) never allocate on the audio thread (the counting
  allocator on that thread). Surviving nodes keep their state, and only changed buses fade.
- **Real-time hygiene:** a 10-minute soak on the fake backend with a counting allocator and a lock-detection wrapper
  over the audio thread. No allocation, no lock, no growing queue.
- **Devices (fake backend):**
  - loss → silence, a flagged gap and an event;
  - return → reconnect and fade in;
  - a reordered enumeration → the same binding;
  - same name with a new id → `PossibleMatch`, and no rebind;
  - clock device lost → the timer clock takes over with no timeline jump;
  - two inputs at ±200 ppm → the bridges hold their fill with no underrun over 10 minutes.
- **Taps:** gaps are reported exactly, `DropNewest` keeps contiguity, `Overwrite` never blocks the writer, and the 16 kHz
  decimator meets its response mask.
- **Clap test:** ±2 ms on the synthetic changer matrix, and it rejects when there are too few onsets.
- **Drift:** the monitor warns within three estimates after a step change of 40 ms, and never on a steady state.
- **Budgets:**
  - `voice.fx` and `voice.fx.pitch` from declarations at 48 kHz with a 256 block (a unit test);
  - `voice.fx.cpu` as a criterion bench of the full chain pinned to a Dev E-core, joined to the budget harness.
- **The privacy rules:** each op's egress check, table-driven, with agent and person actors; every `MicMode`
  transition with both actors; a project saved in `CohostOnly` reopens `Muted`.
- **The program limiter:** the true peak never exceeds the ceiling + 0.1 dB, measured by an independent 16x
  reconstruction; transparent below the ceiling.
- **Echo cancellation:** ERLE of at least 25 dB on simulated rooms, near-end preserved during double talk, and the
  co-host's own voice no longer triggers `listen`.

### 9.4 Loopback on Dev

A manual, scripted run, recorded in the PR that adds it and repeated before releases:
1. **Electrical loopback:** a cable from the interface's output to its input (the tester's hardware).
2. **Virtual loopback:** a virtual cable (VB-CABLE class, test-only, never shipped or required).
3. **What it measures:**
   - the round trip in WASAPI shared mode at requested buffers of 64, 128, 256 and 512, by the onset estimator on a
     click train;
   - `program_out` → `voice` route latency against the report (declared plus device must match the measurement within
     ±1 ms);
   - the `fx` chain's added latency against `voice.fx` and `voice.fx.pitch`;
   - the bridge's drift between the two devices over 30 minutes.
4. **Output:** a table in the PR. If the shared-mode round trip exceeds 30 ms at 256 frames, the native WASAPI backend
   task (exclusive / `IAudioClient3`) goes ahead; otherwise it is closed with the numbers.

## 10. Interfaces with the other contracts

- **`omuzaki-ai-api` (ADR 0017):**
  - in the graph, the model node uses `BlockSink` / `BlockSource`, `StreamSpec`, `StreamStats` and `Block::Missed`;
  - outside it, consumers push from tap threads.
  - Nothing here defines a model, a deadline or a fallback model.
- **`omuzaki-anim-api`:** the new `PlayoutDelay` (section 5.2); `Cues` and the audio `Layer`, stamped with performance time
  by `omuzaki-voice`.
- **`omuzaki-stream`:** reads `program` and per-bus post-fader taps (`DropNewest`, 2 s) and maps samples to video time with
  `EngineClock`. A recording gap is filled with silence and flagged, so no frame is dropped (BR-18).
- **`omuzaki-ops-api`:** `AudioEngine` is a `Service`; `Actor` for the privacy rules; approvals arrive.
- **`omuzaki-doc-api`:** bus, tap and ducking settings in the project; presets in the character; the A/V delay in the
  perform settings.
- **`omuzaki-core`:** `Clock`, `Seconds`, ids.

## 11. Decisions taken on review (2026-10-12)

| # | Question | Decision |
|---|---|---|
| 1 | Rename crate | **Yes:** `omuzaki-speech` after merges. |
| 2 | The AI hearing the person while muted | **No.** Mute means nobody hears, the AI included, because people mute to say something private. "Talk to co-host only" is a separate, explicit, always-shown `MicMode` that is never the default and that agents can't enter (section 4.2). |
| 3 | Echo for speaker users | **Echo cancellation ships**, P0 for the AP-03 co-host loop: the co-host must never hear its own voice. Research picks a permissive implementation (WebRTC AEC3 BSD-3, speexdsp BSD, or ours), and `fx.aec` is built on it (section 4.6). |
| 4 | A native WASAPI backend | **The Dev loopback test decides** (section 9.4). |
| 5 | ASIO | **Stays banned.** |
| 6 | Hotkeys and the A/V delay | **One delay for the whole performance stream.** Hotkey expressions, triggers and tracking stay coherent with the voice viewers hear. UI feedback is immediate, and scene, OBS and overlay actions are outside the avatar and never delayed (section 5.2). |
| 7 | A program limiter | **On by default:** a transparent true-peak safety limiter at −1 dBTP on `program`, with its gain reduction on the meter and an off switch (section 3.5). |

### 11.1 Questions from the `omuzaki-audio-api` implementation (design review, 2026-10-12)

None of these changes a decision of ADR 0018.

| # | Question | Decision |
|---|---|---|
| 1 | `create()` has no channel count | Kept as the PR has it: `create(config, channels)`. |
| 2 | `Bindings`, `CapturedSource`, `Egress` | Kept as the PR defines them. `Egress` becomes `#[non_exhaustive]`. Network sinks go through `leaves_device` taps (section 7.2). |
| 3 | Cuts in `egress_pairs` | Kept as the PR has them, written into section 7.2. The executor matches them with separate `gain` and `mute` ramps (section 3.6). |
| 4 | Send-gain smoothing | The fader ramp (240 samples). |
| 5 | Duck envelope | Pinned in section 3.6: a 50 ms level follower, a 1,024-sample trigger delay, hold, dB moved at fixed rates, factors summed in dB. |
| 6 | Mono and stereo constants | `1/√2` both ways, as the PR has it. |
| 7 | Stereo sidechain | `(L + R) / 2`, from the previous block, the one block-plan-dependent signal (section 3.6). |
| 8 | Node id scope | Unique per bus. |
| 9 | The `AudioEngine` method set | Section 3.7: no `clap_test` (`set_changer_latency` instead), `subscribe()` instead of `drain_events()`, a closable `TapHandle`, `set_mic_mode` returns the mode, `meters(&MeterPoint)`, and `apply` diffs. |
| 10 | Backend signatures | Kept: interleaved `f32`, `CallbackTime` in the pipeline clock's timebase. |
| 11 | Ring metadata, and flags on sinks | The PR's second `AtomicU32` per slot is kept. `TapSink::flags` is added, a provided no-op. |
| 12 | ADR 0017 types | Correctly absent: the model node is `omuzaki-audio`'s. |
| — | `conformance.rs` in the contract | It moves to `omuzaki-audio-mock`. |

## 12. Tasks

All are in Phase 2. The design is accepted; each task waits only on its listed blockers. Each is a sub-issue of its
feature.

| Area | Tasks |
|---|---|
| Contract and test kit (BR-22) | `omuzaki-audio-api` · mock I: offline engine · mock II: fake backend, signals, conformance |
| Engine (BR-22) | compiler and executor · structural swaps · ducking and meters · `audio.bus.*` ops with the egress rule · `fx.limiter` (program safety limiter) · Dev loopback · research: echo cancellation (P0) |
| Co-host loop (AP-03) | "talk to co-host only" mic mode · `fx.aec` echo cancellation |
| Devices, taps, delay (PF-12) | devices (cpal) · bridges and clock · taps, feeds, voice taps · speech host on the tap · clap test · `PlayoutDelay` (anim-api) · stage wiring and `perform.av_delay.*` · device, tap and readout ops |
| Health meter (PF-26) | drift monitor and audio health readouts |
| Effects (BR-24) | crate and `fx.eq` · gate · compressor · robot · radio and telephone · F0 and pitch marks · pitch and formant shifter · preset ops · budget gates |
| ML-14 | `model.stream` node |
| Soundboard (BR-21) | player source · file decoding |
| Voice cues (PF-08) | `omuzaki-voice` cues |
| Speech host (ML-03) | rename to `omuzaki-speech` |

