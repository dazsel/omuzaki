---
title: "0018: One 48 kHz mixer graph on a real-time thread, cpal behind a seam, taps for every listener, and the A/V delay with the performance channels (2026-10-12)"
summary: "One 48 kHz mixer graph on a real-time thread, cpal behind a seam, taps for every listener, and the A/V delay with the performance channels"
area: decisions
---

# 0018: One 48 kHz mixer graph on a real-time thread, cpal behind a seam, taps for every listener, and the A/V delay with the performance channels (2026-10-12)

**Status:** accepted (2026-10-12). The review answers are folded into decisions 6, 7, 13 and 14. **Context:** PF-08, PF-12, PF-26, BR-18, BR-21, BR-22, BR-24, ML-03, ML-14, SE-04,
SE-05; spec §8, §9, §10 (`voice.fx`, `voice.fx.pitch`, `voice.fx.cpu`), §39.1; ADR 0016 (user-supplied models), ADR
0017 (ML-01 model slots, `BlockSink` / `BlockSource`); research `2026-10-voice-changers.md`; the speech host.
The design, with the types, is `docs/design/audio-api.md`.

## Context

Several features each need audio, and none of them owns the audio path:
- the voice-changer input and its A/V delay (PF-12);
- the mixer (BR-22);
- voice effects (BR-24);
- the soundboard (BR-21);
- recording (BR-18);
- captions and the AI performer's listening (BR-13, AP-03);
- lip sync (FE-08, PF-08);
- voice conversion (ML-14).

They need one clock, one real-time thread, a stable way to bind devices that Windows reorders, and a rule for which
signal each listener hears. The speech host already exists as `crates/omuzaki-audio` and takes 16 kHz mono through
its own lock-free ring. The ML-01 design (ADR 0017) already defines streaming model slots with block-counted deadlines.

## Decision

