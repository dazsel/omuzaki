---
title: "Echo cancellation when the streamer uses speakers (2026-10-15)"
summary: "which echo canceller `fx.aec` is built on (#1268): licences by layer, ERLE, latency, CPU and allocations measured on synthetic signals, prior art, and the cheaper gate fallback."
area: research
---

# Echo cancellation when the streamer uses speakers (2026-10-15)

> Research for #920 (BR-22, AP-03). Echo cancellation ships (Director, 2026-10-12, `docs/design/audio-api.md` §4.6, open question 3). This note only **picks the implementation**; #1268 builds it. All numbers are from synthetic, seeded signals in `spikes/aec-bench` on one cloud VM, so they rank candidates; they do not predict how a real room, real speech or a real speaker sounds. Nothing here is a recording of a person.

## 1. Answer

**Recommendation: build `fx.aec` on the AEC3 family, using the pure-Rust port `sonora-aec3` (BSD-3-Clause), behind our own small `EchoCanceller` trait so the C++ build can replace it.** Keep the gate in §6 only as the "cannot converge" fallback.

| Candidate | Steady ERLE, 5 rooms (dB) | Near-end gain in double talk | Added latency | CPU per 10 ms frame, one core | Heap allocations per frame, Rust / native |
|---|---|---|---|---|---|
| **`sonora-aec3` (Rust AEC3)** | **27.8 to 48.6** | **−2.8 to −5.7 dB** | **9.0 ms** | **0.17 to 0.18 ms (1.7 to 1.8 %)** | **40.5 / 0.0** |
| WebRTC C++ AEC3 (`webrtc-audio-processing`) | 22.3 to 44.8 | −2.3 to −4.8 dB | 9.0 ms | 0.13 to 0.16 ms | 2.0 / 0.0 |
| `aec3` (second Rust port) | 26.7 to 48.3 | −2.9 to −5.2 dB | 9.0 ms | 0.20 to 0.25 ms | 67.0 / 0.0 |
| SpeexDSP MDF alone | 22.0 to 24.7 | −21.6 to −22.7 dB | 1.0 ms | 0.28 to 0.34 ms | 0.0 / 0.0 |
| SpeexDSP MDF + preprocessor | 35.7 to 44.6 | −33.8 to −43.9 dB | 9.5 ms | 0.34 to 0.48 ms | 0.0 / 0.0 |
| Own NLMS, 200 ms | 13.4 to 36.2 | not meaningful (§5.2) | none, by construction | 6.7 to 6.8 ms (67 to 68 %) | 0.0 / 0.0 |
| Own PBFDAF, 300 ms | 14.7 to 29.6 | 0.0 to +0.1 dB | none | 0.09 to 0.14 ms | 15.0 / 0.0 |

"Near-end gain" is the projection gain of the output on the true near-end talker (0 dB: the talker passes untouched). Residual echo and noise in the output do not count as preserved speech; "DT fit SDR" in §5.2 measures them. "Native" allocations are the C and C++ ones (`malloc`, `new`), counted separately from the Rust ones (§5.3).

**Fix round (after review of 71abc17).** Two measurement fixes were made: the near-end level is now the projection gain instead of total output energy over near-end energy (which counted residual echo as preserved speech), and allocations are counted natively as well as in Rust. The recommendation does not change: the AEC3 family is still the only one that reaches the ERLE target and passes the talker, and its CPU share is now 7 % of the `cpu.total` allowance on this VM (was 6 %). What did change: the AEC3 builds lose 2 to 6 dB of the talker (was reported as 1.6 to 3.2 dB), SpeexDSP alone turns the talker down about 22 dB (was 11 to 13), and the own NLMS no longer reads as "keeps the talker". The native count is 0 for every candidate, so the C++ fallback's case rests on its 2 Rust-side allocations per frame (the binding), not on a native claim.

The target to beat is audio-api §9: ERLE of at least 25 dB on simulated rooms with the near end preserved in double talk. Why this pick:

