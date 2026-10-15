---
title: "Live caption translation"
summary: "Live caption translation"
area: research
---

# Live caption translation

Question: BR-13 says "captions can be translated live into another language". Which licence-clean models can do it on the device, for English and the five other launch languages (ja, ko, zh-Hans, es-419, pt-BR), at what latency, and should it be a model slot in the registry (`translate_text@1`), a connector the person pays for, or both?

This is a research note written in an environment without access to model hosts. Not legal advice: it says what each text says and which clause, and lists open legal points.

## Read this first: what could not be done here

1. **No sentence was timed.** The acceptance check asks for 100+ sentences timed on the CPU candidates. This pass was told to download no models or datasets, and `huggingface.co` (every model card) was blocked in that environment, so no candidate could even be fetched. Latency below is **derived from vendor-published benchmarks** and labelled so; the timing protocol to run later is in §7.
2. **Second pass (2026-10-13):** the Hugging Face model API and cards were reachable and are now read (licence tag, card text, training-data statements); the M2M-100 and MADLAD-400 papers (arXiv 2010.11125, 2309.04662) were read from the PDFs; the Google and DeepL service pages were fetched. Rows below say `verified` with the source, or keep `unverified` with the reason. The first pass could not reach any of these.
3. **Training-data licences remain partly unread:** `opus.nlpl.eu` loads but its per-corpus licence terms are not machine-readable there, so the OPUS corpora used by each OPUS-MT pair stay `unverified`.
4. No account was created and no hosted API was called.

Evidence legend: **verified** = I read the primary source in this session; **secondary** = a vendor or registry page that is not the licence text; **unverified** = not read. All retrieved 2026-10-13. Raw files were fetched from `raw.githubusercontent.com` (path given).

## 1. Summary

- **Update (2026-10-13, second pass):** after reading the cards, most per-pair OPUS-MT repos on Hugging Face (`ko-en`, `en-es`, `es-en`, `en-zh`, `zh-en`, `tc-big-*`) are Tatoeba-Challenge builds and so `NO`; MADLAD-400 MT is `NO` (TED talks, CC BY-NC-ND, among its parallel data) unless the maintainers rule otherwise; M2M-100 (MIT weights, Common Crawl mined data, no NC source found) is the one multilingual candidate left to clear. The recommendation below stands: no default model yet, Whisper-to-English plus an opt-in connector.
- **No candidate is licence-clean end to end on what could be read.** The OPUS-MT weights tags are permissive (`apache-2.0` or `cc-by-4.0` per repo), but the repos for ko, zh and es are Tatoeba-Challenge builds whose training data its README states is CC-BY-NC-SA 4.0: `NO` under `policy.md` ("models trained on non-commercial data, unless the model's authors clear commercial use"). MADLAD-400 is `NO` (TED data). OPUS-MT ja-en and en-ROMANCE and M2M-100 stay `unverified` on unread corpus terms.
- **NLLB-200 is `NO`**: weights CC-BY-NC 4.0, read at the source.
- **Whisper's translate task is `yes` and free, but only into English**, and it needs the multilingual weights (the `.en` files cannot translate). It covers the ja/ko/zh/es/pt → en half of the matrix with no new model.
- **Recommendation:** ship translation as a **model slot `translate_text@1`** (ML-01/ML-13) with **no default model until a data-lineage check clears one** (§6), a **Whisper-to-English path as the one cleared built-in**, and a **paid connector as opt-in** (`spend` scope). Users can bring their own model through ML-13 today with their own licence responsibility.
- Latency budget (§5): final caption translated within about 1.5 s of the last spoken sound on the GPU path; on the CPU fallback it is reported, not gated, exactly like ASR's own CPU fallback.

## 2. Candidates and licence verdicts

Verdicts use the register vocabulary. "Weights" and "training data" are separate columns because the policy asks for both.

