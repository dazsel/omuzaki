---
title: "Which hosted music and sound-effect generators should CN-11 support first? (2026-10-15)"
summary: "Compared ElevenLabs, Google Lyria, Stability AI, Mubert, Beatoven and Suno for bring-your-own-key music and SFX; ElevenLabs first, conditional on the ElevenAPI Terms and resale question (covers both kinds; music terms read in full), Google Lyria second (music only, conditional on one clause and an OAuth contract gap)."
area: research
---

# Which hosted music and sound-effect generators should CN-11 support first? (2026-10-15)

Research for #1470. Source marks: **verified** = I read the vendor's own text and quote it; **secondary** = a search summary or third-party page; **unverified** = no source reached. All pages were read on 2026-10-15.

**No account was created and no request was sent to any generation API.** The only requests were plain reads of public documents and web searches. This note is not legal advice.

## 1. The answer

- **Support ElevenLabs first, on condition.** It is the only candidate that covers both `Music` and `Sfx` through one key, with an API that returns audio in one call. Its music terms are public, long and specific, and I read them in full. It has a looping flag for SFX, a seed for music, and a training opt-out. A paid plan gives commercial use. The condition: the ElevenAPI Terms (SFX limits) were not read and the Model Resale question (section 9) is open, so the register row stays `unverified`, which `docs/licensing/policy.md` treats as `NO`. CN-11 may build and test the adapter, but nothing ships on a shippable path until a maintainer clears both. This is a legal call for the owner.
- **Support Google Lyria (Vertex AI, `lyria-002`) second, music only,** and only after a maintainer settles one clause and the OAuth gap (section 7, item 7): the Google Cloud terms bar use in "a Customer Application ... likely to be accessed by individuals under the age of 18" (section 3). Everything else about it is verified and clean: output is Customer Data, no training on it, fixed price, a seed.
- **Do not build Mubert, Beatoven or Suno now.**
  - Suno has no public self-serve API (secondary) and its terms limit commercial use to what its own conditions allow.
  - Mubert's API is a B2B product with a sublicensing plan and a no-Content-ID rule; the cheapest plan is $49 a month.
  - Beatoven's API terms could not be reached.
- **Stability AI is a credible third** (its terms assign outputs to the customer, verified) but I could not read its audio API page (JavaScript only), so its shape and price stay unverified. Read it before building.
- **The risk that matters for streamers is not ownership, it is Content ID.** No vendor's terms promise that an AI track will not be matched or claimed. Only Mubert's terms say anything on registering (they forbid it). ElevenLabs is silent. Treat every generated track as claimable and keep provenance (section 8).
- **What this decides:** CN-11 ships one adapter pair (ElevenLabs music and SFX), a second music adapter (Lyria) behind the age-clause answer, and a provenance record that carries plan tier, model and seed. Seven items in section 7 need the maintainers: items 1 to 5 and 7 change the connector contract; item 6 (resale) is a legal question for the owner. Items 1 to 4 and 7 block the adapters; 5 only adds a warning.

## 2. Summary table

| | ElevenLabs | Google Lyria (Vertex AI) | Stability AI | Mubert API | Beatoven.ai | Suno |
|---|---|---|---|---|---|---|
| Kinds | Music and SFX | Music only (instrumental) | Music and SFX (Stable Audio 3.0 family; secondary) | Music only | Music and SFX (marketing page) | Music (with vocals) |
| Public API | Yes | Yes | Yes (docs unreadable) | Yes (B2B plans) | Marketing page only | No (partner programme; secondary) |
| Output ownership | **verified**: customer retains rights, paid plans only | **verified**: output is Customer Data; Google claims no ownership | **verified**: assigns its rights in Outputs to the customer | **verified** (FAQ): Mubert owns copyright; you get a licence | **unverified** | **verified**: assignment, but commercial use gated by tier |
| Commercial use | Paid plans; no film, TV, radio or Studio Games below Enterprise | No tier limit in the terms; age clause | Terms silent on commercial use | Pro plan or API plan | **unverified** | Paid tier only |
| Streaming and VOD | Not named; "music streaming platform" distribution is barred on Free and Starter | Not named | Not named | Not named for live; YouTube allowed on Pro | **unverified** | **unverified** |
| Content ID | Silent | Silent | Silent | **Forbidden** to register | **unverified** (secondary: forbidden) | **unverified** |
| Training on your prompts | Allowed, opt-out in account | Not without permission | Allowed, opt-out | Not addressed | **unverified** | Broad licence on Submissions |
| Seed | Music: yes, but not with `prompt`. SFX: none documented | Yes, but not with `sample_count` | Secondary only | Not seen | **unverified** | n/a |
| Price (stated, dated 2026-10-15) | Music $0.15 per minute; SFX $0.120 per minute or per generation (page contradicts itself) | $0.06 per 30 s (Lyria 2) | Unverified | $49 to $499 a month plans | Secondary only | n/a |
| Verdict | Support first | Support second, conditional | Read the API, then decide | Not now | Not now | Not available |

