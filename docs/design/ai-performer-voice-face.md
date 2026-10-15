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
  a first chunk cut at a comma. PCM buffers are allocated here, off the audio thread; at most 3 are live (≈ 4.2 MB at
  220 chars: 220 / 15 chars per s = 14.7 s × 24,000 × 4 bytes = 1.41 MB each).
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
- **The playhead (new in `omuzaki-audio-api`)** is a **bounded log of what each audio block consumed**, not a latest
  snapshot, so a reader that looks late can still place every sample. `FeedWriter::playhead() -> FeedPlayhead`, `Clone`
  as a read-only probe, offers `latest() -> PlayRecord`, `records_since(cursor) -> Records` and `cut_at_sample() -> u64`.
  ```rust
  pub struct PlayRecord { pub index: u64,     // 0, 1, 2, ... one per recorded block, never reused
                          pub consumed: u64,  // feed sample index of the first sample this block consumed
                          pub n: u32,         // feed samples consumed in this block, 1..=block (n < block: underrun inside it)
                          pub at_sample: u64 }// engine sample where that first sample enters the bus (after the resampler's delay)
  ```
  - **What is recorded.** The audio thread appends one record per block **in which the feed consumed at least one
    sample**; a block that consumed none (a stall) records nothing, and a partial block (an underrun part-way) records
    the `n` it did consume. Sample `s` of the feed that lies in record `r` plays at engine sample
    `r.at_sample + (s − r.consumed)·(engine rate / feed rate)`, so `time_of` of that is its program time **exactly**; a
    gap shows as a jump of `at_sample` between consecutive records beyond `n` samples. Nothing is extrapolated across
    records, only inside one.
  - **History kept, and how a slot is published (a sequence lock, as `EngineClock` in `clock.rs`).** A ring of
    `⌈2 s / engine block⌉` slots per feed (about 400 at 5.3 ms, 2,048 at most), each slot:
    `seq: AtomicU64`, and the four record fields as `AtomicU64` (`n` widened), so **no field is ever read or written
    non-atomically** (no data race, no `unsafe`). A feed-wide `head: AtomicU64` counts the records published.
    *Writer* (the audio thread, the only writer; wait-free, never retries or blocks), for record number `i` in slot
    `i mod R`:
    1. `s = slot.seq.load(Relaxed)` (even); `slot.seq.store(s + 1, Relaxed)` (odd: write in progress);
    2. `fence(Release)`;
    3. `store(Relaxed)` of each of the four fields;
    4. `slot.seq.store(s + 2, Release)` (even: complete);
    5. `head.store(i + 1, Release)`.
    The fence in 2 orders the odd `seq` before the field stores, so a reader that sees any new field value also sees the
    odd (or later) `seq` on its second read. *Reader* (a non-real-time thread), for record `i`:
    1. `h = head.load(Acquire)`; if `i ≥ h` the record is not there yet; if `i + R < h` the slot is already reused: **lapped**;
    2. `a = slot.seq.load(Acquire)`; if `a` is odd, retry;
    3. load the four fields (`Relaxed`);
    4. `fence(Acquire)`; `b = slot.seq.load(Relaxed)`;
    5. accept only if `a == b` **and** the record's `index == i`; otherwise retry. A mismatched `index` with `a == b`
       means the slot holds a later record: **lapped**.
    *Why a torn record is never accepted:* the writer changes `seq` to odd before any field and back to even (a
    different value, `+2`) after all of them. A reader that copied fields while the writer was between those two points
    either saw an odd `a`, or saw `b ≠ a` (the Acquire fence pairs with the Release fence/store: if a field load
    observed a value stored after the odd `seq`, the following `seq` load observes the odd value or later), so it
    retries. A reader that sees `a == b` even read between two complete writes of the same slot. *Bound:* the writer
    needs `R ≥ 400` records to come back to a slot, which is at least 2 s, so a reader needs to be preempted for a whole
    lap to fail twice. The reader retries **at most 4 times** per slot, then reports the record **lapped** and the
    drive drops its ticks (below), so a reader never spins unbounded and never blocks the audio thread.
    2 s covers the 1 s feed capacity (§3.1) plus a scheduling hiccup of the reader.
  - **A reader that misses records.** Readers keep a cursor and ask `records_since(cursor)`. If the ring was lapped
    (`cursor` older than the oldest record), the call says how many records were lost. The samples of the lost records
    **cannot be placed and are never extrapolated**: the mouth drive drops the ticks whose feed samples lie there
    (`mouth_history_lost` counts them, the health meter shows it), the stage interpolates across, and the next tick is
    stamped from the first surviving record. `Playing.from` of the speech host uses the same log and can only be wrong
    by a lapped record, never by a guess.
  - **The cut.** `cut_at_sample` is the engine sample where a closed feed went silent (`u64::MAX` while open), set by
    the audio thread when the close fade ends; the last record's `n` ends at the cut. It is **the authority on what
    played**. `FeedCloser` is `Clone` as a read-only probe too (`is_raised()`).
  This is a contract change to `omuzaki-audio-api`: it lands first and alone (`CLAUDE.md`, how work flows, step 3).
  ADR 0031's `Playing { from }` is `time_of(record at the chunk's first sample)`, with the sample placed as above, and
  the audio contract had no way to say it.

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
(§3.3). `lead = min(lead_min, 100 ms)` with
`lead_min = (queue + 2)·B/f + L/f + 10 ms` (`B` the model's block, `L` its latency, both in samples at its rate `f`; §4.2
derives it). **One policy when `lead_min` exceeds 100 ms (the lead stays at 100 ms): ticks are never delayed; they are released on time
(`P_g(v) − D`) and a key not yet decided when its tick reaches the head of the queue is replaced by its viseme key
(§4.2 3). The cost is fidelity, not timing, and the health meter says "mouth: visemes 30 %" with the share of ticks
that used one (`late_keys` / ticks).** Delaying ticks instead would put the mouth behind the voice, the worse error
(above). On a turn's **first** chunk the pump starts the feed `lead` late, so the
mouth's first frame is ready: that adds ≤ 100 ms to ADR 0031 §5.3 (2,396 + 100 ≤ 2,500 ms p95). On a cut the pump calls
`VoiceLead::cut(at)`, which only tidies: the drive's release and drop decisions never wait for it (§4.2).

### 4.2 The mouth drive (`omuzaki-voice`)

One thread, one model session, and **one timeline** in which stamping, interpolation, release, stalls and cuts are
defined together (earlier rounds found each wrong when specified apart). `f` is the model rate, `B` its block and `L` =
`StreamSpec::latency` in samples, `W = ⌈L/B⌉`. The drive works on a **voice axis**: `v` = seconds of voice since the
generation's first sample, counted on the lead PCM, which has no gaps. **Program time comes only from the playhead**
(§3.3): `P_g(v)` is the program time at which the voice sample at `v` actually enters the bus. With no stall
`P_g(v) = at₀ + v`; a stall (an underrun, a late chunk, §3.2) adds its length to `P_g` for every later `v`.

**Model.** One `voice_to_face@1` stream session (`ai-api.md` §10). The drive resamples the lead to `f`, cuts blocks,
pushes block `k` and takes `k − queue` in the same call, paced by the lead's predicted `at` (a pacing hint only; stamps
never use it). The slot returns **one ARKit-52 frame per block**; `Block::Missed` means "use the fallback".

**1. Keys, on the voice axis.**
- *Origin per generation.* `BlockSink::push` returns the block's sequence number and `restart` keeps counting
  (`stream.rs`), while `take(seq, out)` answers only `Ready` or `Missed`. For each generation `g` (one per reply after a
  cut) the drive keeps `seq₀(g)`, the number `push` returned for its first block, and records `(seq, generation)` of each
  block in flight; it calls `take` only with a `seq` it chose.
- *Key time.* Block `q` is the key at voice time `v_q = ((q − seq₀)·B − L) / f`: the start of the input interval its
  output depicts (the same start-of-block convention at `L = 0`). It reaches the stage at the stamp `P_g(v_q)`, never at
  the `at` of the push that returned it and never counted from the first reply.
- *Warm-up.* The first `W` blocks depict model pre-roll: **drop them**. The first retained block has `v = (W·B − L)/f
  ≥ 0`. If that is `> 0` the generation also has a **head key** at `v = 0`, a viseme frame from the lead PCM
  `[0, W·B − L)`; if it is `0` (`L = B`, `2B`, ...) there is no head key and the first retained key (the model frame, or
  its fallback if `Missed`) sits at `v = 0`. Keys are strictly increasing on the axis.
- *Fallback.* A `Missed` block's key is the viseme frame (FE-08: energy and spectral tilt) of the **same depicted
  interval**, at the same `v_q`. Key value `= w_q·model + (1 − w_q)·viseme`; `w` moves toward 1 on a `Ready` block and
  toward 0 on a `Missed` one by `(B/f) / 50 ms` per block (a 50 ms crossfade), and starts at 1; `stalled` pins `w = 0`
  until the stream restarts ("voice to face stalled: visemes").
- *Tail.* After the last real sample the drive pushes `queue + W` silent blocks so the last audio comes out.
- *Why the lead is `lead_min`.* A tick at `v` needs the first key at or after it, which is less than one block later:
  `v_q < v + B/f`. Key `q` is decided when `push(q + queue)` runs, i.e. when block `q + queue` is complete, `(queue+1)·B`
  samples after block `q` began, which is `L/f + (queue+1)·B/f` after `v_q` on the voice axis; the pump delivers it
  `lead` ahead of its playback and in 10 ms steps. So it is decided by `P(v_q) + L/f + (queue+1)·B/f + 10 ms − lead`, and
  that must not be after `P(v)`: `lead ≥ B/f + L/f + (queue+1)·B/f + 10 ms = lead_min`. (`B` = 20 ms, `L` = 5 ms, queue 2:
  `lead_min` = 80 + 5 + 10 = 95 ms; a smaller lead leaves the key `at₀+35` for the tick at `at₀+20` decided at `at₀+25`.)

**2. Ticks: the drive evaluates the curve; the stage only holds it.** `PlayoutDelay` interpolates only between stamps it
has been given, so the drive interpolates **itself**, in its own cancellable buffer, on a fixed **tick of 10 ms of
voice**: tick `n` is `v = n·10 ms`, and its frame is the **linear interpolation of the keys around `v`** (the first key
held before it, the last held after it). A tick may use a key up to one block after it: audio that has not played yet,
which is the point of the lead and is accepted; what a cut bounds is the tick's **stamp**, not what it was computed
from. Ticks are `Copy` 52-wide `Layer`s in a ring of 64 (640 ms at 100 Hz, as anim-api §6.1 sizes the stage's ring).

**3. Release: one ordered queue of ticks, released for audio that played.** The queue holds, in voice order, the
evaluated ticks and the fade items of §4 and §5 below. **Release is in queue order, one item at a time, so stamps rise by
construction**: no item is released before the one ahead of it, and `PlayoutDelay::push` is never offered an older
stamp. An item is stamped `P_g(v) − D` (anim-api §6.1: AI layers are `program_time − D`, so it is due at once when
sampled at `now − D`). Program time does not prove the audio played: audio closes through the `FeedCloser` flag (§3.2),
while `VoiceLead::cut` travels through the pump, and a frame in `PlayoutDelay` cannot be taken back. So the release
thread holds the feed's `CutProbe` (§2) and its play log (§3.3) and polls them at least twice per engine block;
**immediately before each handoff** it checks `probe.state()`:
1. `Raised`: release nothing; the queue waits.
2. `Cut { at: t }`, `t = time_of(cut_at_sample)`: release the items with `P_g ≤ t` (they did play), drop the rest,
   start the close.
3. `Live`: the head item `v` is released when the log shows its feed sample consumed (a record with
   `consumed ≤ s < consumed + n` for `s = fs(v)`, the feed index of voice time `v`), stamped from **that record**, not
   from the latest one: `P_g(v) = time_of(r.at_sample + (s − r.consumed)·ratio)`. If the log was lapped over `s` the tick
   is dropped (§3.3). Reading records late changes nothing: the stamp depends on the record, not on when it was read.
*Why no lock is needed:* the audio thread reads the flag at the start of a block and the release thread reads it just
before releasing. An item released while the flag read `Live` at time `T` has `P(v) ≤ T`; a flag raised after `T` can
only be seen by a block that starts after `T`, so that item's audio was already in a block that played. Items leave
only in states 3 and 2, so no stamp in `PlayoutDelay` is ever past the cut, whatever the pump does. *Sampling:* the
stage reads at `now − D`, at or after the newest item by less than one tick, so it holds the newest tick for at most
10 ms of the curve and never reads ahead of the clock. `D` ramps at most 10 % (anim-api §6.1), so `P − D` still rises.
*A tick is never late, only coarser.* A tick is evaluated **when it reaches the head of the queue**, from the keys
decided by then; a key not decided yet (the lead was capped below `lead_min`, §4.1, or the model is slow) is replaced by
its viseme key (`w = 0` for it) and counted in `late_keys`. So a model that is late costs fidelity, never ordering or a
refused push.

**4. Stalls: the mouth follows the audio, not the plan.** From the log: a stall begins at `g₀ = time_of(r.at_sample +
r.n·ratio)` for the last record `r`, and ends at `g₁ = time_of(r'.at_sample)` of the next. The drive sees the stall
begin when the engine clock has passed `g₀` by one block with no new record. The stage **holds** the newest stamped
tick (anim-api §6.1: after the newest stamp the newest sample is held), so a mouth left open would stay open through the
silence; the drive therefore **emits fade ticks as they become due, never in advance**: items in the same queue at
`g₀ + 10 ms·k`, `k = 1..6`, each `last·(1 − k/6)` where `last` is the last released value (a linear fade to rest over
60 ms), released one at a time when the clock reaches their stamp. When the audio resumes, the fade items not yet released
are dropped, and a **resume tick** is queued at `g₁` with the curve's value at the voice time where the audio resumed;
ticks for later samples follow at their records' times. Because the queue is ordered, a tick whose sample was consumed
before `g₀` is always ahead of every fade item: it is released first (it is never late, above), and nothing is stamped
before it afterwards. A stall shorter than one engine block is not a stall (no record is missing).
The voice axis is unchanged by a stall.

**5. The close, cancellable.** After a cut at `t` the close is a run of items `c + 10 ms·k`, `k = 1..6`, with
`c = max(t, last released stamp + 1 µs)`, easing the last released value to rest (`last·(1 − k/6)`; the same mechanism as the stall fade); they are queue items released **one at a time as
the clock reaches them**, never published ahead. A new generation's first tick is queued behind the close; when it
would be released, every unreleased close item with a stamp `≥` it is dropped (the new tick wins a tie), so a reply
that starts at once (a 50 ms lead after the restart, inside the 60 ms close) cannot make `PlayoutDelay::push` refuse
anything, and the stage interpolates from the last close item to the new reply's first tick.

**6. After a cut, stale results are filtered.** The drive keeps a `generation` (a `u32` bumped at each cut, by whichever
of the probe or the pump reports it first) and a persistent cutoff `t`. Each pushed block records its generation and
`seq`; a result from an older generation, or with stamp `> t` of the generation that was cut, is thrown away however
late it arrives. The cut also calls `BlockSink::restart`, so the queue is emptied and the next reply does not inherit
the cut word's model state; it starts a new generation with its own `seq₀` (`restart` keeps counting) and its own
playhead, so it has its own `P_g`.

**Output layer:** a `Source::Audio` layer, mode `Max` (anim-api §6), mask `MOUTH` plus `jawOpen` and `tongueOut`; brows
and eyes stay with emotes and life (a manifest may opt in later). §6.3 walks one timeline through all of the above.

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
for a consonant, 0 for space and punctuation; `FakeVoiceToFace` (block 320 samples at 16 kHz = 20 ms, queue 1, latency 0)
outputs `jawOpen = min(1, 2.5 × block RMS)`, so a vowel gives 0.53 and a consonant 0.14. Fake engine blocks are 5 ms;
`D` = 0; `lead` = `lead_min` = (1 + 2)·20 + 0 + 10 = 70 ms; no pitch (`r` = 1).

### 6.1 One sentence: "Hi chat!"

| t (ms) | TTS and handle | Feed `ai_voice.a` fill (ms) | Mouth (`jawOpen`, at program time) |
|---|---|---|---|
| 760 | `speak` → `Queued`; synthesis done: 8 chars = 480 ms (11,520 samples), `Ready { 0.48 }` | 0 | lead starts: `at` = 830 + offset |
| 760–830 | first chunk: the feed start is held back `lead` = 70 ms | 0 | block 0 (830–850) pushed at 780, taken at 800 with block 1's push, pending |
| 825 | pump writes the whole chunk (480 ms ≤ the 500 ms lead) | 480 | |
| 830 | playhead reaches it: `Playing { from: 0.830, duration: 0.480 }` | 480 | `H` 0.14 (830–890) |
| 890 / 950 | | 420 / 360 | `i` 0.53 / space 0 |
| 1010 / 1070 / 1130 / 1190 | | 300 / 240 / 180 / 120 | `c` 0.14, `h` 0.14, `a` 0.53, `t` 0.14 |
| 1250 | | 60 | `!` 0 |
| 1310 | `Done { at: 1.310 }` | 0 | closed, mouth at rest by 1370 |

The mouth column gives key values (`L` = 0, keys every 20 ms at 830, 850, ...); between keys the drive interpolates (§4.2, §6.3 has the 10 ms ticks). Every tick is due at its program time, 70 ms after the model saw its audio: none is late, none is early. A cut at 1000
would raise the closer; the next block starts at 1005 and the fade makes `ai_voice` silent by 1007.5 (one block plus
2.5 ms). From the raise the drive releases nothing, whether or not the pump has said anything; once the audio thread
publishes the cutoff it releases held ticks up to it, drops later ones and closes the mouth by 1067.5 (`c` = 1007.5 plus 60 ms). This example has
latency 0; §6.3 walks the ticks around one key boundary with `L` = 0 and `L` = 160 samples (10 ms), and a cut.

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

### 6.3 One timeline through stamping, interpolation and release

Same fakes as above, `f` = 16 kHz, `B` = 320 (20 ms), queue 1, `P(0)` = `at₀` = 830 as the playhead measures it (fake A's `lead_min` is 70 ms; fake B's is `(1+2)·20 + 10 + 10` = 80 ms, so its feed starts at 750 + 80 and `P(0)` is again 830), `seq₀` = 0, "Hi chat!" as in §6.1 (`H` is voice
time 0 to 60 ms, `i` 60 to 120, space 120 to 180). `jawOpen` of a key = 2.5 × the RMS of the interval it depicts
(vowel 0.530, consonant 0.141; a half-and-half interval has RMS² = the mean of the two squares; the viseme fallback
uses the same envelope in this example). Two fakes: **A**, `L` = 0; **B**, `L` = 160 samples (10 ms): its output sample
`m` is the envelope of input sample `m − 160`, zero before.

**Keys** (voice time `v_q = 20(q − seq₀) − L/f` ms, stamp `P(v_q) = 830 + v_q` while nothing stalls):

| fake | stamp (ms) → key value |
|---|---|
| A | 830, 850, 870 → 0.141; 890, 910, 930 → 0.530; 950 → 0 |
| B | **830 (head, viseme of voice time 0..10) → 0.141**; q1 840 → 0.141; q2 860 → 0.141; q3 880 → 0.388 (`√((0.0032+0.045)/2)·2.5`); q4 900 → 0.530; q5 920 → 0.530; q6 940 → 0.375 (`√(0.045/2)·2.5`); q7 960 → 0. Block q0 depicts −10..10 ms (0.100) and is dropped: `0·B < L` |

**Ticks** (every 10 ms from 830; the value is the linear interpolation of the keys around it) and **release**
(a tick is handed to the stage at program time `τ`, stamped `τ − D`):

| program time (ms) | tick A | tick B | stamp for B, `D` = 0 / 200 | what the stage has at this instant |
|---|---|---|---|---|
| 860 | 0.141 | 0.141 | 860 / 660 | tick 860 released |
| 870 | 0.141 | 0.265 (between key 860 and key 880) | 870 / 670 | B: key 880 is not released; the **drive** used it |
| 880 | 0.336 (870→890) | 0.388 | 880 / 680 | |
| 890 | 0.530 | **0.459** (key 880 → key 900, halfway) | 890 / 690 | B: the 900 key is withheld, tick 890 is not: sampling at 890 gives 0.459, not 0.388 |
| 900 | 0.530 | 0.530 | 900 / 700 | |
| 910 | 0.530 | 0.530 | 910 / 710 | |
| 920 | 0.530 | 0.530 | 920 / 720 | |

The stage **holds the newest released tick** between ticks and never reads ahead (sampling B at 895 gives 0.459, the curve
there is 0.494: under one tick of the curve). With `D` = 200 the ticks carry the stamps in the right column and the stage
samples at `now − 200`; a tick stamped `τ − 200` is released at program time `τ` and sampled at exactly its stamp, so
the output is **identical at identical program times for `D` = 0 and `D` = 200** (the delay moves where the audio and the
face are produced, not their alignment with each other). Every sampled value below is a hold of the newest tick with `τ ≤ t`.

**A cut** raised at 893 ms: the engine blocks are 5 ms, the next starts at 895 and the fade ends at 897.5, so the audio
thread publishes `t` = 897.5. Tick 890 was released at 890 (state `Live`); between 893 and the publication nothing is
released (`Raised`); tick 900 is dropped (`900 > t`). The close starts at `c = max(897.5, 890 + 1 µs)` = 897.5 and is released one tick at a time at 907.5, 917.5, ..., 957.5 (`k` = 1..6), each
`last·(1 − k/6)` with `last` = 0.530 (A) or 0.459 (B): A 0.442, 0.353, 0.265, 0.177, 0.088, 0; B 0.383, 0.306, 0.230, 0.153, 0.076, 0, released when
the clock reaches it, stamped `− D`. Sampled (hold): 900 → tick 890 (A 0.530, B 0.459), 910 → tick 907.5, and so on. If the pump stalls and never says `cut`, nothing changes. A result for a block
after the cut belongs to the old generation and is dropped.

**Immediate resume.** A model with 10 ms blocks, `L` = 0, queue 2 has `lead_min` = (2+2)·10 + 0 + 10 = 50 ms. A new
reply right after the cut above has its first sample on the bus at `restart + 50`, i.e. `P(0)` = 947.5 or later, while
the close runs to 957.5. The close ticks released by then are 907.5, 917.5, 927.5, 937.5 (A: 0.442, 0.353, 0.265, 0.177); the ones at 947.5 and
957.5 are dropped because the new reply's first tick has the stamp 947.5 (a tie goes to the new reply). Every stamp
the stage sees rises, nothing is refused, and the stage holds 0.177 from 937.5 until the new reply's first tick at 947.5.

**Stall (underrun), partial block, missed records.** Fake A, fake engine blocks of 5 ms (120 feed samples at 24 kHz),
`at₀` = 830. The pump starves: the block starting at 890 consumes only 60 samples (2.5 ms), then nothing until a full
block at 990, so the log has:

| record | `consumed` (feed samples) | `n` | `at_sample` as program time (ms) |
|---|---|---|---|
| r12 | 1,440 (voice 60.0 ms) | 60 | 890.0 |
| r13 | 1,500 (voice 62.5 ms) | 120 | 990.0 |
| r14 | 1,620 (voice 67.5 ms) | 120 | 995.0 |

(blocks starting 895 to 985 consumed nothing and recorded nothing). The jump 890.0 + 2.5 = 892.5 → 990.0 is the gap:
`g₀` = 892.5, `g₁` = 990. **The release thread is descheduled from 900 to 1,010 and sees r12, r13, r14 at once.** Tick v = 60
(feed 1,440) is in r12: stamp 890.0 (released, correct). Tick v = 70 (feed 1,680) is in r14 at offset 60 samples = 2.5 ms:
**997.5**, not the 1,000 that extrapolating from the latest record would give (and not the 890 + 10 of a gap-free plan). The
queue is: tick 60 at 890 (0.530), fade items 902.5, 912.5, ..., 952.5 (0.442, 0.353, 0.265, 0.177, 0.088, 0), the resume tick at 990 (the curve
at voice 62.5 ms: 0.530), tick 70 at 997.5 (0.530), tick 80 at 1,007.5, ..., all rising. Sampled (hold): 895 → 0.530, 910 → 0.442, 920 → 0.353
(tick 912.5), 950 → 0.088, 960 → 0, 989 → 0, 990 → 0.530. If the log had been lapped over r12 and r13 (a reader away for more than 2 s), ticks 60 and 62.5 would be
dropped and counted, tick 70 would still be stamped 997.5 from r14.

**Capped lead plus underrun.** A model whose `lead_min` is 130 ms but whose lead stays at 100 ms (§4.1: ticks on time,
undecided keys become visemes). In this example the fake's viseme frame is 0.8 × the model's value (a coarser mouth), so
the two differ. Key v = 60 (model 0.530, viseme 0.424) is decided 30 ms late, at 920. Tick v = 50 reaches the head of
the queue at 880 and tick 60 at 890, so both are evaluated before it exists: tick 50 uses the key at v = 40 (model
0.141) and the **viseme** key at v = 60 (0.424): 0.283 (it would be 0.336 with the model key); tick 60 is 0.424
(`late_keys` = 2). They are released at 880 and 890, on time. The stall above begins at 892.5, so tick 60 is **ahead of
the fade items** in the queue. The fade starts from the last released value, 0.424: items 902.5, 912.5, ..., 952.5 are
0.353, 0.283, 0.212, 0.141, 0.071, 0. The resume tick at 990 is evaluated then, when key 60 (decided at 920) is long decided:
the curve at voice 62.5 ms is the model's 0.530. **Sampled output, chronological (hold of the newest released tick; `D` = 0 and
`D` = 200 give the same values at the same program times):**

