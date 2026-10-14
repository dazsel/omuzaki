# The AI performer's voice and face: TTS into the feed, voices and consent, voice to face, big moves

**Status:** proposed (, 2026-10-13). **Features:** AP-02, AP-05, with CN-10, ML-04, ML-05, ML-07, ML-14,
PF-12, PF-23, PF-24, SE-05, AP-11. **Decision record:** `docs/decisions/0041-ai-performer-voice-face.md`. **Builds on:**
the performer loop (`ai-performer.md`, ADR 0031: §2 `Voice`, `SpeakHow`, `SpeechHandle`; §5.4 barge-in; §8 events), the
audio contract (`audio-api.md` §3.1, §3.7, §4.2, §4.4, §5.2, §7.2), the slots (`ai-api.md` §3, §5, §7, §10), the blend
order (`anim-api.md` §6, §6.1, §7, §10), the LLM seam (ADR 0032) and, pending, the assistant runtime (ADR 0040).

## In one minute

The speech host's **TTS host** turns each sentence the chunker sends into 24 kHz PCM through a `tts@1` session (local)
or a hosted connector, and a **pump** thread writes it 10 ms at a time into one of **two alternating `ai_voice` feeds**,
keeping 300 to 500 ms ahead. A cut closes the feed through a flag the audio thread reads at the next block. The same pump
hands the **mouth drive** each slice **ahead of playback**, stamped with the program time it will play at, so the
`voice_to_face@1` model's latency is hidden instead of making the mouth late; visemes (FE-08) cover a stalled model. A
rule-based **big-moves planner** turns the floor state and the speech into lean, head, body-turn and gaze keys inside the
score layer, so a human tracker above it always wins. A voice belongs to the character (`characters/<id>/voice-assignment.toml`),
is validated against the model's or connector's voice list, and a cloned voice needs a consent record.

```text
 chunker ─▶ Voice::speak ─▶ synth thread ─(PCM 24 kHz, ≤ 2 ahead)─▶ pump (10 ms) ─▶ FeedWriter a|b ─▶ ai_voice ─▶ program
            (SpeakHow)      tts@1 | hosted                            │ playhead: Ready, Playing{from}, Done, Cut
                                                                      └─ VoiceLead (at, pcm, private) ─▶ mouth drive
 floor + TalkState + StyleKnobs ─▶ BigMovePlanner ─▶ score layer      voice_to_face@1 │ visemes ─▶ audio layer (Max)
```

## 1. Scope

Here: the `Voice` implementation, voices and their document, consent, hosted TTS and its money, what speech-to-speech
means, the face drive, big moves, tests. Not here: the loop, persona, intents (ADR 0031); TTS model runtimes and the G2P
(ML-04); the A2F counsel review; the viewer chat reader (AP-11), which only shares the slot (§3.6).

## 2. Types (D1), in `omuzaki-performer-api`

```rust
pub struct VoiceAssignment { pub character: CharId, pub voice: VoiceId, pub provider: VoiceProvider,
                             pub speed: f32, pub pitch_semitones: f32, pub consent: Option<ConsentRef> }
pub enum VoiceProvider { Local, Hosted(ConnectorId) }
pub struct TalkState { pub speaking: bool, pub question: bool, pub laughing: bool, pub since: Seconds }
pub struct MoveContext { pub floor: Floor, pub addressee: Addressee, pub look_held: bool, pub private: bool }   // Copy
pub enum Floor { Idle, Listening, Thinking, Speaking }
pub enum Addressee { Camera, Chat, Member { yaw: f32 } }       // yaw: radians from the member to the other one
pub struct StyleKnobs { pub energy: f32, pub gaze_aversion: f32, pub lean: f32, pub turn: f32 }   // 0..1 each
/// Per frame, no allocation: turns the floor and the speech into keys on the channels it may own.
pub trait BigMovePlanner: Send { fn plan(&mut self, tick: Tick, ctx: &MoveContext, talk: &TalkState,
                                         knobs: &StyleKnobs, rng: &mut Rng, out: &mut Layer); }
/// The TTS host's view of the mouth drive: PCM ahead of playback, stamped with its program time.
pub trait VoiceLead: Send { fn attach(&mut self, probe: CutProbe); fn push(&mut self, at: Seconds, rate: u32, pcm: &[f32], private: bool);
                            fn cut(&mut self, at: Seconds); }
/// The drive's own, pump-independent view of one feed: read-only clones of its `FeedCloser` flag and playhead (§4.2).
pub struct CutProbe { /* closer, playhead */ }   // state() -> CutState
pub enum CutState { Live, Raised, Cut { at: Seconds } }   // Raised: close requested; Cut: the audio thread went silent at `at`
pub trait VoiceCatalog: Send + Sync { fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError>; }
pub struct VoiceInfo { pub id: VoiceId, pub languages: Vec<LanguageTag>, pub imitates: Option<String>, pub cloned: bool }
/// One per performer, shared by its LLM brain and its TTS host: bounds reserved together never pass the cap (§3.6).
pub struct SpendGate { /* session, outstanding µ$ */ }   // reserve(bound) -> Result<Reservation, Exhausted>; Reservation::settle(actual)
```

