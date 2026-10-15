---
title: "Hosted image-generation APIs for CN-06: terms, output ownership, price, API shape"
summary: "terms, output ownership, price, API shape"
area: research
---

# Hosted image-generation APIs for CN-06: terms, output ownership, price, API shape

**Status: verified by the local lane on 2026-10-13 (checked 2026-10-13), except where a claim still says `unverified`.** The cloud box that wrote this note could reach only `cloud.google.com`; the local lane re-read the vendor sources (Google terms and pricing, Stability and fal.ai and BFL terms, OpenAI pricing and data docs).
**Still open:** OpenAI's business terms (every fetch returns a 403 or a JavaScript gate, so the ownership clause stays `secondary`), Stability's pricing page (JavaScript only), seeds on every API, and BFL's separate Developer Terms.
**Local-lane corrections:** (1) Stability's Terms of Service assign outputs to the customer (section 4(a)); the cloud note's "possibly adverse" worry is resolved. (2) fal.ai's Terms have no assignment and grant only "personal or internal business use" of the service: not cleared. (3) Google: the Service Terms bar use in services directed at or likely to be accessed by under-18s (new restriction, see section 2). (4) OpenAI: newer models and ZDR support found (section 3).
This note is not legal advice. It states what a text says and which clause, and lists questions for legal review.

**No account was created and no request was sent to any image-generation API.** The only requests made were plain page reads of public documents (`cloud.google.com` pricing and service terms) and web searches.

Evidence labels: `verified` = I read the primary source and quote it; `secondary` = a search-result summary or third-party page that I did not check against the vendor's text; `unverified` = no source reached. Retrieval date for everything: 2026-10-13.

## 0. What was and was not reachable

*This table records the cloud box's results. The local lane (2026-10-13) reached all of these hosts except the openai.com policy pages (403 / JavaScript gate) and JavaScript-only pricing pages.*

| Site | Result from the build box |
|---|---|
| `cloud.google.com` (pricing, service terms) | reachable, read in full |
| `docs.cloud.google.com`, `ai.google.dev`, `developers.googleblog.com` | unreachable (403 from the egress proxy) |
| `openai.com`, `platform.openai.com`, `developers.openai.com` | unreachable |
| `stability.ai`, `platform.stability.ai`, `docs.stability.ai` | unreachable |
| `fal.ai`, `docs.fal.ai`, `www.fal.ai` | unreachable |
| `bfl.ai`, `bfl.ml`, `docs.bfl.ai` | unreachable |
| `replicate.com`, `docs.replicate.com`, `huggingface.co`, `together.ai`, `aws.amazon.com`, `developers.cloudflare.com` | unreachable |

Also missing: `docs/design/connectors.md` was not yet written when this note was made. The "fit with the connector contract" paragraphs therefore test each service against the three properties the issue quotes (blocking `run` with polling; cap checked before each billable request; seed recorded), not against the design text. Re-check when that doc lands.

## 1. Summary

