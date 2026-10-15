---
title: "AI disclosure and viewer-data rules for an AI performer (2026-10-14)"
summary: "AI-disclosure and viewer-data rules for an AI performer (EU AI Act Art. 50, CA SB 243, China, Korea, platform labels, GDPR/CCPA for memory), with a default per feature; draft, primary sources not yet read."
area: research
---

# AI disclosure and viewer-data rules for an AI performer (2026-10-14)

> Research (SE-06, AP-07, AP-08, AP-06, AP-11, SE-05). Spec: v3 §23, §31. **These are engineering notes, not legal advice; a legal review comes before release.** Names no private material and no people.
>
> **Draft status: primary sources could not be read.** The research environment's egress policy answered HTTP 403 on CONNECT for `eur-lex.europa.eu`, `leginfo.legislature.ca.gov`, `digital-strategy.ec.europa.eu`, `twitch.tv`, `support.google.com`, `oag.ca.gov` and `gdpr-info.eu` (checked 2026-10-14 with the proxy status page; I did not route around it). So:
> - **No statute, regulation, guideline or platform policy was read at the source.** Everything below is `secondary` (a web-search summary of a law-firm, news or vendor page seen on 2026-10-14; the pages themselves were not opened) or `unverified`. Per the issue, `unverified` counts as unknown.
> - Clause numbers (Art. 50(1) to (5), Art. 3(60), SB 243 section numbers) come from my recollection of the texts and are `unverified` until someone reads them. The "primary source to read" column gives the URL that should be read first.
> - The issue's acceptance (every row has a read primary source) is **not met**. This PR stays a draft until the lead rules (see "Blocker").
>
> "Retrieved" cells say what was done: "not retrieved" means the host was blocked and nothing was read; "search summary read" means a search result summary, not the page, was seen on that date. No cell records a primary-source read.
>
> Confidence key: `verified` = primary source read; `secondary` = a summary of a non-primary page seen; `unverified` = unknown, treat as unknown. Reading ("R:") is mine; "Rule:" is what a source says.

## 1. Blocker

A person or a session with access must read these and fill the "primary" cells. Until then nothing here is `verified`:
1. EU AI Act, Regulation (EU) 2024/1689, Art. 3(60), Art. 50, Art. 99, Art. 113: https://eur-lex.europa.eu/eli/reg/2024/1689/oj
2. The Digital Omnibus on AI as adopted (secondary reports call it Regulation (EU) 2026/1744, Official Journal 24 July 2026; `unverified`), and what it changed in Art. 50 and Art. 113.
3. Commission Art. 50 guidelines (reported 20 July 2026) and the Code of Practice on marking and labelling AI-generated content (reported 10 June 2026): https://digital-strategy.ec.europa.eu/en/news/commission-publishes-code-practice-marking-and-labelling-ai-generated-content
4. California SB 243 chaptered text: https://leginfo.legislature.ca.gov/faces/billTextClient.xhtml?bill_id=202520260SB243
5. California SB 942 as amended by AB 853, and any 2026 transparency bill in force.
6. China: the CAC Measures for Labeling AI-Generated Synthetic Content and mandatory standard GB 45438-2025.
7. Korea: the AI Basic Act and its enforcement decree.
8. Each platform's own policy and API reference page (section 3).
9. GDPR (Regulation (EU) 2016/679) Arts. 5, 6, 7, 8, 9, 12 to 22, 35; CCPA/CPRA (Cal. Civ. Code §1798.100 ff., §1798.140) and the CPPA regulations.

## 2. Requirements table

"Who" uses: provider = the party that develops or places the AI system on the market under its own name (Omuzaki, for its built-in TTS and persona); deployer = the party using it in a professional capacity (the streamer); platform = the service hosting the stream. Whether the streamer becomes a "provider" by configuring a persona is an open question (section 7).