**Deviations from the issue outline, with reasons:**
- The planner's speech input is **`TalkState`**, not `SpeechState`: ADR 0031 already names the `SpeechHandle` state enum
  `SpeechState` (§2 of the loop design), and two types of one name in one crate would not compile.
- `plan` takes a **`MoveContext`** where the outline had `&Intent`: intents are events (an `emote` at 1.28 s), while the
  planner runs every frame on state (who has the floor, who is addressed). Explicit intents still win: an `IkGoals.look_at`
  from a `look` intent sets `look_held`, and a score key on a channel beats the planner (§5.4). It also takes the `Tick`,
  because every rule is timed.
- `SpeechState` (ADR 0031) is kept as is. `Playing { from, duration }` is filled from the feed's playhead (§3.3).

## 3. Voice out (AP-02, D2)

### 3.1 The TTS host

`omuzaki-speech` implements `Voice` with two threads per performer, so a slow synthesis never starves the feed:
- **synth:** runs one `tts@1` `Call` per chunk (or one hosted request), at most **2 chunks ahead** of the one playing.
  A chunk is what the chunker sends: one sentence, ≤ 220 chars (`ai-performer.md` §5.2). Whole chunks only, hosted
  included: ADR 0031 needs `Ready { duration }` before playback, which a streamed synthesis cannot give. Leading and
  trailing silence below −50 dBFS is trimmed to 40 ms; the pump then pads **180 ms** after `. ! ? …` and **80 ms** after
  a first chunk cut at a comma. PCM buffers are allocated here, off the audio thread; at most 3 are live (≈ 3.8 MB at
  220 chars).
- **pump:** every **10 ms** (240 samples at 24 kHz) it tops the active feed up to **500 ms** of lead and never lets it
  fall under **300 ms** while a chunk is ready; it reads the playhead and moves each handle's state; it pushes the
  `VoiceLead` (§4.1).

The feeds: `FeedRequest { id: ai_voice.a | ai_voice.b, rate: 24_000 × 2^(p/12), mono, capacity: 1.0 s }`, both sources
of the `ai_voice` bus. 1 s holds the 500 ms lead plus a 500 ms pump stall.

### 3.2 Gapless speech, underruns and stopping

- **Gapless:** chunk `k + 1` is synthesised while `k` plays; it is gapless when its synthesis takes less than `k`'s audio
  plus the pad. A chunk not ready when the pad ends lengthens the pause (silence, never a cut word: chunks are written
  whole) and counts `late_chunks`; `Playing.from` is measured, so intent timing stays right.
- **Underrun:** a starved pump (OS stall over 300 ms) leaves the feed empty; the engine plays silence and counts it
  (audio-api §4.4). The rest of the chunk then plays late, and `from` is corrected from the playhead.
- **Stopping within one block plus 2.5 ms:** `SpeechHandle::cut()` raises the feed's **`FeedCloser`** flag directly
  from the performer thread (no hop through the pump); the audio thread reads it at the start of the next block and
  fades out over **2.5 ms** (120 samples, the model node's crossfade, audio-api §4.5), so the cut never clicks. Worst
  case on `R`: a block (5.3 ms) plus the fade (2.5 ms), 7.8 ms. ADR 0031 §5.4's "feed closed at the next block" row
  counted 5.3 ms, so **barge-in becomes 167.6 ms** (157.6 ms with headphones), still under the 200 ms budget; a fade
  inside the block would keep 165.1 ms but clicks on short blocks, so the fade is taken. The audio thread publishes the cut
  (`FeedPlayhead.cut_at_sample`, §3.3) when the fade ends; the mouth drive reads that and the closer itself (§4.2),
  never the pump. The pump then drops the queued chunks; a synthesis already
  running finishes and is thrown away (a `Call` cannot be interrupted, `ai-api.md` §3).
- **Two alternating feeds:** after a cut, the next speech goes to the **other** feed, which is open and silent, while
  the cut one is re-opened off-thread (a structural compile, audio-api §3.7). A cut never waits for that compile. A
  second cut before the re-open lands leaves `speak` `Queued` until a feed is open. The resume of ADR 0031 §5.4 (after a
  cough) re-uses the cut chunk's PCM, kept as a one-entry cache keyed by (text, voice, `SpeakHow`): no second synthesis.
- **Private chunks are routed by the request, not by the mic mode.** `SpeakHow.private` audio goes to a third feed,
  `ai_voice.private`, the only source of an **`ai_voice_private` bus whose one output is `monitor_out`** and which has
  no send at all. The mute of `ai_voice` to `program` in `CohostOnly` (audio-api §4.2) only covers the mode's lifetime:
  leaving it for `Muted` while private PCM is buffered or still being synthesised would put it on stream. On its own
  bus it can't reach `program` whatever the mode, and `egress_pairs` proves it (audio-api §7.2: a path from that bus
  ends only at `monitor_out`). A cut closes that feed like the others; with one private feed, a second private cut
  before its re-open leaves `speak` `Queued`. The lead is marked private, so the face does not move (§4.3).

