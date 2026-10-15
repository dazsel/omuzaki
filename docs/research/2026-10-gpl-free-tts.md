---
title: "Can we ship Kokoro or Chatterbox TTS with no GPL anywhere in the stack? (2026-10-15)"
summary: "Kokoro-82M weights run through our own ort code with a lexicon-only English G2P are the best GPL-free candidate, but they stay unverified (treated as NO) until lexicon and training-data provenance is cleared; Chatterbox weights are MIT but need more work and open provenance questions, so it is a later add-on."
area: research
---

# Can we ship Kokoro or Chatterbox TTS with no GPL anywhere in the stack? (2026-10-15)

Research for #537. Source marks: **verified** (I read the primary source on 2026-10-15), **secondary** (someone's
summary), **inference** (my reading of verified facts), **unverified** (not settled).

## 1. Answer

- **Pick Kokoro-82M as the first TTS model to build a spike on (shipping waits for the clearance gate in section 6).** Weights are Apache-2.0 (verified). They run on ONNX Runtime through
  `ort` behind the `Runtime` seam, which ADR 0061 already assigns to `tts@1` (verified, repo file).
- **The weights are not the problem; the Python front end is.** The GPL enters through `espeak-ng` (GPL-3.0-or-later,
  verified) and the `phonemizer` packages that wrap it. We drop all of that.
- **G2P: write our own small Rust English G2P that reads only the *gold* lexicons** (`us_gold.json`, `gb_gold.json`,
  Apache-2.0 on the repo and the Hugging Face dataset). Do **not** ship the *silver* lexicons, and do not ship the
  `misaki` BART fallback weights: the author says silver was produced by running `espeak-ng` over a word list
  (verified, misaki #51), and the BART fallback was trained on silver (verified, misaki #74).
- **Out-of-dictionary words:** no GPL tool, no silver. Use inflection rules plus a small letter-to-sound model we
  train on gold plus CMUdict (BSD-style licence, verified). Until that exists, spell the word out, and let users and
  characters override a word with an IPA string. That is a quality cost, not a licence cost.
- **The issue's pointer to "misaki issue #508" is wrong.** No such issue exists in `hexgrad/misaki` or
  `hexgrad/kokoro` (both return 404). The lexicon-provenance question is **misaki #107** (open, unanswered since
  2026-09-19) and the older **misaki #51** (answered, 2025-02-14). Neither confirms the gold sources.
- **Chatterbox: MIT weights, but not the first pick.** English Chatterbox needs no G2P at all (a BPE text tokenizer
  reads letters), which is a point in its favour. Against it: training data is undisclosed, the Llama-3 relationship
  is unclear, Perth must be ported to run beside ONNX, and the stock package pins a GPL `pykakasi` (verified).
  Treat it as a P1 follow-up, English only, Perth on, after the provenance questions get an answer.
- **Keep the Perth watermark on** if Chatterbox ships (section 5). The cost is one extra neural pass on every output
  and a port job; it is not a licence duty.
- **Japanese (second):** `jpreprocess` (BSD-3-Clause, Rust) replaces `pyopenjtalk` for the Japanese front end. Parity
  with misaki's `ja.py` output is **unverified** and is part of the Japanese spike, not the English one.

**Decision this supports:** audit blocker 2 (`2026-10-licence-audit.md`) moves from "NO as installed" to a **build
candidate**, not to "yes". The path is Kokoro weights + `ort` + own G2P over gold lexicons. Two provenance gaps stay
open: who seeded the gold lexicons, and which closed vendors' synthetic audio is in Kokoro's training set. Under
`docs/licensing/policy.md` (line 29: `unverified` is "treated as `NO`" until a primary source or legal review settles
it; lines 115-118: nothing `unverified` merges into a shippable path, and every new crate, model and build dataset
gets a register row in the same PR) the register rows stay `unverified`, and **nothing ships** until the clearance
gate in section 6 is met. The spike may run, because it is not a shippable path.

## 2. What is GPL, and where it enters

