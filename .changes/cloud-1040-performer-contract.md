# omuzaki-performer-api and omuzaki-performer-mock: the AI performer contract and its fake harness

- New API crate `omuzaki-performer-api` (ADR 0031, ADR 0041). It holds:
  - the intents, with `GazeTarget`, `PlaceId` and the closed group of `StyleTag`s;
  - the seams `Brain` (a non-blocking `TurnStream`), `Voice` with `SpeechHandle`, `Ears` with the early `Held`, `Stage` with `Inventory`, and `Perception`;
  - `PerformerEvent` and the JSON Lines `TranscriptLine`, whose private lines carry no text;
  - `AiLine`, defined but not yet registered as a live event kind (#1915);
  - the limits, and the voice and face types: `SpendGate`, `VoiceAssignment`, `VoiceCatalog`, `VoiceLead`, `CutProbe` and the per-frame `BigMovePlanner`;
  - the `conformance` checks every implementation of a seam must pass (#1040, #1247).
- New mock crate `omuzaki-performer-mock`. Its fakes cover every seam on one `FakeClock`, and `Transcript` writes and diffs golden transcripts. The §10 five-turn exchange, driven through `FakeLlm`, matches `goldens/five-turns.jsonl` byte for byte (#1190).
- `ai-performer-voice-face.md` §3.6: an LLM reservation settles from the stream's final `Usage` event, which comes just before `Stop`, not from `Done { usage }`.