### 3.3 Rate, pitch, speed and the playhead

- **24 kHz to 48 kHz:** the feed declares its rate and the engine resamples at the edge (audio-api §4.4). The TTS host
  never resamples the voice itself.
- **Pitch is the declared rate.** Pitch by `p` semitones is ratio `r = 2^(p/12)`: synthesise at speed `s / r`, declare
  `24_000 × r` Hz, and the edge resampler shifts pitch by `r` and gives back tempo `s`. No pitch shifter, no extra code;
  formants move with it, so `pitch_semitones` is limited to **−4..+4**, and `speed` to **0.7..1.4** (`s / r` must stay in
  the model's own range, Kokoro 0.5..2.0). A model with no speed control ignores `speed`, and pitch then changes tempo by
  `r` (≤ 26 %); the assignment op warns.
- **The playhead (new in `omuzaki-audio-api`):** `FeedWriter::playhead() -> FeedPlayhead { consumed: u64, at_sample: u64, cut_at_sample: u64 }`,
  written once per block by the audio thread (atomics, read wait-free): feed samples consumed so far,
  the engine sample where the next one enters the bus, after the resampler's delay, and `cut_at_sample`, the engine
  sample where a closed feed went silent (`u64::MAX` while open), set by the audio thread when the close fade ends, so
  it is **the authority on what played**. `FeedCloser` and the playhead are `Clone` as read-only probes
  (`is_raised()`, `read()`). This is a contract change: it lands first and alone (`CLAUDE.md`, how work flows, step 3). Then
  `from = clock.time_of(at_sample) + (chunk_start − consumed) / rate`. ADR 0031's `Playing { from }` needs exactly this,
  and the audio contract had no way to say it.

### 3.4 Voices and where the assignment lives

- **The character owns its voice:** `characters/<id>/voice-assignment.toml` (one `VoiceAssignment`), not the persona.
  **Not `voice.toml`:** audio-api §6.2 and `omuzaki-audio-api/src/preset.rs` already keep the character's effect presets
  there, owned by `audio.fx.*`. Two writers with two schemas in one file would clobber each other, so the assignment gets
  its own file next to it, and each op reads and writes only its own file. If the two are ever merged, the merge must
  give each a namespaced table (`[assignment]`, `[[presets]]`) and keep the round-trip test of §7 passing. The voice is
  also what scratch voiceover (CN-10) and score lines (§5.6) use, and ADR 0031 lets `performer.start` borrow another
  character's persona for tests, which must not borrow its voice. **Where docs disagree:** `ai-performer.md` §4 has
  `voice = "voices/mochi"` in `persona.toml`. This design drops that field (unlocked task T7).
- **`VoiceId`** is `<model id>/<voice>` for a local model (`kokoro-82m/af_heart`) or `<connector id>/<voice>` for a
  hosted one. A local model's voice list is a `[[voices]]` table in its `model.toml` (`id`, `languages`, `imitates`,
  `cloned`); a project's own cloned voice is `voices/<name>/voice.toml` plus its reference clip (an asset, ADR 0003). A
  hosted connector lists its voices through its CN-10 `tts` capability. `VoiceCatalog` (App-side, over the registry and
  the connectors) answers both.
- **Validation:** `voice.assign` refuses a voice not in the catalog, a cloned or imitating voice without consent (§3.5),
  a `speed` or pitch out of range; it warns when the voice's languages miss the persona's first language. At
  `performer.start` the voice is checked again; a missing voice refuses the start with the reason and three ways out
  (install, pick another, start text only). There is **no silent fallback** to another voice: a different voice is a
  different character.
- **Ops** (in `omuzaki-speech`, scope `edit:character`, `Undo::Auto`): `voice.get`, `voice.assign`, `voice.list` (query,
  the catalog for a provider), `voice.preview` (synthesise one sentence to `monitor_out`, never `program`).

### 3.5 Consent for cloned voices (SE-05)

- **Every cloned voice needs a consent record, the streamer's own included** (CN-10: "any cloned voice"). Stock voices
  that ship with a model fall under that model's licence row and need none.
