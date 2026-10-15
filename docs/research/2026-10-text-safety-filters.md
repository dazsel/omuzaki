---
title: "Text-safety filters we can ship"
summary: "text-safety filters for the AI performer's speech (AP-07): twelve candidates with the three licence checks, language coverage and claimed latency, the recommended fail-closed chain and a licence-clean test-set plan."
area: research
---

# Text-safety filters we can ship

Question (#972, AP-07): the AI performer needs a "text filter before speech" (`ai-performer.md` §5, the `Speak` hook). Which filters can we ship under `docs/licensing/policy.md` (weights licence, training data, runtime), in which of the six launch languages (English, Japanese, Korean, Chinese, Spanish, Portuguese), and within a 30 ms per-sentence budget? Not legal advice.

## Read this first: what could not be done here

1. **No model was downloaded and nothing was timed.** The brief forbids downloading models. Every latency below is **claimed** (a vendor figure, or arithmetic from a size) and labelled so. The timing protocol to run later is in §6.
2. **Hugging Face was unreachable** (the environment's proxy answered 403 to `huggingface.co`), so no model card, `LICENSE` or file listing on the Hub was read. Licences are read from GitHub-hosted files where they exist. Where the weights licence lives only on the Hub, the cell says "not read" and the verdict is `unverified`, which `policy.md` treats as `NO`.
3. **Hosted services: only search summaries were reachable** (`platform.openai.com` and `ai.google.dev` did not load). Their terms are unread. No account was created and no service was called.
4. A candidate's verdict is the **worst of its three checks** (weights licence, training data, runtime). Retrieval date for everything below: **2026-10-14**.

Evidence legend: **read** = the file or page was read in this session; **search** = a search-result summary only; **not read** = not reached.

## 1. Summary

- **Nothing open and licence-clean fits the in-process budget yet.** Every classifier or guard model that was readable is `unverified` on training data or weights licence, and every guard LLM (0.6B and up) is a server-class model, not a 30 ms CPU step.
- **The one thing we can ship today is our own rule layer** (normalisation, word lists, regex, run limits): own code, no weights, no training data. It covers the six languages only as far as the word lists do, so the gaps (§5) are stated, not hidden.
- **Recommendation:** the shippable chain is the rule layer, failing closed, plus the platform's own moderation (BR-14) and a persona safety preamble. A learned classifier enters the shippable path only after one candidate gets all three checks read from primary sources and a measured latency (§6). Optional heavier filters come from the person's own model slot or LLM provider and are never a dependency.
- **Perspective API is gone for planning purposes:** its own site says it stops on 2026-12-31 and takes no new requests.

## 2. Candidates

"Runtime" says what runs it. "Server" means it needs a Python or vLLM-class process, so it is not in-process for us. Languages are checked against the six launch languages: en, ja, ko, zh, es, pt.

| # | Candidate | Weights licence | Training data | Runtime and deps | Verdict |
|---|---|---|---|---|---|
| 1 | **Own rule layer**: Unicode normalisation, per-language word lists, regex for links and personal data, run and repeat limits | none (no weights); our code, written by us | none; word lists we write (§6) | in-process Rust; `aho-corasick`, `regex` and `unicode-normalization` (runtime licences in §2.1; register rows added) | `yes` |
| 2 | **Detoxify** `original`, `unbiased`, `multilingual` (Unitary) | not read (Hub cards); code is Apache-2.0 | Jigsaw Kaggle competitions 2018, 2019, 2020 ([README, read](https://raw.githubusercontent.com/unitaryai/detoxify/master/README.md)); Kaggle data terms not read | PyTorch; `ort` needs an ONNX export we would make; `multilingual` is `xlm-roberta-base` | `unverified` |
| 3 | **Llama Guard 3-1B** (Meta) | Llama 3.2 Community Licence ([read](https://raw.githubusercontent.com/meta-llama/PurpleLlama/main/Llama-Guard3/1B/LICENSE)): attribution and copy of the agreement on redistribution, an Acceptable Use Policy flow-down, a 700M monthly-user clause; no non-commercial term | Llama 3 / 2 generations on hh-rlhf prompts plus "human and synthetically generated" multilingual data ([model card, read](https://raw.githubusercontent.com/meta-llama/PurpleLlama/main/Llama-Guard3/1B/MODEL_CARD.md)); the extra data is not described | generative LLM, 1B params; server or llama.cpp-class runtime, not `ort` today | `unverified` (training data); weights are `conditions` |
| 4 | **Llama Prompt Guard 2 22M / 86M** (Meta; injection and jailbreak) | Llama community licence per the family; this card does not state it, not read | open datasets plus Meta's synthetic injections and red-team data; base models `deberta-v3-xsmall` (22M) and `mdeberta-v3-base` (86M), MIT per the card ([card, read](https://raw.githubusercontent.com/meta-llama/PurpleLlama/main/Llama-Prompt-Guard-2/86M/MODEL_CARD.md)) | encoder classifier; ONNX export is plausible, not done | `unverified` |
| 5a | **Granite Guardian** 3.x / 4.1, 2B to 8B (IBM) | Apache-2.0 ([README, read](https://raw.githubusercontent.com/ibm-granite/granite-guardian/main/README.md); `LICENSE` read) | not read | generative LLM; the README's examples use vLLM (server); runtime licences in §2.1 | `unverified` (training data); weights `yes` |
| 5b | **Granite Guardian HAP 38M and 125M** (IBM; hate, abuse, profanity classifiers) | the README lists both and gives the Granite Guardian family as Apache-2.0, but the two HAP model cards are on the Hub and were not read | "several English HAP benchmarks", not named; 38M base is `slate.38m.english.distilled` ([IBM cookbook notebook, read](https://raw.githubusercontent.com/ibm-granite-community/granite-snack-cookbook/main/recipes/Granite_Guardian/HAP.ipynb)); the 125M base is not stated | encoder classifier (4 layers, 12 heads for the 38M); the cookbook runs it in PyTorch with `transformers`; `ort` needs an ONNX export we would make; runtime licences in §2.1 | `unverified` (weights card and benchmark licences not read) |
| 6 | **Qwen3Guard** Gen and Stream, 0.6B / 4B / 8B (Alibaba) | not found: the repository has no `LICENSE` on `main` (404) and the cards are on the Hub | "1.19 million prompts and responses labeled for safety" ([README, read](https://raw.githubusercontent.com/QwenLM/Qwen3Guard/main/README.md)); sources not described | generative LLM; Stream needs Qwen3 token ids and `trust_remote_code`; server-class | `unverified` |
| 7 | **gpt-oss-safeguard** (OpenAI) | gpt-oss is Apache-2.0 ([repo README and `LICENSE`, read](https://raw.githubusercontent.com/openai/gpt-oss/main/README.md)); the safeguard variant's own card is not read | not read | 20B-class (gpt-oss-20b is 21B params, 3.6B active); server | `unverified` |
| 8 | **LLM Guard** (Protect AI) and its prompt-injection / toxicity models | library MIT ([`LICENSE`, read](https://raw.githubusercontent.com/protectai/llm-guard/main/LICENSE)); the models are on the Hub, not read | not read | Python library; README says the project and its models are "no longer under active development or maintained" | `unverified` |
| 9 | **OpenAI moderation endpoint** (`omni-moderation-latest`) | hosted service, no weights | undisclosed | needs an OpenAI account, a network call per sentence: **needs-user**. Free to API users per a third-party summary; OpenAI's terms not read ([search](https://developers.openai.com/api/docs/models/omni-moderation-latest.md)) | `unverified` |
| 10 | **Perspective API** (Jigsaw) | hosted service, no weights | undisclosed | needs an API key: **needs-user**. Stops on 2026-12-31, no new requests ([perspectiveapi.com sunset notice, via search](https://perspectiveapi.com/)) | `NO` (service ends) |
| 11 | **LLM-as-judge through the person's own provider** (`llm-api.md` seam) | none from us: the person's model and provider | the provider's | one extra LLM call per turn through the existing `LlmClient`; **needs-user**, `spend` scope, off by default | `conditions` (opt-in; nothing shipped) |

### 2.1 Runtime and dependency licences (the policy's third check)

Read from each project's `LICENSE` on GitHub, retrieved 2026-10-14. "Needed by" is which candidates in §2 use it. This checks the licence of the runtime itself; its own dependency trees were not walked, so any runtime we would ship still goes through `cargo deny` or the third-party notices (`policy.md`, "Adding anything new").

| Runtime or dependency | Needed by | Licence (primary source) | Verdict |
|---|---|---|---|
| ONNX Runtime and `ort` | 2, 4, 5b after export | MIT, "Copyright (c) Microsoft Corporation" ([LICENSE](https://raw.githubusercontent.com/microsoft/onnxruntime/main/LICENSE)); already registered at 1.28.3 | `yes` |
| PyTorch | 2, 4, 5b (the vendors' reference code) | BSD-style, Facebook, DeepMind and others ([LICENSE](https://raw.githubusercontent.com/pytorch/pytorch/main/LICENSE)); bundled third-party notices unread; not shipped by us | `conditions` |
| Hugging Face `transformers` | 2, 4, 5b, 6 reference code | Apache-2.0 ([LICENSE](https://raw.githubusercontent.com/huggingface/transformers/main/LICENSE)); not shipped by us | `yes` |
| llama.cpp | 3, 5a, 6 (the local LLM runtime option) | MIT, "Copyright (c) 2023-2026 The ggml authors" ([LICENSE](https://raw.githubusercontent.com/ggml-org/llama.cpp/master/LICENSE)) | `yes` |
| vLLM | 5a, 6, 7 (server option) | Apache-2.0 ([LICENSE](https://raw.githubusercontent.com/vllm-project/vllm/main/LICENSE)); its Python and CUDA dependency tree unread | `conditions` |
| Qwen3Guard `trust_remote_code` model code | 6 | not found: no `LICENSE` in the repository (404 on `main`) | `unverified` |
| `aho-corasick` | 1 | Unlicense or MIT ([UNLICENSE](https://raw.githubusercontent.com/BurntSushi/aho-corasick/master/UNLICENSE), [LICENSE-MIT](https://raw.githubusercontent.com/BurntSushi/aho-corasick/master/LICENSE-MIT)); already in `Cargo.lock` | `yes` |
| `regex` | 1 | MIT or Apache-2.0 ([LICENSE-MIT](https://raw.githubusercontent.com/rust-lang/regex/master/LICENSE-MIT), [LICENSE-APACHE](https://raw.githubusercontent.com/rust-lang/regex/master/LICENSE-APACHE)); already in `Cargo.lock` | `yes` |
| `unicode-normalization` | 1 | MIT or Apache-2.0 ([LICENSE-MIT](https://raw.githubusercontent.com/unicode-rs/unicode-normalization/master/LICENSE-MIT)); already a register row | `yes` |

PyTorch, `transformers` and vLLM are Python-side tools for the vendors' reference code; our shippable path loads none of them. The Python candidates become shippable only through an ONNX export run by `ort`, which is why rows 2, 4 and 5b need an export we can reproduce (§5).

Notes that decide the table:

- **Llama licences are not on `policy.md`'s allowed list**; the policy names only OpenRAIL-style and a few proprietary grants for case-by-case review. Rows 3 and 4 would need that review even if the training data were cleared.
- **Hosted endpoints (9, 10, 11)** send a person's chat text to a third party. A `private` transcript never leaves the machine (the rule the translation note records), so they are opt-in connectors, never the default.
- **Gaming and VTuber slang:** no candidate publishes a licence-clean slang set, so none could be assessed on it. §6 builds our own.

## 3. Practical details

Sizes are what the architecture implies, not file sizes read from a Hub listing (not reachable). Latency is **claimed** throughout: no model was run.

| # | Via `ort`? | Size on disk | CPU latency, 40-token sentence | Languages (of en / ja / ko / zh / es / pt) |
|---|---|---|---|---|
| 1 | n/a (pure Rust) | word lists: tens of KB per language (our estimate) | **claimed** under 1 ms (design target, not measured) | all six, as far as our lists go (§5) |
| 2 | after our own ONNX export | `xlm-roberta-base` is about 278M params, roughly 1.1 GB at fp32 (arithmetic, claimed); the Albert `-small` variants are smaller | **claimed** tens of ms on a mid laptop core for a base-size encoder at 40 tokens; unmeasured | `multilingual` trained on en, fr, es, it, pt, tr, ru ([README](https://raw.githubusercontent.com/unitaryai/detoxify/master/README.md)): **en, es, pt only**; no ja, ko, zh |
| 3 | no (LLM) | 1B params; the card ships a pruned INT4 variant | **claimed** hundreds of ms on CPU (1B generative); over budget | en, fr, de, hi, it, pt, es, th ([card](https://raw.githubusercontent.com/meta-llama/PurpleLlama/main/Llama-Guard3/1B/MODEL_CARD.md)): **en, es, pt only** |
| 4 | probably (encoder) | 22M and 86M params | vendor: 19.3 ms (22M) and 92.4 ms (86M) per classification on an **A100 GPU at 512 tokens** ([card](https://raw.githubusercontent.com/meta-llama/PurpleLlama/main/Llama-Prompt-Guard-2/86M/MODEL_CARD.md)); a CPU figure is not given, so **claimed**, unknown | 86M evaluated on en, fr, de, hi, it, pt, es, th: **en, es, pt**; the 22M is English-centric by the card's own limits |
| 5a | no (LLM) | 2B to 8B params | **claimed** seconds on CPU; over budget | **English only**: the README says "The model is only trained and tested on English data" |
| 5b | after our own ONNX export | 38M params (about 150 MB at fp32, arithmetic, claimed); 125M params (about 500 MB, claimed) | the vendor says "low inference latency" on CPUs, with no number ([cookbook, read](https://raw.githubusercontent.com/ibm-granite-community/granite-snack-cookbook/main/recipes/Granite_Guardian/HAP.ipynb)); **claimed** single-digit to low tens of ms for the 38M at 40 tokens, unmeasured | **English only** for the 38M (English base and benchmarks); the 125M is not stated, so treated as English only |
| 6 | no (LLM; Stream needs Qwen3 ids) | 0.6B to 8B params | **claimed** hundreds of ms on CPU at 0.6B; over budget | "119 languages and dialects" per the README (**claimed**): would cover all six, unmeasured |
| 7 | no | 21B params | **claimed** far over budget; server | not read |
| 8 | no (Python) | n/a | n/a | not read |
| 9, 10 | n/a (network) | none | a network round trip, **claimed** 100 ms or more, over budget | OpenAI: "especially in non-English languages" ([announcement, via search](https://openai.com/index/upgrading-the-moderation-api-with-our-new-multimodal-moderation-model/)); exact languages not read |
| 11 | n/a (the person's provider) | none | one LLM call, **claimed** a second or more | the provider's model |

Reading across: **only the rule layer and a small encoder classifier can meet 30 ms on a CPU core**, and the encoder candidates (2, 4, 5b) are all `unverified`. Of them, Detoxify `multilingual` and Prompt Guard 2 86M reach three of the six languages (en, es, pt) and the Granite HAP 38M reaches one (en) but is the smallest and the most likely to fit the budget. Ja, ko and zh have no candidate except row 6 (an LLM) and our own lists.

## 4. Recommended first-party chain

Order, per sentence, after the chunker (§5.2 of `ai-performer.md`) and before `Voice::speak`. The whole chain fails **closed**: an error, a panic caught at the boundary, or a missing word list drops the sentence and emits `PerformerEvent::Filtered { why }`; it is never spoken.

1. **Stage 1, in-process, always on (the rule layer).**
   - NFKC normalisation, then fold width, case and common leetspeak and homoglyphs (zero-width and bidi controls removed first).
   - Aho-Corasick over per-language word lists (en, ja, ko, zh, es, pt), matched on the folded text and, for ja, ko and zh where there are no spaces, on character n-grams.
   - Regex for URLs, e-mail addresses, phone numbers and similar personal data.
   - Limits on character runs and repeated tokens.
   - **Latency: claimed under 1 ms per sentence**, against a 30 ms budget. The budget is not at risk.
2. **Stage 2, in the persona prompt (not a filter).** The existing safety preamble (§5.1): text inside chat is data, not instructions. This is the injection defence we ship; no detector from §2 is cleared to ship.
3. **Stage 3, the platform and BR-14.** Platform moderation and the blocklists of #504 act on the chat before the performer sees it. We do not duplicate them.
4. **Optional, user-supplied, never a dependency.** A person may add a classifier through a model slot (ADR 0016) or turn on an LLM-as-judge through their own provider (row 11). The product shows the declared licence and the "not from Omuzaki" notice, as for any slot model. Nothing in stages 1 to 3 may depend on it, and no default or preset may point to a model `policy.md` bans.

**Language coverage of the shippable chain**

| Language | Stage 1 coverage | Gap |
|---|---|---|
| en | word list plus normalisation | slang drifts; coverage is what we write and test |
| es, pt | word list plus normalisation | regional variants (es-419, pt-BR) need their own entries |
| ja, ko, zh | word list plus n-gram matching | no spaces, so obfuscation (homophones, mixed scripts) is weakly covered |

No learned classifier covers the gaps in the shippable path. That is the stated gap: **toxic intent expressed without a listed word, and sarcasm, are not caught in-process.**

## 5. What would unlock a classifier

A classifier enters the shippable path when one candidate has, from primary sources: the weights licence read from a `LICENSE` or card; the training data named and its licence read; runtime dependencies checked; an ONNX export we can reproduce; and a latency measured on the reference machines (`2026-10-reference-machine-definition.md`) at or under 30 ms for 40 tokens. Row 5b (Granite HAP 38M) is the nearest to the budget but English only; rows 2 and 4 reach en, es and pt; each needs its Hub card and training-data terms read. Until then the verdict stays `unverified`, and the shippable chain is unchanged by adding the runtime check: the only runtimes it needs (`aho-corasick`, `regex`, `unicode-normalization`) are `yes`, and a classifier would run on `ort`, also `yes`.

## 6. Test-set plan (licence-clean)

For AP-07's regression suite (`evals.md` §11 holds real sets out of the repository):

1. **Synthetic adversarial chat, written by us.** Hand-written templates crossed with our own word lists, per language: plain hits, obfuscated hits (leetspeak, spacing, homoglyphs, zero-width characters, width folding), near misses (harmless words that contain a listed word), prompt-injection lines ("ignore your rules"), personal-data lines (fake e-mails, fake phone numbers on reserved ranges) and link spam. Generated deterministically from a seed so the set is reproducible.
2. **Slang and VTuber chat:** written by us from the project's own persona and kit characters; no real streamer, chat log or channel is used.
3. **No scraped or private material of any kind:** no chat logs, no Kaggle or dataset text, no recordings of people. Nothing from the Jigsaw sets, even though Detoxify is trained on them.
4. **Ten percent held out** as a sealed set not used to tune the lists.
5. **Metrics:** recall on hits, false-positive rate on near misses and benign chat, and per-sentence latency (p50, p95, p99) on the reference machines. The latency table in §3 is replaced with measured numbers when this runs.
6. **Rule for any future classifier:** it is scored on this same set, not on its own vendor's benchmark.

## 7. Open points

- Read the Hub cards and training-data statements for rows 2, 4, 5a, 5b, 6 and 7 once `huggingface.co` is reachable; then re-grade.
- Read the Llama Acceptable Use Policy flow-down and decide if it is acceptable to pass to our users (legal review).
- Read OpenAI's usage terms before any hosted-moderation connector (row 9).
- Word lists for ja, ko and zh need a native-speaker review by the maintainers; we do not copy third-party lists, and each list's provenance is recorded.