| # | Instrument and clause | Who | Trigger | What and how | Applies from | Primary source to read | Retrieved | Confidence |
|---|---|---|---|---|---|---|---|---|
| 1 | EU AI Act Art. 50(1) | provider | an AI system intended to interact directly with natural persons | design it so people are told they interact with AI, unless obvious to a reasonably well-informed, observant, circumspect person; form unspecified (text or spoken both plausible) | 2 Aug 2026 (secondary: Omnibus did not move it) | https://eur-lex.europa.eu/eli/reg/2024/1689/oj | not retrieved (host blocked, HTTP 403, 2026-10-14) | `unverified` (clause), date `secondary` |
| 2 | EU AI Act Art. 50(2) | provider | a system generating synthetic audio, image, video or text | outputs marked in a machine-readable format and detectable as AI-generated; solution must be effective, interoperable, robust, reliable as far as technically feasible | 2 Aug 2026; secondary reports say the Omnibus gives systems **already on the market before 2 Aug 2026** until **2 Dec 2026** and that new systems comply at once | https://eur-lex.europa.eu/eli/reg/2024/1689/oj | not retrieved (host blocked, HTTP 403, 2026-10-14); search summaries read 2026-10-14 | `unverified` (clause); delay `secondary`, **sources not agreed on detail** |
| 3 | EU AI Act Art. 50(4), first subparagraph | deployer | a system that generates or manipulates image, audio or video constituting a deep fake (Art. 3(60): resembles existing persons, objects, places, entities or events and would falsely appear authentic) | disclose that the content is artificially generated or manipulated; for an evidently artistic, creative, satirical or fictional work the duty is limited to disclosing the existence of such content in an appropriate manner that does not hamper display or enjoyment | 2 Aug 2026 | https://eur-lex.europa.eu/eli/reg/2024/1689/oj | not retrieved (host blocked, HTTP 403, 2026-10-14) | `unverified` |
| 4 | EU AI Act Art. 50(5) | provider and deployer | any of 50(1) to (4) | information given clearly and distinguishably at the latest at first interaction or exposure, accessible | 2 Aug 2026 | https://eur-lex.europa.eu/eli/reg/2024/1689/oj | not retrieved (host blocked, HTTP 403, 2026-10-14) | `unverified` |
| 5 | Commission Code of Practice (marking and labelling) | provider, deployer (voluntary) | adherence is voluntary; a way to show compliance | secondary summary: at least two marking layers where needed (metadata plus watermark), detection tools offered to deployers and others, an EU icon or equivalent label for deployers | published 10 Jun 2026 (secondary) | https://digital-strategy.ec.europa.eu/en/news/commission-publishes-code-practice-marking-and-labelling-ai-generated-content | search summary read 2026-10-14 (page not opened) | `secondary` |
| 6 | Commission Art. 50 guidelines | all | interpretation, incl. what "obvious" and "deep fake" mean | the guidelines should settle rows 1 and 3 for a VTuber; not read | reported 20 Jul 2026 (secondary) | https://digital-strategy.ec.europa.eu/en/news/commission-publishes-code-practice-marking-and-labelling-ai-generated-content (guidelines page not located) | search summary read 2026-10-14 (page not opened) | `secondary` (existence), content `unknown` |
| 7 | California SB 243 (companion chatbots) | operator (a person making a companion chatbot platform available to users in California) | a "companion chatbot" (R: an AI system with a natural-language interface giving adaptive, human-like responses that can meet a user's social needs and sustain a relationship across interactions; exclusions for narrow bots such as customer service, `unverified`) | clear and conspicuous notice that it is AI and not human when a reasonable person could be misled; for a user the operator knows is a minor: say it is AI, a break reminder at least every 3 hours, no sexual content; a suicide/self-harm protocol published on the website; notice that it may not suit some minors; annual reporting | 1 Jan 2026; reporting from 1 Jul 2027 (secondary) | https://leginfo.legislature.ca.gov/faces/billTextClient.xhtml?bill_id=202520260SB243 | search summary read 2026-10-14 (page not opened) | `secondary` |
| 8 | California AI Transparency Act (SB 942 as amended by AB 853) | covered provider (secondary: a generative system with over 1,000,000 monthly users, publicly accessible in California) | image, video or audio content from such a system | free AI-detection tool, visible and embedded ("latent") disclosures; $5,000 per violation (units disputed in sources); platform and device duties are later | operative 2 Aug 2026 (AB 853 moved it from 1 Jan 2026); platforms 1 Jan 2027, devices 1 Jan 2028 (secondary) | https://leginfo.legislature.ca.gov/faces/billTextClient.xhtml?bill_id=202520260SB942 (SB 942) and ...AB853 (AB 853) | search summary read 2026-10-14 (page not opened) | `secondary`; whether Omuzaki meets the user threshold: it does not today, so likely **not covered** (R, `unverified`) |
| 9 | China: Measures for Labeling AI-Generated Synthetic Content and GB 45438-2025 | service providers and distribution platforms | AI-generated text, image, audio, video, virtual scenes offered in China | explicit label (for audio: a spoken or audible cue such as "AI" by voice or rhythm) plus implicit label in metadata or watermark; platforms check metadata and prompt users to label; keep labels on download or export | 1 Sep 2025 (secondary) | CAC measures text and GB 45438-2025 (URLs not found by search; to be located) | search summary read 2026-10-14 (page not opened) | `secondary`; audio parameters `unknown` |
| 10 | Korea: AI Basic Act | AI business operators | generative AI, and output hard to tell from reality | notify users that a product uses generative AI; label output that is difficult to distinguish from reality; fine up to 30 million won, fines deferred about a year (to about 22 Jan 2027), corrective orders still possible; method and scope left to enforcement decrees | 22 Jan 2026 (secondary) | Korean AI Basic Act text and enforcement decree (URLs not found by search; to be located) | search summary read 2026-10-14 (page not opened) | `secondary`; applicability to an overseas desktop tool `unknown` |

## 3. Platform table

None of the four was readable at its own policy page. Everything is `secondary` or `unverified`. Official pages to read: Twitch Community Guidelines and Branded Content policy, YouTube "Disclosing use of altered or synthetic content" help page and the Data API reference, TikTok Community Guidelines (AI-generated content) and the Content Posting API reference, Bilibili community rules and its open-platform live docs.

| Platform | AI-content label or setting | Set through an API by a third-party tool? | Needs the streamer's own account or action | Retrieved | Confidence |
|---|---|---|---|---|---|
| Twitch | secondary: no AI-content label found; a news item reports the platform's chief executive saying disclosure rules are not needed for now (undated in the snippet). Other rules (labelling mature or branded content) exist and are not AI-specific | none found | the streamer sets any stream info in their own dashboard | search summary read 2026-10-14 (page not opened) | `unverified` (policy), `secondary` (news) |
| YouTube | "altered or synthetic content" disclosure for realistic content; not needed for clearly unrealistic, animated or production-assist use (script, thumbnail, generic TTS voice); secondary reports of automatic labels where creators do not disclose (2026) | secondary: the Data API has a boolean (`containsSyntheticMedia` in one source, `syntheticMedia` in another); applies to uploads, **live-broadcast support unknown** | the streamer's own Google account and OAuth consent; Omuzaki would need a Google API project and its review (an account, said here) | search summary read 2026-10-14 (page not opened) | `secondary`; field name `unverified` |
| TikTok | label for AI-generated realistic image, audio or video; creator-set; sticker or caption also allowed | secondary: an `is_aigc`-style flag in the Content Posting API (name unconfirmed); LIVE support `unknown` | the streamer's own TikTok account; the API needs a registered app and audit | search summary read 2026-10-14 (page not opened) | `secondary`; field `unverified` |
| Bilibili | creators are told to tag AI-generated works (since Sep 2023, secondary); since Sep 2025 platforms check metadata and prompt for labels; digital-avatar users need ID verification (one source) | `unknown` | the streamer's own account; live (直播) rules not found | search summary read 2026-10-14 (page not opened) | `secondary`/`unverified`; live rules `unknown` |

R: no platform setting is known to be settable by a third-party tool for a **live** stream, so the default is that Omuzaki labels inside its own output (overlay, voice) and tells the streamer which switch to flip in their own account.

## 4. Viewer-data table (AP-06: memory and transcripts)

All rows `unverified` as to the clause text; the topics come from secondary summaries (GDPR Art. 8 age 16, Member States may go down to 13; CPRA retention disclosure; EDPB Opinion 28/2024 on legitimate interest, adopted 17 Dec 2024). Streamer = controller (GDPR) or business (CCPA) for what they store; Omuzaki is a tool and, if it stays local and sends nothing, is neither (R: depends on any hosted LLM or TTS the streamer picks, open question 6).

| Topic | GDPR (R) | CCPA/CPRA (R) | Primary to read | Confidence | Default for AP-06 |
|---|---|---|---|---|---|
| Lawful basis | Art. 6: legitimate interest is plausible for chat memory but needs a written balancing test; consent (Art. 7) is withdrawable. EDPB Opinion 28/2024 says legitimate interest is no default | no basis needed, but notice at collection and purpose limitation | GDPR Arts. 6, 7; Civ. Code §1798.100 | `unverified` / `secondary` | memory is **off until the streamer turns it on**; viewers are told once in chat (a pinned line) that the bot remembers; `!forget` works |
| Retention | storage limitation (Art. 5(1)(e)): keep no longer than necessary | disclose the retention period or criteria per category; do not keep longer than reasonably necessary (secondary) | Art. 5; §1798.100(a)(3) | `unverified` / `secondary` | default expiry per fact, shown to the streamer; transcripts follow the show and are deleted unless the streamer saves them (matches `ai-performer.md`) |
| Deletion and export | Arts. 15 to 17, 20: access, erasure, portability; reply within one month (Art. 12) | rights to know, delete, correct; 45 days to respond, extendable (secondary) | Arts. 12 to 22; §1798.105, .110, .130 | `unverified` | per-viewer record keyed by platform user id; one op deletes it, one exports JSON; both usable by the streamer, who handles the request |
| Children | Art. 8 sets 16 (13 to 16 by state) for consent to online services offered directly to a child; platforms bar under-13 or 13 accounts | opt-in needed to sell or share data of under-16s (secondary) | Art. 8; §1798.120(c) | `secondary` | never store per-viewer facts for a viewer the platform marks as a child, or one who says they are under 18; SB 243 minor rules apply if the chatbot is a companion |
| Sensitive categories | Art. 9: health, beliefs, sexuality and the like barred unless a condition applies; chat sometimes volunteers them | sensitive personal information has its own notice and limit-use right (§1798.121, §1798.140(ae)) | Art. 9; §1798.140 | `unverified` | a filter drops facts in those categories before they reach memory (reuses the AP-07 text filters); never store voice biometrics |
| Impact assessment | Art. 35 DPIA when processing is likely high risk | CPPA risk-assessment regulations (status `unknown`) | Art. 35 | `unverified` | not needed for a single streamer's small local store (R); lawyer to confirm |
| Transfers to a hosted LLM or TTS | Chapter V; a processor agreement (Art. 28) | service-provider contract terms | Arts. 28, 44 ff. | `unverified` | the settings screen names every hosted provider that sees chat text; local models are the default |

## 5. The four questions

1. **Is an AI co-host that answers chat a chatbot under Art. 50(1) and a companion chatbot under SB 243?** R: yes under Art. 50(1) (it interacts directly with natural persons, and a viewer cannot always tell a voiced avatar is AI, so rely on the "obvious" exemption never); under SB 243 it is plausibly a companion chatbot if it holds per-viewer memory and a persona that sustains a relationship (AP-06), and plausibly outside it if it is a stateless reader; `unverified` until the definition and its exclusions are read.
2. **Does synthetic voice output need a machine-readable mark under Art. 50(2), and who carries it?** R: yes, the duty is on the provider of the system generating the audio, so Omuzaki for built-in TTS (and the provider of any model the streamer brings, if they are the provider), not the streamer; a live RTMP mix cannot carry metadata, so the mark lives in recordings (BR-18) and exports and any live disclosure is a deployer measure; `unverified`, and the Omnibus grace period (to 2 Dec 2026 for systems already on the market) may apply.
3. **Is cover mode (AP-08) a deep-fake case under Art. 50(4)?** R: possibly: the streamer is the deployer and the voice or persona resembles an existing person (themselves); a stylised avatar may not "falsely appear authentic" but a cloned voice speaking unsaid words may; the artistic/fictional carve-out limits the form of the label, it does not remove it; treat as yes, `unverified`.
4. **What does a cloned or sound-alike voice (SE-05) add?** R: a likely Art. 50(4) audio deep-fake case when it resembles a real person, a GDPR personal-data question for the recorded voice, and non-EU personality and voice-likeness laws I did not read; the consent record SE-05 already demands is necessary, not sufficient; `unverified`.

## 6. Defaults mapped to features

One default per row of section 2 and 3. "Chip" = the overlay chip (BR-13, #503). Each ships on, can be restyled and cannot be turned off for AI voice in a public show (R: the lawful floor; a lawyer decides).

| Feature | Default | Covers rows |
|---|---|---|
| Overlay chip (BR-13, #503) | a small "AI" chip on screen the whole time an AI performer is live, text in the show language, plus a one-time banner at show start | 1, 4, 7, 9, 10 |
| Spoken line | the performer says "I'm an AI" in its own voice at the first turn of a show and when a new viewer first addresses it | 1, 4, 7, 9 (audible cue) |
| Chat-bot prefix | every chat message the bot posts begins with a short tag, e.g. `[AI]`, and the bot's channel description says it is AI (streamer's own account) | 1, 7, platform rows |
| Recording metadata (BR-18, #1430) | write a machine-readable "AI-generated audio" field in the container, and an in-band audio mark if a robust one is licence-clean; flagged as `unverified` against row 2 and the Code | 2 (recordings and exports only; **not** live), 5, 8, 9 |
| Caption marker | live captions of AI speech carry an `AI:` speaker label | 1, 9 |
| Cover-mode label (AP-08, #488) | a larger chip "AI is driving this character" plus a spoken line every 30 minutes and on first speech; cannot be hidden | 3, 4 |
| Chat reader (AP-11, #720) | reader voices are marked AI by the chip and the first line; per-viewer voices never imitate a real person | 1, 3 (not 2: see the live-audio gap below) |
| Platform switches | a setup-checklist step names the streamer's own AI label switch (YouTube, TikTok, Bilibili); no API call is made on their behalf in v1, because each needs an account and an app registration (said here) | section 3 |
| Memory (AP-06) | see section 4 defaults | all |

**Unresolved: machine-readable marking of live-only synthetic audio (row 2).** The chip and the spoken line are human-readable disclosures (rows 1, 3, 4); they do not satisfy a machine-readable mark. Recordings and exports can carry metadata (BR-18). A live RTMP or virtual-camera mix has no supported carrier in this note: an in-band audio watermark that survives platform re-encoding is not designed, `docs/research/2026-10-model-watermarking.md` covers textures and meshes only, and the Code of Practice's two-layer advice is unread. So row 2 has **no default for live audio** until a lawyer says whether the provider duty reaches a live mix and an audio mark is chosen.

Nothing here needs a paid service. The platform API route would need a developer account and review per platform; it is not proposed.

## 7. Open legal questions for a lawyer

1. Is the streamer a deployer only, or a provider, when they configure a persona and a voice in Omuzaki?
2. Does the "obvious to a reasonably well-informed person" exemption in Art. 50(1) cover a visibly animated avatar, and what do the Commission guidelines say?
3. What does the Omnibus grace period to 2 Dec 2026 cover exactly, and does Omuzaki count as "placed on the market" before 2 Aug 2026 (R: not released yet, so probably not)?
4. How does a provider mark live streamed audio so the mark survives a platform's re-encode, and does a metadata-only mark meet "effective, robust"?
5. Does a stylised avatar with a cloned voice make Art. 50(4) apply, and which form of label does the creative-work carve-out allow?
6. Is Omuzaki a processor, joint controller or neither when everything stays local, and when a hosted LLM or TTS is chosen?
7. Is the AI co-host a companion chatbot under SB 243, and is a streamer an "operator"? What counts as knowing a user is a minor?
8. Do the Chinese and Korean rules reach an overseas desktop tool used by a streamer who broadcasts into those markets?
9. Voice-likeness and personality laws for sound-alike voices (US states, EU Member States): which apply?
10. Does a spoken "I'm an AI" line satisfy "clear and distinguishable" for a stream a viewer joins mid-show?

## 8. Next step

Someone with access to the blocked hosts reads the nine items in section 1 and changes each `unverified` or `secondary` cell to `verified` with a retrieval date, corrects the four answers in section 5 where the text disagrees, and checks the Omnibus changes. Until then the defaults in section 6 are a conservative starting point, not a conclusion.