- `VoiceInfo.imitates = Some(person)` or `cloned = true` → `ConsentLookup::find(person, Likeness::VoiceClone)` must
  return a record, and `VoiceAssignment.consent` must name it (`ai-api.md` §5). Checked at `voice.assign`, at
  `performer.start` and when the TTS session opens, so a revoked record stops the next show. The registry's
  `NoConsentRecords` default keeps every cloned voice unusable until the records exist.
- **Impersonation warning:** a voice id, voice name or persona name that matches the shipped public-figure list
  (`omuzaki-speech` data, names and aliases) is a warning finding on `voice.assign` and on the start card, never a block.
- What a cloned voice says carries the consent record in its licence record (`ai-api.md` §5). Chatterbox output keeps its
  Perth watermark (register row 134). AI-voice disclosure (SE-06) is AP-07's hook.

### 3.6 Local or hosted, money, and `chat_tts`

- **Local** (ML-04, `tts@1`): Kokoro-82M or Chatterbox weights through our own runtime (register rows 132, 134; the
  Python stacks are NO). Costs 0.
- **Hosted** (CN-10): the connector carries the person's grant (audio-api §7.2); the start card says "speech text goes
  to <provider>", private turns included, as for a hosted LLM. **Money:** each request is bounded before it is sent
  (chars × the connector's dated per-character price, integer micro-dollars as in ADR 0032), against the same per-show
  cap as its LLM (ADR 0031 §3). Synthesis overlaps the streaming LLM turn, so two bounds checked separately against
  what is left could both pass and overspend together (100 left: 80 and 30). So **every paid call of a performer goes
  through one `SpendGate`** (`omuzaki-performer-api`, shared by the `LlmBrain` and the TTS host, wired by the App):
  `reserve(bound)` succeeds only when the bound plus every outstanding reservation fits the session's remaining
  money, and returns a `Reservation`; `settle(final)` charges `Sessions::charge` and releases the rest. **Only a request
  that completed may settle**, with its final usage (the LLM stream's `Done { usage }`, ADR 0032; a hosted synthesis's
  returned chunk). A provider may bill a cut or failed request for more than it reported so far, so a `Reservation`
  that is cancelled, errors or is dropped is **charged its whole bound, whatever partial usage (`usage_so_far`) it
  saw**: partial usage is shown on the meter, never used to release money. Liability never leaves the gate before
  the final bill is known. A refused reservation is a crossed cap
  (`PauseReason::Budget`: the sentence ends, the performer goes idle; ADR 0040's `request_budget` once it lands).
  The performer still holds only `perform`: the host charges, as for the LLM.
- **The `spend` grant and the cost preview** sit on `voice.assign` with a hosted provider: it commits future money, so
  the actor needs `spend`, and the op's preview says "≈ $X per hour at a 40 % talk share" (15 chars/s × 3,600 × 0.4 =
  21,600 chars per hour, × the price).
- **Style tags into TTS controls.** `tts@1` is fixed as `text u8[any]`, `voice u8[any]`, `speed f32[]`,
  `expressiveness f32[]` (0..1, 0.5 neutral) → `pcm f32[any]` (24 kHz). Our runtimes expose every port and the manifest
  says which ones they honour (`[tts] honours = ["speed"]` for Kokoro; Chatterbox maps `expressiveness` to its
  `exaggeration`). `StyleTag` is a closed set; each multiplies speed, sets expressiveness and adds gain (applied to the
  PCM), from the next chunk to the end of the turn:

  | Tag | speed × | expressiveness | gain dB |
  |---|---|---|---|
  | `calm` | 0.95 | 0.35 | 0 |
  | `excited` | 1.08 | 0.80 | +1 |
  | `sad` | 0.90 | 0.30 | −2 |
  | `whisper` | 0.90 | 0.20 | −8 |
  | `laugh` | 1.00 | 0.75 | 0 (and `TalkState.laughing`) |
  | `fast` / `slow` | 1.15 / 0.87 | | |

  Products are clamped to the §3.3 speed range. Unknown tags are dropped by the parser (ADR 0031 §6.2).
- **`chat_tts` stays separate (AP-11):** its own bus, its own `tts@1` session and per-viewer voices, never the
  performer's feeds or handles, and no lip sync by default. It shares only the slot and the catalog. When both
  synthesise, the performer's session runs first (`threads` from the slot, the reader yields).

### 3.7 What "speech-to-speech" means in AP-02

**Voice conversion of the streamer for cover mode:** the person talks and the character's voice comes out. That is the
ML-14 slot in the `mic` chain (audio-api §4.5), not a performer feature. An end-to-end speech model for the performer is
rejected: it has no text, so no inline tags, no transcript, no AP-07 filter and no word-timed intents, and a hosted one
would send captured audio off the device (SE-04). **What is offered without a shippable model:** the BR-24 pitch and
formant chain, and a model the person brings (ADR 0016), with a consent record when it imitates someone. The ML-14 slot
ships empty (`docs/research/2026-10-voice-changers.md` §5).

## 4. Face (AP-05, D3)

### 4.1 The lead: the mouth hears the voice before the viewers do

The `ai_voice` bus tap (audio-api §1) delivers samples as they play, so a model with latency `L` would draw the mouth
`L` late: audio leading video is the most visible sync error (ITU-R BT.1359 puts detectability at about 45 ms of audio
lead; unverified, unreachable from the build box). TTS audio exists before it plays, so the pump calls
`VoiceLead::push(at, 24_000 × r, pcm, private)` with each slice **`lead` ahead** of the playhead, `at` its program time
(§3.3). `lead = min(added_latency(queue) + block + 10 ms, 100 ms)`; a model needing more is allowed, its extra lag is
shown on the health meter ("mouth 40 ms late"). On a turn's **first** chunk the pump starts the feed `lead` late, so the
mouth's first frame is ready: that adds ≤ 100 ms to ADR 0031 §5.3 (2,396 + 100 ≤ 2,500 ms p95). On a cut the pump calls
`VoiceLead::cut(at)`, which only tidies: the drive's release and drop decisions never wait for it (§4.2).

### 4.2 The mouth drive (`omuzaki-voice`)

- **Model:** one `voice_to_face@1` stream session (`ai-api.md` §10). The drive resamples the lead to the model's 16 kHz,
  cuts blocks, pushes block `k` and takes `k − queue` in the same call, paced by the lead's `at` against the clock, on
  its own thread. Output: ARKit-52 per block.
- **Output time is not input time.** `StreamSpec::latency` (`ai-api.md` §10) says input sample `n` comes out as output
  sample `n + latency`, and `take` returns the block of an earlier push (`queue` blocks behind). So a result is **never
  stamped with the `at` of the push that returned it**. The drive numbers samples from the first pushed lead sample
  (index 0 at program time `at₀`, model rate `f`, block `B`). Output sample `m` depicts input sample `m − latency`, so
  result block `j` (output samples `j·B .. (j+1)·B`) has `at_j = at₀ + (j·B − latency) / f`, taken from the block
  *index* `take` reports, not from when it returned. Frames whose `at` falls before `at₀` (the first `latency` samples
  are warm-up) are clipped. `lead` already pays for `latency` through `added_latency` (§4.1); the mapping puts each
  frame on the sample it describes. Without it the mouth would trail the voice by `latency / f` (10 ms per 160 samples
  at 16 kHz). Fallback viseme frames come from the lead PCM and carry its own `at`, so both sources share one timeline
  and a switch needs no correction.
- **Fallback (FE-08):** visemes from the same lead PCM (energy and spectral tilt per 10 ms), computed every block, so
  switching is instant. `Block::Missed` uses the viseme frame for that block; `stalled` stays on visemes until the
  stream restarts ("voice to face stalled: visemes"). Every switch crossfades over **50 ms** in channel space.
- **Output:** a `Source::Audio` layer, mode `Max` (anim-api §6), mask `MOUTH` plus `jawOpen` and `tongueOut`; brows and
  eyes stay with emotes and life (a manifest may opt in later). Each frame is stamped `at − D` (anim-api §6.1: AI
  layers are `program_time − D`), so after the one A/V delay it lands with the voice.
- **Released only for audio that played; the cut is read, not told.** Program time alone does not prove the audio
  played: audio closes directly through the `FeedCloser` flag (§3.2), but `VoiceLead::cut` travels through the pump. A
  pump stalled across a cut would let the release thread keep handing out frames for a feed that is already silent, and
  a frame in `PlayoutDelay` cannot be taken back. So the release thread holds the feed's `CutProbe` (§2, attached with
  each chunk) and, **immediately before each handoff** to the stage's `PlayoutDelay`, checks `probe.state()`:
  1. `Raised`: release nothing; frames stay in the drive's ring.
  2. `Cut { at: t }`, where `t = time_of(cut_at_sample)` is the audio thread's own number, not the pump's: release the
     held frames with `at ≤ t` (they did play), drop the rest, push the close (below).
  3. `Live`: release a frame once the program clock has reached its `at` and the playhead shows its sample consumed
     (`at ≤ time_of(at_sample)`), stamped `at − D` (sampled at `now − D`, so due at once).
  *Why no lock is needed:* the audio thread reads the flag at the start of a block and the release thread reads it just
  before releasing. A frame released while the flag read `Live` at time `T` has `at ≤ T`; a flag raised after `T` can
  only be seen by a block that starts after `T`, so that frame's audio was already in a block that played. Frames leave
  only in states 3 and 2, so nothing in `PlayoutDelay` is ever past the cut, whatever the pump does. The close (mouth to
  rest over 60 ms) is pushed with the stamp `max(t, last released at + 1 µs) − D`, so it is after the newest stamp and
  `PlayoutDelay::push` accepts it. `D` only ramps at most 10 % (anim-api §6.1), so `at − D` still rises when `D`
  changes. The pump's `VoiceLead::cut(at)` may arrive first, last or late; the outcome is the same.
