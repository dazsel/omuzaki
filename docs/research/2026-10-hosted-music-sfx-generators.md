---
title: "Which hosted music and sound-effect generators should CN-11 support first? (2026-10-15)"
summary: "Compared ElevenLabs, Google Lyria, Stability AI, Mubert, Beatoven and Suno for bring-your-own-key music and SFX; ElevenLabs first (covers both kinds; music terms and ElevenAPI Terms read), Google Lyria second (music only, after its age clause and an OAuth contract gap are settled). All vendors are used with the user's own key, so each vendor's terms bind the user's account (Director ruling)."
area: research
---

# Which hosted music and sound-effect generators should CN-11 support first? (2026-10-15)

Research for #1470. Source marks: **verified** = I read the vendor's own text and quote it; **secondary** = a search summary or third-party page; **unverified** = no source reached. All pages were read on 2026-10-15.

**No account was created and no request was sent to any generation API.** The only requests were plain reads of public documents and web searches. This note is not legal advice.

## 1. The answer

- **Support ElevenLabs first (Director direction, approved by the owner).** It is the only candidate that covers both `Music` and `Sfx` through one key, with an API that returns audio in one call. Its music terms are public, long and specific, and I read them in full; the ElevenAPI Terms were read in the second pass and contain no SFX-specific clause (section 9, ElevenLabs Q7). It has a looping flag for SFX, a seed for music (only with a composition plan), and a training opt-out. A paid plan gives commercial use. Every vendor here is used with the user's own key and account, so the vendor's terms (plan tier, live-stream use, resale, Content ID, training opt-out) bind the user's account, and Omuzaki is the client, not a reseller (Director ruling, section 9). The UI links each vendor's terms where the key is entered. Because the service is the user's own account, the register rows in section 10 record the vendor terms as the user's conditions. This is stream music (CN-11), a later phase, so no build work is scheduled.
- **Support Google Lyria (Vertex AI, `lyria-002`) second, music only,** once its age clause and the OAuth gap are settled (section 7, item 7): the Google Cloud terms bar use in "a Customer Application ... likely to be accessed by individuals under the age of 18" (section 4). Under the bring-your-own-key ruling that clause binds the user's own Cloud project. Everything else about it is verified and clean: output is Customer Data, no training on it, fixed price, a seed.
- **Do not build Mubert, Beatoven or Suno now.**
  - Suno has no public self-serve API (secondary) and its terms limit commercial use to what its own conditions allow.
  - Mubert's API is a B2B product with a sublicensing plan and a no-Content-ID rule; the cheapest plan is $49 a month.
  - Beatoven owns the music and grants only a synchronisation licence with no streaming-service distribution (general Terms of Service, verified); no separate API terms are published.
- **Stability AI is a credible third** (its terms assign outputs to the customer, verified; the $1M revenue rule belongs to the downloadable-model licence, not the hosted terms, and the training opt-out is an account toggle, both from the second pass) but I could not read its audio API page (JavaScript only), so its shape and price stay unverified. Read it before building.
- **The risk that matters for streamers is not ownership, it is Content ID.** No vendor's terms promise that an AI track will not be matched or claimed. Only Mubert's terms say anything on registering (they forbid it). ElevenLabs is silent. Treat every generated track as claimable and keep provenance (section 8).
- **What this decides:** CN-11 ships one adapter pair (ElevenLabs music and SFX), a second music adapter (Lyria) once the OAuth gap and the age clause are settled, and a provenance record that carries plan tier, model and seed. Of the seven items in section 7, items 1 to 4 and 7 are connector-contract changes and block the adapters (item 7, Vertex OAuth, blocks only Lyria); item 5 is a contract addition that only adds a warning; item 6 (resale) is settled by the bring-your-own-key ruling and is not a contract change.

## 2. Summary table

| | ElevenLabs | Google Lyria (Vertex AI) | Stability AI | Mubert API | Beatoven.ai | Suno |
|---|---|---|---|---|---|---|
| Kinds | Music and SFX | Music only (instrumental) | Music and SFX (Stable Audio 3.0 family; secondary) | Music only | Music and SFX (marketing page) | Music (with vocals) |
| Public API | Yes | Yes | Yes (docs unreadable) | Yes (B2B plans) | Marketing page only | No (partner programme; secondary) |
| Output ownership | **verified**: customer retains rights, paid plans only | **verified**: output is Customer Data; Google claims no ownership | **verified**: assigns its rights in Outputs to the customer | **verified** (FAQ): Mubert owns copyright; you get a licence | **verified** (ToS 7.6): Beatoven owns it; you get a sync licence | **verified**: assignment, but commercial use gated by tier |
| Commercial use | Paid plans; no film, TV, radio or Studio Games below Enterprise | No tier limit in the terms; age clause | Terms silent on commercial use | Pro plan or API plan | Sync licence only (ToS 7.6) | Paid tier only |
| Streaming and VOD | Not named; "music streaming platform" distribution is barred on Free and Starter | Not named | Not named | Not named for live; YouTube allowed on Pro | **verified**: no distribution through digital service providers; live use not addressed | **unverified** |
| Content ID | Silent | Silent | Silent | **Forbidden** to register | **unverified** (secondary: forbidden) | **unverified** |
| Training on your prompts | Allowed, opt-out in account | Not without permission | Allowed, opt-out (account toggle for the Platform API) | Not addressed | **unverified** | Broad licence on Submissions |
| Seed | Music: yes, but not with `prompt`. SFX: none documented | Yes, but not with `sample_count` | Secondary only | Not seen | **unverified** | n/a |
| Price (stated, dated 2026-10-15) | Music $0.15 per minute; SFX $0.120 per minute or per generation (page contradicts itself) | $0.06 per 30 s (Lyria 2) | Unverified | $49 to $499 a month plans | Secondary only | n/a |
| Verdict | Support first | Support second, after the age clause and OAuth | Read the API, then decide | Not now | Not now | Not available |