| | Google (Vertex AI: Imagen 4, Gemini image) | OpenAI (`gpt-image` family) | Stability AI (Stable Image API) | fal.ai (model host) | Black Forest Labs (FLUX API) |
|---|---|---|---|---|---|
| Output ownership | **verified**: "Generated Output is Customer Data"; Google "does not assert any ownership rights" | secondary: business terms assign output to the customer (primary page returns 403) | **verified (local lane)**: ToS 4(a) "we assign to you all of our right, title, and interest (if any) in the Outputs" | **verified (local lane)**: no assignment; only a statement that fal claims no IP in Output; access licence is "personal or internal business use" (**not cleared**) | **verified (local lane)**: "We claim no ownership rights in and to Your Content" (no assignment either); 1.3(e) allows "use of your Output" |
| Prompts and uploads | **verified**: not used to train without permission; not stored outside the account beyond need | secondary: not used to improve services without agreement; **verified**: image generation is Zero Data Retention compatible | **verified (local lane)**: may use Content to improve services; customer can opt out of training on Inputs and Outputs | **verified (local lane)**: files public on CDN for at least 7 days by default; terms limit use of Customer Input to providing the Services, but "Usage Data" derived from inputs may be used to build AI models | **verified (local lane)**: licence to use Input and Output to "train and improve"; opt out by emailing legal@ |
| 1024 square price | **verified**: Imagen 4 Fast $0.02, Imagen 4 $0.04, Ultra $0.06; Gemini 3.1 Flash Image $0.067 | **verified (local lane)**: gpt-image-1.5 $0.009 / $0.034 / $0.133 (low / medium / high); gpt-image-2 $0.006 / $0.053 / $0.211 | unverified (pricing page is JavaScript-only) | unverified (per-model; pricing page lists units, not read per image model) | **verified (local lane)**: pricing page shows a per-image rate, for example $0.048 for the FLUX.2 flex default; varies by model and resolution |
| API shape | partly verified (pricing page only) | unverified | unverified | secondary: queue API | secondary: async, polling URL |
| Seeds | unverified | unverified | unverified | unverified | unverified |
| Account needed | Google Cloud project with billing: needs a maintainer decision | OpenAI platform account: needs a maintainer decision | needs a maintainer decision | needs a maintainer decision | needs a maintainer decision |

**Ranked recommendation (provisional, for the maintainers; details in section 8):**

1. **Google Vertex AI (Imagen 4 / Gemini image).** The only candidate whose output-ownership, training and retention clauses I could quote. Cleanest verified terms of the five.
2. **OpenAI `gpt-image`**, once the local lane confirms the business-terms clauses at the source. Secondary evidence is favourable (output assigned to the customer, API data not trained on).

**Local-lane update (checked 2026-10-13):** Stability AI now passes on ownership (assignment clause read at source) and is a credible second or third adapter, provided the training opt-out is exercised and its price is read. fal.ai is **not cleared**: its Terms read at the source assign nothing and grant only personal or internal business use of the service, so it is not recommended until fal confirms commercial rights in Output in writing. BFL's paid API is plausible but its Developer Terms are unread.

## 2. Google (Vertex AI: Imagen 4 and Gemini image models)

Scope: this section covers the **Google Cloud (Vertex AI) service terms**, which I read. The Gemini Developer API (`ai.google.dev`, separate terms) was unreachable and is **not** covered; do not assume it has the same clauses.