1. **Backend.**
   - cpal 0.18 (Apache-2.0; the register's `conditions` row: `asio` stays banned), behind our own `AudioBackend` trait,
     so tests use a scripted fake and a native WASAPI backend (exclusive mode or `IAudioClient3`) can be added later
     as a sibling crate.
   - cpal is shared mode only on WASAPI. The native backend is built only if the Dev loopback test shows shared mode
     costs too much.
   - **ASIO stays out:** its SDK is GPLv3 (banned when linked) or a proprietary Steinberg agreement (a maintainer and
     legal decision), and external changers do their own I/O anyway.
2. **Devices.**
   - Bound by cpal's stable `DeviceId`, never by index, per **role** (`voice`, `raw_voice`, `program_out`, …).
   - Projects name roles; a person-level machine profile maps roles to devices.
   - On loss: silence (or discard), flagged gaps, an event, and the graph keeps running. The timer clock takes over if
     the clock device went.
   - Reconnect only to the same id. A device with the same name and a new id is offered to the person, never taken.
     There is no silent fallback to another device.
3. **Graph.**
   - A mixer (sources → buses with effect chains, gain, mute, ducking → sends → outputs), not a free node graph.
   - It runs at **48 kHz always**, with the clock device's callback size as the block (≤ 1024 frames) and
     block-size-invariant nodes.
   - Planar `f32`, FTZ on.
   - Extra devices come in through bridges with capture-timestamp alignment and drift resampling (rubato).
4. **Real time.** The audio thread never allocates, frees, locks, logs or blocks. There are three change paths:
   - parameters: `Copy` messages on an SPSC ring, smoothed in the node;
   - structure: compiled off-thread, swapped at a block boundary, surviving nodes moved by id with their state, old
     graphs freed on the control thread;
   - devices: the bridge changes state and the graph is untouched.
5. **Latency is declared.**
   - Every node reports a fixed latency while it is in the graph, and a disabled node keeps it.
   - Route latency is computed and reported, with no global delay compensation between sources.
   - The conformance suite checks every declaration.
   - The `voice.fx` gates read the declarations plus one 256-frame block.
6. **Taps and feeds.**
   - A tap copies a graph point into an SPSC ring (the speech host's ring, generalised and moved into the API crate) with a drop
     policy: `DropNewest` for contiguous streams (ASR, recording, the clap test), `Overwrite` for latest-window
     readers (lip sync, cues).
   - Gaps are reported to the reader.
   - A consumer that owns a ring passes a `TapSink`. The speech host plugs in that way, unchanged.
   - Feeds are the mirror image, carrying audio into a bus.
   - PF-12's voice taps resolve `raw` and `heard` per consumer from what is bound.
   - **Mute means nobody hears, the AI included:** every voice tap goes silent, `listen` too. People mute to say
     something private.
   - **"Talk to co-host only"** is a third mic state, `CohostOnly`:
     - the stream mic is muted and only the AI's `listen` tap is open;
     - the AI's replies go to the monitor only;
     - an indicator is always visible;
     - it needs a monitor output, and leaving it lands on `Muted`;
     - it is never the default, and agents can't enter it.
7. **The A/V delay is not the audio engine's.**
   - It is a `PlayoutDelay` in `omuzaki-anim-api`, run by `omuzaki-stage` before the blend.
   - It is **one delay for the whole performance stream**: tracking, hotkey expressions, triggers on the avatar and
     audio-derived layers stay coherent with the voice viewers hear.
   - Sources differ only in their stamp: capture time, firing time, or **performance time**
     (`tap time − capture_offset`). AI-driven layers are stamped `program time − D`, so they land on time with the AI
     voice.
   - UI feedback is immediate, and scene, OBS and overlay actions are never delayed.
   - The audio engine supplies the numbers: the declared chain latency and the clap-test measurement.
   - A change of delay ramps at no more than 10% faster or slower than real time, so a change of Δ takes Δ / 0.1, with
     no minimum: 20 ms takes 0.2 s, 200 ms takes 2 s.
8. **Clap test and drift.**
   - Latency between the raw and the changed voice is estimated by correlating three-band onset envelopes, never
     waveforms, with validation before anything is used.
   - The same estimator runs in the background on speech to warn about drift on the health meter.
9. **Model slots plug in, not duplicated.**
   - ML-14 runs as a `model.stream` node that uses ADR 0017's `BlockSink` / `BlockSource`. Its fixed latency includes
     accumulation and resampling, and a missed or stalled block falls back to the time-aligned dry voice.
   - Other streaming models are tap consumers.
10. **Privacy (SE-04 for audio).**
    - Audio never leaves the device unless a person routes it.
    - Agents can never grow the set of (captured source → egress) pairs, never bind devices, never unmute captured
      audio into an egress, and never receive captured audio as audio.
    - Numbers (meters, latency, drift) are open to them.
11. **Crates.**
    - `omuzaki-audio-api` (contract, wasm-clean), `omuzaki-audio-mock` (reference offline engine, fake backend,
      conformance).
    - `omuzaki-audio` (the engine and the `audio.*` ops), `omuzaki-audio-fx` (the BR-24 nodes and `audio.fx.*`).
    - **The speech host becomes `omuzaki-speech`**, a pure rename: different dependencies, threads and
      ownership (ML-01 migrates it), and separate fences.
    - Lip sync and cue extraction go in a new `omuzaki-voice`.
    - The mixer moves from `omuzaki-stream` to `omuzaki-audio`.
12. **The pitch and formant shifter is ours.** It is pitch-synchronous: F0 tracking, TD-PSOLA on the LPC residual,
    and formants warped through line spectral frequencies. Its latency is one maximum pitch period plus a margin
    (≤ 19 ms), which keeps `voice.fx.pitch` at 24.3 ms against 25.
13. **A program safety limiter, on by default.**
    - `fx.limiter` on `program`: true-peak detection with 4x oversampling (BS.1770-4), a ceiling of −1 dBTP and a
      1.5 ms look-ahead, so it is transparent.
    - Its gain reduction is shown on the program meter, and it has an off switch.
    - Its latency (about 2 ms) is declared and counted in routes through `program`, but not in `voice.fx`.
14. **Acoustic echo cancellation ships** (P0 for the AP-03 co-host loop): the co-host on speakers must never hear its
    own voice.
    - `fx.aec` sits first on `mic` and `raw_voice`, before every voice tap. Its reference is the speaker bus, taken
      from the previous block through a node sidechain.
    - It turns on when speakers are in use.
    - The implementation is permissive only (WebRTC AEC3 BSD-3, speexdsp BSD, or ours), picked by research.
    - A changer outside Omuzaki can't be cancelled on its output (non-linear), so headphones are recommended for that
      path.

## Consequences

- **Spec edits, made on acceptance in the same PR:**
  - §9: `omuzaki-audio` is the engine and mixer, `omuzaki-stream` no longer lists "the audio mixer", and `omuzaki-speech`,
    `omuzaki-audio-fx` and `omuzaki-voice` are added.
  - SE-04: "audio never leaves the device unless a person routes it".
- **`omuzaki-anim-api` gains `PlayoutDelay`.** That is an API change, in its own task.
- **The speech host merges as it is;** a small task renames it and wires its `AudioInput` as a `TapSink`.
- **ADR 0017's text** says speech slots live in `omuzaki-audio`; with this record they live in `omuzaki-speech`.
- **Tasks:** the list is in design doc section 12, filed under PF-12, BR-22,
  BR-24, BR-21, PF-08, PF-26, ML-14, ML-03 and AP-03.

## Rejected

- **A free node graph** (any node to any node): more UI and harder privacy checks for nothing BR-22 asks for.
- **A fixed internal quantum** (128 frames, as in WebAudio): adds up to a quantum of latency. Block-size invariance
  buys the same determinism.
- **Delaying video frames** for PF-12: costs memory and GPU, and delays the life layer and physics too. Delaying
  channels is cheaper and is what PF-12 specifies.
- **Waveform correlation** (GCC-PHAT) for the clap test: a changed voice has a different waveform.
- **The speech host as a module of the engine crate:** couples cpal and DSP to ort and whisper.cpp builds, puts two
  fences in one `lib.rs`, and fights the ML-01 migration.
- **ASIO through the GPLv3 SDK:** banned by the licence policy.
- **An "AI hears me while muted" toggle:** mute must mean private. The explicit, visible `CohostOnly` mode replaces it.
- **Delaying only the tracker:** hotkey expressions would run ahead of the voice. One delay keeps the performance
  coherent.
- **A soft-clip limiter mode in the compressor:** not transparent, and it ignores inter-sample peaks. The program gets
  a true-peak limiter of its own.
- **`rtrb` or `triple_buffer` for rings:** the speech host's safe-Rust ring is already reviewed and fast enough at audio rates.
  `triple_buffer` is MPL-2.0 and adds a licence condition for a 100-line structure.