## 3. Candidate: ElevenLabs (Music API and Sound Effects API)

### Ownership of outputs
- **verified** (Terms of Use section 4(c)(ii), https://elevenlabs.io/terms-of-use, 2026-10-15): "Except as expressly set forth herein, as between you and ElevenLabs, you retain all rights in and to your Output."
- **verified** (same, section 1(c)): "(i) if you access or use our Services free of charge (such a user, a 'Free User'), you may only use the Services for non-commercial purposes; (ii) if you access or use our Services through a paid subscription plan (such a user, a 'Paid User'), you may use the Services for commercial purposes". Free-tier output is out for us.
- **verified** (Music Terms, https://elevenlabs.io/music-terms, last updated 9 October 2026, section 2.2): "as between you and ElevenLabs, you retain any and all rights in and to your Input and Output".
- **verified** (Music Terms, Exhibit A, "Media Rights"): Free through Business: "All online and offline commercial use permitted, except film, TV, radio, and Studio Games". Only Enterprise Music has "All online and offline commercial use permitted". A "Studio Game" is "a video game or other interactive or playable experience made available to the public ... commercialized by sale, advertising, or any other form of monetization" (4.8(j)), with an Indie Game carve-out (under $500,000 lifetime revenue, 4.8(c)).
- **verified** (Exhibit A, "Eligibility Restrictions"): Starter, Creator and Pro are "For Individual Use Only"; Scale is for "Individuals, or entities or organizations with fewer than 10 employees"; Business "fewer than 50 employees". A streamer on their own key is an individual.
- Training data and indemnity: **verified absent from the Music Terms** (no indemnity clause; no training-data statement). **secondary**: ElevenLabs' music docs page says the model "is cleared for nearly all commercial uses" and was built with labels, publishers and artists (https://elevenlabs.io/docs/overview/capabilities/music, 2026-10-15). Third-party pages name Merlin and Kobalt (**secondary**, not checked). The Terms of Use (section 9) have the customer indemnify ElevenLabs, not the reverse.
- Output responsibility, **verified** (Music Terms 3.1): "you are responsible for reviewing Output for clearance before using, publishing, or distributing it". So the "cleared" marketing is not a promise in the contract.
- SFX: the Terms of Use apply (a paid plan is commercial). There are no separate SFX terms: the Service-Specific Terms page lists Music Terms, ElevenCreative Terms and ElevenAPI Terms, and the ElevenAPI Terms (https://elevenlabs.io/elevenapi-terms, read in the second pass) have no SFX clause (section 9, ElevenLabs Q7). Their API-resale wording is covered by the bring-your-own-key ruling in section 9.

### Streaming and claims
- **verified** (Music Terms 4.8(g)): "'Music Streaming Distribution' means making Output available on a music streaming platform, or authorizing or enabling any third party to do so". Exhibit A: Prohibited for Free and Starter; Permitted for Creator and above. A Twitch or YouTube broadcast is not named. It is probably not a "music streaming platform" (Spotify-style), but the text does not say. The user's plan and account terms govern this (section 9), and the UI links them where the key is entered.
- **verified** (4.4): "you are responsible for complying with that platform's policies on the disclosure of AI-generated content, including any labeling, metadata, or tagging requirements."
- **verified** (3.4): ElevenLabs "may mark Output as AI-generated but does not warrant that any Output carries a watermark or provenance signal". The compose API has an optional `sign_with_c2pa` flag (MP3 only).
- **Content ID: not addressed anywhere** in the Music Terms or Terms of Use (I searched both). There is no clause that forbids registering, and none that promises a track will not be matched. The Terms of Use section 7 is a takedown route for rights holders, not a claims policy.
- **verified** attribution (Exhibit A): Free "Required (denote Eleven Music when distributing)"; all paid plans "No Attribution Required".
- **verified** (4.2, 4.3): no Music Library (a collection of Output "made available to third parties ... to use, license, download") and no Model Resale ("offering any Music Model alongside third-party models") without written approval. Exhibit A marks both Prohibited on every self-serve plan. See the fit section: a Connectors page that offers ElevenLabs next to other vendors is only safe because the user holds the key. The definition of Reseller says an entity "does not become a Reseller solely by generating and distributing Output through its own product, provided it does not make the Music Models or API access to them available to any third party". Our host makes the API available to a user who brings their own key. Director ruling: the user holds the key and the account, so the vendor's terms bind the user, and Omuzaki is the client, not a reseller (section 9).
- **verified** (3.3): do not prompt for output that "replicates or mimics the voice, likeness, or identifiable characteristics of any recording artist".

### Prompts and uploads
- **verified** (Terms of Use 4(d)): ElevenLabs gets a licence to use Content "to provide the Services ..., to improve the Services, and to develop new services and products".
- **verified** (4(i)): "you may opt out of our use of your Content for training at any time by navigating to the 'Data use' menu in the 'Terms and Privacy' section of your ElevenLabs account." The opt-out is per account, in the web UI, and "does not affect any uses ... prior to that date". Our host cannot flip it by API. It is a setup step to show the user.
- Retention: no period is published for inputs or outputs (privacy policy section 6 gives 3 years only for voice data); a Zero Retention Mode exists through the account team (data-residency docs). See section 9, ElevenLabs Q6.
- Audio reference uploads and fine-tunes are screened for copyright (docs page, secondary-level summary). We do not need them in v1.

### API shape
- **Music, verified** (https://elevenlabs.io/docs/api-reference/music/compose, 2026-10-15): `POST https://api.elevenlabs.io/v1/music`, JSON body. Fields: `prompt` or `composition_plan` (not both); `music_length_ms` 3000 to 600000 (only with `prompt`); `model_id` one of `music_v1` (default), `music_v2`, `music_v2_5`; `seed` ("It isn't guaranteed to be reproducible, and it can't be used with `prompt`"); `force_instrumental`; `sign_with_c2pa`; query `output_format` (default `auto`, MP3 for v1 and v2). The 200 response is the audio file itself, with the song id in a response header. The page does not say "synchronous", but the file comes back in the response, so there is no job to poll.
- **Duration limits conflict:** the API reference says up to 600000 ms (10 minutes); the pricing page says "5 minute duration limit"; the docs page says 3 seconds to 5 minutes. Treat 5 minutes as the safe cap until a maintainer checks. **Stems and loops for music: none seen.**
- **SFX, verified** (https://elevenlabs.io/docs/api-reference/text-to-sound-effects/convert): `POST https://api.elevenlabs.io/v1/sound-generation`; `text` (required), `duration_seconds` (0.5 to 30 on the API reference; the capabilities page says 0.1 to 30), `loop` (only with `eleven_text_to_sound_v2`), `prompt_influence` 0 to 1, `model_id` (only `eleven_text_to_sound_v2`), `output_format`. Response: MP3 bytes. **No `seed` documented.**
- Auth: `xi-api-key` header (the SFX page examples). Regional hosts exist (US, EU, India, Singapore).

### Price and limits
- **verified** (https://elevenlabs.io/pricing/api, 2026-10-15): Music API "$0.15 per minute"; "API usage is billed in US dollars, not credits". Plans (monthly): Starter $6, Creator $22, Pro $99, Scale $299, Business $990.
- SFX: the pricing table shows "$0.120 per minute", while the FAQ says SFX are "billed per generation". The page does not settle it. The SFX capabilities page says "40 credits per second when duration is specified" (**verified**, https://elevenlabs.io/docs/overview/capabilities/sound-effects). The three statements do not reconcile; the cap code must treat the price as unknown until checked.
- **verified** (Music Terms Exhibit A): monthly generation limit 17 min (Starter), 62 (Creator), 304 (Pro), 1,100 (Scale), 4,800 (Business); concurrency 2 for Starter, Creator and Pro, 5 for Scale and Business; API access "No" on Free.
- Rate-limit numbers beyond concurrency: **unverified**.

### Fit with the connector contract
- **Fits:** blocking `run` (one POST returns the file). `Music` and `Sfx` both exist. `seconds` maps to `music_length_ms` or `duration_seconds`. `looping` maps to SFX `loop`. Output is MP3 or PCM bytes, so it becomes an `Output::Asset`.
- **Does not fit cleanly:**
  - `seed` on music is allowed only with `composition_plan`, not with a plain `prompt`. `check()` must fail a `prompt` request that sets `seed`, naming `seed`. Never drop it silently. A request with no seed records `seed: none` in provenance.
  - `seed` on SFX is not documented. `check()` must fail any SFX request that sets `seed`, naming `seed`.
  - `looping` on music has no equivalent. `check()` should reject `looping: true` with `kind: Music` for this adapter, naming `looping`.
  - `count` has no field. The adapter must make `count` separate calls, so the cap check runs `count` times (or once for the sum).
  - The price is per minute for music but unclear for SFX. The cap needs an estimated cost before each call. Use `seconds * 0.15 / 60` for music; for SFX there is no safe reading while the unit is unresolved: the per-minute and per-generation readings give no common per-call dollar amount, so the adapter must block SFX until a verified price or a user-set numeric upper bound exists (the contract needs a numeric reservation and `max_usd_micros`).
  - Concurrency 2 on most plans: the adapter should send `count` calls one at a time.
  - Model ids are an enum that changed within months (`music_v1`, `music_v2`, `music_v2_5`). Put `model_id` in `params` and declare the allowed values.

### Open questions
See section 9.

## 4. Candidate: Google Lyria on Vertex AI (`lyria-002`)

### Ownership of outputs
- **verified** (Google Cloud Service Specific Terms, section 20(a) "Generative AI Services", https://cloud.google.com/terms/service-terms, last modified October 12, 2026, read 2026-10-15): "'Generated Output' means the data or content generated by a Generative AI Service prompted by Customer Data. Generated Output is Customer Data. As between Customer and Google, Google does not assert any ownership rights in any new intellectual property created in the Generated Output."
- No tier limit on commercial use appears in these terms. This is the Vertex AI service only. The Gemini Developer API has separate terms and was not read.
- Training data and indemnity for Lyria: the Lyria API page says only that "Content safety filters, recitation checking, artist intent checks, and SynthID watermarking are applied" (https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/models/lyria-music-generation, last updated 2026-10-15). A statement on training data is **unverified**. Google's generated-output indemnity exists for some services (the repo's image-API note quotes it); whether it covers Lyria is answered in section 9 (Google Q2): the published indemnified-services list does not name Lyria.
- **Age clause, verified** (section 20(d)): "Customer will not, and will not allow End Users to, use a Generative AI Service as part of a website, Customer Application, or other online service that is directed towards or is likely to be accessed by individuals under the age of 18." A VTuber stream is "likely to be accessed" by minors on most platforms. This is the main risk (same finding as `docs/research/2026-10-hosted-image-apis.md`).

### Streaming and claims
- Nothing on live streams, VODs, Content ID or attribution in the Service Specific Terms (searched for each). **Not addressed.**
- **SynthID:** audio is watermarked (verified on the Lyria page). A watermark is inaudible and is not a Content ID registration, but it does mean Google can detect the origin. Effect on platform matching: **unverified**.

### Prompts and uploads
- **verified** (section 18, "Training Restriction"): "Google will not use Customer Data to train or fine-tune any AI/ML models without Customer's prior permission or instruction." Prompts are Customer Data. The retention clause quoted in the image note (no storage beyond what is needed) applies to Generative AI Services; I did not re-read it here. Abuse-monitoring logging: **unverified** for Lyria.
- No upload is needed for text-to-music.

### API shape
- **verified** (Lyria API page): `POST https://LOCATION-aiplatform.googleapis.com/v1/projects/PROJECT_ID/locations/LOCATION/publishers/google/models/lyria-002:predict`, bearer token from `gcloud` (OAuth, not a simple key). Instances: `prompt` (US English), `negative_prompt`, `seed`. Parameters: `sample_count`. "Cannot be used with sample_count" applies to `seed` both ways.
- Response: JSON, base64 WAV, 48 kHz, each clip 30 seconds, instrumental only. Synchronous.
- Lyria 3 (30 s clip) and Lyria 3 Pro (full song, up to 184 s) are in the pricing table (**verified**). The `lyria-002` page documents only `lyria-002`; Lyria 3 uses a different `interactions` endpoint (**verified**, section 9, Google Q3).
- No SFX, no loop flag, no stems.

### Price and limits
- **verified** (https://cloud.google.com/vertex-ai/generative-ai/pricing, 2026-10-15): "Lyria 3 Pro ... Full song $0.08 / 1 count"; "Lyria 3 ... 30 second music clip $0.04 / 1 count"; "Lyria 2 ... Music $0.06 / 1 count". The Lyria API page: "Lyria 2 usage is priced at $0.06 per 30 seconds of output music generated."
- Quotas: Lyria 3 and Lyria 3 Pro: "Regional online prediction requests per minute per base model : 10" (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/models/lyria/lyria-3, 2026-10-15). `lyria-002` quota: **unverified**.

### Fit with the connector contract
- **Fits:** blocking `run`; a seed; `count` (through `sample_count`); WAV output.
- **Does not fit:**
  - `kind: Sfx` is unsupported. `check()` must refuse it.
  - `seconds` is a fixed clip length, not a continuous value. A longer request must either be refused or built from several clips, and joined clips have audible seams. **The clip length is unresolved:** the model reference says each clip is 30 seconds (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/model-reference/lyria-music-generation, read 2026-10-16) and prices "per 30 seconds", but the generation page says "Each clip is 32.8 seconds long" (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/music/generate-music, read 2026-10-16). Do not hard-code either number in `check()`. The manifest declares a single fixed length and `check()` rejects any other `seconds`, naming `seconds`; which number to declare is settled by decoding the first real `lyria-002` WAV (the 48 kHz sample count gives the length) when the adapter is scheduled. Provenance records the decoded length next to the requested one. Billing is `count * 0.06` per the price page.
  - `seed` and `count > 1` are mutually exclusive. A request with both must fail `check()` naming `seed`; or the adapter spends `count` calls with seeds `seed + i`. The second needs a maintainer decision.
  - `looping`: unsupported.
  - Auth is a Google Cloud project, billing and OAuth. Connectors D3 models only a static key (`Header` or `Env`) or the LLM seam (`Provider`); it has no ADC, OAuth refresh or project credential. Lyria has no supported auth path until the contract decides this (section 7, item 7). The user must also be eligible under the age clause.

### Open questions
See section 9.

## 5. Candidate: Stability AI (Stable Audio, API)

### Ownership of outputs
- **verified** (Terms of Service, section 4(a), https://stability.ai/terms-of-service, effective September 30, 2026, read 2026-10-15): "Subject to your compliance with our Terms, we assign to you all of our right, title, and interest (if any) in the Outputs. So as between Stability and you, you own the Output (to the extent permitted by applicable law)."
- Commercial use: the Terms do not state a limit on commercial use of Outputs (searched). The $1M revenue threshold on the Stability AI License page (https://stability.ai/license) applies to the downloadable models ("Community ... less than $1M in annual revenue"; "Enterprise ... API providers, and businesses with annual revenue exceeding $1M"), not to the hosted API. The Terms of Service (effective September 30, 2026) set no revenue threshold, so the rule does not apply to the hosted API (section 9, Stability Q2).
- Training data, **verified** (marketing, https://stability.ai/stable-audio): "Commercially safe models, trained on fully licensed datasets. Legal indemnification provided under our Enterprise license." So indemnity is Enterprise only.
- User indemnity, **verified** (section 11): the customer indemnifies Stability.

### Streaming and claims
- Content ID, live stream, VOD, attribution: **not addressed** in the Terms (searched). Section 10 is the DMCA procedure.

### Prompts and uploads
- **verified** (4(c)): "we may use Content to improve and develop our Services (but you can opt-out to prevent us from using your Inputs and Outputs to train our models ...)". How to opt out: an account-level toggle "Training: Improve the Model for Everyone" for the Platform API at https://platform.stability.ai/account/overview, set to "No" (https://stability.ai/privacy-center, section 9, Stability Q3). It is not a per-request flag; show it as a setup step.

### API shape
- **unverified.** The API reference at `platform.stability.ai` is JavaScript only and returned no text. **secondary** (fal.ai and ComfyUI listings of Stable Audio 2.5 and 3): duration up to 190 s on 2.5, up to 380 s on 3.0 Medium; a `seed` field exists on ComfyUI's node; Stability's family now has a "Small SFX" model. Whether the first-party API is synchronous: **unverified**.

### Price and limits
- **unverified.** Third-party hosts show different numbers ($0.2 per generation on one, $0.0376 on another), which are those hosts' prices, not Stability's. Stability sells service credits (Terms: credits "are non-refundable", "expire one year after the date of purchase"). Credits per audio generation: Text to Audio "20" credits as of August 1, 2025 (https://stability.ai/api-pricing-update-25; may be out of date); the credit-to-dollar rate is **unverified**. Terms section 6 also bars making the API "available to any third party" except end users reaching it "solely through your own product or service" (see section 9, Stability Q2).

### Fit with the connector contract
- Cannot be judged until the API is read. If it takes `seconds` and `seed` and has an SFX model, it would fit both kinds and be the best match for `MusicRequest`.

### Open questions
See section 9.

## 6. Candidates not recommended now

### Mubert (Render and API)
- **Ownership, verified** (FAQ, https://mubert.com/render/faq, 2026-10-15): "All the copyright is owned by Mubert Inc." You get a licence. "in order to use Mubert music for commercial content and/or monetize posts – a Pro Plan is required."
- **Content ID, verified** (same FAQ): "It is strictly prohibited to register our music under Content ID systems." The pricing page (https://mubert.com/render/pricing) adds: "Tracks cannot be published on music streaming platforms".
- **Live streams:** the FAQ does not address Twitch or live use. The API page markets "Infinite music streams ... for IRL, gaming or creator streams" and "DMCA-free" (https://mubert.com/api). That is marketing, not a licence term.
- **Attribution:** required on the free plan ("it is obligatory to credit copyright to Mubert, wherever music is used").
- **API:** `POST https://music-api.mubert.com/api/v3/public/tracks` with `customer-id` and `access-token` headers; body has `duration`, `bitrate`, `format`, `intensity`, `mode` (**verified**, API page). Durations 15 seconds to 25 minutes. No seed or SFX seen.
- **Price, verified** (API page): Trial $49 a month (100 generations); Startup $199 (5,000); Startup+ $499 (30,000). These are B2B plans with sublicensing. A single streamer would hit "using tracks as background music in an app requires a Business Plan or an API subscription" (FAQ). Whether a streamer's own key under the Trial plan allows live-stream use: **unverified**.
- Why not now: music only, no seed, fixed monthly plans, a high entry price, and the licence is a licence, not ownership. Revisit if a streamer-friendly plan appears.

### Beatoven.ai
- The API page (https://www.beatoven.ai/api) markets a "Music Generation API" and an "SFX Generation API" and says "it's always cleared for commercial use". Terms page (`/terms-and-conditions`) returned 404 and I did not find the right URL. The general Terms of Service are at https://www.beatoven.ai/tos (last update 5 June 2024): Beatoven owns the music and licenses it for use synchronised with your content, with no distribution through streaming services (section 9, Beatoven Q1). No separate API terms are published. Content ID is not mentioned in the general Terms; API terms, prompts, API shape and price: **unverified**. **Secondary** (aggregator pages, not checked): Content ID registration forbidden; about $3 per download minute.
- Why not now: the verified general terms give a synchronisation-only licence that bars streaming-service distribution, and the API-specific terms, shape, price and Content ID policy are unpublished or unverified. Re-run when API terms are published.

### Suno
- **No public API: secondary** (a July 2026 trade-press report says Suno opened an early API partner programme to a curated group, with no published docs or pricing). Unofficial wrappers exist and break the Terms.
- **verified** (Terms of Service, https://suno.com/terms, 2026-10-15): "Free or Basic Tier Accounts: ... you will only use such Outputs for your lawful, personal and non-commercial purposes." "Commercial Use Restriction : Except as permitted in the next section, you agree not to ... use or exploit ... any Output ... for any commercial purposes." The Terms also bar "data mining, robots, scraping, or similar data gathering".
- Why not: no sanctioned API, so any connector would break the Terms.

### Udio
- No official API found (**secondary**). Terms not read. **unverified**; out.

## 7. Contract gaps this research found

Seven items. Items 1 to 5 and 7 are connector-contract changes for the maintainers (each a place where `MusicRequest` or the connector contract does not match the vendors); item 6 is a legal point settled by the bring-your-own-key ruling.

1. **`seed` is optional and often unavailable.** ElevenLabs music allows it only with `composition_plan`; ElevenLabs SFX has none; Lyria forbids it with `count > 1`. The provenance record needs `seed: Option<u64>` plus a `seed_note`, and `check()` needs a rule per adapter.
2. **`seconds` is not continuous.** Lyria is a fixed 30 seconds. The manifest should declare `seconds` min, max and step (or a fixed list) so `check()` fails with `Invalid` naming `seconds` for every value but the declared clip length (30 nominal; the page conflict is in section 4).
3. **`looping` applies to SFX only** in the one API that has it. The manifest should say which kinds support `looping`.
4. **Cost is per second or per clip and sometimes unknown.** The cap check before each billable request needs `estimate(request) -> Money` with an `unknown` state that blocks the call. Confirming an unknown estimate is not enough: a call needs a numeric reservation, so it waits for a verified price or a user-set numeric upper bound.
5. **Output rights depend on the plan tier,** not just the vendor. Provenance should record `plan_tier` if the vendor exposes it (ElevenLabs' API exposes the subscription; I did not verify which endpoint). If it cannot be read, show a warning on the Connectors page naming the clauses (Free = non-commercial; Starter = no music-streaming distribution).
6. **A "no resale" clause** (ElevenLabs 4.3). Settled by Director ruling, not a contract change: with the user's own key and account the vendor's terms bind the user and Omuzaki is the client, not a reseller. A vendor is asked only if we ever proxy or bundle a key.
7. **Vertex AI auth is OAuth.** Connectors D3 has `Header`, `Env` and `Provider` delivery only; no ADC, token refresh or project id. Needs a contract decision (a new `Deliver` variant or a host-side token broker) before any Lyria adapter.

## 8. Content-ID risk, in plain words

- No hosted generator promises that its output will not be matched. Generators produce similar tracks for similar prompts, and a rights holder's matcher can flag a similar recording.
- A streamer should: keep the prompt, model, seed (if any), date, vendor, plan and the generated file (the provenance record); avoid prompts that name artists or songs (ElevenLabs bans mimicry, 3.3); and never register generated tracks in a Content ID system (Mubert forbids it; the others are silent).
- For live streams, prefer short SFX and stingers, which are rarely matched, over long music beds.
- Platform AI-disclosure rules apply (ElevenLabs 4.4 says the user must comply). The host should offer the C2PA flag where the API has one.

## 9. Vendor questions: the ruling and the second-pass findings

**Director ruling (2026-10-16): the vendor questions are settled for now.** Every vendor here is used with the user's own key and account, so the vendor's terms (plan tier, live-stream use, resale, Content ID, training opt-out) bind the user's account, and Omuzaki is the client, not a reseller. We contact no vendor now. We document each vendor's terms at primary sources (below), and the UI links each vendor's terms where the key is entered. A vendor is asked only if we ever proxy or bundle a key. The questions below are kept as the record of what the public text does and does not say; "open" means the public text is silent, not that a maintainer must ask.

Second pass, 2026-10-15: each question was checked against the vendor's own published pages (no account, no API call). **Answered** gives the clause or page and its URL. **Open: ask the vendor** means the public text does not settle it. The tally is at the end of the section.

**ElevenLabs**
1. Is a live broadcast on Twitch or YouTube, or a VOD of it, "Music Streaming Distribution"? Is Starter enough for a monetised stream?
   - **Open: ask the vendor.** Music Terms 4.8(g): "making Output available on a music streaming platform, or authorizing or enabling any third party to do so"; 4.4 speaks of "any streaming platform, digital service provider, or content-hosting service" but names no live-streaming service and says nothing on VODs (https://elevenlabs.io/music-terms, read 2026-10-15).
2. Does a desktop app where each user pastes their own key count as "Model Resale" or make the app a "Reseller"?
   - **Open: ask the vendor.** Music Terms 4.3: "You must not engage in, or permit any third party to engage in, Model Resale"; 4.8(e) includes "making available of Music Models through your or any third-party platform or application". ElevenAPI Terms Exhibit 3: you "shall not resell, repackage, redistribute, sublicense, or otherwise make API access to the Music Service available to any third party" (https://elevenlabs.io/elevenapi-terms, 2026-10-15). A bring-your-own-key app is not addressed in either text.
3. Do music and SFX output carry a Content ID or fingerprint registration, and will ElevenLabs support a disputed claim?
   - **Open: ask the vendor.** "Content ID" and fingerprinting do not appear in the Music Terms, the ElevenAPI Terms, the Terms of Use, the music or SFX docs pages, or the pricing page.
4. What is the SFX price: per minute, per generation, or 40 credits per second? What is the true maximum music length (5 or 10 minutes)?
   - **Open: ask the vendor.** Pricing page (https://elevenlabs.io/pricing/api, 2026-10-15) still says both "$0.12" with the label "Price per minute" and "Music and Sound Effects are billed per generation". The SFX docs say "Cost: 40 credits per second when duration is specified." (https://elevenlabs.io/docs/overview/capabilities/sound-effects). Music length: the API reference says music_length_ms "Must be between 3000ms and 600000ms" (https://elevenlabs.io/docs/api-reference/music/compose); the capabilities page says "a maximum duration of 5 minutes" (https://elevenlabs.io/docs/overview/capabilities/music) and the pricing page "5 minute duration limit". The conflict stays; keep 5 minutes as the cap.
5. Can `seed` be used with `prompt`? Does SFX accept `seed`?
   - **Answered: no to both.** Music compose reference: seed "Cannot be used in conjunction with prompt." The SFX convert reference lists `text`, `loop`, `duration_seconds`, `prompt_influence`, `model_id` and no seed field (https://elevenlabs.io/docs/api-reference/text-to-sound-effects/convert, 2026-10-15). The seed is also "not guaranteed" to reproduce output.
6. Is there an API or account-level way to turn off training, and what is the retention period?
   - **Answered in part.** Opt-out is account-level in the web UI only (Terms of Use 4(i), quoted in section 3); no API switch is documented. Retention: no period is published for inputs or outputs; the privacy policy gives only "We won't keep data we generate about your voice for longer than 3 years after your last interaction with us" (https://elevenlabs.io/privacy-policy, section 6). A Zero Retention Mode exists, enabled by contacting the account team (https://elevenlabs.io/docs/overview/administration/data-residency). **Retention period: open, ask the vendor.**
7. Do the ElevenAPI Terms add limits for SFX?
   - **Answered: no.** The ElevenAPI Terms have no SFX clause; their only service exhibit for a specific model is Exhibit 3, which covers music. They add the general rules that credentials stay inside your organisation (2.1) and that you answer for "each Customer Application, including for authenticating and authorizing its users" (2.3), and that ElevenLabs "need not retain results" (3.2).
8. Does ElevenLabs offer indemnity on any self-serve plan?
   - **Answered: none published.** No indemnity by ElevenLabs appears in the Music Terms, the ElevenAPI Terms or the pricing page; the pricing page lists only Enterprise "Custom terms & assurance around DPA/SLAs". Whether Enterprise adds one is a sales question.

**Google Cloud**
1. Is a streamer-owned app that has no under-18 targeting but could be viewed by minors "likely to be accessed by individuals under the age of 18"?
   - **Open: ask the vendor.** Service Specific Terms 20(d) (re-read 2026-10-15, last modified October 12, 2026): "directed towards or is likely to be accessed by individuals under the age of 18". The text has no test or example (https://cloud.google.com/terms/service-terms).
2. Does the generated-output indemnity cover `lyria-002` and Lyria 3?
   - **Answered: no, not on the published list.** The indemnity applies to a "Generative AI Indemnified Service ... listed at https://cloud.google.com/terms/generative-ai-indemnified-services" (Service Terms 20(i)). That list (last modified July 20, 2026) names the Vertex API "used with generally available versions of these foundation models: Codey, Gemini, Imagen, PaLM, Veo". Lyria is not named, and Lyria 3 is Preview. A sales contract could still add it.
3. What is the request shape and clip length of Lyria 3 and Lyria 3 Pro?
   - **Answered.** `POST https://aiplatform.googleapis.com/v1beta1/projects/PROJECT_ID/locations/global/interactions` with a JSON body holding `"model"` (`lyria-3-clip-preview` or `lyria-3-pro-preview`) and `"input"` items (text, optional image); the reply carries an `audio/mpeg` item (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/music/generate-music). Specs (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/models/lyria/lyria-3): Pro "Maximum audio clip length: 184 seconds", Clip "30 seconds"; "Maximum number of clips per prompt: 1"; MP3, 44.1 kHz, 192 kbps; quota 10 requests per minute per base model; both Preview. No seed field is documented for Lyria 3. This is a different endpoint and shape from `lyria-002:predict`, so the adapter needs two paths, and the fixed-clip-length rule in section 4 holds for `lyria-002` and Lyria 3 Clip only.
4. Can SynthID cause a third-party platform to flag a track?
   - **Open: ask the vendor.** The Lyria pages only say "SynthID watermarking [is] applied" (https://docs.cloud.google.com/vertex-ai/generative-ai/docs/model-reference/lyria-music-generation). They say nothing on platform detection.

**Stability AI**
1. Request and response shape of the Stable Audio endpoints, maximum duration, sync or job, seed, credits per second.
   - **Open: ask the vendor (or read the OpenAPI file in a browser).** `platform.stability.ai` renders only with JavaScript and returned no text to a plain fetch. Public text found: Text to Audio "20" credits, effective August 1, 2025 (https://stability.ai/api-pricing-update-25; may be out of date); "Stable Audio 3.0 Large is available via the Stability AI API" and tracks "up to six minutes in length" (https://stability.ai/stable-audio). Seed, sync or job and the per-second rate are not published there.
2. Does the hosted API sit under the $1M revenue rule, or is commercial use allowed on credits?
   - **Answered: the $1M rule is for the downloadable-model licence, not the API terms.** The licence page offers "Community: researchers, developers, small businesses, and creators with less than $1M in annual revenue" and "Enterprise: enterprise, API providers, and businesses with annual revenue exceeding $1M" (https://stability.ai/license). The Terms of Service (effective September 30, 2026) set no revenue threshold and sell service credits (section 7(d)). New finding: section 6 bars anyone from "mak[ing] the Services, Software, or API, or access to the Services, Software, or API, available to any third party, except that you may permit your end users to access and use functionality enabled by the Services solely through your own product or service, as an integrated component" (https://stability.ai/terms-of-service). A bring-your-own-key app passes the user's own key, so the user is the customer; that fits better than ElevenLabs, but a maintainer should still confirm.
3. How does a user apply the training opt-out to API traffic?
   - **Answered.** "For Platform API: Log in ... Navigate to https://platform.stability.ai/account/overview. View the toggle 'Training: Improve the Model for Everyone' and toggle to 'No.'" (https://stability.ai/privacy-center, 2026-10-15). It is an account toggle; there is no per-request flag. Show it as a setup step.

**Mubert**
1. Does a Trial or Startup plan cover a streamer's live broadcast and VOD, and does the plan allow a desktop app that uses the customer's own token?
   - **Open: ask the vendor.** The Render licence covers "Monetized YouTube, TikTok and Instagram videos" and "apps and services on the Business plan", and says nothing on live broadcasts (https://mubert.com/render/license). The API pricing page lists Trial as "For shipping your first integration" and its FAQ says the Trial plan "is there for testing and early validation, while sublicensing comes in when you move to a real product setup" (https://mubert.com/api). The API terms of use were not found as a page; the footer links a "Terms of Use" that did not load as text.
2. Will Mubert offer a seed and an SFX model?
   - **Answered for today: neither.** The API documentation (https://mubert.com/api/docs) has no seed parameter and no SFX endpoint. Future plans are not published.

**Beatoven**
1. Where are the API terms; who owns the output; is Content ID registration allowed?
   - **Answered in part.** No API terms page is published; the API page links the dashboard and the general Terms of Service (https://www.beatoven.ai/api, https://www.beatoven.ai/tos, last update 5 June 2024). Ownership (7.6): "as between you and Beatoven, Beatoven shall own and be the copyright owner of Your AI Music but you shall control the exercise of your rights pursuant to the License". The licence lets you exploit the music "solely ... to the extent it is ... synchronized with Your Content", and bars distribution "via any digital service provider (e.g., Spotify, Apple Music, etc.)" and use "to train any artificial intelligence tool". Content ID is not mentioned. **API terms and whether they differ: open, ask the vendor.**

**Tally.** Answered: ElevenLabs 4 of 8 (Q5, Q6 opt-out, Q7, Q8), Google 2 of 4 (Q2, Q3), Stability 2 of 3 (Q2, Q3), Mubert 1 of 2 (Q2), Beatoven 0 of 1 in full (ownership answered, API terms not). Still open in the public text (not blocking, per the ruling): ElevenLabs Q1, Q2, Q3, Q4 and the Q6 retention period; Google Q1, Q4; Stability Q1; Mubert Q1; Beatoven Q1 (API terms). The brief counted 19 questions; the note has 18.

## 10. Proposed licence-register rows

The issue asks for register rows. `docs/licensing/register.md` is generated from one TOML file per item in `docs/licensing/deps/`, and this task is docs-only in `docs/research/`, so the rows are given here for the person who adds the files. Kind is `service`. Status stays `unverified` until the quote is checked at the source by a maintainer, as policy requires; the quotes above are the evidence. Under the bring-your-own-key ruling the condition column is what the user's account must satisfy.

| Item | Kind | Status | Condition (output ownership finding) |
|---|---|---|---|
| ElevenLabs Music and Sound Effects API | service | unverified | Paid plan only; "you retain all rights in and to your Output" (Terms 4(c)(ii)); Free is non-commercial (1(c)); no film, TV, radio or Studio Games below Enterprise; no music-streaming distribution on Free and Starter; no Model Resale; no indemnity found |
| Google Lyria on Vertex AI (`lyria-002`) | service | unverified | "Generated Output is Customer Data"; Google "does not assert any ownership rights" (Service Terms 20(a)); no training on Customer Data (18); age clause 20(d) binds the user's Cloud project; indemnity for Lyria unverified |
| Stability AI Stable Audio API | service | unverified | Stability "assign[s] to you all of our right, title, and interest (if any) in the Outputs" (ToS 4(a)); no revenue threshold in the hosted terms; training opt-out is an account toggle; user indemnifies Stability; no reselling the API to third parties except end users through your own product (ToS section 6); API reference unreadable (JavaScript only) |
| Mubert API | service | unverified | "All the copyright is owned by Mubert Inc."; licence only; Pro or API plan for commercial; Content ID registration prohibited |
| Beatoven.ai API | service | unverified | "Beatoven shall own and be the copyright owner of Your AI Music" (ToS 7.6); licence is sync-only, no streaming-service distribution; API terms not published |
| Suno | service | unverified | No public API; Free or Basic: personal non-commercial only; commercial use only as Suno's conditions permit |

## 11. Follow-up tasks

1. Read the Stability audio API reference (a browser-rendered read of `platform.stability.ai` or its OpenAPI file) and update section 5.
2. Show each vendor's terms link beside the key entry in the Connectors UI (per the section 9 ruling); no vendor is contacted unless a key is ever proxied or bundled.
3. Add the six register files under `docs/licensing/deps/` (section 10).
4. Decide the contract gaps in section 7 (including OAuth), then write the `mock-music` cases that cover: seed rejected where unsupported, fixed 30 s duration, SFX-only loop and unknown price blocking.
5. Build the ElevenLabs adapter (music and SFX) behind the cap check, with the Connectors page showing the plan-tier warning.
6. Re-check Beatoven and Suno in one month; both are likely to change.