### Ownership of outputs
- `verified` (Google Cloud Service Specific Terms, "Generative AI Services" section, last modified October 12, 2026; https://cloud.google.com/terms/service-terms, read 2026-10-13): "'Generated Output' means the data or content generated by a Generative AI Service prompted by Customer Data. Generated Output is Customer Data. As between Customer and Google, Google does not assert any ownership rights in any new intellectual property created in the Generated Output."
- `verified` (same section): "a Generative AI Service may, in some scenarios, produce the same or similar Generated Output for multiple customers." So no exclusivity.
- `verified`: Google's indemnity "also appl[ies] to allegations that an unmodified Generated Output ..." (conditions: the customer must have used it responsibly, not after notice of a claim, and so on; read subsection i(i) in full before relying on it).
- `verified` (local lane, https://cloud.google.com/terms/service-terms section 20(d), checked 2026-10-13): "Customer will not, and will not allow End Users to, use a Generative AI Service as part of a website, Customer Application, or other online service that is directed towards or is likely to be accessed by individuals under the age of 18." **Flag:** the clause covers any Customer Application "likely to be accessed by individuals under the age of 18", and what counts as the Customer Application here (the studio app, the account holder, or whoever uses the output) is not settled by the text. Whether Omuzaki's studio use is eligible is `unverified` pending clarification of the intended users and account scope (a question for legal review and Google). A feature that exposes Google generation to viewers or a public/fan-facing surface is the clearest case to avoid.
- Commercial use: the clause does not restrict it by plan tier; there is no free-tier distinction in this document. `unverified` for the Gemini Developer API free tier, which has separate terms.

### Prompts and uploads
- `verified` (Service Specific Terms, "Training Restriction"): "Google will not use Customer Data to train or fine-tune any AI/ML models without Customer's prior permission or instruction."
- `verified` ("Handling of Prompts and Generated Output"): "Absent Customer's prior permission or instruction, Google will not store outside Customer's Account (i) Customer Data prompted to a Generative AI Service for longer than is reasonably necessary to create the Generated Output, or (ii) the Generated Output."
- Caveat `verified`: some features keep data. For example, Grounding with Google Maps stores prompts and output "for thirty (30) days" for debugging. Not relevant to image generation, but shows that per-feature exceptions exist.
- Abuse-monitoring retention (local lane, https://cloud.google.com/vertex-ai/generative-ai/docs/data-governance, checked 2026-10-13): Google may log prompts for abuse monitoring under the Cloud Platform Terms; a customer in scope can request an exception for zero data retention. Image-model-specific retention periods not found on that page: `unverified`.

### Content rules
- `verified`: the Prohibited Use Policy for Generative AI is incorporated into the AUP ("the Prohibited Use Policy located at https://policies.google.com/terms/generative-ai/use-policy ... is incorporated into the AUP"). Local lane read the policy (https://policies.google.com/terms/generative-ai/use-policy, checked 2026-10-13): it bars sexually explicit content, violence/hate content, misinformation, and content that "violates the rights of others, including privacy and intellectual property rights". Original, non-explicit characters are within it; third-party-IP characters are the risk.
- `verified`: customers are "solely responsible for" configuring safety filters (section "Generative AI Services", last subsection).
- Named characters and real people: the policy only bars violations of IP and privacy rights generally; how Imagen/Gemini filters named characters in practice is `unverified` (needs a paid call). This is the question that matters for this project's workflow (§14 asks for original characters, and the licence policy bans third-party-IP inputs); see section 8.

### API shape
- Local lane reached the Imagen API reference (https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/imagen-api, checked 2026-10-13): the word `seed` does not appear on it, so a seed parameter is not documented there (`unverified` for Gemini image models). The rest of the API shape remains `unverified`. The pricing page shows two product families: Imagen (priced per image) and Gemini image models (priced per token, with a per-image equivalent).
- The pricing page also shows editing and customization are separate features for Imagen 3 ("Edit an image", "Customize an image" at $0.04), and Imagen 4 upscaling at $0.06. Whether reference images, LoRA/style tuning and seeds are available on Imagen 4 is `unverified`.
- Auth, sync versus long-running operation, sizes and `n`: `unverified`. (Vertex uses Google Cloud credentials, so an adapter would need a service-account or OAuth flow, which is a larger secret-handling surface than an API key: a question for the maintainers.)

### Price and limits
All `verified` from https://cloud.google.com/vertex-ai/generative-ai/pricing, read 2026-10-13 (Imagen table, "Price / 1 count (USD)"):

| Model | Feature | Price per image |
|---|---|---|
| Imagen 4 Fast | image generation | $0.02 |
| Imagen 4 | image generation | $0.04 |
| Imagen 4 Ultra | image generation | $0.06 |
| Imagen 4 | upscaling to 2K/3K/4K | $0.06 |
| Imagen 3 | generate, edit, customize | $0.04 |

Local lane re-read the pricing page (checked 2026-10-13): every price in the table above is confirmed. Gemini image models (same page, footnotes): "Gemini 3.1 Flash Image ... 1120 tokens ($0.067) for 1K (roughly 1MP)", "Gemini 3.1 Flash-Lite Image ... 1120 tokens ($0.034) for 1K", "Gemini 3 Pro Image ... 1120 tokens ($0.134) for 1K and 2K".
1024 square counts as "1K". Rate limits and quotas: `unverified` (on a docs page that was unreachable).

### Fit with the connector contract
- Blocking `run` with polling: `unverified` (no API documentation could be read).
- Cap before each billable request: fits. Prices are flat per image for Imagen, so a cap can be checked before the call. For Gemini image models the price is per token. The connector design (`docs/design/connectors.md`, D-section on cost: the host refuses before the run when the estimate passes the cap, and the conformance suite requires that the "estimate bounds usage") needs an upper bound, not a close estimate. The pricing-page footnotes give output-image tokens per resolution and input-image tokens, but no documented bound on billable text, reasoning or reference-image tokens, so the fit is `unverified` until a documented maximum is found.
- Seed recorded: `unverified` whether a seed can be sent. If not, the connector can record only the request (prompt, model, parameters), not a reproducible seed.

### Open questions
See section 8, Google.

## 3. OpenAI (`gpt-image` family)

All of this section's sources were unreachable (`openai.com`, `platform.openai.com`, `developers.openai.com`). Claims below come from search-result summaries, `secondary`, not checked against OpenAI's text. Mind that the register already has a row "OpenAI image outputs (Codex CLI)" with status `unverified`, for the Codex route (a different product); this section is about the paid API.

### Ownership of outputs
- `secondary` (local lane could not reach the primary text either: openai.com returns 403 to every fetcher; a second search on 2026-10-13 gave the same section 4.1 wording, "OpenAI hereby assigns to Customer all OpenAI's right, title, and interest, if any, in and to Output", and section 4.2 "OpenAI will not use Customer Content to develop or improve the Services, unless Customer explicitly agrees"; still `unverified` at source). Original note: search summary quoting the May 2025 business terms, https://openai.com/policies/may-2025-business-terms/, not read by me; retrieved 2026-10-13): "(a) retains all ownership rights in Input; and (b) owns all Output. OpenAI hereby assigns to Customer all OpenAI's right, title, and interest, if any, in and to Output", subject to "to the extent permitted by applicable law". Commercial use: implied by the assignment; per-tier differences `unverified`.
- Which document governs a pay-as-you-go API account (business terms, services agreement or the consumer terms of use) is `unverified`; ask legal review.

### Prompts and uploads
- `secondary` (same summary): "OpenAI will only use Customer Content as necessary to provide Customer with the Services, comply with applicable law, enforce the OpenAI Policies, and prevent abuse."
- `secondary` (third-party model-terms page citing OpenAI enterprise privacy): API inputs and outputs may be retained "for up to 30 days to provide the services and to identify abuse", and API data is not used for training. Zero data retention: `verified` (local lane, https://developers.openai.com/api/docs/guides/your-data, checked 2026-10-13): "Image generation is Zero Data Retention compatible when using ... gpt-image-2, gpt-image-1.5, gpt-image-1, and gpt-image-1-mini" (also the newer gpt-image-2.5 family). The 30-day figure was not found on that page: `unverified`.

### Content rules
`unverified`. Not read: usage policies, any moderation parameter of the image endpoint, the treatment of named characters and real people, and whether image models need an organisation identity verification (account steps: needs a maintainer decision).

### API shape
`unverified` (docs unreachable). Not established: endpoint, sizes, `n`, whether a seed is accepted, reference-image/edit support, LoRA (not expected: a closed-model API), synchronous versus job.

### Price and limits
- `verified` (local lane, https://developers.openai.com/api/docs/guides/image-generation and https://developers.openai.com/api/docs/models/gpt-image-1.5, checked 2026-10-13), per image at 1024x1024 low / medium / high: gpt-image-2 $0.006 / $0.053 / $0.211; gpt-image-1.5 $0.009 / $0.034 / $0.133 (unchanged from the cloud note); gpt-image-1 $0.011 / $0.042 / $0.167; gpt-image-1-mini $0.005 / $0.011 / $0.036.
- The docs now steer new integrations to the `gpt-image-2.5` models (Sunburst and Flare), billed per output token (30 USD per million image output tokens): a per-token price, so the cap check is an estimate. The `gpt-image-1.5` model page and image guide do not mention a seed parameter (`unverified`). Rate limits: `unverified`.

### Fit with the connector contract
`unverified`; cannot judge blocking/polling or seed without the docs. Per-image tiered prices would make a pre-request cap check straightforward if the model is billed per image; token metering (suggested by one source for gpt-image-2) would make it an estimate.

### Open questions
See section 8, OpenAI.

## 4. Stability AI (Stable Image API)

All sources unreachable. The search turned up only secondary pages and they **disagree**.

### Ownership of outputs
- `verified` (local lane; Stability AI Terms of Service, effective September 30, 2026, which name the API among the Services; https://stability.ai/terms-of-service, checked 2026-10-13), section 4(a): "Subject to your compliance with our Terms, we assign to you all of our right, title, and interest (if any) in the Outputs. So as between Stability and you, you own the Output (to the extent permitted by applicable law)." Only the customer's own Outputs, not other users'.
- **Correction:** the cloud note's secondary sources (a monitoring summary saying "limited personal license ... commercial use requiring separate permission") are wrong for the current Terms; the assignment is in the vendor's text. Not a licence-only grant, so the policy ban does not apply. The community-licence threshold (free under $1M revenue) concerns downloadable weights, not the hosted API, and is not relied on.
- Restrictions to keep in mind (`verified`, same page): section 3(b) bars using the Services or Outputs to train a model that competes with Stability's, and bars reselling or sublicensing access to the Services (end users may use it through your integrated product). Neither blocks the proposed use (an artist's own images).

### Prompts and uploads
`verified` (same page, section 4(c)): "we may use Content to improve and develop our Services (but you can opt-out to prevent us from using your Inputs and Outputs to train our models ...)". So the default allows training; the connector or owner must opt out for the account .

### Content rules
`unverified`: not read.

### API shape
`unverified`: API docs and seed support not read.

### Price and limits
`unverified`: the pricing page (https://platform.stability.ai/pricing) returned only a JavaScript shell on 2026-10-13, so no credit prices were read; rate limits not read.

### Fit with the connector contract
`unverified`: no API shape, price or seed information was read, so blocking/polling, the pre-request cap and seed recording cannot be judged.

### Open questions
See section 8, Stability.

## 5. fal.ai (model host that runs open and partner models)

Sources: fal.ai Terms of Service (https://fal.ai/legal/terms-of-service, last updated September 8, 2026, read by the local lane, checked 2026-10-13) and the model-API FAQ (https://fal.ai/docs/documentation/model-apis/faq, checked 2026-10-13).

**Not cleared (local lane):** the Terms assign Output Content to nobody and grant no express commercial rights in it. Section 4(a) limits the access licence to "your own personal or internal business use" (section 4(b) lets End Users reach the service through a Customer Solution), and section 4(c) says (as quoted by the page fetch) that nothing in the customer's use should "otherwise entitle Company to any intellectual property rights in any Output Content", which is a disclaimer of fal's claim, not a grant to the customer. Not rejected (no non-commercial bar and no ownership claim), but it does not meet the bar "output the customer may use commercially" until fal confirms in writing.

### Ownership of outputs
- `verified` (local lane; the originality disclaimer was seen in the search summary and not re-read, so that sentence stays `secondary`): the Terms say fal does "not represent, warrant, or covenant that any Output Content will be original" and that output "may not be unique across users". No clause assigning outputs to the customer was found in the summary, and TechCrunch (https://techcrunch.com/2024/09/18/fal-ai-which-hosts-media-generating-ai-models-raises-23m-from-a16z-and-others) reports customers are "largely on their own" for copyright claims.
- Important for a model host: **the licence of the underlying model also applies** (for example a FLUX or Stable Diffusion variant with its own licence). That is per-model and `unverified`; every model used would need its own register row. Only commercially licensed models would pass the policy.

### Prompts and uploads
- `verified` (local lane, FAQ page above): "Generated files are stored on the fal CDN and available for at least 7 days by default", settable per request with the `X-Fal-Object-Lifecycle-Preference` header, and "Media URLs are publicly accessible." Public URLs for generated images are a privacy note for the maintainers.
- Training (local lane, Terms sections 6 and 7): fal's licence to Customer Input is limited to providing the Services, and Customer Input and Output Content are Customer Confidential Information; but "Usage Data" ("may include data based on or derived from Customer Input", anonymized or aggregated) may be used to "design, develop, and offer Company products, services, and AI models". Not a clean no-training promise.

### Content rules
`unverified`. There is an Acceptable Use Policy (https://fal.ai/legal/acceptable-use-policy), not read; moderation depends on the hosted model.

### API shape
- `secondary`: a queue API (submit, poll status, fetch result) is implied by the docs pages found; exact endpoints, auth, seeds, sizes, image inputs and LoRA support are `unverified`. Seed and LoRA support per model are not established here.

### Price and limits
`unverified` (usually per model, per megapixel or per image; no figure read). Rate limits: `unverified`.

### Fit with the connector contract
A queue API (submit then poll) would map onto a blocking `run` with polling, and per-model prices known up front would allow the cap check; both are `unverified` assumptions to confirm. The model-by-model licence is the real obstacle: the adapter would have to pin a model and the register row would carry that model's licence.

### Open questions
See section 8, fal.ai.

## 6. Black Forest Labs (FLUX API)

Sources unreachable; search returned only aggregator pages (OpenRouter listings, tutorials). All `secondary` or `unverified`.

### Ownership of outputs
`verified` (local lane; BFL Terms of Service https://bfl.ai/legal/terms-of-service, checked 2026-10-13): section 1.2(a) "We claim no ownership rights in and to Your Content"; section 1.3(e) bars exploiting the Services for any commercial purpose except "your use of your Output or the Services as expressly permitted by these Terms". No assignment clause; a separate agreement (Developer Terms) governs API use if one applies and was **not read**, so commercial rights for API outputs stay `unverified`. A comparison site claimed commercial use "requires a license" without citing BFL's terms; ignore it. One guide says FLUX.2 [dev] is open-weight and meant for local non-commercial use: a **non-commercial licence, banned by `docs/licensing/policy.md` for any use here**, so only BFL's own paid API models could ever qualify, and only if the API terms assign output rights.

**Open weights (FLUX.2 dev):** the pricing page lists them under a separate "Open Weights Licensing" plan (Builder, Platform); the note's earlier statement that the free [dev] weights are non-commercial is `secondary`, not re-read. **Rejected for any use here unless a paid commercial licence is read and cleared.**

### Prompts and uploads
`verified` (same page, section 1.2(e)): BFL takes a licence to use Input and Output "to provide, develop, train and improve our technologies, products and services"; opt out of future training by contacting legal@blackforestlabs.ai. Content already used stays licensed.

### Content rules
`unverified`.

### API shape
`secondary` (https://developer.puter.com/tutorials/how-to-get-flux-api-key/): asynchronous; the submit call returns a polling URL, status is read with the API key in an `x-key` header, and when status is "Ready" the image URL is in `result.sample`. Seeds, sizes, reference images: `unverified`.

### Price and limits
`verified` that https://bfl.ai/pricing shows per-image rates by model and resolution (for example $0.048 per image for the default FLUX.2 flex setting, checked 2026-10-13); the other figures below remain `secondary` (https://apiframe.ai/guides/flux-api-guide and the OpenRouter listing for FLUX.2 [max]): starting prices of about $0.014 (klein 4B), $0.03 (pro), $0.05 (flex); FLUX.2 [max] at $0.07 for the first megapixel and $0.03 for later ones, plus $0.03 per input megapixel for reference images. Aggregator figures, possibly stale.

### Fit with the connector contract
the submit-then-poll shape fits a blocking `run` with polling (`secondary`). Per-megapixel pricing is knowable before the request. Seed support `unverified`.

### Open questions
See section 8, BFL.

## 7. Not covered

- **Replicate** (a model host like fal.ai): a search found its terms page (https://replicate.com/terms) and a third-party summary claiming Replicate "grants customers whatever rights it has in Output, including commercial use, subject to third-party terms", with per-model terms layered on top (`secondary`). No polling, seed, price or retention facts found. A natural alternate to fal.ai if the maintainers wants a host; the same per-model licence caveat applies.
- **Midjourney, Ideogram, Adobe Firefly, Amazon Bedrock, Together, Cloudflare Workers AI:** not looked at. Sections 2 to 6 meet the issue's minimum of three.

## 8. Ranked recommendation, register rows, questions

### Recommendation (at most two for a first adapter)

1. **Google Vertex AI (Imagen 4 / Gemini image).** Pros (all `verified`): the clause "Generated Output is Customer Data" and no ownership assertion; a training restriction needing the customer's permission; no storage beyond need; a published per-image price ($0.02 to $0.06 at 1K); an indemnity for unmodified output. Cons: **condition:** Service Terms 20(d) (no use in a service "likely to be accessed by individuals under the age of 18"); whether Omuzaki's studio use is eligible is `unverified` pending clarification of the intended users and account scope. Also a Google Cloud account with billing and credentials , larger credential surface than an API key (`unverified` how auth works), API shape, seed and reference-image support all `unverified`.
2. **OpenAI `gpt-image`**, conditional on the local lane reading the business terms and the API docs and confirming that the terms that apply to a pay-as-you-go API account assign outputs to the customer.

The decision does not depend on price; both are cents per image at 1K.

### Register rows (appended to `docs/licensing/register.md`, kind `service`)

All stay `unverified` until the local lane has read the vendor text, except that the Google row carries the verified clause (still `unverified` because the Gemini Developer API tier, the use policy and the API docs were not read). See the file.

### Questions for the maintainers or ask the vendor

**Maintainers (all vendors):**
1. Which account and which credit card may be used, and what monthly cap should the connector enforce by default? (needs a maintainer decision.)
2. Do you want one adapter first (Google) or two?
3. Is a vendor that offers only a licence to use outputs, not an assignment, acceptable? (Policy says no; this is to confirm for Stability.)

**Google:**
- Do the Vertex AI "Generated Output is Customer Data" terms apply to Imagen 4 and the Gemini image models in our region/account type, and do the Gemini Developer API terms (free and paid tiers) differ?
- What does the Generative AI Prohibited Use Policy say about named fictional characters and real people, and what do Imagen/Gemini do when a prompt names one?
- Is abuse-monitoring retention of prompts and images possible even with the "no storage" clause, and can it be disabled?
- Does Imagen 4 accept a seed, reference images or style tuning? What are the default quotas?
- Which authentication would the adapter use, and can a restricted API key or a service account with only the Vertex User role be used?

**OpenAI:**
- Which agreement governs a plain API account (business terms, services agreement, or terms of use), and does "OpenAI assigns to Customer all ... right, title, and interest, if any, in and to Output" appear in it?
- Do image models require organisation verification, and is zero data retention available for images?
- Does the image endpoint accept a seed or reference images? What does it do with prompts naming existing characters or real people?
- Is `gpt-image-2` billed per image or per token, and what is the exact price at 1024x1024?

**Stability AI:** Does the API (not the consumer app) terms assign outputs to the customer or only license them, and is customer content used to improve or train models?

**fal.ai:** Which clause, if any, assigns outputs to the customer? Does fal train on inputs or outputs? Which model licences apply per hosted model, and are generated media URLs private on request?

**Black Forest Labs:** Do the API terms assign outputs to the customer and allow commercial use for the paid API models, and is a seed accepted?

**Legal review:** whether "assigns ... if any" wording gives enough comfort given that purely machine-generated images may carry no copyright at all (Thaler v. Perlmutter is cited in a secondary source; not read); and what the indemnity conditions mean for an artist's workflow.

### What would not fit the connector contract (so far)

- Anything priced per token (Gemini image, possibly `gpt-image-2`) makes the pre-request cap an estimate.
- If a service accepts no seed, "seed recorded" degrades to "request recorded".
- Hosted media URLs (fal.ai) that are public and expire need an immediate download step in the adapter.
- Google Cloud auth is not a single API key.
