---
title: "0031: The AI performer is a `perform`-only session that speaks with inline intent tags (2026-10-13)"
summary: "The AI performer is a `perform`-only session that speaks with inline intent tags"
area: decisions
---

# 0031: The AI performer is a `perform`-only session that speaks with inline intent tags (2026-10-13)

**Status:** proposed. It adds a new API crate
(`omuzaki-performer-api`). **Context:** AP-01, AP-03, AP-04, ST-12, PF-12, ML-03, SE-04, SE-05; spec v3 §8,
§12, §16, §22, §23; ADR 0018 (audio), 0020 (sessions), 0024 (scenes), 0027 (automation), the LLM seam.
Design: `docs/design/ai-performer.md`.

## Context

No design existed for the AI performer, while everything around it did: the `listen` tap, the `ai_voice` feed, the
`CohostOnly` mic mode and echo cancellation (audio-api §4), the `vad@1`, `asr@1` and `tts@1` slots, scores and channel
ownership, agent sessions, and the chat envelope. Spec §12 gives it `perform` only; AP-03 asks for barge-in under
200 ms; the LLM seam is designed in parallel. Implementation crates cannot link each other (§39 rule 10).

## Decision

1. **Crates:** `omuzaki-performer-api` (traits `Brain`, `Voice`, `Ears`, `Stage`, `Perception`; `Persona`, `Intent`,
   events), `omuzaki-performer-mock` (the fake harness), `omuzaki-performer` (loop, LLM brain, chunker, chat intake, ops).
   `omuzaki-speech` implements `Voice` and `Ears`; the perform pipeline implements `Stage`; chat arrives through an
   `EventSink` the App tees; the App wires all of it.
2. **Identity:** one `Transport::InApp` session per performer with role `performer`, ceiling `{perform}`, principal the
   streamer, bound to one cast member. LLM cost is charged with `Sessions::charge` against a per-show cap (default
   $1.00 with a hosted model). Chat never grants anything.
3. **Persona** lives in `characters/<id>/persona.toml`, is versioned by its content hash (shown in every transcript)
   and by VC-01 named versions, and renders to a cached system block of at most 2,000 tokens.
4. **Turn-taking:** the human always holds the floor; the performer answers when addressed by name (or within an 8 s
   follow-up window), within talk-time limits. Barge-in cuts the voice after 3 VAD frames held (a new `Held` event from
   the speech host) by closing the `ai_voice` feed: 165 ms worst case on `R`. End of speech to first audio is budgeted
   at p95 ≤ 2.5 s.
5. **Output protocol:** inline tags (`[emote happy 0.8]`) in the streamed reply, no tools. Malformed tags are dropped,
   never spoken. Chat enters the prompt as quoted data with brackets neutralised; at most 5 messages per turn.
6. **Intents** are validated against the member's own expressions, clips and places, rate-limited (2/s, burst 6),
   applied as `performer.intent` live-action ops, and compiled to score fragments, IK look-at goals or a live
   placement offset.
7. **Privacy:** private (`CohostOnly`) turns go to the monitor only and apply no intents; perception is opt-in per
   source, a camera only to a local model, at most one 768 px frame per 10 s.

## Consequences

- New crates as in 1; the speech host gains `Held` and implements two traits; `Stage` becomes a `Services` entry.
- `omuzaki-performer-api` registers the `ai.line` event kind (one per spoken chunk, never for private turns), published
  through a new `TrustedEventSink` with the performer's session as `acting`; automation handlers it triggers run with
  the show's grants ∩ the performer's. Needs `TrustedEventSink` and `Source::Agent` in `omuzaki-live-api` and that
  narrowing rule in automation D7.
- Private turns keep a separate history that public prompts never include.
- Audio-api §4.6's fallback (gate `listen` while the AI speaks) disables barge-in; the performer says so instead of
  silently degrading.
- Anim-api §10's "intents compile to cues" holds for `emote`, `gesture` and `play_clip`; `look_at` and `move_to` go to
  IK goals and placement.
- AP-06 memory, AP-07 safety and AP-08 modes plug into named hooks without changing these contracts.
- **Rejected:** structured tool calls (no word timing, weak on local models, a wider attack surface); a line protocol
  (no word timing); a new `Transport::Performer` (the role already distinguishes it); the performer holding `read`
  (scene state comes from `Stage::inventory`); speculative replies on partial transcripts (a Whisper pass and an LLM call
  each); hosted vision on camera frames (SE-04).
