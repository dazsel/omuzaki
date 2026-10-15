---
title: "Hosted text-to-speech and voice conversion for CN-10 (2026-10-15)"
summary: "Research for CN-10 on hosted TTS and voice-conversion services. Only Google Cloud could be read at its primary source; the rest is unverified and the note says so."
area: research
---

# Hosted text-to-speech and voice conversion for CN-10 (2026-10-15)

> Research pass for #1469 (parent #575, CN-10; design `docs/design/connectors.md` §3 D14 and
> `docs/design/ai-performer-voice-face.md` §3.5, §3.6). **Partial:** see "What could not be read".

**No account was made, no request was sent to any vendor API, nothing was spent and no voice was uploaded.** Only public
pages were read, on 2026-10-15.

## What could not be read

The session's network policy refuses (HTTP 403 from the egress proxy on the CONNECT, or no DNS from the fetch tool)
every vendor host below, so their terms and price pages were **not read at the primary source**:

`elevenlabs.io`, `aws.amazon.com`, `docs.aws.amazon.com`, `learn.microsoft.com`, `azure.microsoft.com`, `openai.com`,
`platform.openai.com`, `cartesia.ai`, `resemble.ai`, `respeecher.com`, `deepgram.com`, `play.ht`; and, on Google,
`docs.cloud.google.com` and `cloud.google.com/text-to-speech/quotas`.

Reachable: `cloud.google.com/text-to-speech/pricing` and `cloud.google.com/terms/service-terms` (both read 2026-10-15).

Rules used here: a **quote, price or licence verdict appears only for text read on the primary page**. For the other
candidates, a web-search result snippet (2026-10-15) may support a one-line description, only with its page title and URL
recorded and labelled secondary. A claim whose source was not recorded is marked `unverified (source not recorded)` and
is not used as support for any verdict or for the ranking. Every `unverified` item is a question for whoever finishes this note from an environment that allows
those hosts (the follow-up list at the end).

## Summary

| Candidate | Kind | Primary source read? | Price (dated) | Clone | First-adapter verdict |
|---|---|---|---|---|---|
| Google Cloud Text-to-Speech | large cloud TTS | partly (price, service terms) | Chirp 3 HD US$30 / 1M chars (verified) | Instant custom voice: price verified, terms `unverified` | candidate, pending output-ownership and cloning terms |
| Amazon Polly | large cloud TTS | no | `unverified` | Brand Voice only, `unverified (source not recorded)` | cannot rank |
| Azure AI Speech | large cloud TTS | no | `unverified` | Personal Voice, "limited access" `unverified (source not recorded)` | cannot rank |
| ElevenLabs | voice specialist, TTS + voice changer | no | `unverified` | `unverified (source not recorded)` | not ranked: sources not recorded |
| Resemble AI | specialist, speech-to-speech | no | `unverified (source not recorded)`, none usable | `unverified` | cannot rank (the voice-conversion candidate) |

**Ranked recommendation (provisional):** 1. Google Cloud, because it is the only candidate whose price is verified, it
bills per character as the `SpendGate` bound needs, and it needs no cloning for a stock-voice first adapter. There is no
second pick. **Not ranked: sources not recorded:** ElevenLabs (and Polly, Azure and Resemble). Every claim about their
terms comes from a search result whose page title and URL were not recorded, so those claims are `unverified (source not
recorded)` and stay out of the ranking until a follow-up records real sources or reads the primary terms. Voice conversion
has no pick: a hosted stream does not fit ML-14 and would need its own integration design (see the Resemble section). The
pick may not be adopted until the open items below are closed.

## Google Cloud Text-to-Speech