1. **Only the AEC3 family does both jobs.** In the base run `sonora-aec3` and `aec3` reach the 25 dB target in all five rooms and the C++ build misses `large` (22.3 dB); over five seeds per room `sonora-aec3`'s worst run is 26.1 dB, the C++ build's 22.3 and `aec3`'s 11.7 (§5.4). All three keep the person's voice within 5.7 dB while the co-host speaks. Speex alone misses 25 dB and already turns the talker down about 22 dB; with its suppressor it reaches ERLE by turning the talker down 34 to 44 dB, which kills barge-in. Our own filters are linear only: PBFDAF keeps the talker (0.0 to +0.1 dB) but leaves 7 to 52 % of echo frames audible, and NLMS up to 93 % while losing the talker in double talk (§5.2).
2. **The three AEC3 builds are one class.** Over five seeds per room (§5.4) their medians sit within a few dB of each other and the ranking changes with the seed, so the choice between them is about build, licence and allocation, not quality.
3. **Cost is small.** 1.7 to 1.8 % of one core is 0.22 % of the eight-core basis `cpu.total` uses, 7 % of its 3 % allowance (§5.3).
4. **The cost of the Rust port is its allocations.** `sonora-aec3` allocates about 40 times per frame in steady state, all of them Rust allocations (native count 0), which `AGENTS.md` forbids on the audio thread. #1268 must remove them or run the canceller off the audio callback; the C++ build, with 2 Rust-side allocations from its binding and 0 native ones in the C++ itself, is the named fallback (§7).

## 2. Licences, layer by layer

Read at the primary source on **2026-10-15**: each crate's published `.crate` (crates.io, `static.crates.io`) for the manifest, plus the project's own repository file where reachable. `webrtc.googlesource.com` and `chromium.googlesource.com` are refused by this environment's network policy, so Google's original `LICENSE` and `PATENTS` are quoted from the copies bundled in the `webrtc-audio-processing-sys` 2.1.0 crate. Rows are in `docs/licensing/deps/` (`sonora.toml`, `aec3.toml`, `webrtc-audio-processing.toml`, `speexdsp.toml`, `meson-ninja.toml`, `cmake-clang.toml`, `rustfft.toml`). `cargo deny` with the repo's `deny.toml`, run over the spike's whole tree with all features, passes licences and bans.