## 3. Candidate: ElevenLabs (Music API and Sound Effects API)

### Ownership of outputs
- **verified** (Terms of Use section 4(c)(ii), https://elevenlabs.io/terms-of-use, 2026-10-15): "Except as expressly set forth herein, as between you and ElevenLabs, you retain all rights in and to your Output."
- **verified** (same, section 1(c)): "(i) if you access or use our Services free of charge (such a user, a 'Free User'), you may only use the Services for non-commercial purposes; (ii) if you access or use our Services through a paid subscription plan (such a user, a 'Paid User'), you may use the Services for commercial purposes". Free-tier output is out for us.
- **verified** (Music Terms, https://elevenlabs.io/music-terms, last updated 9 October 2026, section 2.2): "as between you and ElevenLabs, you retain any and all rights in and to your Input and Output".
- **verified** (Music Terms, Exhibit A, "Media Rights"): Free through Business: "All online and offline commercial use permitted, except film, TV, radio, and Studio Games". Only Enterprise Music has "All online and offline commercial use permitted". A "Studio Game" is "a video game or other interactive or playable experience made available to the public ... commercialized by sale, advertising, or any other form of monetization" (4.8(j)), with an Indie Game carve-out (under $500,000 lifetime revenue, 4.8(c)).
- **verified** (Exhibit A, "Eligibility Restrictions"): Starter, Creator and Pro are "For Individual Use Only"; Scale is for "Individuals, or entities or organizations with fewer than 10 employees"; Business "fewer than 50 employees". A streamer on their own key is an individual.
- Training data and indemnity: **verified absent from the Music Terms** (no indemnity clause; no training-data statement). **secondary**: ElevenLabs' music docs page says the model "is cleared for nearly all commercial uses" and was built with labels, publishers and artists (https://elevenlabs.io/docs/overview/capabilities/music, 2026-10-15). Third-party pages name Merlin and Kobalt (**secondary**, not checked). The Terms of Use (section 9) have the customer indemnify ElevenLabs, not the reverse.
- Output responsibility, **verified** (Music Terms 3.1): "you are responsible for reviewing Output for clearance before using, publishing, or distributing it". So the "cleared" marketing is not a promise in the contract.
- SFX: the Terms of Use apply (a paid plan is commercial). I found no separate SFX terms (the Service-Specific Terms page lists Music Terms, ElevenCreative Terms and ElevenAPI Terms; I did not open the other two). Whether the ElevenAPI Terms add limits on SFX or on API resale is **unverified**.

### Streaming and claims
- **verified** (Music Terms 4.8(g)): "'Music Streaming Distribution' means making Output available on a music streaming platform, or authorizing or enabling any third party to do so". Exhibit A: Prohibited for Free and Starter; Permitted for Creator and above. A Twitch or YouTube broadcast is not named. It is probably not a "music streaming platform" (Spotify-style), but the text does not say. Ask the vendor (section 9).
- **verified** (4.4): "you are responsible for complying with that platform's policies on the disclosure of AI-generated content, including any labeling, metadata, or tagging requirements."
- **verified** (3.4): ElevenLabs "may mark Output as AI-generated but does not warrant that any Output carries a watermark or provenance signal". The compose API has an optional `sign_with_c2pa` flag (MP3 only).
- **Content ID: not addressed anywhere** in the Music Terms or Terms of Use (I searched both). There is no clause that forbids registering, and none that promises a track will not be matched. The Terms of Use section 7 is a takedown route for rights holders, not a claims policy.
- **verified** attribution (Exhibit A): Free "Required (denote Eleven Music when distributing)"; all paid plans "No Attribution Required".
- **verified** (4.2, 4.3): no Music Library (a collection of Output "made available to third parties ... to use, license, download") and no Model Resale ("offering any Music Model alongside third-party models") without written approval. Exhibit A marks both Prohibited on every self-serve plan. See the fit section: a Connectors page that offers ElevenLabs next to other vendors is only safe because the user holds the key. The definition of Reseller says an entity "does not become a Reseller solely by generating and distributing Output through its own product, provided it does not make the Music Models or API access to them available to any third party". Our host makes the API available to a user who brings their own key. Whether that counts is a question for ElevenLabs.
- **verified** (3.3): do not prompt for output that "replicates or mimics the voice, likeness, or identifiable characteristics of any recording artist".

### Prompts and uploads
- **verified** (Terms of Use 4(d)): ElevenLabs gets a licence to use Content "to provide the Services ..., to improve the Services, and to develop new services and products".
- **verified** (4(i)): "you may opt out of our use of your Content for training at any time by navigating to the 'Data use' menu in the 'Terms and Privacy' section of your ElevenLabs account." The opt-out is per account, in the web UI, and "does not affect any uses ... prior to that date". Our host cannot flip it by API. It is a setup step to show the user.
- Retention period and zero-retention mode: **unverified** (the music docs page does not address them).
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
- Training data and indemnity for Lyria: the Lyria API page says only that "Content safety filters, recitation checking, artist intent checks, and SynthID watermarking are applied" (https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/models/lyria-music-generation, last updated 2026-10-15). A statement on training data is **unverified**. Google's generated-output indemnity exists for some services (the repo's image-API note quotes it); whether it covers Lyria is **unverified**.
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
- Lyria 3 (30 s clip) and Lyria 3 Pro (full song) are in the pricing table (**verified**), but the API page documents only `lyria-002`; their request shape is **unverified**.
- No SFX, no loop flag, no stems.

### Price and limits
- **verified** (https://cloud.google.com/vertex-ai/generative-ai/pricing, 2026-10-15): "Lyria 3 Pro ... Full song $0.08 / 1 count"; "Lyria 3 ... 30 second music clip $0.04 / 1 count"; "Lyria 2 ... Music $0.06 / 1 count". The Lyria API page: "Lyria 2 usage is priced at $0.06 per 30 seconds of output music generated."
- Quotas: **unverified**.

### Fit with the connector contract
- **Fits:** blocking `run`; a seed; `count` (through `sample_count`); WAV output.
- **Does not fit:**
  - `kind: Sfx` is unsupported. `check()` must refuse it.
  - `seconds` is fixed at 30 per clip. A longer request must either be refused or built from several clips, and joined clips have audible seams. Recommend `check()` require `seconds == 30` exactly (shorter and longer both fail, naming `seconds`) unless the contract defines rounding, and bill `count * 0.06`.
  - `seed` and `count > 1` are mutually exclusive. A request with both must fail `check()` naming `seed`; or the adapter spends `count` calls with seeds `seed + i`. The second needs a maintainer decision.
  - `looping`: unsupported.
  - Auth is a Google Cloud project, billing and OAuth. Connectors D3 models only a static key (`Header` or `Env`) or the LLM seam (`Provider`); it has no ADC, OAuth refresh or project credential. Lyria has no supported auth path until the contract decides this (section 7, item 7). The user must also be eligible under the age clause.

### Open questions
See section 9.

## 5. Candidate: Stability AI (Stable Audio, API)

### Ownership of outputs
- **verified** (Terms of Service, section 4(a), https://stability.ai/terms-of-service, effective September 30, 2026, read 2026-10-15): "Subject to your compliance with our Terms, we assign to you all of our right, title, and interest (if any) in the Outputs. So as between Stability and you, you own the Output (to the extent permitted by applicable law)."
- Commercial use: the Terms do not state a limit on commercial use of Outputs (searched). The $1M revenue threshold on the Stability AI License page (https://stability.ai/license) applies to the downloadable models ("Community ... less than $1M in annual revenue"; "Enterprise ... API providers, and businesses with annual revenue exceeding $1M"), not to the hosted API. Which applies to a streamer using the API is **unverified**.
- Training data, **verified** (marketing, https://stability.ai/stable-audio): "Commercially safe models, trained on fully licensed datasets. Legal indemnification provided under our Enterprise license." So indemnity is Enterprise only.
- User indemnity, **verified** (section 11): the customer indemnifies Stability.

### Streaming and claims
- Content ID, live stream, VOD, attribution: **not addressed** in the Terms (searched). Section 10 is the DMCA procedure.

### Prompts and uploads
- **verified** (4(c)): "we may use Content to improve and develop our Services (but you can opt-out to prevent us from using your Inputs and Outputs to train our models ...)". How to opt out through the API key: **unverified**.

### API shape
- **unverified.** The API reference at `platform.stability.ai` is JavaScript only and returned no text. **secondary** (fal.ai and ComfyUI listings of Stable Audio 2.5 and 3): duration up to 190 s on 2.5, up to 380 s on 3.0 Medium; a `seed` field exists on ComfyUI's node; Stability's family now has a "Small SFX" model. Whether the first-party API is synchronous: **unverified**.

### Price and limits
- **unverified.** Third-party hosts show different numbers ($0.2 per generation on one, $0.0376 on another), which are those hosts' prices, not Stability's. Stability sells service credits (Terms: credits "are non-refundable", "expire one year after the date of purchase"). Credits per audio generation: **unverified**.

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
- The API page (https://www.beatoven.ai/api) markets a "Music Generation API" and an "SFX Generation API" and says "it's always cleared for commercial use". Terms page (`/terms-and-conditions`) returned 404 and I did not find the right URL. **All of ownership, streaming, Content ID, prompts, API shape and price: unverified.** **Secondary** (aggregator pages, not checked): Content ID registration forbidden; about $3 per download minute.
- Why not now: nothing verifiable. Re-run when its terms are located.

### Suno
- **No public API: secondary** (a July 2026 trade-press report says Suno opened an early API partner programme to a curated group, with no published docs or pricing). Unofficial wrappers exist and break the Terms.
- **verified** (Terms of Service, https://suno.com/terms, 2026-10-15): "Free or Basic Tier Accounts: ... you will only use such Outputs for your lawful, personal and non-commercial purposes." "Commercial Use Restriction : Except as permitted in the next section, you agree not to ... use or exploit ... any Output ... for any commercial purposes." The Terms also bar "data mining, robots, scraping, or similar data gathering".
- Why not: no sanctioned API, so any connector would break the Terms.

### Udio
- No official API found (**secondary**). Terms not read. **unverified**; out.

## 7. Contract gaps this research found

These are for the maintainers; each is a place where `MusicRequest` or the connector contract does not match the vendors.

1. **`seed` is optional and often unavailable.** ElevenLabs music allows it only with `composition_plan`; ElevenLabs SFX has none; Lyria forbids it with `count > 1`. The provenance record needs `seed: Option<u64>` plus a `seed_note`, and `check()` needs a rule per adapter.
2. **`seconds` is not continuous.** Lyria is a fixed 30 seconds. The manifest should declare `seconds` min, max and step (or a fixed list) so `check()` fails with `Invalid` naming `seconds` for every value but 30.
3. **`looping` applies to SFX only** in the one API that has it. The manifest should say which kinds support `looping`.
4. **Cost is per second or per clip and sometimes unknown.** The cap check before each billable request needs `estimate(request) -> Money` with an `unknown` state that blocks the call. Confirming an unknown estimate is not enough: a call needs a numeric reservation, so it waits for a verified price or a user-set numeric upper bound.
5. **Output rights depend on the plan tier,** not just the vendor. Provenance should record `plan_tier` if the vendor exposes it (ElevenLabs' API exposes the subscription; I did not verify which endpoint). If it cannot be read, show a warning on the Connectors page naming the clauses (Free = non-commercial; Starter = no music-streaming distribution).
6. **A "no resale" clause** (ElevenLabs 4.3) may apply to a host that lists several music vendors. Legal question, not a contract change. Open for the owner: get written confirmation before shipping.
7. **Vertex AI auth is OAuth.** Connectors D3 has `Header`, `Env` and `Provider` delivery only; no ADC, token refresh or project id. Needs a contract decision (a new `Deliver` variant or a host-side token broker) before any Lyria adapter.

## 8. Content-ID risk, in plain words

- No hosted generator promises that its output will not be matched. Generators produce similar tracks for similar prompts, and a rights holder's matcher can flag a similar recording.
- A streamer should: keep the prompt, model, seed (if any), date, vendor, plan and the generated file (the provenance record); avoid prompts that name artists or songs (ElevenLabs bans mimicry, 3.3); and never register generated tracks in a Content ID system (Mubert forbids it; the others are silent).
- For live streams, prefer short SFX and stingers, which are rarely matched, over long music beds.
- Platform AI-disclosure rules apply (ElevenLabs 4.4 says the user must comply). The host should offer the C2PA flag where the API has one.

## 9. Questions a maintainer must ask the vendors

**ElevenLabs**
1. Is a live broadcast on Twitch or YouTube, or a VOD of it, "Music Streaming Distribution"? Is Starter enough for a monetised stream?
2. Does a desktop app where each user pastes their own key count as "Model Resale" or make the app a "Reseller"?
3. Do music and SFX output carry a Content ID or fingerprint registration, and will ElevenLabs support a disputed claim?
4. What is the SFX price: per minute, per generation, or 40 credits per second? What is the true maximum music length (5 or 10 minutes)?
5. Can `seed` be used with `prompt`? Does SFX accept `seed`?
6. Is there an API or account-level way to turn off training, and what is the retention period?
7. Do the ElevenAPI Terms add limits for SFX?
8. Does ElevenLabs offer indemnity on any self-serve plan?

**Google Cloud**
1. Is a streamer-owned app that has no under-18 targeting but could be viewed by minors "likely to be accessed by individuals under the age of 18"?
2. Does the generated-output indemnity cover `lyria-002` and Lyria 3?
3. What is the request shape and clip length of Lyria 3 and Lyria 3 Pro?
4. Can SynthID cause a third-party platform to flag a track?

**Stability AI**
1. Request and response shape of the Stable Audio endpoints, maximum duration, sync or job, seed, credits per second.
2. Does the hosted API sit under the $1M revenue rule, or is commercial use allowed on credits?
3. How does a user apply the training opt-out to API traffic?

**Mubert**
1. Does a Trial or Startup plan cover a streamer's live broadcast and VOD, and does the plan allow a desktop app that uses the customer's own token?
2. Will Mubert offer a seed and an SFX model?

**Beatoven**
1. Where are the API terms; who owns the output; is Content ID registration allowed?

## 10. Proposed licence-register rows

The issue asks for register rows. `docs/licensing/register.md` is generated from one TOML file per item in `docs/licensing/deps/`, and this task is docs-only in `docs/research/`, so the rows are given here for the person who adds the files. Kind is `service`. Status stays `unverified` until the quote is checked at the source by a maintainer, as policy requires; the quotes above are the evidence.

| Item | Kind | Status | Condition (output ownership finding) |
|---|---|---|---|
| ElevenLabs Music and Sound Effects API | service | unverified | Paid plan only; "you retain all rights in and to your Output" (Terms 4(c)(ii)); Free is non-commercial (1(c)); no film, TV, radio or Studio Games below Enterprise; no music-streaming distribution on Free and Starter; no Model Resale; no indemnity found |
| Google Lyria on Vertex AI (`lyria-002`) | service | unverified | "Generated Output is Customer Data"; Google "does not assert any ownership rights" (Service Terms 20(a)); no training on Customer Data (18); age clause 20(d) unresolved; indemnity for Lyria unverified |
| Stability AI Stable Audio API | service | unverified | Stability "assign[s] to you all of our right, title, and interest (if any) in the Outputs" (ToS 4(a)); training opt-out; user indemnifies Stability; API terms and price unread |
| Mubert API | service | unverified | "All the copyright is owned by Mubert Inc."; licence only; Pro or API plan for commercial; Content ID registration prohibited |
| Beatoven.ai API | service | unverified | Terms not located; ownership unknown |
| Suno | service | unverified | No public API; Free or Basic: personal non-commercial only; commercial use only as Suno's conditions permit |

## 11. Follow-up tasks

1. Read the Stability audio API reference (a browser-rendered read of `platform.stability.ai` or its OpenAPI file) and update section 5.
2. Send the section 9 questions to ElevenLabs and Google, and record the answers in this note.
3. Add the six register files under `docs/licensing/deps/` (section 10).
4. Decide the contract gaps in section 7 (including OAuth), then write the `mock-music` cases that cover: seed rejected where unsupported, fixed 30 s duration, SFX-only loop and unknown price blocking.
5. Build the ElevenLabs adapter (music and SFX) behind the cap check, with the Connectors page showing the plan-tier warning.
6. Re-check Beatoven and Suno in one month; both are likely to change.
