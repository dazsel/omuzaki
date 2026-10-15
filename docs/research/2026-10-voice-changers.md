---
title: "Voice changers and voice DSP (2026-10-12)"
summary: "Voice changers and voice DSP (2026-10-12)"
area: research
---

# Voice changers and voice DSP (2026-10-12)

> Research pass for PF-12 (voice-changer input), BR-24 (voice effects) and ML-14 (voice-conversion slot). Checked 2026-10-12 against repos, licence files and Hugging Face metadata. "(?)" = unverified or vendor claim. Spec rows are from the approved edit to `docs/product/02-studio-spec-v3.md`.

## 1. How VTubers use voice changers
- **Tools.** Voicemod, VoiceMeeter chains, the [w-okada client](https://github.com/w-okada/voice-changer) (RVC and Beatrice), RVC WebUI, Beatrice 2, Clownfish, MorphVOX, Roland VT-4. I found **no independent usage data**; only vendor blogs, who call Voicemod best known. PF-12 should favour none.
- **Pipeline.** Changer, virtual cable (VB-CABLE class), then OBS and the avatar app both reading the cable. Windows re-ordering devices breaks it ([vendor guide](https://voxbooster.com/blog/vtuber-voice-changer-setup-complete-guide/)).
- **Mic choice.** VTube Studio picks one mic; a noise gate before it helps the cutoff ([wiki](https://github.com/DenchiSoft/VTubeStudio/wiki/Lipsync)). VSeeFace picks one mic on the start screen, and lip sync can stop when Windows' output device changes ([VSeeFace](https://www.vseeface.icu/)). Neither documents voice changers. Warudo and VNyan docs were unreachable: unverified.
- **Complaints.** Mouth trails the changed voice. Fix: an OBS sync offset on the mic source, or a delay VST on the OBS feed only, since a global delay also delays the avatar's input ([OBS forum](https://obsproject.com/forum/threads/mic-audio-not-in-sync-with-webcam.28561)). "Changed voices recognise worse" is the spec's premise; I found no measurement.

## 2. Latency
| Item | Figure | Source |
|---|---|---|
| RVC WebUI real-time | 170 ms end to end; about 90 ms with ASIO; no hardware or method given | [RVC README](https://github.com/RVC-Project/Retrieval-based-Voice-Conversion-WebUI) |
| w-okada | No published numbers; chunk size is tunable; "under 200 ms on a decent NVIDIA card" (third-party (?)). RVC needs CUDA in practice | [setup guide](https://dubbingai.io/articles/en/article/w-okada-voice-changer-setup-guide) |
| Beatrice 2 | About 50 ms, measured with external recording gear; load target RTF < 0.2 on one i7-1165G7 thread; CPU only, ~35 MB VST | [prj-beatrice.com](https://prj-beatrice.com/), [trainer README](https://huggingface.co/fierce-cats/beatrice-trainer) |
| Seed-VC (RTX 3060 laptop) | 430 ms latency, 150 ms per 180 ms chunk; "algorithm delay ~300 ms" | [Seed-VC README](https://github.com/Plachtaa/seed-vc) |
| Phase vocoder | About one FFT window: 2048 at 48 kHz ~42 ms, 512 ~10 ms with more smearing (?) | [guide](https://voxbooster.com/id/blog/how-to-pitch-shift-voice/) |
| PSOLA | Small grains, lower latency, can sound bad | [LAU list](https://lists.linuxaudio.org/archives/linux-audio-user/2016-March/104405.html) |
| WORLD | No source found; an offline-oriented vocoder (unverified) | n/a |

## 3. Licences (can we ship it under ML-14?)
| Component | Code | Weights and data | Verdict |
|---|---|---|---|
| RVC WebUI | MIT ([LICENSE](https://github.com/RVC-Project/Retrieval-based-Voice-Conversion-WebUI/blob/main/LICENSE)) | `pretrained_v2`: HF repo tagged MIT, but the README only says the base model used ~50 h of VCTK and "no copyright worries"; v2 data not stated | **unclear** |
| HuBERT base, ContentVec | MIT (fairseq, ContentVec); HF HuBERT tag Apache-2.0 | LibriSpeech (?) | yes |
| RMVPE | Apache-2.0 (Dream-High); yxlllc repo has **no LICENSE file** | training data undisclosed; eval sets include MIR-1K and MIR_ST500, YouTube pop ([paper](https://arxiv.org/abs/2306.15412)) | **unclear, treat NO** |
| w-okada client | MIT, with bundled third-party MIT texts | per-model terms: Beatrice is "custom" | client yes; models vary |
| Beatrice 2 | trainer and trained models MIT per its [README](https://huggingface.co/fierce-cats/beatrice-trainer); but the Python API [readme](https://huggingface.co/wok000/vcclient_model/blob/main/beatrice_v2_beta/readme.md) says "COMMERCIAL USE IS PROHIBITED" (JVS criteria), redistribution needs permission, and training data includes JVS as "proprietary" | official character models have their own terms | **conflicting: NO until the project clears it in writing** |
| kNN-VC | MIT | rides on WavLM-Large, whose pretraining includes GigaSpeech ([paper](https://arxiv.org/abs/2110.13900)); [GigaSpeech access terms](https://huggingface.co/datasets/distil-whisper/gigaspeech-l) restrict it to non-commercial research | **training-data problem** |
| FreeVC | MIT | same WavLM-Large; checkpoints on OneDrive with no licence; VCTK | **NO (data, no licence)** |
| Seed-VC | **GPL-3.0** | HF weights also tagged GPL-3.0; data undisclosed | **NO** (copyleft; policy allows GPL only as an external program) |
| OpenVoice v2 | MIT, "free commercial use" | MIT, but training data only described as a "massive-speaker" set; it is a tone-colour converter, not built for streaming | **unclear (data)** |

Nothing here passes the policy cleanly. Every candidate has an undisclosed or non-commercial-tainted data chain, a copyleft term or conflicting terms.

## 4. DSP building blocks (BR-24)
| Crate | Licence | Notes |
|---|---|---|
| [`signalsmith-stretch`](https://crates.io/crates/signalsmith-stretch) 0.1.3 | MIT (C++ upstream MIT) | Pitch plus formant factor (`setFormantFactor`), but needs a rough F0. Default preset block 0.12 s, hop 0.03 s, so latency is a large fraction of 120 ms at any rate. **Far over 10 ms**; I did not compile and measure. |
| [`pitch_shift`](https://crates.io/crates/pitch_shift) 2.1.0 | MIT | Phase vocoder; fixed latency `1024 - out_samples` samples, ~21 ms at 48 kHz. No formant control. |
| [`fundsp`](https://crates.io/crates/fundsp) 0.23 | MIT OR Apache-2.0 | Graph of filters and dynamics; no formant shifter that I confirmed (?). |
| [`biquad`](https://crates.io/crates/biquad) 0.6, [`dasp`](https://crates.io/crates/dasp) 0.11 | MIT OR Apache-2.0 | EQ filters, no look-ahead; dasp is plumbing only (last release 2020). |
| `rubberband` 0.1.5 | **GPL-2.0-or-later** | Ruled out. |

Robot, radio, gate, EQ and compressor are short and need no look-ahead; write them ourselves.

## 5. Recommendations
- **Default ML-14 model: ship none.** The slot stays empty under ADR 0016 (user brings RVC or Beatrice). Revisit when a model's weights, data and terms are all permissive and written down. Best lead: ask Project Beatrice for a written commercial grant covering trainer, base model and API.
- **Voice taps.** Recognition, captions and agent listening: raw mic; mouth and cues: the signal viewers hear. Matches PF-12.
- **A/V delay.** 0 ms with no changer; fill from the clap test. Fallback presets if skipped: DSP chain 20 ms, Beatrice 60 ms, RVC 200 ms. The 500 ms cap covers even Seed-VC's 430 ms.
- **Spec changes suggested (spec not edited).**
  - **BR-24 and `voice.fx` (10 ms):** 10 ms is out of reach for pitch and formant with the permissive code available (21 ms for `pitch_shift`, 100+ ms for Signalsmith). Split it: other effects 10 ms; pitch and formant declare their own latency (about 25 ms with a small-window design) and feed PF-12's delay. Or fund a custom low-latency shifter.
  - **ML-14:** a shipped model also needs a training-data review, not just a code and weights licence (kNN-VC, FreeVC).
  - **PF-12:** apps bind a mic by device, so handle device reordering.