| Layer | Declared in the manifest | File read, and what it says | Source |
|---|---|---|---|
| WebRTC AEC3 (Google, original C++) | n/a | `webrtc/LICENSE` is BSD-3-Clause; `webrtc/PATENTS` begins "Additional IP Rights Grant (Patents)" and grants a "perpetual, worldwide, non-exclusive, no-charge, irrevocable" patent licence, which terminates if you sue over WebRTC | `webrtc-audio-processing-sys-2.1.0.crate`, `webrtc-audio-processing/webrtc/{LICENSE,PATENTS}` |
| `webrtc-audio-processing`, `-sys`, `-config` 2.1.0 (tonarino) | `license-file = "COPYING"`, no `license` field | `COPYING`: "Copyright (c) 2011, Google Inc. All rights reserved. Redistribution and use in source and binary forms, with or without modification, are permitted provided that…" (BSD-3-Clause; "Neither the name of Google nor the names of its contributors may be used to endorse or promote…") | `https://github.com/tonarino/webrtc-audio-processing` (raw `COPYING`, 200) and the 2.1.0 crates |
| bundled in that C++ tree | n/a | ooura FFT: "You may use, copy, modify and distribute this code for any purpose (include commercial use) and without fee"; Mark Olesen's fft: permission "for any purpose without fee… provided that this entire notice is included"; pffft/FFTPACK (UCAR): BSD-style, no endorsement; rnnoise: BSD-3-Clause (Mozilla, Xiph.Org); `spl_sqrt_floor`: "public domain… use them freely for any purpose"; abseil-cpp 20240722.0: Apache-2.0 (tarball hash matches the wrap file) | `…/webrtc/**/third_party/*/LICENSE`, `https://github.com/abseil/abseil-cpp/releases/download/20240722.0/abseil-cpp-20240722.0.tar.gz` |
| `sonora`, `sonora-aec3`, `sonora-agc2`, `sonora-common-audio`, `sonora-fft`, `sonora-ns`, `sonora-simd` 0.2.0 | `license = "BSD-3-Clause"` in all seven (`sonora-agc2` and `sonora-ns` are compiled by the `sonora` crate even with its gain-control and noise-suppression stages off) | repository `LICENSE`: "Copyright (c) 2011, The WebRTC Project Authors… Copyright (c) 2016, Arun Raghavan and contributors… Copyright (c) 2026, dignifiedquire" with the BSD-3-Clause body. **No `PATENTS` file at the repository root** (404 on `PATENTS`, `PATENT`, `NOTICE`); the `sonora-aec3` package contains no licence file (its README links to the repository's) | `https://github.com/dignifiedquire/sonora` (raw `LICENSE`, `Cargo.toml`) and the seven crates |
| `aec3` 0.4.0 (RubyBit) | `license = "MIT OR BSD-3-Clause"` | `LICENSE` is titled "WebRTC Derivative Work License and Patent Grant": the Rust code "is licensed under the MIT License", and "the following license applies to the portions of this software derived from the WebRTC project" (BSD-3-Clause), so the text is both, not a choice. `PATENT` restates Google's grant | `https://github.com/RubyBit/aec3-rs` (raw `LICENSE`, `PATENT`) |
| libspeexdsp (Xiph.Org) | n/a | `COPYING`: "Copyright 2002-2008 Xiph.org Foundation… Neither the name of the Xiph.org Foundation nor the names of its contributors may be used to endorse or promote…" (BSD-3-Clause). The copy vendored in `aec-rs-sys` is byte-identical | `https://github.com/xiph/speexdsp` (raw `COPYING`) |
| `aec-rs` 1.0.0, `aec-rs-sys` 1.0.0 | `license = "MIT"` | `LICENSE`: "MIT License, Copyright (c) 2025 thewh1teagle"; `-sys` vendors libspeexdsp and builds it with CMake and bindgen | `https://github.com/thewh1teagle/aec-rs` |
| `speexdsp` 0.1.2, `speexdsp-sys` 0.1.2 (rust-av) | `license = "MIT"` | `LICENSE`: "MIT License, Copyright (c) 2018 Luca Barbato"; `-sys` links a system libspeexdsp or builds one under a `build` feature whose source is a `TODO` | `https://github.com/rust-av/speexdsp-rs` |
| build tools for the C++ build: meson, ninja | n/a | `COPYING` in each repository is Apache-2.0 | `https://github.com/mesonbuild/meson`, `https://github.com/ninja-build/ninja` |
| build tools for the Speex candidate: CMake, Clang | n/a | CMake `LICENSE.rst` is BSD-3-Clause (Kitware); Clang's `LICENSE.TXT` in llvm-project is Apache-2.0 with LLVM exceptions | `https://github.com/Kitware/CMake`, `https://github.com/llvm/llvm-project` |
| our own NLMS / PBFDAF | n/a | none, except an FFT: `rustfft` 6.4.1 is `MIT OR Apache-2.0` (manifest; `LICENSE-MIT` and `LICENSE-APACHE` in the package) | `rustfft-6.4.1.crate` |

Verdicts against `policy.md`: every layer is on the allowed list (BSD-3-Clause, MIT, Apache-2.0, public domain). The traps:

- **The patent grant.** Google's PATENTS grant is the reason to prefer WebRTC code, and the Rust port `sonora` does not restate it (neither the repository nor the packages carry the file). A port of WebRTC source is a derivative of it, so the grant most likely still covers the port, but that is a question for legal review before release, not something engineering decides. The C++ binding and `aec3` ship the grant.
- **`cargo deny` cannot classify the C++ binding.** Its three manifests have no `license` field (`warning[no-license-field]`). Adding it needs a `clarify` entry in `deny.toml`, which needs a maintainer's review.
- **`aec3`'s SPDX string is `OR`, its text is "and".** `cargo deny` would pass it on the MIT arm while the file also requires the WebRTC notice. Ship both.
- **Notices.** The Rust port needs the three-line copyright block and BSD-3-Clause text in the notices bundle. The C++ build adds the five third-party notices above and Google's PATENTS.

## 3. Prior art

Read on 2026-10-15; hosts that this environment refuses are listed in §8.

- **OBS Studio's filter plugin has no echo canceller.** The `obs-filters` plugin on `master` (`CMakeLists.txt`) compiles these sources: async delay, chroma key, colour correction, colour grade, colour key, compressor, crop, EQ, expander, gain, GPU delay, HDR tonemap, invert audio polarity, limiter, luma key, mask, noise gate, scale, scroll, SDR on HDR, sharpness. Noise suppression is added only when `ENABLE_SPEEXDSP` is on ("Enable building with SpeexDSP-based noise suppression filter") plus `cmake/rnnoise.cmake` (not read); the locale file offers `NoiseSuppress.Method.Speex="Speex (low CPU usage, low quality)"` and `…RNNoise="RNNoise (good quality, more CPU usage)"`. `grep -i "echo\|aec"` over the locale file, the filter source and the build file finds nothing. What the generic OBS guides advise instead (a headset, a noise gate, speaker placement, §4) is the shape of the cheap fallback in §6. This is a statement about the plugin source, not about every OBS plugin or the other audio paths in `libobs`.
- **Web clients (WebRTC) expose echo cancellation as a capture constraint.** MDN's `MediaTrackConstraints.echoCancellation` page (from the `mdn/content` repository) defines `true` ("The browser decides what audio will be removed from the signals recorded by the microphone"), `false` ("No audio is removed; no echo cancellation will take place") and, where supported, `"remote-only"` and `"all"`. The page is about the constraint; it does not name AEC3, and whether each browser's default is on is **unverified** here. That AEC3 is "WebRTC's modern echo canceller" is the crates' own description (`sonora-aec3`, `aec3`), not a vendor document I could reach.
- **Discord: unverified.** `support.discord.com` is refused by the network policy, and search returned only third-party blogs, which say a toggle called "Echo Cancellation" sits in Voice & Video settings. I did not read a primary Discord source, so I do not state how Discord cancels echo.
- **What the three show:** the streaming-software side (OBS) has none; call software has it as an on/off feature of the capture path, before the app's own processing. That matches the design here: `fx.aec` first on the `mic` bus, off with headphones.

## 4. Question 1: how often do VTubers stream on speakers?

**Unverified.** I searched for vendor documents, surveys and forum threads. The results were generic "how to stop echo in OBS" guides (which all recommend a headset, a gate and speaker placement, i.e. they assume speakers happen), one OBS forum thread titled "i want to use speakers when streaming" that I could not fetch (`obsproject.com` is refused), and no survey of VTubers or streamers on headphones against speakers. I found no number and will not guess one. Because the Director has already decided AEC ships, the figure only matters for the default (on, off, or ask): §7 says to follow audio-api §4.6 ("when the person says they use speakers, or when a bound output is a speaker device").

## 5. Measurements

### 5.1 Method and machine

`spikes/aec-bench` is its own workspace (never a main-workspace member; its README has the commands). All signals are seeded and synthetic at 48 kHz mono in 10 ms frames:

- **Far end:** a speech-like source (gliding pitch, two moving formants, syllables with pauses, some fricative noise), 24 s, peak 0.5.
- **Room:** a seeded impulse response: a bulk delay, two early reflections, then an exponentially decaying noise tail 18 dB under the direct path, at an echo gain of 0.25. Five rooms: `small` (12 ms delay, RT60 150 ms), `medium` (40 ms, 300 ms), `large` (80 ms, 450 ms), `bt-delay` (180 ms, 300 ms) and `medium-nl` (medium plus a soft-clipped speaker).
- **Near end:** a second seeded speech-like talker from 14 s to 20 s (double talk), plus −70 dBFS white room noise.
- **ERLE:** echo power over output power, in the echo-only part (8 s to 14 s), on frames that have echo, capped at 60 dB. **Leak %:** echo frames whose output exceeds −50 dBFS, a crude energy proxy for "would trigger `listen`", not a VAD. **Lag:** the shift that best aligns the output with the true near end. **DT level:** the near-end gain in double talk, in dB: the projection gain of the output on the true near-end talker (0 dB is untouched), over blocks with both a talker and echo, after removing the measured lag. Residual echo and noise in the output are uncorrelated with the talker and do not raise it. **DT fit SDR:** signal-to-error after fitting one gain, so it measures leftover echo and distortion apart from level.
- **Machine:** Intel Xeon @ 2.80 GHz (AVX-512), 4 vCPUs, 15 GB, Linux 6.18, `rustc` 1.99.0, release build with thin LTO, idle (load below 1). It is a shared cloud VM, **not** reference tier `R`; no conversion is applied. Metrics repeat exactly; per-frame times varied by about 10 % between runs.

The harness is calibrated by a pass-through row (ERLE 0 dB, 94 to 96 % of echo frames leak, near-end gain −0.1 to +0.1 dB while the fitted SDR falls to between −4.7 and +0.4 dB because the echo is still there) and by the gate, which silences the output (ERLE hits the 60 dB cap, DT level −120 dB). These calibration cases (a known lag found exactly, a constructed half-amplitude talker at −6.02 dB, a perfect canceller, and convergence on a synthetic room) are `#[test]`s in the spike (`cargo test`), so a change that breaks a metric fails.

### 5.2 Quality by room

ERLE in dB (steady, echo-only). The 25 dB target is audio-api §9.

| Candidate | small | medium | large | bt-delay | medium-nl |
|---|---|---|---|---|---|
| WebRTC C++ AEC3 | 44.8 | 40.6 | **22.3** | 42.1 | 39.1 |
| **`sonora-aec3`** | 48.6 | 44.2 | 30.3 | 41.6 | 27.8 |
| `aec3` | 48.3 | 38.5 | 30.6 | **26.7** | **27.3** |
| SpeexDSP MDF, 500 ms | **24.7** | **24.5** | **24.6** | **24.3** | **22.0** |
| SpeexDSP MDF + preprocessor | 44.6 | 39.0 | 37.9 | 38.0 | 35.7 |
| own NLMS, 200 ms | 36.2 | 34.8 | 25.1 | **13.4** | **17.7** |
| own PBFDAF, 300 ms | 28.4 | 29.6 | 27.0 | **23.4** | **14.7** |

Echo frames still audible after cancelling (leak %), and the talker's level in double talk (DT level, dB):

| Candidate | leak % small / medium / large / bt-delay / nl | DT level small / medium / large / bt-delay / nl |
|---|---|---|
| WebRTC C++ AEC3 | 0.0 / 0.4 / 26.8 / 0.0 / 0.8 | −2.3 / −3.0 / −3.8 / −4.8 / −3.6 |
| `sonora-aec3` | 0.0 / 0.0 / 7.9 / 0.2 / 3.0 | −2.8 / −3.6 / −5.7 / −4.7 / −3.7 |
| `aec3` | 0.0 / 1.5 / 8.0 / 9.3 / 8.1 | −2.9 / −4.2 / −5.2 / −5.0 / −3.9 |
| SpeexDSP MDF | 16.0 / 46.6 / 67.9 / 52.3 / 73.7 | −22.7 / −22.0 / −21.6 / −21.7 / −22.0 |
| SpeexDSP MDF + preprocessor | 0.0 / 0.6 / 1.8 / 0.6 / 2.7 | −43.2 / −43.9 / −34.4 / −33.8 / −39.8 |
| own NLMS | 0.0 / 0.0 / 63.4 / 93.0 / 18.6 | −40.1 / −39.8 / −35.7 / −38.2 / −39.4 (not meaningful, below) |
| own PBFDAF | 7.5 / 9.3 / 35.5 / 51.1 / 51.7 | +0.1 / 0.0 / 0.0 / +0.1 / 0.0 |

Reading it:

- The AEC3 builds pull the talker down by 2 to 6 dB in double talk (its suppressor acts on the near end too; the loss is largest in the `large` room, 3.8 to 5.7 dB), and improve the signal-to-error against the clean talker by 6 to 10 dB over doing nothing. They reach 25 dB within the first half-second but take 2.5 to 14 s to *stay* above it (`large` is slowest: 13 to 14 s for all three), because the metric needs every later half-second window to hold.
- Speex alone leaves 16 to 74 % of echo frames audible and already turns the talker down about 22 dB (the first draft's energy ratio read 11 to 13 dB because leftover echo counted as speech); with its suppressor the echo is gone but the talker drops 34 to 44 dB, so a human interrupting the co-host would be nearly inaudible to `listen`.
- PBFDAF leaves the talker alone (0.0 to +0.1 dB), but it is linear. The unguarded NLMS does not: in the `small` room its output holds only 13 % of the talker's amplitude at the true lag (−17.8 dB), and the harness's lag search then settles on 8 ms, so its lag and DT columns are not meaningful and its "none" latency in §1 rests on the filter having no framing. A double-talk guard is the missing part. PBFDAF's adaptation control is a one-line guard; it needs 11 s to reach 25 dB, and a 300 ms window cannot cover the 180 ms `bt-delay` path plus its tail (23.4 dB). The nonlinear speaker drops it to 14.7 dB, where the AEC3 builds hold 27 to 39. A production version would need a delay estimator, a residual-echo suppressor and double-talk detection: the parts that make up AEC3.

### 5.3 Latency, CPU and allocations

| Candidate | Added latency | Mean per frame (min to max over rooms) | Worst p99 | Worst single frame | One core | Of `cpu.total` | Allocs per frame, Rust / native |
|---|---|---|---|---|---|---|---|
| WebRTC C++ AEC3 | 9.0 ms | 134 to 163 µs | 442 µs | 2.6 ms | 1.3 to 1.6 % | 0.17 to 0.20 % | 2.0 / 0.0 |
| **`sonora-aec3`** | 9.0 ms | 165 to 177 µs | 542 µs | 7.5 ms | 1.7 to 1.8 % | 0.21 to 0.22 % | 40.5 / 0.0 |
| `aec3` | 9.0 ms | 198 to 254 µs | 568 µs | 6.1 ms | 2.0 to 2.5 % | 0.25 to 0.32 % | 67.0 / 0.0 |
| SpeexDSP MDF | 1.0 ms | 280 to 339 µs | 639 µs | 2.6 ms | 2.8 to 3.4 % | 0.35 to 0.42 % | 0.0 / 0.0 |
| SpeexDSP MDF + preprocessor | 9.5 ms | 336 to 477 µs | 860 µs | 9.2 ms | 3.4 to 4.8 % | 0.42 to 0.60 % | 0.0 / 0.0 |
| own NLMS, 200 ms | none | 6.67 to 6.77 ms | 9.6 ms | 22.4 ms | 67 to 68 % | 8.3 to 8.5 % | 0.0 / 0.0 |
| own PBFDAF, 300 ms | none | 89 to 137 µs | 310 µs | 2.7 ms | 0.9 to 1.4 % | 0.11 to 0.17 % | 15.0 / 0.0 |

- **Basis.** `cpu.total` is 3 % of eight cores, so 0.24 core, so 2.4 ms of CPU per 10 ms of audio, in total for the whole process. Column "Of `cpu.total`" is the candidate's share of a single core divided by eight, to be compared with the 3 % line. This VM has four vCPUs and an unscaled CPU (the fix-round run was on a faster VM than the first run, so its times differ from the first draft's: read them as ratios between candidates); the figure on tier `R` is an open question.
- **AEC3 costs 6 to 11 % of the allowance** (0.17 to 0.32 % of 3 %; 7 % for `sonora-aec3`). A plain NLMS at 200 ms costs more than the whole allowance and is out.
- **Latency.** The three AEC3 builds delay the near-end path by 9.0 ms, as measured end to end through the processing module's own framing. That matches the "about 10 ms for AEC3-class designs" in audio-api §4.6. The engine's device block is 256 frames by default, and these cancellers process 10 ms (480-frame) blocks, so `fx.aec` needs a re-blocking buffer that can add up to a further 10 ms; #1268 must declare the sum. Using `sonora-aec3`'s block processor directly instead of the full module may avoid some of it; I did not measure that.
- **Spikes.** Worst single frames of 2 to 9 ms occur in every real candidate, and the pass-through row also shows a 0.3 ms worst frame on this VM, so some of it is VM jitter; but one frame above 2.4 ms is the whole `cpu.total` allowance for that interval. An audio callback must not depend on the canceller finishing in the same callback unless #1268 measures this on `R`.
- **Allocations, Rust and native, counted separately.** Rust allocations come from a counting `GlobalAlloc`, which sees nothing the C and C++ code does. Native allocations come from interposing glibc's `malloc`, `calloc`, `realloc`, `memalign`, `posix_memalign` and `aligned_alloc` in the bench binary (so the statically linked Speex and WebRTC code, and libstdc++'s `new`, land on the counter) and subtracting the Rust allocations that went through `malloc`. Tests check that a plain C `malloc` is counted native, that a `Vec` is counted Rust, and that setting up the Speex and WebRTC candidates registers native allocations, so the zeros are not a blind counter. Both are counted over every steady-state frame after a 3 s warm-up, on Linux with glibc only (memory taken straight from `mmap` or from a custom allocator would not be seen). `AGENTS.md` bans allocation in real-time hot loops and audio-api §3.3 says the audio thread does none. `sonora-aec3` makes about 40 Rust allocations per 10 ms frame (4,000 per second), `aec3` about 67; the C++ build 2, which are Rust-side (the binding), since its native count is 0. Speex makes none of either kind. Every candidate's native count is 0 in steady state: the C and C++ code preallocates. What separates the candidates is the Rust allocations, and the C++ build is a sound fallback because its C++ is allocation-free and its 2 Rust ones sit in a thin binding we could replace, not because of any native figure. My own PBFDAF allocates 15 because it is a first draft with `Vec`s in the loop.