- **Results still in the model after a cut are filtered, not trusted.** The drive keeps a `generation` (a `u32`
  bumped at each cut, by whichever of the probe or the pump reports it first) and a persistent cutoff `t`. Each pushed block records its generation; `take` returns the
  block index, and a result from an older generation, or with `at > t` of the generation that was cut, is thrown
  away however late it arrives. The cut also calls `BlockSink::restart` (`ai-api.md` §10), so the queue is emptied
  and the next reply does not inherit the cut word's model state. The next reply's first `at` is at least `lead` after
  the restart, so its frames always stamp after the close.

### 4.3 Privacy and the human's mouth

A private lead (`SpeakHow.private`) makes the drive clear its layer: the face stays at rest, as ADR 0031 §5.5 asks. The
human's lip sync is separate: the `mouth` voice tap on `mic` (audio-api §4.2) feeds the human character's own drive; the
two never mix, and the AI's voice reaches its tap digitally, never through a speaker.

## 5. Big moves (AP-05, D4)

### 5.1 What it owns, and against whom

The planner writes **`headYaw`, `headPitch`, `lean`, `bodyTurn` and the eight `eyeLook*`** channels, nothing else. The
stage samples the member's live score into the `score` layer, then calls `plan` with a scratch `Layer` and copies in
the channels the score did not key: **an explicit intent beats a planned move**. While a `look` intent holds
`IkGoals.look_at`, the planner leaves gaze and head yaw alone (`look_held`). No new layer:
- **Tracker (PF-23):** the order is rest, score, tracker, audio, trigger, so a channel a human tracks is never
  overridden; on an AP-08 assist character the planner simply loses.
