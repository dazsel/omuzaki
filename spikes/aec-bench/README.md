# Spike: echo cancellers for `fx.aec`

Measures echo return loss enhancement (ERLE), double-talk behaviour, added latency, CPU per 10 ms frame and heap
allocations per frame for each echo-cancellation candidate, on **synthetic, seeded signals only** (research #920, the
note is `docs/research/2026-10-echo-cancellation.md`). It is its own Cargo workspace (like the other spikes), so none of
its dependencies reach the main workspace. Nothing here is a recording of a person.

| Name | What it is |
|---|---|
| `webrtc-cpp` | WebRTC's C++ AEC3 through `webrtc-audio-processing` (needs `--features webrtc-cpp`, see below) |
| `sonora` | pure-Rust port of the same module (`sonora`) |
| `aec3-port` | a second pure-Rust port (`aec3`) |
| `speex`, `speex+pp` | SpeexDSP's MDF canceller, alone and with its preprocessor (`aec-rs`) |
| `nlms`, `pbfdaf` | our own first-draft time-domain and partitioned-block frequency-domain filters (`src/own.rs`) |
| `gate`, `duck20` | the cheap fallback: silence or duck the signal while the reference is active |
| `none` | pass-through, the calibration row |

```sh
cargo run --release -- table    # every candidate in five rooms (default)
cargo run --release -- sweep    # ERLE against the speaker-to-mic delay: each estimator's range
cargo run --release -- seeds    # the AEC3 builds over five seeds
```

Metrics are exact and repeat run to run; times vary by about 10 % on a shared VM, so read them as ratios between
candidates. `src/signals.rs` has the signal model, `src/main.rs` the metric definitions.

## The C++ build

`--features webrtc-cpp` builds WebRTC's own C++ with meson, ninja and abseil-cpp on the machine, which is why it is off by
default. The crate's build script fetches abseil through `wrapdb.mesonbuild.com`; where that host is unreachable, build
abseil 20240722.0 from its GitHub release tarball (SHA-256 `f50e5ac311a81382da7fa75b97310e4b9006474f9560ac46f54a9967f07d4ae3`) with CMake into a prefix and
point `PKG_CONFIG_PATH` at its `lib/pkgconfig`. Without the feature the `webrtc-cpp` rows print "could not be built".
The Speex candidate needs `cmake` and `clang` (bindgen).