### Ownership of outputs
- **Unverified.** The Cloud Terms of Service clause on Customer Data and outputs was not read. The service terms page
  (https://cloud.google.com/terms/service-terms, "Last modified October 12, 2026", read 2026-10-15) has no
  Text-to-Speech section that I found; the only voice text there is for the Gemini Live API (next section).
- Commercial use and live streaming: unverified. Stock-voice licence (performer rights, attribution): unverified.

### Prompts, uploads and reference audio
- Retention, training on our audio, deletion: **unverified** (the Cloud Data Processing Addendum and the TTS data-logging
  page were not read).
- Region: unverified.

### Voice cloning and content rules
- Chirp 3 "Instant custom voice" exists and is priced (below). Its consent flow, who may be cloned and penalties are
  **unverified**: its docs page (`docs.cloud.google.com/text-to-speech/docs/chirp3-instant-custom-voice`) is refused.
- For Gemini Live custom voices, the service terms say, read 2026-10-15 at https://cloud.google.com/terms/service-terms:
  > "The functionality of creating custom voices and avatars in Gemini Live API (the "Custom Voice and Avatar
  > Functionality") is available only to Customers approved by Google."

  > "Customer will not use any image, audio recording, voice sample, or other data depicting or derived from any
  > individual(s) under the age of 18 with the Custom Voice and Avatar Functionality"

  The same terms also bar integrating that functionality into end-user-facing platforms (the clause is cut off in my
  extraction, so its full text is `unverified`). This is the Gemini Live API, not Cloud TTS; whether Instant custom voice
  has the same limits is the first question to ask. If it does, a streamer-facing app cannot use it.
- AI-voice disclosure, watermark: unverified.

### API shape
- Unverified except what the price page says: billing is by characters sent, and "All Speech Synthesis Markup Language
  (SSML) tags (except the `<mark>` tag) are also included in the character count" (pricing page, read 2026-10-15), so
  SSML is supported. Streaming, sample rates, auth, seed, voice count and languages: unverified (the service-terms page is
  not a product page and is not cited for them). Latency claims: none read.

### Price and limits
Read 2026-10-15 at https://cloud.google.com/text-to-speech/pricing, per character, "The total number of characters in the
input string are counted for billing purposes, including spaces and newline characters":

| Voices | Free per month | After free |
|---|---|---|
| Chirp 3: HD | 0 to 1 million characters | US$0.00003 per character (US$30 per 1 million characters) |
| Instant custom voice | Not available | US$0.00006 per character (US$60 per 1 million characters) |
| WaveNet | 0 to 4 million characters | US$4 per 1 million characters |
| Standard | 0 to 4 million characters | US$4 per 1 million characters |
| Neural2 | 0 to 1 million characters | US$16 per 1 million characters |
| Studio | 0 to 1 million characters | US$160 per 1 million characters |
| Polyglot (Preview) | 0 to 1 million characters | US$16 per 1 million characters |

Gemini-TTS is billed per token instead ("Input tokens: $0.50 per 1 million text tokens"; "Output tokens: $9.00 per 1 million
audio tokens" for the Preview Flash model through 2026-12-31, doubling on 2027-01-01; "25 tokens per second of audio").
A per-token price does not give a per-character bound, so it does not fit the dated per-character price in the design
(§3.6) without a conversion rule. Rate limits: unverified.

### Fit with the connector contract
- **Cap before each billable request:** fits for per-character voices, but the bound is computed from the **complete
  serialized input sent to Google**, not from the 220-character text chunk. The pricing page (read 2026-10-15) counts
  spaces, newlines and every SSML tag except `<mark>`, so the adapter builds the final request string first and then
  reserves: `bound = billable_chars(serialized_input) × price_per_char`, where `billable_chars` counts every character of
  the string (text after XML escaping, all tags and attributes, all whitespace) minus any `<mark>` tags (leaving them in
  only over-reserves). Escaping counts: `&` becomes `&amp;` (5 characters). Whether Google counts code points or bytes is
  `unverified`; until read, count the larger of the two. Worked example, Chirp 3 HD at US$0.00003 per character: a
  220-character chunk wrapped as `<speak><prosody rate="slow">…</prosody></speak>` adds 7 + 21 + 10 + 8 = 46 characters, so
  the serialized input is 266 characters and `bound = 266 × 0.00003 = US$0.00798` (7,980 micro-dollars), not the
  6,600 micro-dollars that `220 × price` would give. The `SpendGate` reservation uses 7,980.
- **Blocking `run`, whole chunks only:** fits; `ai-performer-voice-face.md` §3.1 wants whole chunks, no streamed synthesis.
- **`Preset { id }`:** fits if the catalog lists the voice names (unverified). **`Clone { reference, person }`:** would
  need the Instant custom voice consent flow and the answer above; the reference audio leaves the machine, so a person
  confirms (§3.6), and `ConsentLookup::find(person, Likeness::VoiceClone)` must return a record first.
- Provenance is host-built from the response; the vendor offers a watermark: unverified.

### Open questions
1. Cloud TTS output-ownership and commercial use of audio, including live streaming (quote the clause).
2. Instant custom voice: eligibility, consent mechanism, own-voice-only, the end-user-facing-platform restriction.
3. Data logging, training and retention for TTS input and reference audio; region options.
4. Streaming API, formats, sample rates, rate limits, SSML tags, seed.

## Amazon Polly

All of it is `unverified`: `aws.amazon.com` and `docs.aws.amazon.com` were refused.

### Ownership of outputs
Unverified. Stock-voice licence, commercial use, live streaming: unverified.

### Prompts, uploads and reference audio
Unverified (AWS service terms on AI content use and opt-out were not read).

### Voice cloning and content rules
Unverified. A claim that custom voices are a managed "Brand Voice" engagement, not self-service, came from a search result whose
title and URL I did not record: `unverified (source not recorded)`, not used for ranking.

### API shape
Unverified. Polly's SSML, speech marks and streaming support is widely documented but was not read here.

### Price and limits
Unverified. Search results naming https://aws.amazon.com/polly/pricing/ (2026-10-15; titles not recorded, secondary)
disagreed on the Neural rate, so no number is recorded.

### Fit with the connector contract
A per-character price would fit the cap bound if confirmed. Cloning is the gap: no self-service clone is established.

### Open questions
Everything in the Google list, for Polly; plus whether any self-service cloning exists.

## Azure AI Speech

All `unverified`: `azure.microsoft.com` and `learn.microsoft.com` were refused.

### Ownership of outputs
Unverified.

### Prompts, uploads and reference audio
Unverified.

### Voice cloning and content rules
Unverified. A claim that Personal Voice is a "limited access" feature that needs an application and is restricted to
approved use cases came from a search result whose title and URL I did not record: `unverified (source not recorded)`,
not used for ranking. If true, it is not self-service for an open-source app and the
disclosure and consent rules apply.

### API shape
Unverified.

### Price and limits
Unverified. No price source was recorded.

### Fit with the connector contract
Preset voices would fit if billed per character; clone fit depends on the limited-access answer.

### Open questions
Everything in the Google list, for Azure; plus the access-application terms for Personal Voice.

## ElevenLabs (voice specialist; TTS and voice changer)

All `unverified`: `elevenlabs.io` was refused. The claims below came from search results (2026-10-15) whose page titles
and URLs I did not record. They are secondary, **not** terms text, and each is `unverified (source not recorded)`. None is
used in the ranking; the follow-up must find and record real sources or read the primary terms.

### Ownership of outputs
Unverified. Open question: whether commercial use differs by plan, and whether the vendor keeps a licence on content
(`unverified (source not recorded)` for both). A vendor licence on our audio, if one exists, may conflict with the
project's rules on what leaves the machine; this must be read, not assumed. Per the issue, a free tier that leaves rights
with the vendor is out.

### Prompts, uploads and reference audio
Unverified. A claim of a training opt-out in account settings is `unverified (source not recorded)`. Retention and deletion: unverified.

### Voice cloning and content rules
Unverified. Claims that a consent confirmation precedes cloning, that professional cloning is self-voice only, and
that cloning public figures is prohibited are all `unverified (source not recorded)`. None quoted.

### API shape
Unverified. (Widely described as HTTP and WebSocket streaming with API-key auth; not read here.)

### Price and limits
Unverified: no figure recorded.

### Fit with the connector contract
The reference audio would leave the machine, and a voice-changer product, if it exists, would face the hosted-conversion
question under Resemble below (not an ML-14 fit). Nothing else can be said until the terms are read.

### Open questions
Output ownership and the vendor licence on content per plan; training default and opt-out; self-voice rule and consent
verification; disclosure and watermark rules; streaming protocol and billing unit; voice-changer licence and latency.

## Resemble AI (hosted voice conversion)

All `unverified`: `resemble.ai` was refused. Price figures seen in search results conflicted by about twelvefold; their titles and URLs were not recorded
(`unverified (source not recorded)`), so **no price is recorded**.

### Ownership of outputs
Unverified.

### Prompts, uploads and reference audio
Unverified.

### Voice cloning and content rules
Unverified. Conversion of a live voice is `Likeness::VoiceConversion`; the consent scope is `voice_conversion`
(`crates/omuzaki-ai-api/src/consent.rs`), and a hosted converter sends captured audio off the device, which the design
rejects for the performer (`ai-performer-voice-face.md` §3.7, SE-04).

### API shape
Unverified.

### Price and limits
Unverified (see above).

### Fit with the connector contract
Hosted conversion does not fit the performer path, and it does **not** fit the ML-14 slot either. ML-14 is the real-time
registry slot for model files run through the sandbox (`docs/product/02-studio-spec-v3.md` line 634), and
`docs/design/connectors.md` (lines 91-96) defines the reverse bridge only (a local registry slot exposed as a connector)
and rejects making hosted connectors registry runtimes. A hosted conversion network stream would therefore need its own
connector/audio integration design (consent record, a person's confirmation, latency and spend handling for a live audio
stream). That design does not exist; it is an open question for a follow-up, not a recommendation.

### Open questions
What connector/audio integration design a hosted conversion stream would need, given it cannot be an ML-14 registry
runtime; whether streaming conversion exists with a latency that fits a live mic; output ownership; whether the audio is retained
or trained on; whether anyone but the account holder can be the target voice.

## What local TTS cannot do (so we buy nothing we ship)

From the repository only (no vendor claim): ML-04 runs Kokoro-82M and Chatterbox through our own runtime
(`ai-performer-voice-face.md` §3.6), costs nothing per request, keeps text on the machine and has a speed control
(`tts@1`). A hosted voice should be bought only for what it adds:
- languages and voices beyond Kokoro's list (needs a vendor list read; unverified);
- expressive, prompt-steered delivery beyond `speed` and `expressiveness`;
- a machine with no GPU for Chatterbox-class quality;
- a consented clone of a person, if the vendor's flow is stronger than our `ConsentLookup`.

Stock voices at Google's Standard price (US$4 per 1M characters) are not worth buying: Kokoro costs 0.

## Comparison and recommendation

See the summary table above. Provisional ranking: Google Cloud first (the only verified price and the per-character
billing the `SpendGate` bound needs), conditional on the open questions. ElevenLabs is not ranked: its sources were not recorded, and it stays out until a follow-up records them. Voice conversion: no pick.

## Follow-up tasks I would file (not filed)

1. **Finish this note from an environment that allows the vendor hosts** (the list above): read and quote output
   ownership, data retention and training, cloning consent, content rules, API shape and price for all five candidates;
   re-verify Google's Instant custom voice terms.
2. **Update the licence rows** in `docs/licensing/deps/` (added by this PR, status `unverified`) once the primary terms
   are read, and move each to a register status: `yes`, `conditions` or `NO`. An adverse output-ownership or cloning finding stays `NO`; `unverified` remains until the primary terms are read.
3. **A hosted-TTS connector adapter** for the maintainer's pick, as its own task (an account, a key), per-character bound
   through `SpendGate`.
4. **A conversion rule for per-token prices** (Gemini-TTS style) if a token-billed voice is ever wanted.
5. **Design question (not a recommendation):** a hosted voice-conversion stream cannot be an ML-14 registry runtime
   (`docs/design/connectors.md` lines 91-96; `docs/product/02-studio-spec-v3.md` line 634). It would need its own
   connector/audio integration design, and only if a streaming service with acceptable terms is found.