| Candidate | Runtime / code licence | Weights licence | Training data | Verdict |
|---|---|---|---|---|
| OPUS-MT (Helsinki-NLP, Marian), per-pair Hugging Face repos | Opus-MT repo: MIT, "Copyright (c) 2004-2020 Joerg Tiedemann" (verified, `Helsinki-NLP/Opus-MT/LICENSE`) | **permissive tag per repo** (HF API, checked 2026-10-13): `opus-mt-ja-en`, `-ko-en`, `-en-es`, `-es-en`, `-en-zh`, `-en-ROMANCE` carry `apache-2.0`; `-zh-en`, `-tc-big-en-ko`, `-tc-big-en-pt` carry `cc-by-4.0` (the Opus-MT README says CC-BY 4.0) | **depends on the repo.** Repos whose card says `original_repo: Tatoeba-Challenge` (`ko-en`, `en-es`, `es-en`, `en-zh`, `zh-en`, the `tc-big-*` family) were built from Tatoeba-Challenge training data, which its README says is "released under the CC-BY-NC-SA 4.0 license" (verified, `Tatoeba-Challenge/README.md` §License, 2026-10-13). `opus-mt-ja-en` (`opus-2019-12-18`, Opus-MT-train, "dataset: opus") and `opus-mt-en-ROMANCE` ("dataset: opus") are not Tatoeba-Challenge builds; their OPUS corpora terms are **unverified** | Tatoeba-Challenge builds: **`NO`**. ja-en and en-ROMANCE: `unverified` |
| OPUS-MT Tatoeba-Challenge models (`opus-tatoeba-*` **and the Hugging Face `opus-mt-*` repos listed above whose card names Tatoeba-Challenge**) | as above | permissive tags as above | Tatoeba-Challenge README, "License": "Training data is released under the CC-BY-NC-SA 4.0 license" (verified) | **`NO`** (**corrected:** the earlier draft limited this to `opus-tatoeba-*` names; the plain `opus-mt-ko-en`, `en-es`, `es-en`, `en-zh`, `zh-en` repos are also Tatoeba-Challenge builds) |
| M2M-100 (Meta, 418M / 1.2B / 12B) | fairseq: MIT, "Copyright (c) Facebook, Inc." (verified, `fairseq/LICENSE`) | **MIT**: HF API and card for `facebook/m2m100_418M` and `facebook/m2m100_1.2B` both `license: mit` (verified 2026-10-13) | fairseq `examples/m2m_100/README.md`: "we use a combination of CCMatrix and CCAligned" (verified); the paper (arXiv 2010.11125) describes mining bitext from Common Crawl plus backtranslation, 7.5B pairs, and uses Tatoeba only as an evaluation set. No non-commercial term found in either; the CCMatrix and CCAligned dataset terms (Common Crawl terms of use) were not read | `unverified` (data terms), best candidate to finish clearing: no known NC source |
| MADLAD-400 MT (Google, 3B / 7.2B / 10.7B) | `google-research` repository: Apache-2.0 (verified, `google-research/LICENSE`) | **Apache-2.0** (HF API and card for `madlad400-3b-mt` and `-7b-mt`, verified 2026-10-13) | MADLAD-400 monolingual data is `odc-by` plus Common Crawl terms (HF dataset API, paper datasheet). The **parallel data** (arXiv 2309.04662 Table 11) includes **TED57** (TED talks, which TED publishes under CC BY-NC-ND 4.0, https://www.ted.com/about/our-organization/our-policies-terms/ted-talks-usage-policy, checked 2026-10-13), OPUS100, JESC, Tanzil, Paracrawl, WikiMatrix and others of mixed terms | **`NO`** under `policy.md` (a known non-commercial-no-derivatives source in the training data). The Apache-2.0 release is the authors' own; whether that counts as clearing commercial use is a ruling for the maintainers, as for first-party permissive releases (there no NC source was known) |
| NLLB-200 (Meta; 600M / 1.3B / 3.3B / 54B) | fairseq: MIT (README "License": "NLLB and fairseq(-py) is MIT-licensed", verified) | **CC-BY-NC 4.0**: README "All models are licensed under CC-BY-NC 4.0" and `LICENSE.model.md` titled "Attribution-NonCommercial 4.0 International" (both verified, `fairseq` branch `nllb`); HF API for `facebook/nllb-200-distilled-600M` also `cc-by-nc-4.0` (verified 2026-10-13) | n/a | **`NO`** |
| Whisper translate-to-English (OpenAI) | whisper: MIT, "Copyright (c) 2022 OpenAI" (verified; already `yes` in the register) | MIT (register row). **Note:** the Hugging Face API for `openai/whisper-small` reports `apache-2.0`; both are permissive, but record which one the pinned weights file carries | not disclosed in detail (model card, verified: "weak supervision") | `yes` (already registered) for the **into-English** task only |
| Small general LLM, Qwen3 family | llama.cpp: MIT (verified, `llama.cpp/LICENSE`) | Qwen3 README, "License Agreement": "All our open-weight models are licensed under Apache 2.0" (verified); HF API for `Qwen/Qwen3-1.7B` and `Qwen/Qwen3-4B` both `apache-2.0` (verified 2026-10-13) | undisclosed | `conditions` (Apache-2.0; training data undisclosed, so the ruling on first-party permissive releases is the relevant precedent) |
| Small general LLM, Gemma family | n/a | HF API: `license: gemma`, gated (manual approval) for `gemma-3-1b-it` and `-4b-it`. Gemma Terms of Use read at https://ai.google.dev/gemma/terms (2026-10-13): no non-commercial term found; third parties must receive a copy of the Agreement and the use restrictions "as an enforceable provision in any agreement"; the Prohibited Use Policy is incorporated by reference; Google "reserves the right to restrict (remotely or otherwise) usage"; distillation outputs count as Model Derivatives; the page says Gemma 4 has a separate licence not read | undisclosed | `conditions` on paper (flow-down plus remote-restriction clause: for legal review); not preferred over Qwen3 |
| Bergamot / Firefox Translations student models (Mozilla) | bergamot-translator: MPL-2.0 (verified, file-level copyleft; same as the policy's MPL rule) | "The model files are distributed under the MPL 2.0 license" (verified, `mozilla/translations/README.md`, 2026-10-13) | built by Mozilla's pipeline from public parallel data (OPUS and others, per the project's slides); per-corpus terms and whether NC corpora are excluded were **not found** | `unverified` (data lineage) |
| CTranslate2 (runtime) | MIT, "Copyright (c) 2018- SYSTRAN" (verified); PyPI metadata 4.8.3, MIT (secondary) | n/a | n/a | `yes` |
| `ct2rs` (Rust binding of CTranslate2) | crates.io API: 0.10.1, MIT (verified 2026-10-13); its transitive native build and sentencepiece dependency **unverified** | n/a | n/a | `conditions` (read its dependency licences before adding) |
| `ort` / ONNX Runtime | already in the register | n/a | n/a | `conditions` (existing row) |
| Hosted translation or LLM connector (Google Cloud Translation, others) | service | n/a | n/a | `conditions`: paid, opt-in, `spend` scope, never called in tests or CI. DeepL: its public product page gives **no API terms**: `unverified` |

Notes on the verdicts:
- **OPUS-MT**: the CC-BY-4.0 grant covers the weights and requires attribution (verified: the licence line). It says nothing about the corpora. The project's own README warns that its training pipeline applies "no quality control besides of the automatic evaluation" and trains "on all OPUS data" (verified). Until someone reads the per-corpus terms for the OPUS subsets used by the specific pair we would ship, the policy's "unverified is treated as NO" applies. This is the main question for legal review.
- **M2M-100 and MADLAD-400**: the cards are now read. Both carry permissive weights licences (MIT, Apache-2.0). M2M-100 was trained on Common Crawl mined data with no non-commercial source found (data-set terms unread: `unverified`). MADLAD-400 MT's parallel data includes TED talks (CC BY-NC-ND), so it is `NO` unless the maintainers rule the authors' Apache-2.0 release a clearance.
- **Tatoeba-Challenge**: the NC-SA training data is a stated fact in the repository, not an inference. Do not use any `opus-tatoeba-*` model, nor any Hugging Face `opus-mt-*` repo whose card names `original_repo: Tatoeba-Challenge` (`ko-en`, `en-es`, `es-en`, `en-zh`, `zh-en`, `tc-big-*`), without the authors clearing commercial use.
- **FLORES-200** (the evaluation set, §4) is CC-BY-SA 4.0: `LICENSE_CC-BY-SA` in `facebookresearch/flores` opens with "Attribution-ShareAlike 4.0 International" (verified). Evaluation only; never shipped; ShareAlike would attach to anything adapted from it.

## 3. Language coverage

Pairs asked for: en ↔ {ja, ko, zh-Hans, es-419, pt-BR}.

| Candidate | into English | out of English | Notes |
|---|---|---|---|
| OPUS-MT (per-pair models) | ja, ko, es exist; **zh-en exists on Hugging Face** (`Helsinki-NLP/opus-mt-zh-en`, verified 2026-10-13; the earlier "no zh-en" came from looking only in `Opus-MT-train/models`). Most of these are Tatoeba-Challenge builds, hence `NO` | es, pt exist; **ja** appears as `en-jap` with only a Bible benchmark; **ko, zh**: found on Hugging Face as `opus-mt-tc-big-en-ko` and `opus-mt-en-zh` (verified 2026-10-13), both Tatoeba-Challenge builds (`NO`); `en-ja`: `opus-mt-tc-big-en-ja` exists as a repo name but its API metadata was empty | per-pair models need one model per direction; Latin-American Spanish and Brazilian Portuguese are not separate models (variant tag support is **unverified**) |
| M2M-100 | 100 languages incl. all six (paper read in the second pass; the per-language list was not parsed here: **unverified**) | same | one multilingual model for every direction; variant handling (es-419, pt-BR vs pt-PT) **unverified** |
| MADLAD-400 | 400+ languages (README title, verified: "Multilingual"; counts **unverified**) | same | decoder-style T5 with a `<2xx>` target token (**unverified**) |
| Whisper | **all six → en** (model card: "zero-shot translation from multiple languages" into English, verified) | **none** | needs multilingual weights; the `.en` sizes the speech spike uses cannot translate |
| Qwen3 LLM | vendor README claims "100+ languages and dialects" with strong translation (verified as a vendor claim, not as quality) | same | quality per pair **unmeasured** |
| Hosted | broad (vendor) | broad | opt-in |

Take-away (after the second pass): **only multilingual models cover the whole matrix in one model**, and of those MADLAD-400 is `NO` (TED data), leaving M2M-100 (data terms unread), an LLM (Qwen3: Apache-2.0 weights, undisclosed data) or a hosted connector. The per-pair OPUS-MT repos that exist for ko, zh and es are Tatoeba-Challenge builds and so `NO`; ja-en and en-ROMANCE (es, pt) are OPUS builds with unread corpus terms. Whisper fills only the into-English half.

## 4. Quality (published numbers only; none measured here)

Published chrF from the OPUS-MT per-pair READMEs (verified, `Opus-MT-train/models/<pair>/README.md`). These are **not comparable across rows**: different test sets, and Tatoeba sentences are short and easy (the OPUS-MT README itself warns "those scores will be too optimistic when running the models with other more realistic data sets").

| Pair | Test set | chrF | BLEU |
|---|---|---|---|
| en→es | newstest2013 | 0.566 | 32.4 |
| en→es | Tatoeba | 0.700 | 50.1 |
| es→en | (file identical size to en-es; contents not compared) | not read | |
| en→pt | Tatoeba | 0.630 | 40.6 |
| ja→en | Tatoeba, two model releases | 0.455 and 0.589 | 28.6 and 41.7 |
| ko→en | Tatoeba, two model releases | 0.511 and 0.576 | 29.6 and 39.2 |
| en→ja | `bible-uedin` only | 0.960 | 42.1 (a chrF of 0.96 on kana text is a scoring artefact, not quality) |

Spoken-style captions are closer to Tatoeba (short, conversational) than to news, but Tatoeba-trained models are `NO` above, so these numbers say how capable the architecture is, not how a shippable model scores.

**Evaluation set to use:** FLORES-200 `devtest` (CC-BY-SA 4.0, verified above), with the 2022 NLLB metric chrF++ (the NLLB README reports `chrF++` and `spBLEU` on FLORES-200; verified). It is a written-text set, so also keep a small hand-written spoken-caption set of our own (original sentences, CC0) for the partial/final tests in §5. FLORES-200 and Tatoeba both have to be kept out of any training or tuning we ever do.

## 5. Latency, partial text and failure modes

### Latency (not measured)

**Nothing was timed**, and no single-caption latency is inferred. The only primary number reachable is a throughput figure from the CTranslate2 README (verified): an OPUS-MT En→De base Transformer, CTranslate2 3.6.0, int8, on a c5.2xlarge (Xeon Platinum 8275CL, 4 threads) = **696.1 target tokens/s**, peak memory 516 MB; on an A10G GPU int8 = **7,521.9 tokens/s**. The README reports it over the whole newstest2014 set; the batch and beam settings sit in the project's benchmark scripts (`tools/benchmark`), which were not read here. A review of this note states they are batch size 32 and beam size 4, so aggregate throughput says nothing about batch-one latency, and encoder and beam-search work are inside it. It shows only that a Marian-class model is in a different cost class from an LLM.

Open until it is measured (§7): batch-one latency for a 20-word sentence and a 5-word partial, on a typical 4-core CPU and on tier `R`. The same applies to download sizes and tier-`R` estimates per candidate: none were recorded here. The latency budget in §6 is therefore a proposal to be checked against those numbers, not a result.

### Partial versus final text

ASR emits partials that are revised and finals at end of utterance (`Transcript.is_final`). Options:
1. **Translate finals only.** Simple, stable, one call per utterance. Cost: the translated caption arrives after `asr.final` plus the translation latency; the original can show first.
2. **Translate stabilised partials.** Re-translating each partial makes the translation flicker as the source changes, and word order differs by language (ja/ko verbs come last), so early words of the target often change. The design below only translates a partial when it has been unchanged for N ms or grows by a whole clause, and never shows a translated partial as final.
3. **Incremental (simultaneous) translation models** are out of scope: none of the candidates is one.

Recommendation: **finals only in v1**, original shown immediately, translation swapped in when ready (`mode` Both keeps the original above); revisit stabilised partials once the flicker rate has been measured.

### Failure modes (all to be tested)

- **Hallucination and repetition** on very short or noisy inputs (a known behaviour of sequence models; a length-ratio guard between source and output is the usual defence — **unverified for these models**).
- **Names and handles** (VTuber names, chat handles, brand names): models transliterate or translate them. A protected-terms list (per character) passed to the slot is needed; whether each candidate honours constraints is **unverified**.
- **Slurs and unsafe output:** the output of translation is new text on stream. **AP-07's text filter must run on the translated text**, not just on the source, in every language the filter supports; a filter that only knows English would miss them. FLORES's `Toxicity-200` lists exist (`facebookresearch/flores/toxicity`) but their licence was not read: `unverified`, not used.
- **Whisper-translate** inherits Whisper's own hallucinations on silence and music (model card caution on reliability, verified in general terms); gating by VAD, as the speech host already does, applies.
- **Privacy:** a `private` transcript must never reach a hosted connector. Hosted translation sends the person's words to a third party; it is opt-in, off by default, and the UI shows the destination.

## 6. Recommended design

### Registry slot

```text
translate_text@1
  input  : { text: String, src: BcpTag, dst: BcpTag, protected: Vec<String>, final: bool }
  output : { text: String, src_detected: Option<BcpTag>, model: ModelRef, ms: u32 }
  errors : UnsupportedPair | TooLong | ModelMissing | Timeout
```

- `src` may be `und` (undetermined); the speech host already knows `Transcript.lang`, so pass it.
- Tags are BCP 47 (`ja`, `ko`, `zh-Hans`, `es-419`, `pt-BR`, `en`), the same as `Transcript.lang`. A model declares the pairs it supports in its manifest; the registry rejects an unsupported pair before running (smoke test, ML-13).
- `protected` carries names that must pass through untranslated.
- Contract rules from ML-01/ML-13 apply: manifest with licence record, hash-verified download on demand (the installer stays ≤ 150 MB), a CPU path, a fallback when the model is missing (**show the original caption**, never an empty line), and an eval suite. A user-supplied model (ONNX, GGUF or CTranslate2 format, ML-13) fills the same slot with the person's licence acknowledgement.
- No shipped default until a data-lineage check clears one (below).

### Which models

1. **Built-in, cleared: Whisper multilingual `translate` task into English** for ja/ko/zh/es/pt speech. Costs one more Whisper decode per utterance; no new weights, but the multilingual Whisper size must be chosen (the speech host currently documents `.en` models).
2. **Built-in candidate, blocked on one read** (MADLAD-400 is now `NO`; M2M-100 is the remaining one, blocked on the CCMatrix and CCAligned terms): an M2M-100 class model through CTranslate2 (MIT) covers every pair in one model. It ships only after the card, the weights licence and the training data terms are read and recorded in the register. If either has a non-commercial or unclear data term, the answer for the non-English directions is the connector below.
3. **OPUS-MT per-pair models:** the Tatoeba-Challenge builds (ko-en, en-es, es-en, en-zh, zh-en, `tc-big-*`) are `NO` and are not offered; only ja-en and en-ROMANCE stay `unverified`, until their OPUS corpus terms are read. If cleared they are expected to be the cheapest option (Marian-class; size and latency unmeasured).
4. **Connector (opt-in, `spend` scope):** Google Cloud Translation (its pricing page was read on 2026-10-13; rates are not recorded here) or an LLM provider. Never called from tests or CI; mock it. Always shows the destination and the cost estimate before first use.
5. **Small LLM (Qwen3 class) through the registry:** only if a Marian-class model is not cleared, and only as a choice the person makes (quality for ja/ko/zh is a vendor claim; latency is unmeasured).

### How captions show it (`mode`)

`Original | Translated | Both` already exists in `CaptionConfig`. Translation arrives in `payload.translated` on the final transcript event, so:
- `Original` shows what was heard (no translation call is made at all).
- `Translated` shows the original until the translation is ready, then swaps once; a late translation arriving after the hold expired is dropped.
- `Both` shows the original above in the smaller size, the translation below. Lines are laid out per language (CJK has no spaces) with the translated text's own `lang` so the right font is chosen.
- The translated text carries the AP-07 filter result; a blocked translation shows nothing for that line (not the original, so the viewer does not see what the filter rejected).

### Latency budget (speech to translated caption)

| Stage | Budget on the GPU recogniser | CPU fallback |
|---|---|---|
| last spoken sound to final text (`asr.final`) | ≤ 1.0 s, p95 (spec §10) | 1.4 to 1.8 s measured, reported only (`2026-10-speech-host-cpu.md`) |
| `translate_text@1` for a final | ≤ 0.3 s, p95 (proposed; to be checked against the §7 batch-one measurements) | reported, not gated |
| AP-07 filter on the output | ≤ 20 ms | ≤ 20 ms |
| **total: last spoken sound to translated caption** | **≤ 1.5 s p95** | not gated |

These are proposals for the maintainers, not measured results. Translation runs only while a caption consumer holds a transcript claim **and** `mode` is not `Original`, like ASR itself, so a person who does not use translation pays nothing.

## 7. Timing protocol

Needed to close the acceptance check, since nothing could be timed here:
1. Environment to state: CPU model, core count, OS, CTranslate2 version, thread count, int8 vs fp16; run on a typical 4-core machine and on `Dev`.
2. Sentences: at least 100 spoken-style sentences, 20 words each, plus 100 five-word partials, per direction; from FLORES-200 `devtest` (CC-BY-SA) for quality (chrF++) and from an original CC0 spoken set for latency. Report p50, p95, warm-up and peak RSS. 20 warm-up sentences discarded.
3. Models: for each of M2M-100 418M and 1.2B, MADLAD-400 3B, an OPUS-MT pair per direction, Whisper small multilingual `translate`, and one Qwen3 1.7B/4B-class model through llama.cpp: only after the cards are read (licence first, weights second).
4. Record the output of each model on a 30-sentence name and slur probe (protected terms, VTuber handles) and run it through the AP-07 filter.
5. Never call a hosted API.

## 8. Not verified (list)

- OPUS corpus per-corpus licences (the per-corpus terms are not machine-readable on `opus.nlpl.eu`), and therefore `opus-mt-ja-en`, `opus-mt-en-ROMANCE` and Bergamot student data lineage.
- CCMatrix and CCAligned dataset terms (M2M-100 data) and Qwen3 / Gemma training data.
- Gemma 4 licence (separate from the Gemma Terms of Use read).
- Whether OPUS-MT handles `es-419` and `pt-BR` as variants; language lists of M2M-100 and MADLAD-400 beyond the README claims.
- DeepL API terms.
- `ct2rs` transitive licences.
- **Every latency figure, per-candidate download size and tier-`R` estimate for our targets** (only the CTranslate2 vendor throughput is cited; no `R` scaling). The acceptance check asks for 100+ timed sentences: **not met**; the measurement is deferred to the protocol in §7.
- Every quality figure except the OPUS-MT per-pair READMEs above.
- Flicker rate of translated partials; the length-ratio guard; name-preservation behaviour.

Resolved on 2026-10-13 (second pass): weights licence tags for every Hugging Face candidate, the Tatoeba-Challenge scope, the M2M-100 and MADLAD-400 data statements, TED's licence, Gemma Terms of Use, Bergamot model licence, `ct2rs` licence, Google service page.

## 10. Next steps

- Task: read the cards, fill §8, run §7, and update the register rows to `yes` / `conditions` / `NO`.
- Task: `translate_text@1` slot in `omuzaki-ai` once a model is cleared (blocked on the above); includes the AP-07 filter hook and the protected-terms list.
- Task: Whisper multilingual translate path in `omuzaki-speech` (needs the multilingual weights; blocked on the registry row).
- Decision for the maintainers: ship translation with only the Whisper-to-English path and the connector, or wait for a cleared multilingual model.
