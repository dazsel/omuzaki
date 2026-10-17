---
title: "Hosted text-to-speech and voice conversion for CN-10 (2026-10-15)"
summary: "Research for CN-10 on hosted TTS and voice-conversion services, now read at each vendor's own pages (2026-10-15): Google Cloud first, ElevenLabs second on conditions, Polly third; Azure prices unreadable; Resemble withdrew voice generation."
area: research
---

# Hosted text-to-speech and voice conversion for CN-10 (2026-10-15)

> Research pass for #1469 (parent #575, CN-10; design `docs/design/connectors.md` §3 D14 and
> `docs/design/ai-performer-voice-face.md` §3.5, §3.6). Finished at the primary sources for #1906; see "Verification status".

**No account was made, no request was sent to any vendor API, nothing was spent and no voice was uploaded.** Only public
pages were read, on 2026-10-15 (first pass) and again on 2026-10-15 from the local lane (second pass, #1906).

## Verification status

The first pass ran in a cloud session whose network policy refused the vendor hosts. The second pass (#1906) ran from the
local lane with open web access and read the vendors' own pages. **Every quote below is from a vendor's own page, read
2026-10-15**; no third-party page is used. Source marks: **verified** = read and quoted at the primary page;
**not published** = the vendor's public text does not answer it (ask the vendor); **unreadable** = the page needs a browser
(script-rendered) and returned no text to a plain read.

What stays open after the second pass:
- Azure AI Speech **prices** (the price page renders `$-` without script) and Azure Speech **output ownership** (the Product
  Terms clause "Microsoft does not own Customer's Output Content" sits under "Microsoft Generative AI Services", and I could
  not confirm that Azure Speech text to speech is in that definition).
- Output licence for stock voices on live streams, per vendor: no vendor names live streams.
- Whether Google's Instant custom voice has the Gemini Live restrictions: the docs page does not say; access is by sales
  contact only.

## Summary

| Candidate | Kind | Primary source read? | Price (dated 2026-10-15) | Clone | First-adapter verdict |
|---|---|---|---|---|---|
| Google Cloud Text-to-Speech | large cloud TTS | yes (terms, price, docs) | Chirp 3 HD US$30 / 1M chars | Instant custom voice: allow-listed via sales; owner-consent script required | 1st: candidate, pending the allow-list answer for cloning |
| ElevenLabs | voice specialist, TTS + voice changer | yes (terms, policy, pricing, docs) | v4 US$0.08 / 1K chars (US$80 / 1M); v4 Turbo and v3 Conversational US$0.04 / 1K | Instant (Starter+) and Professional (Creator+); self-voice only for Professional | 2nd, on conditions: paid plan, voice licence to ElevenLabs, training opt-out in the web UI |
| Amazon Polly | large cloud TTS | yes (pricing, FAQ, service terms, quotas) | Neural US$16 / 1M, Generative US$30 / 1M, Standard US$4 / 1M | No self-service clone (Brand Voice by engagement) | 3rd: AWS may train on Polly input unless an Organizations opt-out is set |
| Azure AI Speech | large cloud TTS | partly (docs, limited-access, privacy; price page unreadable) | not published as text | Personal Voice: Limited Access, registration and approval | not ranked: price and output terms unconfirmed |
| Resemble AI | specialist | yes | none | n/a | out: "We've stopped offering voice generation as a commercial product" |

**Ranked recommendation (second pass):** 1. Google Cloud: verified per-character price at the same US$30 per 1M as Polly
Generative but with a stated no-logging data posture ("Google does not log any customer Cloud TTS text or audio data") and
output that is Customer Data owned by the customer. 2. ElevenLabs: the richest API (HTTP stream, WebSocket, seed, voice
changer) and clear cloning rules, but US$40 to US$80 per 1M characters, a free tier that is non-commercial, a content
licence in its favour, and no indemnity. 3. Amazon Polly: cheapest per character and tags are not billed, but it trains on
input by default and has no self-service clone. Azure is not ranked until its price is read in a browser. Resemble is
withdrawn. Voice conversion: ElevenLabs Voice Changer exists (US$0.12 per minute, 5-minute segments), but a hosted
conversion stream still does not fit ML-14 (see the voice conversion section); no pick for the performer path. The pick may
not be adopted until the open items above are closed.

## Google Cloud Text-to-Speech

### Ownership of outputs
- **Verified** (Google Cloud Terms of Service, https://cloud.google.com/terms, "Last modified October 12, 2026", 5.1, read
  2026-10-15): "As between the parties, Customer retains all Intellectual Property Rights in Customer Data and Customer
  Applications". "Customer Data" includes "data that Customer or End Users derive from that data through their use of the
  Services" (definition, GCP Services), so the audio we synthesise is Customer Data.
- The Service Specific Terms (https://cloud.google.com/terms/service-terms, same date) have no Text-to-Speech section; the
  only voice text there is for the Gemini Live API (next section). For Gemini-TTS, the Generative AI clause 20(a) applies:
  "Generated Output is Customer Data. As between Customer and Google, Google does not assert any ownership rights in any new
  intellectual property created in the Generated Output."
- Commercial use and live streaming: **not published**. No Cloud TTS page names live streams, attribution or performer
  rights for stock voices; nothing found restricts them either.

### Prompts, uploads and reference audio
- **Verified** (https://docs.cloud.google.com/text-to-speech/docs/data-logging, 2026-10-15): "Cloud Text-to-Speech is both
  stateless and resourceless" and "Google does not log any customer Cloud TTS text or audio data."
- **Verified** (Cloud Terms 5.2): "Google will only access, use, and otherwise process Customer Data in accordance with the
  Cloud Data Processing Addendum and will not access, use, or process Customer Data for any other purpose." For
  Generative AI Services (Gemini-TTS) the Service Terms add: "Google will not use Customer Data to train or fine-tune any
  AI/ML models without Customer's prior permission or instruction."
- Region options: **not read** for TTS; the Chirp 3 HD page lists regional availability and is the place to read them.

### Voice cloning and content rules
- **Verified** (https://docs.cloud.google.com/text-to-speech/docs/chirp3-instant-custom-voice, 2026-10-15): "Important:
  Access to Instant Custom Voice is restricted to allow-listed users. To request access, contact a member of the sales
  team." So it is not self-service.
- **Verified** (same page): the caller records a consent statement, up to 10 seconds, in the same environment as the
  reference audio: "I am the owner of this voice, and I consent to Google using this voice to create a synthetic voice
  model." and "You must use the provided consent statement script for your chosen language" (no custom script). Reference
  audio is also up to 10 seconds. Limits: 10 voice cloning keys per minute per project.
- For Gemini Live custom voices, the service terms say, read 2026-10-15 at https://cloud.google.com/terms/service-terms:
  > "The functionality of creating custom voices and avatars in Gemini Live API (the "Custom Voice and Avatar
  > Functionality") is available only to Customers approved by Google."

  > "Customer will not use any image, audio recording, voice sample, or other data depicting or derived from any
  > individual(s) under the age of 18 with the Custom Voice and Avatar Functionality"

  The same clause bars Customer from "(a) integrat[ing] any Custom Voice and Avatar Functionality into any End User-facing
  platforms, tools, or applications (including Customer Applications) and/or (b) permit[ting] End Users of Customer
  Applications to access or use any Custom Voice and Avatar Functionality." That is the Gemini Live API, not Cloud TTS.
  The Instant custom voice page does not repeat these limits and does not say they do not apply: **not published**, ask
  Google when requesting the allow-list. The age clause 20(d) (no use "as part of a website, Customer Application, or other
  online service that is directed towards or is likely to be accessed by individuals under the age of 18") applies to
  Generative AI Services such as Gemini-TTS.
- AI-voice disclosure, watermark: **not published** on the Cloud TTS pages read.

### API shape
- **Verified** (https://docs.cloud.google.com/text-to-speech/docs/chirp3-hd, 2026-10-15): a streaming method and a batch
  method. Output formats: streaming "ALAW, MULAW, OGG_OPUS and PCM"; batch "ALAW, MULAW, MP3, OGG_OPUS and PCM"; default
  LINEAR16. Auth is Application Default Credentials (the page's sample says "set up Application Default Credentials"), so the
  Google Cloud auth gap in `docs/research/2026-10-hosted-music-sfx-generators.md` section 7 applies here too.
- **Verified** SSML for Chirp 3 HD is Preview and "not currently supported for streaming requests"; supported tags include
  `<speak>`, `<say-as>`, `<p>`, `<s>`, `<phoneme>`, `<sub>`, `<break>`, `<audio>` and `<prosody>`; "Any tags that are not on
  this list will be ignored".
- Billing is by characters sent, and "All Speech Synthesis Markup Language (SSML) tags (except the `<mark>` tag) are also
  included in the character count" (pricing page).
- Seed, voice count and languages: **not read**; no seed field was seen on the pages read.
- **Verified** limits (https://docs.cloud.google.com/text-to-speech/quotas): Chirp3RequestsPerMinutePerProject 200;
  ChirpVoiceCloningRequestsPerMinutePerProject 30; ConcurrentStreamingSessionsPerProject 100;
  GenerateVoiceCloningKeyRequestsPerMinutePerProject 10; Gemini-TTS `gemini-2.5-flash-tts` 150 QPM, `gemini-2.5-pro-tts` 125 QPM.

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
(§3.6) without a conversion rule. Rate limits: see the API shape list above.

### Fit with the connector contract
- **Cap before each billable request:** fits for per-character voices, but the bound is computed from the **complete
  serialized input sent to Google**, not from the 220-character text chunk. The pricing page (read 2026-10-15) counts
  spaces, newlines and every SSML tag except `<mark>`, so the adapter builds the final request string first and then
  reserves: `bound = billable_chars(serialized_input) × price_per_char`, where `billable_chars` counts every character of
  the string (text after XML escaping, all tags and attributes, all whitespace) minus any `<mark>` tags (leaving them in
  only over-reserves). Escaping counts: `&` becomes `&amp;` (5 characters). Whether Google counts code points or bytes is
  **not published**; count the larger of the two. Worked example, Chirp 3 HD at US$0.00003 per character: a
  220-character chunk wrapped as `<speak><prosody rate="slow">…</prosody></speak>` adds 7 + 21 + 10 + 8 = 46 characters, so
  the serialized input is 266 characters and `bound = 266 × 0.00003 = US$0.00798` (7,980 micro-dollars), not the
  6,600 micro-dollars that `220 × price` would give. The `SpendGate` reservation uses 7,980.
- **Blocking `run`, whole chunks only:** fits; `ai-performer-voice-face.md` §3.1 wants whole chunks, no streamed synthesis.
- **`Preset { id }`:** fits if the catalog lists the voice names (the voices page lists them; not read here). **`Clone { reference, person }`:** would
  need the Instant custom voice consent flow and the answer above; the reference audio leaves the machine, so a person
  confirms (§3.6), and `ConsentLookup::find(person, Likeness::VoiceClone)` must return a record first.
- Provenance is host-built from the response; no watermark is documented on the pages read.

### Open questions
1. Is commercial and live-stream use of Chirp 3 HD stock-voice audio permitted? (No clause either way.)
2. Instant custom voice: does the Gemini Live "End User-facing platforms" restriction apply, and what does allow-listing
   require for a bring-your-own-project desktop app? Ask Google sales.
3. Region options and characters counted as code points or bytes.

## Amazon Polly

Read 2026-10-15 at primary pages: AWS Service Terms (https://aws.amazon.com/service-terms/, "Last Updated: October 13,
2026"), the Polly pricing page (https://aws.amazon.com/polly/pricing/), the Polly FAQ (https://aws.amazon.com/polly/faqs/)
and the Polly quotas page (https://docs.aws.amazon.com/polly/latest/dg/limits.html).

### Ownership of outputs
- **Verified** (Service Terms 50.2): "The output that you generate using AI Services is Your Content." Polly is on the AI
  Services list (50.1).
- Stock-voice licence, commercial use: the FAQ says of static prompts and mass notification "The service does not restrict
  this and there are no additional costs for doing so." Live streaming and attribution are **not published**.

### Prompts, uploads and reference audio
- **Verified** (Service Terms 50.3): "for ... Amazon Polly ...: (a) we may use and store AI Content that is processed by each
  of the foregoing AI Services to develop and improve the applicable AI Service and its underlying technologies; (b) we may
  use and store AI Content that is not personal data to develop and improve AWS and affiliate machine-learning and
  artificial-intelligence technologies". So input text is used for training **by default**.
- **Verified** opt-out (50.3): "by configuring an AI services opt-out policy using AWS Organizations"; for access through
  AWS Builder ID or a third-party sign-in, "by using the opt-out mechanism indicated in the applicable service
  documentation". A bring-your-own-account streamer must set this up; the host cannot set it per request.

### Voice cloning and content rules
- **Verified** (FAQ): Brand Voice is by engagement: "please reach out to your AWS Account Manager or contact us"; "Every
  voice is unique". No self-service clone exists on the pages read.

### API shape
- **Verified** (https://docs.aws.amazon.com/polly/latest/dg/limits.html): `SynthesizeSpeech` input "up to 3000 billed
  characters (6000 total characters). SSML tags are not counted as billed characters."; "The output audio stream (synthesis)
  is limited to 10 minutes"; default quota for standard voices is 80 transactions per second per region per account; up to
  five lexicons per request. SSML is supported. Streaming, seed and sample-rate details: **not read** (the AWS SDK reference
  is the place).

### Price and limits
Read 2026-10-15 at https://aws.amazon.com/polly/pricing/: "You are billed monthly for the number of characters of text that
you processed." Standard US$4.00, Neural US$16.00, Long-Form US$100.00 and Generative US$30 per 1 million characters
(outside the free tier; GovCloud is higher). The FAQ: "You will be charged for every request for speech or Speech Marks
based on the number of characters you send to the service."

### Fit with the connector contract
Per-character billing fits the cap bound, and because SSML tags are not billed the bound can use the plain text length
(over-reserving by counting tags is safe). Cloning has no self-service path. The default training use is a conflict with the
rule that a person's audio and text stay private unless the user opts out; the host must warn.

### Open questions
1. Live-stream use of Polly audio (no clause either way).
2. Whether the Organizations opt-out is available to an individual bring-your-own-key account.

## Azure AI Speech

Read 2026-10-15: the Speech quotas page (https://learn.microsoft.com/en-us/azure/ai-services/speech-service/speech-services-quotas-and-limits),
the text to speech data, privacy and security page (https://learn.microsoft.com/en-us/azure/ai-foundry/responsible-ai/speech-service/text-to-speech/data-privacy-security,
updated 2026-02-27), the Limited Access page (same folder, `limited-access`), the Personal Voice overview and the Microsoft
Product Terms for Online Services (https://www.microsoft.com/licensing/terms/product/ForOnlineServices/all).

### Ownership of outputs
- **Not answered for Speech.** The Product Terms say, under "Microsoft Generative AI Services", "Microsoft does not own
  Customer's Output Content", and that Output Content "is Customer Data". The page lists no Azure AI Speech section and does
  not define the term in the text read, so whether text to speech falls under it is **not confirmed**. Ask Microsoft.

### Prompts, uploads and reference audio
- **Verified** (data, privacy and security page): "Neither input text nor output audio content is stored in Microsoft logs"
  for prebuilt neural voice. For Personal Voice: "Microsoft may independently retain a copy of personal voice models for as
  long as necessary", and "Personal voice doesn't support BYOS" (bring your own storage). Training-data deletion by the
  customer is available through the API.

### Voice cloning and content rules
- **Verified** (Limited Access page): "Personal voice ... is a Limited Access feature and requires registration and
  approval. Customers must register to access the demo and/or the API for business use." So it is not self-service for an
  open-source app.
- **Verified** (Personal Voice overview, updated 2026-09-16): "it's required that every voice be created with explicit
  consent from the user. A recorded statement from the user is required"; the voice is made from "a 5-90-second audio
  prompt".

### API shape
- **Verified** (quotas page): real-time TTS "Maximum audio length produced per request: 10 minutes"; Standard (S0) "30
  transactions per second (TPS) (default value)", adjustable to 1,000; Free (F0) "20 transactions per 60 seconds";
  "Maximum total number of distinct <voice> and <audio> tags in SSML: 50". The pricing footnote states "speech synthesis
  usage is billed per character". Seed, formats, streaming protocol: **not read**.

### Price and limits
- **Unreadable.** The price page (https://azure.microsoft.com/en-us/pricing/details/cognitive-services/speech-services/)
  shows "$- per 1M characters" without script, for every voice class. It does state "Text to Speech (per character
  billing) Neural 0.5 million characters free per month". Read the live page in a browser to record the numbers.

### Fit with the connector contract
Preset voices would fit per-character billing. Personal Voice does not fit a self-service app (approval needed).

### Open questions
1. The per-character prices (browser read).
2. Whether text to speech output falls under the Generative AI output clause.
3. Approval criteria for Personal Voice for an open-source desktop app.

## ElevenLabs (voice specialist; TTS and voice changer)

Read 2026-10-15 at primary pages: Terms of Service non-EEA (https://elevenlabs.io/terms-of-use, "Last Updated: 09 October
2026"), the Prohibited Use Policy (https://elevenlabs.io/use-policy), the API pricing page (https://elevenlabs.io/pricing/api),
the pricing page (https://elevenlabs.io/pricing) and the docs.

### Ownership of outputs
- **Verified** (Terms of Use 4(c)(ii)): "Except as expressly set forth herein, as between you and ElevenLabs, you retain all
  rights in and to your Output."
- **Verified** (1(c)): free users "may only use the Services for non-commercial purposes"; paid subscription users "may use
  the Services for commercial purposes". The pricing page: Starter adds "Commercial License" and "Instant Voice Cloning";
  Creator adds "Professional Voice Cloning". A free tier is out, as the issue required.
- **Verified** (4(d)): ElevenLabs gets "a license to use, reproduce, modify, adapt, publish, translate, create derivative
  works from, distribute, publicly or otherwise perform and display, and use your Content to provide the Services ..., to
  improve the Services, and to develop new services and products", and, "to the extent that your Content includes your
  voice, ... [to] use your voice, and other indicia of your persona". This is a wide licence over what we send, which
  must be shown to the user before any reference audio is uploaded.

### Prompts, uploads and reference audio
- **Verified** (4(i)): "you may opt out of our use of your Content for training at any time by navigating to the 'Data use'
  menu in the 'Terms and Privacy' section of your ElevenLabs account." It does "not affect any uses ... prior to that date"
  and is web-UI only.
- **Verified** (TTS API reference, https://elevenlabs.io/docs/api-reference/text-to-speech/convert): `enable_logging=false`
  uses zero retention mode, which "may only be used by enterprise customers".
- **Verified** (privacy policy section 6): the only retention figure is "We won't keep data we generate about your voice for
  longer than 3 years after your last interaction with us".

### Voice cloning and content rules
- **Verified** (Prohibited Use Policy section 3): no "Cloning, synthesizing, or impersonating the voice, identity, or likeness
  of any living individual without their explicit and informed consent or other valid authorization". Section 1: "You may not
  submit voice recordings of anyone under 18 to create or train a voice model."
- **Verified** (Instant Voice Cloning docs): "confirm that you have the right and consent to clone the voice" (a checkbox).
  Professional: "For now, we only allow you to clone your own voice. You will be asked to go through a verification process".
- **Verified** (Policy section 10, AI Transparency): "You must not remove, alter, or conceal ElevenLabs watermarks, metadata
  or other provenance signals for the purpose of disguising AI-generated content".

### API shape
- **Verified** (API reference, 2026-10-15): `POST /v1/text-to-speech/:voice_id` (audio returned), `/stream`, and a WebSocket
  `wss://api.elevenlabs.io/v1/text-to-speech/:voice_id/stream-input`; auth is the `xi-api-key` header; `output_format` such as
  `mp3_44100_128` (MP3 at 192 kbps needs Creator or above; 44.1 kHz PCM needs Pro or above); `seed` integer 0 to 4294967295
  with "best effort to sample deterministically ... Determinism is not guaranteed"; `enable_ssml_parsing` default false.
- **Verified** (voice changer docs): "Maximum segment length: 5 minutes"; "Billing: 1,000 characters per minute of processed
  audio".

### Price and limits
- **Verified** (https://elevenlabs.io/pricing/api): v4 Text to Speech "$0.08 Price per 1K characters"; v4 Turbo and v3
  Conversational "$0.04"; v3 and v2 Multilingual "$0.08"; Voice Changer "$0.12 Price per minute". v3 has a "5,000 character
  limit", v2 Multilingual "10,000 character limit". Plans (monthly): Starter $6, Creator $22, Pro $99, Scale $299, Business
  $990.
- No indemnity by ElevenLabs on any self-serve plan (not in the pages read; Enterprise has "Custom terms & assurance around
  DPA/SLAs").

### Fit with the connector contract
Per-character price fits the cap bound (US$80 per 1M characters at v4). Whole-chunk synthesis fits the blocking `run`
(`/v1/text-to-speech/:voice_id`); `seed` fits as best effort. Reference audio would leave the machine under the 4(d)
licence, so a person confirms (§3.6) and `ConsentLookup` must return a record first. The voice changer is file-based
(5-minute segments), so it is not a live-microphone path.

### Open questions
Live-stream use of stock voices; whether the 4(d) persona licence can be narrowed for a clone; Enterprise-only zero
retention for a bring-your-own-key streamer; whether the API plan allows an app where each user holds the key (see
`docs/research/2026-10-hosted-music-sfx-generators.md` section 9, ElevenLabs Q2 for the music-side wording).

## Resemble AI (hosted voice conversion)

Read 2026-10-15: https://www.resemble.ai/speech-to-speech/, the pricing page (https://www.resemble.ai/pricing/) and the
Terms of Service (https://www.resemble.ai/terms-of-service, "Last Update: August 14, 2026").

**Withdrawn.** The speech-to-speech page now reads: "This product is no longer available. We've stopped offering voice
generation as a commercial product and are now focused on AI detection, watermarking and multimodal security. The voice
models we built remain open and free to use for research and development." The pricing page lists only detection plans
(Flex, Team, Business). So Resemble cannot be a hosted voice-conversion candidate. The earlier open questions on its price,
ownership and streaming are moot.

For the record, its Terms of Service keep "Voice cloning consent. Resemble AI may require consent from the individual or third
party whose voice is being cloned. Unless otherwise stated by Resemble AI, that consent must be verbal and captured in the
form we specify."

### Fit with the connector contract
Hosted conversion never fit the performer path, and it does **not** fit the ML-14 slot either. ML-14 is the real-time
registry slot for model files run through the sandbox (`docs/product/02-studio-spec-v3.md` line 634), and
`docs/design/connectors.md` (lines 91-96) defines the reverse bridge only (a local registry slot exposed as a connector)
and rejects making hosted connectors registry runtimes. A hosted conversion network stream would need its own
connector/audio integration design (consent record, a person's confirmation, latency and spend handling for a live audio
stream). That design does not exist; it is an open question for a follow-up, not a recommendation. The only hosted
converter found with a public price is ElevenLabs Voice Changer (US$0.12 per minute, file segments up to 5 minutes).

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

See the summary table above. Ranking after the second pass: Google Cloud first (verified per-character price, no-logging
data posture, output is Customer Data), ElevenLabs second on conditions (paid plan; wide content licence; training opt-out
in the web UI only; no indemnity), Amazon Polly third (default training use with an Organizations opt-out; no clone).
Azure is not ranked until its prices are read in a browser. Resemble is withdrawn. Voice conversion: no pick for the
performer path.

## Follow-up tasks I would file (not filed)

1. **Read the Azure price page in a browser** and record the Neural, Neural HD and Personal Voice per-character prices;
   ask Microsoft whether text to speech output is covered by the Generative AI output clause.
2. **Update the licence rows** in `docs/licensing/deps/` (status `unverified`) from this note's quotes and move each to a
   register status: `yes`, `conditions` or `NO`. A maintainer checks the quotes at the source first, as policy requires.
3. **A hosted-TTS connector adapter** for the maintainer's pick, as its own task (an account, a key), per-character bound
   through `SpendGate`.
4. **A conversion rule for per-token prices** (Gemini-TTS style) if a token-billed voice is ever wanted.
5. **Design question (not a recommendation):** a hosted voice-conversion stream cannot be an ML-14 registry runtime
   (`docs/design/connectors.md` lines 91-96; `docs/product/02-studio-spec-v3.md` line 634). It would need its own
   connector/audio integration design, and only if a streaming service with acceptable terms is found.