| program time (ms) | 880 | 890 | 900 | 910 | 920 | 930 | 940 | 950 | 952.5 | 960 | 989 | 990 | 997.5 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| newest tick released | 880 | 890 | 890 | 902.5 | 912.5 | 922.5 | 932.5 | 942.5 | 952.5 | 952.5 | 952.5 | 990 | 997.5 |
| value | 0.283 | 0.424 | 0.424 | 0.353 | 0.283 | 0.212 | 0.141 | 0.071 | 0 | 0 | 0 | 0.530 | 0.530 |

Release times are the stamps plus `D`: 880, 890, 902.5, ..., 952.5, 990, 997.5; every push is accepted, stamps rise, no
tick was delayed.

**Whole-block latency.** `L` = 320 (`W` = 1, `W·B − L` = 0): block 0 is dropped, **no head key**, block 1 is the key at
`at₀` = 830 (value 0.141, its own depicted interval 0..20 ms). `L` = 640 (`W` = 2): blocks 0 and 1 dropped, block 2 at
830. `L` = 480 (`W` = 2): the head interval is `[0, 160)` samples = 10 ms, so a head key at 830 and block 2 at 840.

**Resume.** After a cut at the end of block 6 (seqs 0 to 6 pushed) `restart` leaves the counter at 7. The next reply's
first `push` returns 7, so `seq₀` = 7 and `at₀` is its own first slice's program time: its keys are the table above
shifted by `at₀(B) − 830` (seq 8 is its block 1, stamp `at₀ + 10`), nothing of the first reply's seven blocks leaks in.

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
- **Mouth drive** (the sampled-output tests read the stage's `PlayoutDelay` at 5 ms steps, not only the ticks):
  §6.1 as a golden (key values and ticks); **§6.3 exactly** for fake A and fake B (`FakeVoiceToFace` with a `latency`
  field: output = the envelope delayed by that many samples, zero before): keys in integer samples at 16 kHz, tick
  values to 3 decimals, and the sampled output (chronological, hold) between 860 and 920, identical at identical program times for `D` = 0 and `D` = 200 ms, **the lead:** a fake with `L` = 80 samples (5 ms), queue 2, B = 320: `lead_min` = 95 ms, every tick's bracketing key is decided before the tick is due (the tick at `at₀+20` needs the key at `at₀+35`, decided at `at₀+5`, or `at₀+15` with the pump's 10 ms rounding), and with the lead forced to 75 ms the tick is still released on time and evaluated with the viseme key (`late_keys` > 0 and the sampled value differs from the model's, never an early or delayed stamp); where the value at
  890 ms is 0.459 and at 895 ms is 0.459 (the held tick), never 0.388; the same fake with the `− L` removed fails the
  table by 10 ms (proof the test sees it); **warm-up:** `L` = 160 drops block 0 and starts with the head key at `at₀`;
  `L` = `B` (320) and `L` = `2B` (640) have **no head key**, the first retained key sits at `at₀`, and no push to
  `PlayoutDelay` is refused; `L` = 480 has a 10 ms head key; a `Missed` block 3 is replaced by the viseme of the same
  depicted interval at the same stamp 880; the 50 ms crossfade ramps `w` by 0.4 per block; `stalled` stays on visemes;
  **cuts** (each with `D` = 0 and 200): a cut raised at 893 ms between ticks, one exactly on a tick (900), and one
  during the head interval: ticks past the published cutoff are dropped, the close is accepted and stamps after the
  newest tick, and the sampled output after the close decays to rest over 60 ms; **a delayed-pump cut:** the fake pump
  stalls 300 ms across a cut at 1000 (no `VoiceLead::cut`, no new `push`): zero stamps past the audio thread's published
  cutoff reach `PlayoutDelay`, nothing is released between the raise and the publication, held ticks up to the cutoff
  are then released in order (run with the stall ending before and after the publication); **cut and resume, exact
  alignment:** reply A (seqs 0 to 6, cut after block 6, a late result for seq 6 arriving after the cut, dropped), then
  reply B (first push returns seq 7): B's keys as `stamp − at₀(B)` equal A's as `stamp − at₀(A)` row for row
  (sample-exact), and for both replies every key minus the program time its depicted audio plays at is zero, so the
  check is on alignment, not on stamps merely rising; the next reply right after a cut with `D` = 200 ms releases its
  ticks in order, all stamped after the close; private leads leave the layer clear.
  **immediate resume:** a 10 ms-block, queue-2, `L` = 0 fake (`lead_min` 50 ms) restarted at once after a cut: the close
  ticks and the new reply's ticks are all accepted, strictly rising, the tie goes to the new reply (§6.3), with `D` = 0
  and 200; **stall and resume (play log):** a pump stalled 100 ms mid-word (§6.3): every key's stamp minus the program time its
  depicted audio plays at is zero on both sides of the gap, the fade items sit at `g₀ + 10·k` and the resume tick at `g₁`, a 40 ms stall
  releases three fade items and drops the rest when the resume tick is queued, and a stall during a chunk that is then cut behaves as the cut tests.
  **play log:** the audio thread under test writes records for a gap including a **partial-block underrun** (n < block) and
  a full-block stall; a reader that polls only every 100 ms (skipping the observations in between) must stamp every
  tick from its record exactly as a reader that polls every block (§6.3: 890.0 and 997.5), never from the latest; a
  reader that is lapped (ring of 16 records in the test) drops precisely the ticks of the lost records, counts them
  (`mouth_history_lost`), stamps the next tick from the first surviving record and is never offered a refused push;
  the seqlock hammer test (one writer lapping a 16-slot ring while readers copy every slot) never returns a record whose fields come from two writes; **ordering under
  lateness:** a capped-lead (100 ms) model with `lead_min` 130 ms plus an underrun (§6.3): release **times** equal the stamps plus `D` (880, 890, 952.5, 980, 997.5, none delayed), accepted pushes only, stamps
  strictly rising, tick 60 released before every fade item, `late_keys` = 2, and the **values** match the §6.3 table **sampled chronologically** (the test steps the clock and samples
  `PlayoutDelay` at each program time, never preloading frames that are not yet due: 0.283 at 880, 0.424 at 890 and 900, 0.353 at 910, 0.283 at 920, 0 at 952.5, 0.530 at 990), **identical at identical program times for `D` = 0 and 200**;
  the health meter reads the viseme share; **torn records:** a test hook pauses the writer **between steps 3 and 4** of the
  slot write (some fields new, `seq` odd, then again after step 4 with `seq` even), and a reader copying that slot never
  accepts it before step 4 completes, accepts the complete record after, and for a slot reused during a read reports it
  lapped after at most 4 retries.

- **Planner:** §6.2 as a golden channel trace; determinism (two runs, same bits; another seed, other side); a tracked
  `HEAD` wins on every frame; a score key on `lean` beats the planner; `look_held` leaves gaze alone; **a private turn** (the §6.2 script with
  `private`) gives an all-rest trace, and a private turn starting mid look-away eases back with no new move; the bench.
- **Money:** `SpendGate` with 100 left: reserving 80 then 30 refuses the second until the first settles; a reservation
  cut or failed before any usage arrives charges its whole bound, so a new 80 is still refused; **after partial
  usage** (80 reserved, `usage_so_far` worth 20, then cancel, or then a stream error) it still charges 80 and a new 80
  is refused; only a completed request settles with its final usage and releases the rest; an LLM turn and two syntheses in flight never exceed the cap.
- **Needs the owner (needs-user, local lane):** first-chunk TTS latency for Kokoro and Chatterbox on `R`; A2F-3D's `L`;
  a lip-sync viewing test (offset sweep −60..+60 ms); hosted provider latency and price.
