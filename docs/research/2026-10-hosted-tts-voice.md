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
candidates, a web-search result snippet (2026-10-15) may support a one-line description, marked `unverified (snippet)`, and
never a verdict. Every `unverified` item is a question for whoever finishes this note from an environment that allows
those hosts (the follow-up list at the end).

## Summary

| Candidate | Kind | Primary source read? | Price (dated) | Clone | First-adapter verdict |
|---|---|---|---|---|---|
| Google Cloud Text-to-Speech | large cloud TTS | partly (price, service terms) | Chirp 3 HD US$30 / 1M chars (verified) | Instant custom voice: price verified, terms `unverified` | candidate, pending output-ownership and cloning terms |
| Amazon Polly | large cloud TTS | no | `unverified (snippet)` | Brand Voice only, `unverified` | cannot rank |
| Azure AI Speech | large cloud TTS | no | `unverified` | Personal Voice, "limited access" `unverified (snippet)` | cannot rank |
| ElevenLabs | voice specialist, TTS + voice changer | no | `unverified` | self-voice rules `unverified (snippet)` | candidate, pending terms |
| Resemble AI | specialist, speech-to-speech | no | conflicting snippets, none usable | `unverified` | cannot rank (the voice-conversion candidate) |

**Ranked recommendation (provisional, at most two):** 1. Google Cloud, because it is the only candidate whose price is
verified, it bills per character as the `SpendGate` bound needs, and it needs no cloning for a stock-voice first
adapter. 2. ElevenLabs, as the voice specialist, **only after** its terms are read: its snippets describe a broad vendor
licence on content and a self-voice-only rule for professional cloning, both of which decide the verdict. Voice
conversion has no ranked pick: ML-14 ships empty and the hosted-conversion question needs a primary-source read first.
Neither pick may be adopted until the open items below are closed.

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
  SSML is supported. Streaming, sample rates, auth, seed and languages: unverified (the marketing text on the
  service-terms page says "220+ voices and 40+ languages", a vendor claim). Latency claims: none read.

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
- **Cap before each billable request:** fits for per-character voices: `bound = chars × price`; a chunk is at most 220
  chars, so Chirp 3 HD is at most 6,600 micro-dollars (US$0.0066) per chunk at the price above.
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
Unverified. A search snippet suggests custom voices are a managed "Brand Voice" engagement, not self-service
(`unverified (snippet)`).

### API shape
Unverified. Polly's SSML, speech marks and streaming support is widely documented but was not read here.

### Price and limits
Unverified. Search snippets for https://aws.amazon.com/polly/pricing/ (2026-10-15) disagree on the Neural rate (one
mentions a GovCloud rate), so no number is recorded.

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
Unverified. A search snippet says Personal Voice is a "limited access" feature that needs an application and is restricted
to approved use cases (`unverified (snippet)`). If true, it is not self-service for an open-source app and the
disclosure and consent rules apply.

### API shape
Unverified.

### Price and limits
Unverified. The price page's amounts did not appear in the snippet.

### Fit with the connector contract
Preset voices would fit if billed per character; clone fit depends on the limited-access answer.

### Open questions
Everything in the Google list, for Azure; plus the access-application terms for Personal Voice.

## ElevenLabs (voice specialist; TTS and voice changer)

All `unverified`: `elevenlabs.io` was refused. The snippets below describe third-party summaries and are **not** terms
text.

### Ownership of outputs
Unverified. Snippets (third-party blogs, 2026-10-15) say commercial use is not allowed on the free tier and is on paid
tiers, and that ElevenLabs keeps a broad licence on content. If a vendor licence on our audio exists, it may conflict
with the project's rules on what leaves the machine; this must be read, not assumed. Per the issue, a free tier that
leaves rights with the vendor is out.

### Prompts, uploads and reference audio
Unverified. A snippet mentions a "Data use" opt-out of training in account settings. Retention and deletion: unverified.

### Voice cloning and content rules
Unverified. Snippets say a consent confirmation precedes cloning, professional cloning is self-voice only, and cloning
public figures is prohibited with account suspension. None quoted.

### API shape
Unverified. (Widely described as HTTP and WebSocket streaming with API-key auth; not read here.)

### Price and limits
Unverified: no figure recorded.

### Fit with the connector contract
Strong on voice quality by repute and on a voice-changer product that bears on ML-14, but the reference audio leaves the
machine and a self-voice-only rule would make `Clone { person }` for anyone but the account holder a violation. Nothing
else can be said until the terms are read.

### Open questions
Output ownership and the vendor licence on content per plan; training default and opt-out; self-voice rule and consent
verification; disclosure and watermark rules; streaming protocol and billing unit; voice-changer licence and latency.

## Resemble AI (hosted voice conversion)

All `unverified`: `resemble.ai` was refused. Snippets from third-party price sites conflict by about twelvefold on the
speech-to-speech rate (one says US$0.0005 per second, others US$0.006), so **no price is recorded**.

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
Hosted conversion does not fit the performer path; it fits only ML-14's mic chain, with a person's confirmation and a
consent record. The design says the ML-14 slot ships empty and is bridged like the local TTS bridge.

### Open questions
Whether streaming conversion exists with a latency that fits a live mic; output ownership; whether the audio is retained
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
billing the `SpendGate` bound needs), ElevenLabs second (specialist), both conditional on the open questions. Voice
conversion: no pick.

## Follow-up tasks I would file (not filed)

1. **Finish this note from an environment that allows the vendor hosts** (the list above): read and quote output
   ownership, data retention and training, cloning consent, content rules, API shape and price for all five candidates;
   re-verify Google's Instant custom voice terms.
2. **Licence register rows** for each candidate (kind `service`, condition column with the ownership and cloning finding)
   once the quotes are in (this PR adds none; the issue's fence names `docs/licensing/register.md`, but the lane's
   conventions for this pass say findings live in the note).
3. **A hosted-TTS connector adapter** for the maintainer's pick, as its own task (an account, a key), per-character bound
   through `SpendGate`.
4. **A conversion rule for per-token prices** (Gemini-TTS style) if a token-billed voice is ever wanted.
5. **Hosted voice conversion for ML-14**, only if a streaming service with acceptable terms is found.