- **Life (PF-24):** life runs after the blend and treats the bus as its target (anim-api §7 rule 2): the planner sets
  where the head and eyes go, life adds noise, nods, saccades and blinks on top. Life's 5 to 12 s glances may land during
  a look-away and only add to it; that is accepted, not arbitrated.
- Stamps: planned keys are AI-driven, stamped `program_time − D` like the mouth.
- **Private turns move nothing (ADR 0031 §5.5).** The stage sets `MoveContext.private` from the heard utterance
  (`Heard::Final { private }`) until that turn ends, and fills `TalkState` with `speaking`, `question` and `laughing`
  false for a private chunk. With `private` set the planner starts no move and ends the active ones, easing back
  over each rule's own "back" time, so the face does what an idle face does: a thinking look-away or a lean-in on stream
  would leak the exchange as surely as a mouth would.

### 5.2 The rules (rule-based first)

| Rule | When | Move | Ease, hold, back |
|---|---|---|---|
| Look away while thinking | `Thinking` for 250 ms, draw < `gaze_aversion` | gaze to (±0.35, +0.25) look units, side from the seed; `headYaw` 0.06 rad the same way | gaze 0.18 s, head 0.45 s, `out_cubic`; held ≤ 2.5 s (then back, again after 1 to 2 s); back when the first chunk plays: gaze 0.15 s, head 0.4 s |
| Lean in on a question | `speaking && question` (the chunk ends in `?`) | `lean` +0.08 rad × (0.5 + `lean`), `headPitch` +0.03 | 0.4 s `out_cubic`; held to chunk end + 0.3 s; back over 0.8 s `smooth` |
| Turn to the co-host | `Addressee::Member { yaw }` while `Speaking` or `Listening` | gaze to the full angle, `headYaw` 0.6 × yaw (≤ 0.45 rad), `bodyTurn` 0.4 × yaw × `turn` (≤ 0.35 rad) | eyes 0.15 s, head 0.45 s, body 0.8 s (the eyes lead); back to camera 0.5 s after the turn ends |
| Laugh | `laughing` | `headPitch` −0.04 then back, once per 1.5 s | 0.2 s up, 0.5 s down |

`StyleKnobs` come from the persona's `life` preset: energetic (energy 0.9, aversion 0.5, lean 0.7, turn 0.8), calm (0.4,
0.6, 0.4, 0.5), shy (0.5, 0.9, 0.3, 0.4), sleepy (0.2, 0.7, 0.2, 0.3), natural (0.5 each). `energy` scales every
amplitude by 0.6 + 0.8 × energy. `TalkState` is filled by the stage from the handles and the floor: `speaking` while a
chunk plays, `question` from the chunk's last char, `laughing` from a `laugh` style tag or a laugh expression, `since`
the time since `speaking` last changed.

### 5.3 Determinism and budget