### 5.4 Is the AEC3 ranking stable? Delay range, and seeds

ERLE (dB) against the bulk delay, medium tail (RT60 300 ms), one seed:

| Candidate | 10 ms | 100 ms | 300 ms | 500 ms | 550 ms | 600 ms | 650 ms | 700 ms |
|---|---|---|---|---|---|---|---|---|
| WebRTC C++ AEC3 | 42.5 | 40.6 | 49.9 | 49.9 | 15.5 | 4.9 | 1.7 | 5.7 |
| `sonora-aec3` | 44.6 | 44.3 | 49.2 | 46.9 | 5.3 | 14.4 | 0.8 | 0.4 |
| `aec3` | 44.5 | 41.9 | 39.3 | 33.7 | 13.6 | 12.9 | 1.2 | 1.3 |
| SpeexDSP MDF | 25.1 | 24.4 | 23.8 | 17.0 | 16.8 | 16.5 | 16.6 | 16.3 |
| SpeexDSP MDF + preprocessor | 41.7 | 38.6 | 36.9 | 34.3 | 32.5 | 36.9 | 33.9 | 35.6 |
| own PBFDAF, 300 ms | 25.9 | 27.7 | 3.4 | −0.1 | −0.1 | −0.1 | −0.1 | −0.1 |

