# 0041: The performer's voice is a two-feed TTS pump with a playhead; its face hears the voice ahead of playback; big moves are rules inside the score layer (2026-10-13)

**Status:** proposed (the cloud lead accepts it on the PR). **Context:** AP-02, AP-05, CN-10,
ML-04, ML-05, ML-07, ML-14, PF-12, PF-23, PF-24, SE-05, AP-11; ADR 0016 (user models), 0017 (registry), 0018 (audio),
0031 (the performer loop), 0032 (the LLM seam), 0040 (assistant runtime, pending in). Design:
`docs/design/ai-performer-voice-face.md`.

## Context

ADR 0031 fixed the seams: `Voice::speak(sentence, voice, &SpeakHow)`, a `SpeechHandle` whose `Ready` and `Playing`
carry the chunk's duration before playback, a cut within one audio block for a 165 ms barge-in, and two alternating
`ai_voice` feeds. It left the implementation, voices, consent, the face drive and big moves to this record. The audio
contract had no way to say when a feed sample plays, a streaming face model is late by its own latency, and the
blend order already puts a human tracker above the score layer.

## Decision

1. **TTS host** (`omuzaki-speech`): a synth thread (one `tts@1` call or hosted request per sentence chunk, whole chunks,
   at most 2 ahead) and a pump thread (10 ms slices, 300 to 500 ms of lead, feeds of 1 s). Two feeds alternate per cut;
   the cut feed re-opens off-thread. Edge silence is trimmed and re-padded (180 ms after a sentence, 80 ms after a comma
   split); a late chunk lengthens the pause and is counted. A cut is silent within one block plus a 2.5 ms fade, so
   ADR 0031's barge-in becomes 167.6 ms worst case on `R` (budget 200). Private speech goes to a third feed on an
   `ai_voice_private` bus whose only output is `monitor_out`, so leaving `CohostOnly` mid-turn can't put it on stream.
2. **`omuzaki-audio-api` gains three things:** `FeedWriter::playhead()` (samples consumed and the engine sample where the
   next enters the bus, written once per block), `FeedWriter::closer()` (a flag the performer thread raises directly on
   a cut) and a 2.5 ms fade when a feed closes. `Playing { from }` is computed from the playhead.
3. **Pitch is the declared feed rate** (`24 kHz × 2^(p/12)`, synthesis at `speed / r`), limited to ±4 semitones; speed
   0.7 to 1.4. `tts@1` is fixed as `text`, `voice`, `speed`, `expressiveness` → `pcm` (24 kHz); style tags are a closed
   set mapped to speed, expressiveness and gain.
4. **The voice belongs to the character:** `characters/<id>/voice-assignment.toml` holds a `VoiceAssignment` (`voice.toml` stays the effect presets, audio-api §6.2); `persona.toml`'s
   `voice` field is dropped. `VoiceId` is `<model>/<voice>` or `<connector>/<voice>`, validated against a
   `VoiceCatalog` (model manifests' `[[voices]]` tables and connectors' lists) at assign and at start, with no silent
   fallback to another voice.
5. **Consent:** every cloned voice, the streamer's own included, needs a consent record (`ConsentLookup`,
   `Likeness::VoiceClone`), checked at assign, at start and at session open; a public-figure match warns.
6. **Hosted TTS** is bounded before and charged after each request on the performer's session, against its per-show
   cap, through one `SpendGate` per performer shared with its LLM brain: overlapping bounds are reserved together, so
   they can never pass the cap between them, and only a completed request settles with its final usage: a
   cancelled, failed or dropped reservation is charged its whole bound, whatever partial usage it saw; `voice.assign` with a hosted provider needs `spend` and shows a per-hour cost preview. `chat_tts` (AP-11) shares
   only the slot and the catalog.
7. **Speech-to-speech in AP-02** means ML-14 voice conversion of the streamer (cover mode). Without a shippable model
   the person gets BR-24 and their own model (ADR 0016); an end-to-end speech model for the performer is rejected.
8. **Face:** the pump hands the mouth drive (`omuzaki-voice`) each slice up to 100 ms ahead of playback, stamped with its
   program time (`VoiceLead`); the drive runs `voice_to_face@1` paced on those stamps, falls back to visemes per missed
   block (50 ms crossfades), releases results only when the program clock reaches them (so a cut never has to take
   one back), drops the pending ones on a cut and filters late model results by a cut generation (with a stream
   restart), and writes a `Source::Audio` `Max` layer on
   the mouth stamped `program_time − D`. A turn's first chunk starts up to 100 ms later to make room. Private speech
   leaves the face at rest, mouth and big moves alike.
9. **Big moves:** a `BigMovePlanner` trait in `omuzaki-performer-api`, rules first (`omuzaki-performer`): look away while
   thinking, lean in on a question, turn to the co-host, a laugh bob. It writes head, lean, body turn and gaze into the
   score layer, below any explicit score key and below the tracker; life rides on top. A private turn starts no move
   and ends the active ones. Seeded (`moves` stream),
   ≤ 2 µs per frame on `R`. ML-07 replaces it only when it beats it on liveliness, a blind preference test, cost and the
   same seam.

## Consequences

- `omuzaki-performer-api` gains `SpendGate`, `VoiceAssignment`, `VoiceProvider`, `TalkState` (named so because ADR 0031's
  `SpeechState` exists), `MoveContext`, `StyleKnobs`, `BigMovePlanner`, `VoiceLead`, `VoiceCatalog`, `VoiceInfo`.
- `audio-api.md` §3.7 and §4.4 carry one-line notes for the playhead, the closer and the fade; ADR 0031's §5.3 budget
  absorbs up to 100 ms of first-chunk lead (2,496 ms p95 against 2,500).
- `ai-performer.md` §4 loses `voice`; the unlocked tasks include that edit.
- `omuzaki-ai-mock` gains `FakeTts` and `FakeVoiceToFace`; goldens pin one spoken sentence and one look-away and lean-in.
- **Rejected:** driving the mouth from the `ai_voice` bus tap (late by the model's latency); a delay on the `ai_voice`
  bus to hide it (adds the delay to barge-in); pushing future-stamped frames straight into `PlayoutDelay` (a cut could
  not take them back); streamed hosted synthesis (no duration before playback); a pitch shifter for the AI voice (the
  feed rate does it); a planner layer of its own (the score layer already has the right place in the order); a silent
  voice fallback.
