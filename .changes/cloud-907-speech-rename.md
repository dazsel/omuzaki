# omuzaki-speech: the speech host's new name

- The speech host crate is now `omuzaki-speech` (it was `omuzaki-audio`), so `omuzaki-audio` is free for the audio engine (ADR 0018).
- The rename changes no behaviour. Code that depends on the crate imports `omuzaki_speech` instead of `omuzaki_audio`.