| Piece | Licence | Source and mark | Where it enters |
| --- | --- | --- | --- |
| espeak-ng | GPL-3.0-or-later. README: "released under the GPL version 3 or later license"; a `getopt_long` file is BSD-2 | https://github.com/espeak-ng/espeak-ng (README, `COPYING`), 2026-10-15, **verified** | `misaki[en]`, `kokoro`, `kokoro-onnx`, `sherpa-onnx`, Rust crate `espeak-rs` |
| `phonemizer-fork` / `phonemizer` | GPL-3.0 (from the licence audit; not re-read here) | `2026-10-licence-audit.md` row 158, **secondary** | `misaki[en]` extra |
| `espeakng-loader` | bundles a prebuilt `libespeak-ng` 1.52.0 (a reporter says the wheel metadata was missing a licence until they filed thewh1teagle/espeakng-loader#9) | hexgrad/kokoro#247 comment, 2026-10-05, **secondary** | `misaki[en]` extra |
| `pykakasi==2.3.0` | GPL-3.0-or-later (from the licence audit) | `2026-10-licence-audit.md` row 159, **secondary** | `chatterbox-tts` hard pin, used only for Japanese text |
| Rust crate `espeak-rs` | crates.io metadata says **MIT**, but it builds espeak-ng from source | https://crates.io/crates/espeak-rs (v0.2.0), 2026-10-15, metadata **verified**; bundling **secondary** (kokoro `pguso/kokoro` README: "Misaki links bundled espeak-ng (via CMake)") | `misaki-rs` default feature `espeak` |

Two traps for `cargo deny`:
- `espeak-rs` is labelled MIT while carrying GPL code. A licence-field check passes it. **The spike must deny the
  crate by name** (`bans`), not rely on the licence list.
- The `misaki-rs` crate (MicheleYin) turns `espeak` on by default. It must be used with `default-features = false`,
  or not at all (section 4).

The Python `kokoro` package imports espeak unconditionally and hands `EspeakFallback` to misaki, so even misaki's
newer BART fallback does not remove it there (hexgrad/kokoro#247, comment of 2026-10-05, **secondary**, consistent
with the `pyproject.toml` read below). That is why the answer is "our own inference", not "a flag".

`misaki/pyproject.toml` (main, read 2026-10-15, **verified**):

```
en = ["num2words", "spacy", "spacy-curated-transformers", "phonemizer-fork", "espeakng-loader", "torch", "transformers"]
ja = ["fugashi", "jaconv", "mojimoji", "unidic", "pyopenjtalk"]
```

## 3. Kokoro-82M, piece by piece

### 3.1 Weights and voices

| Item | Licence and clause | Source, date | Mark |
| --- | --- | --- | --- |
| `kokoro-v1_0.pth`, 82M parameters, StyleTTS 2 + ISTFTNet, 178 input tokens | YAML front matter `license: apache-2.0`. Card: "With Apache-licensed weights, Kokoro can be deployed anywhere from production environments to personal projects." and "This is an Apache-licensed model, and Kokoro has been deployed in numerous projects and commercial APIs. We welcome the deployment of the model in real use cases." | https://huggingface.co/hexgrad/Kokoro-82M (README, `config.json`), 2026-10-15 | **verified** |
| 54 voice packs (`voices/*.pt`) | No separate licence; covered by the repo card. Per-voice training data is not published; users have asked hexgrad (kokoro #376, #379, open, unanswered) | https://github.com/hexgrad/kokoro/issues/376 and /379, 2026-10-15 | **verified** (the gap), answer **unverified** |
| ONNX exports (fp32, fp16, q8f16, q4 ...) | `license: apache-2.0`, `base_model: hexgrad/Kokoro-82M` | https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX, 2026-10-15 | **verified** |
| `kokoro-onnx` release files (`kokoro-v1.0.onnx`, `voices-v1.0.bin`) | README: "kokoro-onnx: MIT", "kokoro model: Apache 2.0" | https://github.com/thewh1teagle/kokoro-onnx, 2026-10-15 | **verified** (via fetch summary) |
| Base model | HF tags: `base_model:finetune:yl4579/StyleTTS2-LJSpeech`. StyleTTS2 code is MIT; its README asks users of the *pre-trained* models to "inform the listeners that the speech samples are synthesized ... unless you have the permission to use the voice you synthesize". Kokoro's own voices are its trained voices, so this clause reads as aimed at cloning, but nothing says so | https://github.com/yl4579/StyleTTS2 README, 2026-10-15 | clause **verified**; its reach to Kokoro is **inference** |

### 3.2 Training data (what the model card says, word for word)

From https://huggingface.co/hexgrad/Kokoro-82M (README, "Training Details", read 2026-10-15, **verified**):

> **Data:** Kokoro was trained exclusively on **permissive/non-copyrighted audio data** and IPA phoneme labels. Examples
> of permissive/non-copyrighted audio include:
> - Public domain audio
> - Audio licensed under Apache, MIT, etc
> - Synthetic audio[1] generated by closed[2] TTS models from large providers
>
> [1] https://copyright.gov/ai/ai_policy_guidance.pdf
> [2] No synthetic audio from open TTS models or "custom voice clones"
>
> **Total Dataset Size:** A few hundred hours of audio

Reading (**inference**):
- The synthetic-audio line is the one real unknown. The card says the audio came from "closed TTS models from large
  providers". Those providers' terms of service may restrict training other models on their output. The card does not
  name them and I could not check their terms. This is a risk we cannot reduce by engineering. It is shared by
  every Kokoro user; the model has been deployed in commercial APIs per the card, but that is not a legal finding.
- Nothing here is non-commercial. No dataset on the list is NC, ND or SA. That meets the "nothing non-commercial"
  rule on the evidence published.

### 3.3 Creative Commons attributions (the card's table, verbatim)

| Audio Data | Duration Used | License | Added to Training Set After |
| --- | --- | --- | --- |
| [Koniwa](https://github.com/koniwa/koniwa) `tnc` | <1h | [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/deed.ja) | v0.19 / 22 Nov 2024 |
| [SIWIS](https://datashare.ed.ac.uk/handle/10283/2353) | <11h | [CC BY 4.0](https://datashare.ed.ac.uk/bitstream/handle/10283/2353/license_text) | v0.19 / 22 Nov 2024 |

Source: same README, 2026-10-15, **verified**. The card gives no required credit string. I did not read the Koniwa or
SIWIS credit requirements, so the exact wording they ask for is **unverified**; the draft in section 7 names the
dataset, the licence and the link, which is what CC BY 3.0/4.0 section 3(a) asks for in outline (**inference**).

### 3.4 Tokenizer

The model takes integer token ids for 178 IPA-like symbols. The vocabulary is `n_token: 178` in `config.json`
(verified) and the id map ships in the ONNX repo's `config.json`/`tokenizer.json` (file listing verified, contents not
read). The map is part of Apache-2.0 repos. Inputs of the ONNX export, from the model card's Python example:
`input_ids` (shape `(1, <=512)`, pad id 0 at both ends, at most 510 real tokens), a `style` vector `(1, 256)` taken
from the voice pack at row `len(tokens)`, and `speed` (**verified** by reading the example).

### 3.5 English G2P: what `misaki` is made of

| Piece | Licence | Provenance | Mark |
| --- | --- | --- | --- |
| `misaki` code | Apache-2.0 (`LICENSE` is the Apache 2.0 text, GitHub API `spdx_id: Apache-2.0`) | https://github.com/hexgrad/misaki | **verified** |
| `us_gold.json`, `gb_gold.json` | Same repo, Apache-2.0; HF mirror dataset `hexgrad/misaki` front matter `license: apache-2.0` | Author, misaki #51: "I seeded some local datasets of IPA labels, and then mapped and filtered them programmatically using some heuristics that I wrote as a native (American) English speaker." The seed datasets are **not named** | licence **verified**, provenance **unverified** |
| `us_silver.json`, `gb_silver.json` | Same repo, Apache-2.0 | Author, misaki #51: "I ran `espeak-ng==1.52` against https://github.com/dwyl/english-words/blob/master/words_alpha.txt and then transformed the espeak phonemes ... Then I filtered the silver dictionaries by applying consistency heuristics against the gold dictionaries" | origin **verified**, legal effect **unverified** |
| BART fallback weights `PeterReid/graphemes_to_phonemes_en_us` and `_gb` | HF front matter `license: apache-2.0` | misaki #74: first trained on gold; then "used 90% of the silver as more training data, and 10% as eval" | licence **verified**, training mix **verified** |
| `num2words` (Python) | LGPL-2.1 (licence audit row 158) | n/a | **secondary** |

What this means (**inference**):
- **Silver is derived from espeak-ng output.** Whether that output is a derivative of a GPL program is a legal
  question. The usual view is that a program's output is not covered by the program's licence unless the output
  contains the program's own code or data; I did not read the GPL FAQ for this, so it is **unverified**. Pronunciation
  of a word is also close to a fact. We do not need to decide: gold alone covers about 178,000 entries across US and
  GB (a third-party port reports that count, **secondary**: studio-ransom/best-tts-rust README), so we can skip silver
  and keep the question closed.
- **Gold's seed data is unnamed.** The most likely seeds are public pronunciation resources, but I cannot say which.
  The issue asks us to "verify its lexicon provenance". The upstream answer does not exist yet (misaki #107 is open).
  Two things reduce the risk: (a) the author licensed the files Apache-2.0 on two hosts and says he wrote the mapping
  heuristics; (b) the spike can diff gold against CMUdict and report the overlap, so we know whether gold is a
  re-mapping of one known source. If it is CMUdict, that source's licence is BSD-style (below) and fine.
- **The BART fallback inherits silver.** Even if the net's weights are Apache-2.0, 90% of its extra training data was
  espeak-derived. Skip it.

### 3.6 Neighbouring open G2P resources

| Resource | Licence | Use to us | Source, date, mark |
| --- | --- | --- | --- |
| CMUdict | CMU BSD-style notice: source retains notice; binary reproduces notice in docs; "AS IS" disclaimer; no endorsement clause was seen in the part I read | English lexicon seed and a cross-check for gold; needs an ARPAbet to Kokoro-phoneme map (work, no licence issue) | https://raw.githubusercontent.com/cmusphinx/cmudict/master/LICENSE, 2026-10-15, **verified** (GitHub tags it `NOASSERTION` because the text is custom) |
| `cmudict-fast` Rust crate | MIT/Apache-2.0 | Loader if we want it | https://crates.io/crates/cmudict-fast v0.8.0, 2026-10-15, **verified** |
| `num2words` Rust crate | MIT OR Apache-2.0 | Number-to-words, replaces the LGPL Python one | https://crates.io/crates/num2words v1.2.0, **verified** |
| `language-tokenizer` Rust crate | WTFPL | Used by `misaki-rs`; we do not need it. WTFPL was already flagged as an open item (audit row 202) | https://crates.io/crates/language-tokenizer, **verified** (licence field) |
| `Kyubyong/g2p` (`g2p_en`) | Apache-2.0 | Python, neural OOV; not for shipping, only as a prior-art comparison | GitHub API, 2026-10-15, **verified** (tag only) |
| `jpreprocess` | BSD-3-Clause | Japanese front end in Rust (OpenJTalk-compatible) | https://github.com/jpreprocess/jpreprocess README, 2026-10-15, **verified**. Its bundled `naist-jdic` has a `NOTICE.txt` I did not read: **unverified** |
| `pyopenjtalk` | MIT ("Expat") | Reference for parity tests only (Python) | https://github.com/r9y9/pyopenjtalk `LICENSE.md`, **verified** |

### 3.7 Prior art: Rust Kokoro ports and what they do about GPL

Evidence that the path works, and where each project stumbles (all read 2026-10-15):

| Project | What it does | GPL status | Mark |
| --- | --- | --- | --- |
| `pguso/kokoro` (Apache-2.0, `ort` rc.11) | Kokoro on `ort`, G2P via `misaki-rs` + CMUdict | Default feature `g2p-espeak` builds espeak-ng; `--no-default-features` removes it | **verified** (Cargo.toml, README) |
| `MicheleYin/misaki-rs` (MIT, crates.io `misaki-rs` 0.6.0) | Rust port of misaki English with embedded lexicons and a POS tagger | `espeak` is default-on, optional off. Its `expand_dicts.py` calls `espeak-ng -q --ipa=3` to grow the silver lexicons (the shipped silver files are about 11.8 MB, against 3.1 MB upstream) | **verified** |
| `studio-ransom/best-tts-rust` (Apache-2.0) | Kokoro in pure Rust, bundled gold lexicons plus its own letter-to-sound fallback, no dependencies | No espeak. Its `THIRD_PARTY.md` calls misaki "MIT"; the repo is Apache-2.0, so that notice is wrong, which shows why we write our own | **verified** (THIRD_PARTY.md); quality **unverified** |
| `thewh1teagle/kokoro-onnx` (MIT) | Python runtime over ONNX Runtime; README says to use misaki | Pulls `phonemizer` (GPL) | **verified** (README), GPL pull **secondary** (audit row 158) |

## 4. Runtime and graph features

- **ONNX Runtime through `ort`.** ADR 0061 puts `tts@1` on `onnx` ("they want GPU providers"). `ort` and ONNX Runtime
  are MIT or MIT/Apache and already carry register conditions (`ort.toml`, `onnx-runtime.toml`: build or take
  Microsoft's tarballs, not Pyke's binaries; ship `ThirdPartyNotices.txt`). Kokoro ONNX exports run on
  ONNX Runtime in `kokoro-onnx`, `kokoro-js` (transformers.js) and the `pguso/kokoro` Rust crate (**verified** by
  their READMEs; I did not run any of them).
- **LiteRT:** no TFLite export of Kokoro was found. Per ADR 0061 "nothing is converted into TFLite", so LiteRT is out.
- **candle:** `candle-core` is MIT OR Apache-2.0 (verified, crates.io). No candle port of Kokoro was found in this
  search. Writing one means re-implementing StyleTTS 2 + ISTFTNet; not worth it while `ort` runs the export.
- **Graph features an export needs:** the ONNX files carry the whole model in one graph (token ids, style, speed in;
  waveform out). The opset and any custom or quantised ops (q8f16 uses int8 dynamic quantisation) are **unverified**;
  the spike must open the graph and list operators and the minimum ONNX Runtime version. `kokoro-onnx` states
  "ONNX Runtime >=1.20.1" as its badge, and our pin is 1.28.3, so the version gap points the right way.
- **Quality choice for the spike:** start from the fp32 or fp16 export; keep q8f16 as an option behind a listening
  test, since quality must not be traded down to save bytes.

## 5. Chatterbox

| Item | Licence and clause | Source, date | Mark |
| --- | --- | --- | --- |
| Code (`chatterbox-tts` 0.1.7) | MIT, "Copyright (c) 2025 Resemble AI" | https://github.com/resemble-ai/chatterbox `LICENSE`, 2026-10-15 | **verified** |
| Weights: multilingual V3 / V2 (`ResembleAI/chatterbox`), Turbo (`ResembleAI/chatterbox-turbo`, 350M, English) | HF front matter `license: mit` on both | https://huggingface.co/ResembleAI/chatterbox and `-turbo`, 2026-10-15 | **verified** |
| ONNX exports | `onnx-community/chatterbox-ONNX`, `onnx-community/chatterbox-multilingual-ONNX`, `ResembleAI/chatterbox-turbo-ONNX`: all `license: mit`. They split into `embed_tokens`, `language_model`, `speech_encoder`, `conditional_decoder`, with `.onnx_data` side files | HF API, 2026-10-15 | **verified** |
| Perth watermarker | Repo `LICENSE` is MIT; `pyproject.toml` says `license = { text = "MIT" }`. The trained net `perth_net_250000.pth.tar` (37.4 MB) lives in the same MIT repo; no separate weights licence | https://github.com/resemble-ai/Perth, 2026-10-15 | **verified** (no separate clause found) |
| English text path | A BPE `Tokenizer` over letters. No phonemizer. `pykakasi` is imported lazily for Japanese only, `dicta_onnx` for Hebrew, `spacy_pkuseg` for Chinese | `src/chatterbox/models/tokenizers/tokenizer.py`, 2026-10-15 | **verified** |
| GPL in the package | `pyproject.toml` pins `pykakasi==2.3.0` (GPL-3.0-or-later per the audit). Do not install the package; our ONNX path never imports it | `pyproject.toml`, 2026-10-15; licence **secondary** | pin **verified** |

### 5.1 Training data and ancestry

- Card: "Trained on 0.5M hours of cleaned data" and "0.5B Llama backbone". GitHub README Disclaimer: "Don't use this
  model to do bad things. Prompts are sourced from freely available data on the internet." (README, 2026-10-15,
  **verified**.) No dataset list, no per-dataset licences. "Freely available" is not a licence. **Unverified**, and the
  same gap the audit already recorded.
- Acknowledgements list CosyVoice (Apache-2.0), Real-Time-Voice-Cloning, Llama 3 and others. CosyVoice,
  S3Tokenizer and Matcha-TTS are Apache-2.0 / Apache-2.0 / MIT (GitHub API, **verified**); 3D-Speaker (the speaker
  encoder family) is Apache-2.0 (**verified**). Real-Time-Voice-Cloning shows no SPDX licence in the API
  (`NOASSERTION`) and I could not fetch its licence file: **unverified**.
- Whether the T3 backbone started from Meta's Llama weights, which would bring the Llama community licence, is
  **unverified**. The cards say "Llama backbone" (an architecture); the README links Llama 3 only as an
  acknowledgement. **Ask Resemble AI** (one GitHub issue on their repo) before relying on Chatterbox.
- Nothing I found says the training data is non-commercial. Nothing proves it is not.

### 5.2 Perth: keep it on, and what it costs

- **No contract to keep it.** The MIT licence has no watermark clause and the cards say only "Watermarked outputs"
  (stock) or "Watermarked outputs (optional)" (ONNX card). I found no term that forbids removing it. Because it is
  MIT code applied in `generate()`, it can be removed.
- **Recommendation: keep it on, always, and do not offer a switch.** Reasons: it is Resemble's only safeguard against
  misuse of cloning, the product's consent model (spec section 14) is stronger with it than without, and a user-facing
  off switch would be a promise we cannot police.
- **Cost (all **inference**, unmeasured):**
  - One more neural pass (STFT-based encoder/decoder, 37 MB of weights) on every output.
  - The ONNX exports do **not** contain it; the ONNX model card installs `resemble-perth==1.0.1` in Python for it.
    To keep it on, we must either run Perth's PyTorch net through our own ONNX export (a conversion job with parity
    tests; **unverified** that it exports cleanly) or ship Python, which we will not.
  - Latency and a small quality hit, to be measured in the spike if Chatterbox goes ahead.

### 5.3 Cost of Chatterbox as a runtime

- Autoregressive speech-token LLM (0.5B, or 350M Turbo) with KV cache plus a flow-matching decoder plus a voice
  encoder: four ONNX graphs and a Rust decode loop (sampling, repetition penalty, classifier-free guidance, cache
  handling). Kokoro is one graph, one call.
- Voice cloning needs a consent record (ADR 0041, item 5) before any reference clip is accepted.
- Verdict: worth building after Kokoro, for cloning and expressive speech, English first.

## 6. Recommendation

0. **Clearance gate (blocks shipping, not the spike).** Kokoro and the gold lexicons stay `unverified` (treated as
   `NO`) until one of these is true for each gap:
   - *Gold lexicon seeds:* an authoritative upstream statement naming the seed datasets and their licences (misaki #107),
     or legal review, or we replace gold with a lexicon we build only from sources already cleared (CMUdict).
   - *Closed-vendor synthetic audio in the weights:* an upstream statement naming the vendors plus legal review of
     their terms, or a second model whose training data is fully documented.
   The overlap report in spike item 5 is diagnostic only. It cannot clear provenance (see item 5).
1. **Build the Kokoro-82M spike** (Apache-2.0 weights) via `ort` behind `tts@1`, ONNX export from
   `onnx-community/Kokoro-82M-v1.0-ONNX` (or our own re-export of the same weights, with the graph audited). **Ship it
   only after the clearance gate.**
2. **Write `omuzaki-g2p` (or a module in `omuzaki-audio`): a Rust English G2P** that:
   - loads `us_gold` and `gb_gold` only (embedded or shipped as a data file, with the Apache-2.0 text);
   - handles plurals, past tense and `-ing` using the same rules that misaki documents (read from its code, which is
     Apache-2.0; port, do not copy silver);
   - converts numbers with `num2words` (Rust, MIT/Apache);
   - for unknown words, applies an own letter-to-sound model trained on gold plus CMUdict; until then, spells letters
     out and honours inline overrides of the form `[word](/ipa/)` (the syntax misaki documents);
   - has **no** espeak, no silver, no BART fallback.
3. **Japanese next:** `jpreprocess` with its bundled `naist-jdic`, mapped into Kokoro's `j` voices; run the parity
   test against `pyopenjtalk` output on a fixed sentence list (`pyopenjtalk` stays a test-only reference, never shipped).
4. **Chatterbox later (P1, English, ONNX, Perth on).** File a research issue to Resemble AI about training data and
   the Llama relationship before building. Treat its register row as "conditions, provenance unverified" until then.
5. **Do not use** the stock Python `kokoro`, `misaki[en]`, `kokoro-onnx`, `sherpa-onnx` TTS, or `chatterbox-tts`
   packages in any shipped path. `espeak-ng` as a separately installed program stays a legal-review question (audit
   action 1), not part of this design.

### Register rows (to apply when the spike merges, as the issue's fence lists them)

| File | `condition` text |
| --- | --- |
| `docs/licensing/deps/kokoro-82m-weights.toml` | "unverified (treated as NO, not shippable): Apache-2.0 on the card, but the synthetic-training-data terms of unnamed closed TTS vendors are not cleared; needs upstream statement and legal review; when cleared: credit Koniwa (CC BY 3.0) and SIWIS (CC BY 4.0) in the licences screen; run with own G2P only" |
| `docs/licensing/deps/chatterbox-weights.toml` | "MIT; Perth stays on; training data and any Llama-weights ancestry unverified (ask Resemble AI); consent for cloning; not shipped until the spike for it passes" |
| `docs/licensing/deps/misaki.toml` (new) | "code Apache-2.0; gold lexicon seed datasets unnamed (misaki #107 open), so the data is unverified (treated as NO) until upstream or legal clearance, or replaced by an own lexicon from cleared sources; silver lexicons and the PeterReid BART fallback are espeak-derived: never ship; do not install the Python package" |

I did not edit these files in this note (the research PR is docs-only, one file). They belong in the spike's PR.

## 7. Attribution text (draft)

For the licences screen and `THIRD_PARTY_NOTICES`:

> **Kokoro-82M** (hexgrad), Apache License 2.0. https://huggingface.co/hexgrad/Kokoro-82M. The model was trained in
> part on audio licensed under Creative Commons Attribution:
> - Koniwa `tnc` corpus, CC BY 3.0, https://creativecommons.org/licenses/by/3.0/ (source: https://github.com/koniwa/koniwa)
> - SIWIS French Speech Synthesis Database, University of Edinburgh, CC BY 4.0,
>   https://creativecommons.org/licenses/by/4.0/ (source: https://datashare.ed.ac.uk/handle/10283/2353)
>
> **Misaki** pronunciation lexicons (hexgrad), Apache License 2.0. https://github.com/hexgrad/misaki.
> **ONNX Runtime**, MIT, Copyright (c) Microsoft Corporation (see `ThirdPartyNotices.txt`).
> **CMUdict**, Copyright (C) 1993-2015 Carnegie Mellon University. All rights reserved.
> [only if we build the letter-to-sound model from it; reproduce the full notice]

Open details for this text: the exact credit wording Koniwa and SIWIS request, and whether the SIWIS one needs the
creator names (**unverified**). The `SIWIS` French recordings are in a French dataset; the card does not say which
Kokoro voices used them. The Apache-2.0 text for Kokoro and misaki goes in the same file.

For Chatterbox, if it ships: MIT notice "Copyright (c) 2025 Resemble AI" and Perth's MIT notice (Perth's copyright
line was not read: **unverified**).

## 8. The spike (part 2): scope for a task issue

**Title:** TTS spike: Kokoro-82M on `ort` with an own English G2P and no GPL in the tree.

**Fence:**
- `spikes/tts-kokoro/` (new crate, not a workspace member of shipping crates), reading models from the existing model
  catalogue path; no change to `omuzaki-audio` APIs.
- `docs/licensing/deps/kokoro-82m-weights.toml`, `chatterbox-weights.toml`, `misaki.toml` (new), plus the attribution
  text file named in the licences-screen task.
- New register rows, in the same PR, for every other crate, model and build dataset the spike adds (policy lines
  115-118): `num2words` (Rust crate, MIT OR Apache-2.0), `cmudict-fast` (only if used), the CMUdict data (BSD-style
  notice, used for the overlap report and number/word cross-checks), and the Kokoro ONNX export. The letter-to-sound
  model is **not** in this spike (follow-up 4): unknown words are spelled out, so no derived model needs a row yet.
  Dev-only reference tooling (Python `kokoro-onnx`) is not shipped and is not registered, but is recorded in the PR as
  external (see item 1).
- No new dependency of any kind without a register row.

**Acceptance (all evidenced in the PR):**
1. **No GPL in the tree or the binary.** `cargo deny check licenses bans sources` passes with `bans` denying
   `espeak-rs`, `espeak-rs-sys` and every `*-sys` that builds espeak-ng. The spike also carries a script modelled on
   `spikes/speech/tools/check-no-espeak.sh` (checks the dependency tree, the symbol table, the strings and the dynamic
   dependencies of the release binary, with a positive control that finds ONNX Runtime symbols in the same binary), and
   it must find nothing. Two changes to that script's tree check, because `cargo tree` defaults hide edges: run it as
   `cargo tree --target all --all-features -e normal,build,dev`, so build, dev, target-specific and optional-feature
   paths are covered. Cargo checks cannot see Python packages, so: the Python reference environment (`kokoro-onnx`,
   `onnxruntime`, and whatever they pull in) lives outside the repo, is pinned and listed in the PR, and is audited for
   `phonemizer*`, `espeakng*`, `espeak*` and `pykakasi`; and a repository check fails if any checked-in
   `requirements*.txt`, `pyproject.toml`, lock file or helper script names one of those packages.
2. A script lists the operators and opset in the exported graph and the minimum ONNX Runtime version it needs.
3. Ten fixed sentences (including numbers, plurals, past tense, a name, one OOV word) synthesise at 24 kHz through
   `ort`; output is compared with the reference Python `kokoro-onnx` run on the *same phoneme strings*, the same voice
   row and speed, the same fp32 export and the same ONNX Runtime version on the CPU provider (the reference is test
   tooling, never shipped). **Proposed thresholds, fixed here before the spike so it cannot pick its own:** (a) equal
   sample count (the duration predictor is deterministic, so no alignment step is needed); (b) mean absolute difference
   of the natural-log mel spectrogram (80 bins, 1024-point window, hop 256, 24 kHz) of at most 0.05; (c) peak sample
   difference of at most 0.02 of full scale. The decoder uses a random noise source, so the spike first runs the
   reference twice against itself; if the reference's own run-to-run difference already exceeds (b) or (c), the spike
   reports that, seeds the noise (or compares with noise inputs fixed) and states the seeding, instead of widening the
   bounds. Changing a threshold needs a review comment, not a spike decision.
4. The G2P unit tests: gold lookups match `us_gold.json` entries; plurals and `-ed`/`-ing` cases match misaki's output
   on a fixed word list; the unknown-word path is exercised.
5. A report of gold vs CMUdict overlap (how many gold entries equal an ARPAbet-mapped CMUdict entry). **Diagnostic
   evidence only.** Full overlap would not show which dataset seeded gold or that every entry may be redistributed,
   and partial overlap leaves the unmatched entries unexplained. Provenance is settled only by the clearance gate in
   section 6 (upstream statement, legal review, or an own lexicon from cleared sources).
6. Speed, against thresholds **proposed for this spike** (spec v3 section 10 has no `tts.*` row; its table, lines
   318-339, ends at `asr.final`, so this is a spec gap, filed as follow-up 8). Measure on 10 fixed sentences of 8 to 25
   words, after one warm-up run, as p95 over 20 runs: real-time factor (compute time over audio time) and
   first-chunk latency (text in to the first 100 ms of audio samples out). Proposed: CPU on `Dev`'s E-cores scaled to
   tier `R` by the spec's rule, RTF <= 0.5 and warm first-chunk <= 1.0 s; GPU path (tiers `U` or `D`), RTF <= 0.1 and
   warm first-chunk <= 300 ms. Cold model load (process start to ready) is measured and reported, not gated. Numbers
   taken off tier are printed as estimates, as spec v3 section 10 requires.
7. Register rows and attribution text match section 6 and 7 of this note; `cargo xtask check --affected` passes.

**Not in the spike:** Japanese (separate task: `jpreprocess` parity), Chatterbox, voice cloning, any model download
into the repo (models stay outside the repo).

## 9. Open questions and what settles them

| Question | What settles it |
| --- | --- |
| What datasets seeded the gold lexicons, and may they be redistributed? | Only an upstream statement (misaki #107, open), legal review, or replacing gold with an own lexicon built from cleared sources (CMUdict). The overlap in spike item 5 is diagnostic and does not settle it. **Open, blocks shipping** |
| Does the closed-TTS synthetic data in Kokoro's training set breach any vendor's terms? | Only hexgrad can name the vendors; ask on hexgrad/kokoro, then legal review of their terms, or a second model with documented training data. **Open, blocks shipping**; a maintainer accepting the risk is a legal call, not a research result |
| Is espeak-ng output (silver) covered by GPL? | Counsel, or avoid (this note avoids) |
| Credit wording for Koniwa and SIWIS | Read the two licence pages; the spike's attribution file task |
| Is the StyleTTS 2 "inform the listeners" clause a condition on Kokoro? | Read how Kokoro's authors treat it; ask on hexgrad/kokoro. Low risk, since voices are Kokoro's own; it also fits spec section 14 (disclosure of AI voices) |
| Chatterbox: training data, Llama-weights ancestry | An issue on resemble-ai/chatterbox; the answer comes before any Chatterbox build |
| Can Perth export to ONNX with parity? | A small export test, only if Chatterbox goes ahead |
| `jpreprocess` / `naist-jdic` notice text and parity with `pyopenjtalk` | Read `NOTICE.txt`; the Japanese spike's parity list |
| Operators and opset of the Kokoro ONNX graph | Spike item 2 |

## 10. Follow-up tasks

1. **Spike:** TTS spike (section 8). P0, area `ai-performer`, suggested model Sonnet (design judgement on the OOV model).
2. **Research issue (Haiku can post it):** ask `hexgrad/misaki` #107 and `hexgrad/kokoro` for lexicon seeds and
   synthetic-data vendors; ask `resemble-ai/chatterbox` about training data and Llama weights. Comment on the existing
   threads rather than opening duplicates.
3. **Task:** Japanese front end with `jpreprocess` and a parity list. After the English spike.
4. **Task:** letter-to-sound model for unknown English words, trained on gold + CMUdict only. After the spike.
5. **Task:** add `espeak-rs` and friends to `deny.toml` `bans` repo-wide, with a test, so no later crate can bring them.
6. **Task (P1):** Chatterbox spike (English, ONNX, Perth port). Blocked on follow-up 2.
7. **Task:** licences-screen text for the TTS attribution (section 7).
8. **Spec change (needs maintainer review):** add `tts.rtf` and `tts.first_chunk` rows to spec v3 section 10, taking
   the proposed values in spike item 6 once the spike has measured them.
9. **Legal review request:** the two clearance-gate items in section 6 (gold seeds, closed-vendor synthetic audio).

## Sources (all read 2026-10-15)

- https://huggingface.co/hexgrad/Kokoro-82M (README, `config.json`, file list)
- https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX
- https://huggingface.co/datasets/hexgrad/misaki
- https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us
- https://github.com/hexgrad/misaki (`LICENSE`, `pyproject.toml`, `misaki/en.py`, `misaki/ja.py`, README, issues #51, #74, #107, #109)
- https://github.com/hexgrad/kokoro (issues #247, #256, #376, #379)
- https://github.com/espeak-ng/espeak-ng (README, `COPYING`)
- https://github.com/yl4579/StyleTTS2 (README)
- https://github.com/thewh1teagle/kokoro-onnx (README)
- https://github.com/MicheleYin/misaki-rs, https://github.com/pguso/kokoro, https://github.com/studio-ransom/best-tts-rust
- https://github.com/resemble-ai/chatterbox (`LICENSE`, `pyproject.toml`, README, `src/`)
- https://github.com/resemble-ai/Perth (`pyproject.toml`, file tree)
- https://huggingface.co/ResembleAI/chatterbox, `-turbo`, `-turbo-ONNX`, https://huggingface.co/onnx-community/chatterbox-ONNX
- https://github.com/cmusphinx/cmudict (`LICENSE`), https://github.com/jpreprocess/jpreprocess, https://github.com/r9y9/pyopenjtalk
- crates.io metadata for `misaki-rs`, `espeak-rs`, `num2words`, `language-tokenizer`, `cmudict-fast`, `ort`, `candle-core`