**This answers audio-api §4.6's question about the delay estimator's range.** With no delay hint given, all three AEC3 builds find a 500 ms lag and lose it between 500 and 550 ms (they do not always fail the same way at 550 and 600 ms). Speex has no estimator and simply a 500 ms filter, and it keeps working at 700 ms (16.3 dB alone, 35.6 dB with its preprocessor), at lower ERLE than the AEC3 builds reach below 500 ms (34 to 50 dB). So the OBS-monitoring path (reference is `program`, lag found by the estimator) is covered up to about 500 ms; a Bluetooth monitor with a lag beyond that is not.

Five seeds per room (a different speech, room response and noise each), ERLE min / median / max in dB:

| Room | WebRTC C++ AEC3 | `sonora-aec3` | `aec3` |
|---|---|---|---|
| small | 39.6 / 42.4 / 45.9 | 47.5 / 48.4 / 51.7 | 47.1 / 47.8 / 51.3 |
| medium | 37.7 / 39.3 / 43.9 | 41.3 / 42.8 / 47.9 | 34.1 / 41.5 / 47.5 |
| large | 22.3 / 28.0 / 35.9 | 26.1 / 30.3 / 33.3 | 11.7 / 22.1 / 33.6 |
| bt-delay | 34.6 / 39.6 / 44.1 | 29.4 / 41.6 / 47.0 | 24.0 / 40.7 / 46.6 |
| medium-nl | 36.8 / 37.8 / 40.8 | 27.8 / 40.5 / 47.8 | 27.3 / 40.5 / 47.3 |

