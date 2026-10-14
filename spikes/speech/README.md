# Speech spike: a GPL-free ASR and VAD stack

**Answer: yes.** Silero VAD through `ort`, plus Whisper through `whisper-rs`, replaces `sherpa-onnx` with no espeak-ng
anywhere in the build, and both run comfortably on the CPU. This spike proves it with a streaming VAD, a
segmenter, a Whisper wrapper, a benchmark harness and tests. It is its own Cargo workspace (like `toon-viewport`), so its
dependencies stay out of the main workspace until `omuzaki-audio` is built from it.

## Recommended stack for `omuzaki-audio`

| Job | Choice | Licence | Why |
|---|---|---|---|
| Voice activity | Silero VAD v6.2.3 (`silero_vad.onnx`) through `ort` 2.0.0-rc.13 with `load-dynamic` | MIT | 0.1 to 0.2 ms per 32 ms frame; matches the Python reference to 6e-8; no false alarms on noise, hum or clicks |
| ONNX Runtime | Microsoft's official 1.28.3 release, loaded from a path we choose | MIT | Not Pyke's prebuilt binaries (register: `ort`) |
| ASR | Whisper through `whisper-rs` 0.16 (whisper.cpp built from source) | Unlicense (crate), MIT (whisper.cpp, weights) | See the tables: `base.en` or `small.en` q5_1 for live use, `large-v3-turbo` q5_0 as the offline ceiling |
| Glue | the `segmenter` here: two thresholds, minimum speech and silence, padding, a 28 s cut | ours | Streaming, no allocation per frame |

Not used: `sherpa-onnx` and `sherpa-onnx-sys` (they link GPL-3.0 espeak-ng; banned in `deny.toml`).

## What is here

| Path | What |
|---|---|
| `src/vad.rs` | `SileroVad`: ONNX session, 64-sample context and LSTM state carried between 512-sample frames |
| `src/segmenter.rs` | Streaming speech start/end events from per-frame probabilities (hysteresis, padding, max length) |
| `src/asr.rs` | `Whisper`: greedy or beam decoding, a token cap and no temperature fallback so latency is bounded |
| `src/pipeline.rs` | VAD over a recording, frame agreement and acoustic boundary lag |
| `src/metrics.rs` | Word error rate with alignment, latency percentiles |
| `src/synth.rs` | Seeded noise, hum, clicks, a vowel-like syllable train, SNR mixing |
| `src/bin/speech-bench.rs` | `vad-synth`, `vad`, `pipeline` subcommands that produced the numbers below |
| `tests/` | Unit tests plus integration tests that skip (with a message) when models are absent; a golden test against the Python reference (`tools/vad_reference.py`) |
| `tools/check-no-espeak.sh` | The acceptance check: dependency tree, symbols, strings and linked libraries |

Models, the ONNX Runtime library and the speech clips live outside git, in
`$OMUZAKI_ASSETS/models/speech/` and `$OMUZAKI_ASSETS/datasets/speech-pd/`. Each folder has a
`PROVENANCE.md` (sources, revisions, licences) and `SHA256SUMS.txt`.

## Running it

