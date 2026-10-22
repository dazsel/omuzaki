# Voice services: speech to text, text to speech and the mouth

- The speech host crate is now `omuzaki-speech` (#907, ADR 0018); the `omuzaki-audio` name is the audio engine's.
- Voice detection and speech to text are model slots now: `vad@1` (Silero VAD 6.2.3 on the ONNX runtime) and `asr@1`
  (Whisper ggml models on a new `whisper-ggml` runtime), each with a smoke test on public-domain LibriVox fables that
  every model passes before it installs. Whisper `tiny.en` misses 2.8 % of the words and `base.en` 2.4 %. The speech
  host opens its sessions from the model registry like every other model, so one ONNX Runtime serves the whole app
  (#1163, #1269, #838, #1155).
