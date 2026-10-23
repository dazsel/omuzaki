# Voice services: speech to text, text to speech and the mouth

- The speech host crate is now `omuzaki-speech` (#907, ADR 0018); the `omuzaki-audio` name is the audio engine's.
- Voice detection and speech to text are model slots now: `vad@1` (Silero VAD 6.2.3 on the ONNX runtime) and `asr@1`
  (Whisper ggml models on a new `whisper-ggml` runtime), each with a smoke test on public-domain LibriVox fables that
  every model passes before it installs. Whisper `tiny.en` misses 2.8 % of the words and `base.en` 2.4 %. The speech
  host opens its sessions from the model registry like every other model, so one ONNX Runtime serves the whole app
  (#1163, #1269, #838, #1155).
- A feed now reports where its audio played and can be cut cleanly: `FeedWriter::playhead` logs, block by block,
  which engine sample each written sample played on, exactly at any block size and at a voice's own rate (24 kHz and
  pitched rates included), and `FeedWriter::closer` fades the feed out over 2.5 ms within one block of the close. The
  mock and the real engine pull feeds through the same code, so tests see what the app hears (#1248).
