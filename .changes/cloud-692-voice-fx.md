# omuzaki-audio-fx: the voice effects, voice presets and the `audio.fx.*` ops

- The voice effect kinds `fx.eq`, `fx.gate`, `fx.compressor`, `fx.robot`, `fx.radio`, `fx.limiter` (-1 dBTP, 8x
  oversampled true-peak detector) and `fx.pitch`, our low-latency pitch and formant shifter (pitch-synchronous overlap
  and add with an LPC formant warp that keeps the level, a YIN F0 tracker, and a monotone mode). Every kind passes the
  node conformance suite in `omuzaki-audio-mock`.
- Eight built-in presets (Clean, Deeper, Higher, Giant, Tiny, Robot, Radio, Telephone) and a character's own presets in
  its `voice.toml` (`characters/<id>/voice`, now a built-in sibling key in `omuzaki-char-api`).
- The ops `audio.fx.kinds`, `audio.fx.preset.list`, `.get`, `.save`, `.set_param`, `.rename`, `.delete`,
  `audio.fx.apply` (a preset on the `mic` bus) and the jobs `audio.fx.render` and `audio.fx.compare`, which put a
  synthetic voice or a WAV file (person only) through presets and report its F0, formants, BS.1770 loudness, centroid,
  peak and latency, so an agent can tune a voice in numbers.
- Budgets: the voice chain adds 5.3 ms (24.3 ms with the pitch shifter), and costs 0.33% of an 8-core machine
  (`tests/budget.rs`, `benches/chain.rs`, `examples/voice_fx_budget.rs`).
- `omuzaki-audio-api` gains `VoiceFile` and `GRAPH_DOC_PATH`. The CLI and the studio link the crate.