The medians of the C++ build and `sonora-aec3` are within a few dB of each other and swap places by room; `aec3` is the least consistent (11.7 dB in one `large` run). `sonora-aec3`'s worst run, 26.1 dB, is the best worst case of the three. Five seeds is a small sample; the claim is "the same class", not "better".

## 6. Question 4: the cheaper mitigation (gate or duck while the AI speaks)

Measured on the same signals as a mic-path gate: closed whenever the reference is above −50 dBFS, plus a 500 ms hangover to cover the lag and tail (a 10 ms ramp). It costs about 1 µs per frame.

| Mitigation | Echo frames still audible (small / medium / large / bt-delay / nl) | Talker's level in double talk (same order) |
|---|---|---|
| gate | 0.0 / 0.0 / 0.0 / 0.0 / 0.0 % | −120 dB on every room: nothing passes |
| duck −20 dB | 28.6 / 37.1 / 42.5 / 35.4 / 47.9 % | −19.9 / −20.1 / −19.9 / −19.9 / −20.1 dB |

- **The gate removes the echo completely and also removes the person.** While the co-host speaks the `listen` tap hears nothing, so barge-in (AP-03, under 200 ms) cannot happen; audio-api §4.2 says the tap stays open while the AI speaks for exactly that reason. A hangover of 500 ms also closes the tap for half a second after each AI sentence.
- **Ducking keeps the person 20 dB down (the duck depth itself; the first draft's energy ratio read 14 to 17 dB because leftover echo counted as speech) and still leaves a third to a half of the echo frames audible** (ERLE is the duck depth, 20 dB, so a loud speaker still leaks).
- **So the fallback is a safety net, not an equal.** It is right for "the canceller has not converged yet or has lost the path": gate `listen` and show "headphones recommended", accepting that barge-in is off for that interval. A smarter gate (let sound through when the mic is much louder than the expected echo) is a possible third option; I did not measure one.

## 7. Recommendation for #1268

1. **Build `fx.aec` on `sonora-aec3` 0.2.0** (pin `=0.2.0`; closure of `sonora-aec3` alone: `sonora-common-audio`, `sonora-fft`, `sonora-simd`, `derive_more` (MIT) and `cpufeatures` (MIT OR Apache-2.0), plus build-time proc-macro crates; the bench uses the `sonora` facade crate, whose closure also compiles `sonora-agc2` and `sonora-ns` (BSD-3-Clause, §2), which depending on `sonora-aec3` directly avoids; `cargo deny` passes), through an `EchoCanceller` trait inside `omuzaki-audio-fx` so a second implementation can replace it. Why it beats the C++ build: no meson, ninja or abseil on the three build platforms (the C++ build's abseil fetch from `wrapdb.mesonbuild.com` was refused here and needed a manual abseil install), a plain `license` field for `cargo deny`, no C++ notices, and no worse quality in the measurements above.
2. **Close the allocation gap before it lands on the audio thread.** Either remove the roughly 40 per-frame allocations (a patch to a vendored or forked `sonora-aec3`, BSD-3-Clause allows it) or run the canceller on its own thread behind an SPSC ring and declare the extra delay. Add the counting allocator from the spike as a test that fails above zero.
3. **Fallback if (2) cannot be met or the Rust port regresses: the C++ build** (`webrtc-audio-processing` 2.1.0 with `bundled`), 2 Rust-side allocations per frame (the binding) and no native ones, same latency and similar CPU. It needs the `deny.toml` `clarify` entry (maintainer review) and the extra notices.
4. **Do not build our own** at this stage. A linear filter alone leaves 7 to 52 % (PBFDAF) to 93 % (NLMS) of echo frames audible and a 300 ms window loses the Bluetooth path; the missing parts (delay estimator, suppressor, double-talk control) are AEC3.
5. **Do not choose Speex.** Without its suppressor it fails the 25 dB target, alone it already turns the talker down about 22 dB and leaves the echo, with it barge-in loses 34 to 44 dB.
6. **Declare the latency honestly:** 9 ms from the canceller plus any re-blocking from the engine's 256-frame device block to the 480-frame canceller block. The ERLE gate in §9.3 should use this spike's numbers as its starting point, not as its pass mark: a regression gate on 25 dB on synthetic rooms, and "the co-host's own voice no longer triggers `listen`" on a VAD, not on my energy proxy.
7. **Default state:** follow audio-api §4.6 (on when the person says speakers or a bound output is a speaker; off with headphones), since question 1 has no data.

## 8. Open questions

- **How often VTubers use speakers:** unverified (§4).
- **Legal:** whether the BSD-3-Clause `sonora` port, which ships no `PATENTS` file, still carries Google's patent grant for what we ship; whether to ship Google's `PATENTS` text in the notices bundle regardless.
- **Maintenance risk of `sonora`:** version 0.2.0 (updated 2026-07-29 on crates.io), one maintainer named in the licence line; I did not review its code for correctness against the C++ reference, only its behaviour on these signals.
- **Native allocation counting** is Linux/glibc only and sees `malloc`-family calls, not memory taken straight from `mmap`; tests cover the counter, and the steady-state native count of every candidate is 0 (§5.3). The first draft counted Rust allocations only, so its "0" for Speex and the C++ build proved nothing; this one does.
- **Real signals:** everything is synthetic. Real speech, real speakers (including bass-heavy ones with strong nonlinearity), room movement and a person walking in front of the mic are untested. The measurement must be repeated on recorded private material that the repository cannot hold, by a person with a speaker setup.
- **Tier `R`:** CPU on the Ryzen reference laptop; worst-frame spikes on it.
- **The `listen` false-trigger rate** against the actual VAD (Silero, `spikes/speech`): needs the model, which this environment may not download. The leak % above is an energy proxy.
- **Block-size interaction:** `sonora-aec3`'s block processor used directly (instead of the full module) for latency, and the exact re-blocking cost at the engine's 256-frame block.
- **Unreachable here, so unverified:** Discord's documentation (`support.discord.com`), the OBS knowledge base and forum (`obsproject.com`), the W3C media capture specification (`w3.org`), MDN's rendered site (`developer.mozilla.org`), and Google's WebRTC repositories (`webrtc.googlesource.com`, `chromium.googlesource.com`). MDN's page was read from its source repository, the WebRTC licence from the bundled copy.
- **Not benchmarked:** WebRTC's mobile canceller (AECM) in the C++ build, which is a lower-complexity, lower-rate design; the C++ module's own `stream_delay_ms` hint; the `sonora` AGC and noise-suppression stages (turned off here).