From this directory (the `.cargo/config.toml` here must be picked up, see "Findings"):

    cargo test                  # unit tests; integration tests skip without the model files
    cargo build --release
    M=$OMUZAKI_ASSETS/models/speech
    D=$OMUZAKI_ASSETS/datasets/speech-pd
    B=$(cargo metadata --format-version 1 --no-deps | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/release/speech-bench
    $B --runtime $M/onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so --vad-model $M/silero_vad.onnx vad-synth
    $B --runtime ... --vad-model ... vad $D/aesop-52-farmer-and-his-sons.wav
    $B --runtime ... --vad-model ... pipeline --clip $D/aesop-52-farmer-and-his-sons.wav:$D/aesop-52-farmer-and-his-sons.txt \
        --model $M/ggml-base.en.bin --threads 4 --runs 3 --fit-audio-ctx

Environment for the tests: `OMUZAKI_SPEECH_MODELS`, `OMUZAKI_SPEECH_DATA`, `ORT_DYLIB_PATH` (defaults point at a local assets folder).

## Test audio

No TTS. Recordings of people are never committed.
- **Synthetic** (`synth.rs`, seeded, bit-identical everywhere): silence, white and pink noise, hum, keyboard clicks, a vowel-like
  syllable train. Used for false alarms and per-frame timing.
- **Public-domain speech**: four LibriVox readings of Aesop's Fables (volume 3, sections 51, 52, 54, 67; four readers),
  trimmed to the fable body, 194.5 s and 534 words in all, with the 1912 Project Gutenberg text as reference.
  LibriVox donates its recordings to the public domain (https://librivox.org/pages/public-domain/). Sources, reader names, cut
  points and hashes are in `datasets/speech-pd/PROVENANCE.md`. They are 64 kbps MP3 decoded to 16 kHz mono, so slightly
  worse than studio audio, and the readers sometimes depart from the printed text (every model pays for that equally).

## Results

Machine: a high-end desktop CPU (32 logical CPUs), WSL2, release build, CPU only. **The machine was shared**: the load average was 30
to 55 during every timing run (other builds and tests), so the wall-clock numbers below are pessimistic, and
the 8-thread rows compete with that load. Raw logs for every table are in `datasets/speech-pd/results-2026-10-12/` under the assets folder.
Word error rate (WER) uses 534 reference words, so one word is 0.19 percentage points: differences of a few tenths are noise.

### Voice activity (Silero VAD through `ort`)

| Measure | Result |
|---|---|
| Matches the Python reference (`tools/vad_reference.py`, stock onnxruntime) | worst frame deviation 6e-8 over 1,679 frames (test asserts under 1e-4) |
| False alarms on 90 s of non-speech: silence, white noise -40/-20 dBFS, pink noise -30/-12 dBFS, 50 Hz hum, 220 Hz tones, clicks, vowel-like syllables | 0 segments; highest probability 0.07, except 0.56 in one frame of the 220 Hz tone stack (no segment) |
| Inference per 32 ms frame (4 clips, 1 thread) | mean 0.16 to 0.20 ms, p99 0.6 to 1.5 ms, worst 7 ms (under load 40); real-time factor 0.005 to 0.006 |
| Sound to `Start` event (first frame at 0.5, 32 ms frames) | median 32 to 40 ms, i.e. one frame; p95 up to 384 ms on soft or breathy starts |
| Last speech energy to `End` event (200 ms hangover) | median 272 to 352 ms |
| Speech kept | 11 to 18 segments per 40 to 55 s clip, none rejected as too short |
| Frame decisions vs the clean run when noise is added (F1, 4 clips) | white 20/10/5/0 dB SNR: 0.985 to 0.995, 0.976 to 0.990, 0.972 to 0.985, 0.965 to 0.974; pink: 0.992 to 0.994, 0.979 to 0.988, 0.974 to 0.982, 0.927 to 0.971 |

The 32 ms frame is also the floor for barge-in (AP-03, 200 ms): the VAD can say "speech started" about 32 to 64 ms after the
sound, and the ASR is not on that path.

### Whisper accuracy and cost (greedy, 4 threads unless stated, VAD segments, full 30 s window)

| Model (ggml, English) | Size | WER | ASR time per segment, median (p95) | ASR time / audio |
|---|---|---|---|---|
| `tiny.en` | 78 MB | 2.25 % | 0.58 s (2.3 s) | 0.28 |
| `base.en` | 148 MB | 1.50 % | 1.63 s (2.6 s) | 0.58 |
| `small.en` q5_1 | 190 MB | 1.69 % | 5.89 s (7.6 s) | 1.98 |
| `small.en` q5_1, 8 threads | 190 MB | 1.69 % | 2.63 s (4.1 s) | 0.96 |
| `large-v3-turbo` q5_0, 8 threads, segments split at 500 ms | 574 MB | 1.12 % | 15.1 s (17.4 s) | 2.73 |

- The segments here come from the default segmenter (200 ms hangover, 100 ms padding): 57 segments of a few seconds each.
- On clean read speech the models are within a few words of each other; the 1.1 % of `large-v3-turbo` is the practical
  floor, set by the readers departing from the printed text and a few real errors ("made at once" heard as "made it once").
- Cost is almost flat in segment length because whisper.cpp always encodes a 30 s window, so many short segments are
  expensive. `base.en` thread scaling: 2 threads 2.35 s, 4 threads 1.63 s, 8 threads 0.64 s median per segment.
- Model load: 0.2 to 1.0 s from the local disk.

### Latency and compute levers (`base.en`, 4 threads)

| Setting | Segments | WER | ASR time / audio |
|---|---|---|---|
| hangover 200 ms (default) | 57 | 1.50 % | 0.58 |
| hangover 600 ms | 28 | 0.94 % | 0.30 |
| hangover 1000 ms | 12 | 1.31 % | 0.17 |
| no VAD, fixed 28 s windows | 8 | 1.50 % | 0.11 |
| encoder window fitted to the audio (`--fit-audio-ctx`), `tiny.en` | 57 | **91 %** | 0.12 |

A longer hangover merges phrases into sentences: fewer Whisper calls and fewer words cut at a pause, in exchange for
the end-of-speech delay. Fitting the encoder window looks like a free speed-up and is not: the decoder repeats itself
(`tiny.en` WER 2.25 % to 91 %, almost all insertions). Do not use it.

**Utterance latency** (end of speech to final text) is the hangover plus the ASR time for that segment. With a 200 ms
hangover that is about 0.3 s plus 0.6 s (`tiny.en`) or 1.6 s (`base.en`) on 4 contended threads, or about 1 s for `base.en`
on 8. `small.en` and `large-v3-turbo` do not keep up with real time on this CPU under load; they suit offline
transcription, or the GPU (follow-up).

### Noise, end to end (`base.en`, pink noise mixed in before the VAD)

| SNR | WER |
|---|---|
| clean | 1.50 % |
| 10 dB | 4.49 % |
| 0 dB | 26.97 % |

The VAD still finds the speech (11 to 20 segments per clip, against 11 to 18 clean); the loss is Whisper `base.en`'s, and is
worst on the already noisy Monkey recording.

## Licence and GPL checks (the acceptance criteria)

- **`cargo deny check licenses bans sources`** with the repo's `deny.toml` (`../../deny.toml`) run on this workspace:
  `bans ok, licenses ok, sources ok`, exit 0 (only "multiple versions of `syn`" warnings, build-time only).
- **The ban fires.** A scratch crate that depends on `sherpa-onnx = "1.13"` fails `cargo deny check bans` with exit 2, naming
  `sherpa-onnx` and `sherpa-onnx-sys` (negative control: the ban is real, not decorative).
- **No espeak in the binary:** `tools/check-no-espeak.sh` checks the dependency tree (all targets, all
  features), the symbol table, the strings and the linked libraries of a release binary built with symbols kept, and
  asserts a positive control (whisper.cpp symbols are visible to `nm`, so "no espeak" means something).
  Result: `ALL CHECKS PASSED`. Dependency tree: none of `sherpa`, `espeak`, `piper_phonemize`, `phonemizer`, `ucd_`. Symbols (4,747 in a 4.9 MB binary, 220 of them `whisper_*`): none. Strings: none. Linked libraries: only libstdc++, libgcc_s, libm and libc (ONNX Runtime is `dlopen`ed; a `strings` scan of `libonnxruntime.so.1.28.3` also finds none). Raw log: `datasets/speech-pd/results-2026-10-12/espeak-check.log` on D:.
- **Licences of what we pull in:** `whisper-rs` and `whisper-rs-sys` Unlicense; whisper.cpp MIT; `ort` and `ort-sys`
  MIT OR Apache-2.0; `hound` Apache-2.0; ONNX Runtime MIT; Silero VAD MIT; Whisper weights MIT (Hugging Face tag `mit`
  on `ggerganov/whisper.cpp`). `bindgen`, `cmake` and `cc` are build-time only. Register rows are updated in
  `docs/licensing/register.md`.

## Findings worth knowing before building `omuzaki-audio`

1. **The repo's linker breaks whisper.cpp.** The distro mold 1.0.3 (`.cargo/config.toml` at the repo root) leaves
   whisper.cpp's C++ static initialisers broken: the first model load aborts with `std::out_of_range` from an empty
   `static const std::map`. GNU ld, lld and mold 2.42.1 all work. This directory carries its own `.cargo/config.toml`
   (bfd); the real fix is a newer mold on the machines and runners.
2. **Whisper's cost is the 30 s window, not your audio.** Short VAD segments still cost a full encoder pass, so a 2 s phrase
   takes about as long as a 25 s one. Shrinking the encoder window to the audio (`audio_ctx`) makes a segment about 2.5 times
   faster, but the decoder then fails to stop and repeats itself ("and said to him and said to him ..."): `tiny.en` goes from
   2.25 % to 91 % WER, and one unguarded run spent 34 s on a 2 s phrase running to the 224-token limit. The option stays in
   the code only so it can be re-measured. The levers that work are the model size, the thread count and the hangover (above).
   `asr.rs` also caps tokens at 8 per second of audio plus 16 and turns off the temperature fallback (which re-decodes a
   failed window up to six times), so one bad segment cannot stall a live pipeline.
3. **`ort` with `load-dynamic` panics if the library is missing**, unless you call `ort::init_from(path)` first.
   `vad::load_runtime` does, and returns an error instead. `ort` is still a release candidate (`=2.0.0-rc.13` is pinned);
   it checks the library's minor version and refuses an older one.
4. **Silero needs its context.** Each call takes the previous frame's last 64 samples in front of the 512 new ones, plus the
   LSTM state. Without the context the probabilities are wrong; the golden test against the Python reference catches that.
5. **Why there is no VAD precision/recall against labels.** Energy-based "ground truth" turned out useless on these
   recordings (noise floors from -38 to -65 dBFS), so VAD accuracy is judged by what it is for: false alarms on non-speech,
   stability when noise is added (decisions compared with the clean run), no words lost (Whisper on VAD segments against the
   Whisper ceiling), and the golden match. Hand-labelled boundaries are a follow-up.
6. `whisper-rs` also wraps whisper.cpp's built-in Silero VAD (`WhisperVadContext`). It works on whole buffers only, so it
   does not replace the streaming wrapper here; it would be a way to drop the ONNX Runtime dependency for offline VAD.

## Not covered

Multilingual accuracy (the clips and models are English), GPU Whisper, hand-labelled VAD boundaries, live partial captions,
the `ort` stable release, the production crate and a fix for the linker problem above.