State is a fixed struct (active moves, timers, the last eased values); the `Rng` is a stream forked from the member's
seed with the id `moves`, after life's four, so adding a rule never shifts life. Same seed, same inputs: the same `Layer`
bits, frame by frame. **Budget: `plan` ≤ 2 µs per frame on `R`** (≤ 1.4 µs on `Dev`; it touches 12 channels): a bench in
`omuzaki-performer` (`benches/moves.rs`, 10,000 frames of the §6.2 script) gates it.

### 5.4 Later: ML-07

- **Trains on:** consented tracked takes (tracker channels + the `heard` voice + the transcript), puppet-page recordings,
  and score-authored performances; never the planner's own output (it would learn the rules), never private material.
- **Score-authored performances are recorded** as `performances/<id>/score.toml` plus `line.toml` (text, `VoiceId`, the
  TTS model and version, `speed`, style tags, seed), so the audio regenerates bit-exactly; an exporter writes (16 kHz
  audio, 30 Hz channel tracks, the `TalkState` and floor stream) triples.
- **It replaces the rules only when it beats them on all four:** the spec §15 liveliness metrics closer to the human
  takes than the rules are; a blind preference ≥ 60 % over the rules on ≥ 30 clips by ≥ 5 raters (needs-user); ≤ 1 ms per
  frame on `R`; and the same `BigMovePlanner` seam, so ownership and determinism hold by construction.

## 6. Worked examples (D5)

Fakes only (§7): `FakeTts` gives 60 ms of audio per char at 24 kHz, a 200 Hz tone at amplitude 0.3 for a vowel, 0.08
for a consonant, 0 for space and punctuation; `FakeVoiceToFace` (block 320 samples at 16 kHz = 20 ms, queue 2, latency 0)
outputs `jawOpen = min(1, 2.5 × block RMS)`, so a vowel gives 0.53 and a consonant 0.14. Fake engine blocks are 5 ms;
`D` = 0; `lead` = 40 + 20 + 10 = 70 ms; no pitch (`r` = 1).

### 6.1 One sentence: "Hi chat!"

| t (ms) | TTS and handle | Feed `ai_voice.a` fill (ms) | Mouth (`jawOpen`, at program time) |
|---|---|---|---|
| 760 | `speak` → `Queued`; synthesis done: 8 chars = 480 ms (11,520 samples), `Ready { 0.48 }` | 0 | lead starts: `at` = 830 + offset |
| 760–830 | first chunk: the feed start is held back `lead` = 70 ms | 0 | block 0 (830–850) pushed at 780, taken at 820 with block 2's push, pending |
| 825 | pump writes the whole chunk (480 ms ≤ the 500 ms lead) | 480 | |
| 830 | playhead reaches it: `Playing { from: 0.830, duration: 0.480 }` | 480 | `H` 0.14 (830–890) |
| 890 / 950 | | 420 / 360 | `i` 0.53 / space 0 |
| 1010 / 1070 / 1130 / 1190 | | 300 / 240 / 180 / 120 | `c` 0.14, `h` 0.14, `a` 0.53, `t` 0.14 |
| 1250 | | 60 | `!` 0 |
| 1310 | `Done { at: 1.310 }` | 0 | closed, mouth at rest by 1370 |

Every mouth frame is due at its `at`, 70 ms after the model saw its audio: none is late, none is early. A cut at 1000
would raise the closer; the next block starts at 1005 and the fade makes `ai_voice` silent by 1007.5 (one block plus
2.5 ms). From the raise the drive releases nothing, whether or not the pump has said anything; once the audio thread
publishes the cutoff it releases held frames up to it, drops later ones and closes the mouth by 1060. This example has
latency 0; §7 repeats it with a model of 160 samples (10 ms) of latency, and the mapping of §4.2 keeps these stamps.

### 6.2 A planned look-away and lean-in (golden channel trace)

The human asks "Mochi, what's your favourite fold?" (final at 0); the reply is "The crane, obviously. Have you tried
one?" Thinking 0 to 1,100 ms; chunk 0 plays 1,100 to 2,360 (21 chars), pad 180 ms, chunk 1 (a question) 2,540 to 3,680.
Knobs: natural. The seed's side draw is "left" (the golden task picks a seed that draws it). Gaze is in look units (x
toward the character's left), angles in radians; the golden file is JSON Lines at 30 Hz, these are its rows at key times:

| t (ms) | why | gaze x | gaze y | `headYaw` | `lean` | `headPitch` |
|---|---|---|---|---|---|---|
| 0 | `Thinking` | 0 | 0 | 0 | 0 | 0 |
| 250 | look-away starts | 0 | 0 | 0 | 0 | 0 |
| 300 | | 0.218 | 0.156 | 0.0179 | 0 | 0 |
| 350 | | 0.319 | 0.228 | 0.0318 | 0 | 0 |
| 500 | gaze held | 0.350 | 0.250 | 0.0547 | 0 | 0 |
| 1,100 | chunk 0 plays: back | 0.350 | 0.250 | 0.0600 | 0 | 0 |
| 1,150 | | 0.104 | 0.074 | 0.0402 | 0 | 0 |
| 1,300 | | 0 | 0 | 0.0075 | 0 | 0 |
| 2,540 | chunk 1 is a question: lean in | 0 | 0 | 0 | 0 | 0 |
| 2,640 | | 0 | 0 | 0 | 0.0463 | 0.0173 |
| 2,740 | | 0 | 0 | 0 | 0.0700 | 0.0263 |
| 2,940 | held | 0 | 0 | 0 | 0.0800 | 0.0300 |
| 3,980 | chunk end + 0.3 s: back | 0 | 0 | 0 | 0.0800 | 0.0300 |
| 4,380 | | 0 | 0 | 0 | 0.0400 | 0.0150 |
| 4,780 | | 0 | 0 | 0 | 0 | 0 |

(`out_cubic(u) = 1 − (1 − u)³`, `smooth(u) = 3u² − 2u³`; natural knobs give lean 0.08 × (0.5 + 0.5) and an energy factor
of 1.0.) The same script on a member whose `HEAD` a tracker owns resolves to the tracker's values on every row.

## 7. Tests (D6)

- **Fakes in `omuzaki-ai-mock`:** `FakeTts` (the §6 waveform, deterministic, scripted latency per chunk and scripted
  failures) and `FakeVoiceToFace` (the §6 envelope, run through `InlineStream` with scripted `Missed` blocks).
- **TTS host** on a fake engine with a fake clock: gapless when synthesis is fast; a late chunk lengthens the pad and
  counts; a starved pump plays silence and corrects `from`; `cut` silences within one block plus the 2.5 ms fade (7.5 ms on the fake engine),
  the next `speak` uses the other feed; the resume re-uses the cached PCM (no second synthesis); `Playing.from` within
  one block of the fake engine's truth; pitch +4 gives a declared rate of 30,238 Hz and the same duration at speed 1;
  **private routing:** leaving `CohostOnly` for `Muted` with private PCM buffered, and again with a private synthesis in
  flight, puts zero private samples on `program` (the fake engine's program tap) while `monitor_out` plays them.
- **Voices:** **one file, two schemas:** a character with both `voice.toml` (an `FxPreset` chain) and `voice-assignment.toml`: `voice.assign` leaves the bytes of `voice.toml` unchanged and `audio.fx` saves leave the assignment unchanged, and both round-trip; unknown id, out-of-range speed or pitch, a cloned voice without consent, a revoked consent at start, a
  public-figure name warns; a hosted assign without `spend` is denied; the preview number for a dated price.
- **Mouth drive:** §6.1 as a golden (stamps and values); a `Missed` block switches to visemes with the 50 ms crossfade;
  stalled stays on visemes; a cut drops pending frames; **a delayed-pump cut:** the fake pump stalls 300 ms across a cut at 1000 (no `VoiceLead::cut`, no new `push`): zero frames past the audio thread's published cutoff reach `PlayoutDelay`, none are released between the raise and the cutoff publication, held frames up to the cutoff are then released in order, and the close is accepted (run with the stall ending before and after the publication); **nonzero latency:** a `FakeVoiceToFace` with `latency` = 160 samples (the §6 envelope delayed by that many samples, zero before) reproduces the §6.1 stamps and values exactly, the same fake with the mapping disabled misses by 10 ms (proof the test sees it), and warm-up frames are clipped, never stamped before `at₀`; private leads leave the layer clear; the §6.1 golden again with `D` = 200 ms (every stamp
  200 ms earlier, nothing released before its program time, a cut at 1000 pushes an accepted close); **across a cut:** a result for a block before the cut, scripted to
  arrive after it, is dropped and the mouth stays closed; the next reply right after the cut, with `D` = 200 ms,
  releases its frames in order, all stamped after the close.
- **Planner:** §6.2 as a golden channel trace; determinism (two runs, same bits; another seed, other side); a tracked
  `HEAD` wins on every frame; a score key on `lean` beats the planner; `look_held` leaves gaze alone; **a private turn** (the §6.2 script with
  `private`) gives an all-rest trace, and a private turn starting mid look-away eases back with no new move; the bench.
- **Money:** `SpendGate` with 100 left: reserving 80 then 30 refuses the second until the first settles; a reservation
  cut or failed before any usage arrives charges its whole bound, so a new 80 is still refused; **after partial
  usage** (80 reserved, `usage_so_far` worth 20, then cancel, or then a stream error) it still charges 80 and a new 80
  is refused; only a completed request settles with its final usage and releases the rest; an LLM turn and two syntheses in flight never exceed the cap.
- **Needs the owner (needs-user, local lane):** first-chunk TTS latency for Kokoro and Chatterbox on `R`; A2F-3D's `L`;
  a lip-sync viewing test (offset sweep −60..+60 ms); hosted provider latency and price.
