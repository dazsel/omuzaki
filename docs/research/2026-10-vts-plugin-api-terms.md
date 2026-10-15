---
title: "VTube Studio plugin API terms for the CN-13 bridge (2026-10-12)"
summary: "VTube Studio plugin API terms for the CN-13 bridge (2026-10-12)"
area: research
---

# VTube Studio plugin API terms for the CN-13 bridge (2026-10-12)

> Research that CN-13 depends on, in `docs/product/02-studio-spec-v3.md`. Sources read 2026-10-12: the DenchiSoft/VTubeStudio README, `LICENSE` and `Permissions/` page, the VTS EULA v5.2 (2024-04-15), the branding page and the plugin wiki. This is an engineering reading, not legal advice.

## Verdict: GO, with conditions

A free or paid third-party app may use the VTS public API with no permission, fee or royalty. The grant is explicit in two places, the repo README and the EULA. The conditions are about naming, the user's own VTS licence and a few API limits. None blocks CN-13.

## 1. May a free (or paid) third-party app use the API?

Yes. Three primary statements:

- README, "Legal Info": "You are free to write, publish and sell any plugin you want for VTube Studio. No permission is required and there are no licensing fees or royalties." ([README](https://github.com/DenchiSoft/VTubeStudio#legal-info))
- EULA v5.2 §10 "VTube Studio Plugins": "Developers may distribute and sell these plugins however they want. No permission, license or payment to DenchiSoft is required." ([EULA PDF](https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf))
- EULA §5, after the reverse-engineering ban: "If you are interested in developing extensions/plugins for VTube Studio, please use the VTube Studio Public API ... You may freely distribute and sell plugins created for VTube Studio".

The `DenchiSoft/VTubeStudio` repo is MIT-licensed (Copyright (c) 2019 DenchiSoft; [LICENSE](https://github.com/DenchiSoft/VTubeStudio/blob/master/LICENSE)). That covers the docs and example files in the repo. MIT permits copying and sublicensing, but needs the copyright notice kept in copies. We should not copy docs or code verbatim. Our own client is written from the documented wire protocol (clean-room source list in section 7), so the MIT notice only applies if we do copy something.

What the EULA forbids does not touch us. §5 bans modifying, decompiling or reverse engineering VTS, deriving works from it, bypassing its licence check, and using it to advertise crypto, NFTs or AI "art" projects. The bridge only talks to VTS over its public WebSocket. We ship no VTS code and no Live2D SDK. Two points need care in marketing copy: the AI-"art" clause is about advertising such projects with VTS or its assets, so the bridge pages should not advertise Omuzaki's generation features using VTS assets or screenshots; and §9 says the bundled Akari model must not be used to look officially affiliated.

Precedent: VBridger (PiPuProductions and Serp) is sold on Steam as a paid "face tracking plugin designed for VTube Studio and Live2D" and is listed on the [official plugin wiki](https://github.com/DenchiSoft/VTubeStudio/wiki/Plugins). Streamer.bot has an "official VTubeStudio integration" and is listed in the README table; the README also lists Mix It Up. VNyan is free on itch.io and I found no VTS-side listing for it (not checked in depth). Paid and free plugins therefore coexist. The EULA itself puts no limit on commercial plugins.

## 2. Attribution, naming and logo rules

No attribution is required for using the API. Naming is covered by two sources:

- EULA §12 "Branding": "If you would like to use the VTube Studio logo and other branding in your videos, please do not modify them. You may use them, but please only do so in a way that does not make it appear as if your project/stream is officially related to or partnered with VTube Studio."
- [Branding page](https://denchisoft.com/branding): logos may be used "but please do not modify them", and not "in a way that makes it appear as if your project/stream is officially affiliated" with VTube Studio "unless you have explicit permission". The page does not mention plugins, and neither the EULA nor the README grants a trademark licence.
- The wiki describes listed plugins as unofficial and not developed by DenchiSoft.

Reading: descriptive use of the name ("works with VTube Studio", "connects to your VTube Studio") is the established practice (VBridger: "designed for VTube Studio"). Logos are allowed only unmodified and never as an implied endorsement. The branding terms are written for videos, streams and projects, so putting the logo inside a product UI is a grey area.

CN-13 rules (proposed):
- Say "VTube Studio" in plain text only, as a nominative reference ("Connect to VTube Studio"). Do not name any Omuzaki feature or product with "VTube Studio" or "VTS" in a way that suggests it is part of VTS (so "VTube Studio bridge" in docs and UI, not "Omuzaki for VTube Studio").
- Do not use the VTS logo, logo-like marks, or Live2D marks in the app. If we ever want the logo (store page, docs), use it unmodified, and ask DenchiSoft in writing first because the terms do not cover product UI.
- Add a footer line in docs and the connector UI: "Not affiliated with or endorsed by DenchiSoft or Live2D Inc. VTube Studio is a product of DenchiSoft."
- Plugin identity on the wire: `pluginName` and `pluginDeveloper` must each be 3 to 32 characters and are shown in the user's permission popup. Use "Omuzaki" and "Omuzaki Studio" (see section 5 on why these must stay stable).

## 3. What the user needs

- VTube Studio itself. The free version works; the API is not paywalled in any source I read. Monetising users must own a paid VTS version or DLC under EULA §2.1 ("If you use VTube Studio commercially (superchats, monetized YouTube, etc.), you have to purchase at least one paid version (iOS/Android) or paid DLC"), and removing the watermark also needs one. That obligation is the user's, not ours. Our docs should say so rather than suggest the bridge removes it.
- The user's own Live2D model and their own rights to it. We never load or read the `.moc3` file; VTS renders it.
- In VTS, enable "Allow Plugin API access". README: "you should ask the users to `\"Allow Plugin API access\"` in VTube Studio" (default port `ws://localhost:8001`, user-changeable).
- A one-time authentication popup: a token request shows "a popup inside of VTS asking the user if they want to allow the plugin ... to control VTube Studio". The token (at most 64 characters) is then reused for later sessions. "Users can revoke API access from your plugin at any point from within VTube Studio", after which requests fail and we may re-authenticate.
- Extra permissions are popup-gated per feature. Today only `LoadCustomImagesAsItems` and `ControlNDISettings` exist, and CN-13 needs neither: tracking injection, hotkeys, expressions and model info need only authentication.
- A company with income above $200,000 USD needs a VTS Company Licence (EULA §2.2); a business whose annual sales through Live2D content exceed 20 million JPY may owe Live2D Inc. a licence (EULA §13). Both are the user's business obligations. They matter to Omuzaki only as an FAQ entry: a VTuber agency using the bridge should read them.

## 4. Parameters, rate and latency

Everything below is from the README. No latency or throughput figures are published.

**What we can inject** (`InjectParameterDataRequest`):
- Any default tracking parameter or custom parameter ("You can feed in data for any default or custom parameter"). The default input list comes from `InputParameterListRequest`; the model's own Live2D parameters from `Live2DParameterListRequest`.
- Custom parameters via `ParameterCreationRequest`: names unique, alphanumeric, 4 to 32 characters. "There is a global limit in VTS of 300 custom parameters and a limit of 100 custom parameters per plugin." The user then maps them as inputs in VTS parameter mappings, with smoothing available.
- Values are floats between -1000000 and 1000000; out-of-range or unknown parameters return an error.
- Modes: `set` (default) lets only one plugin own a parameter at a time ("if another plugin is already controlling one ... parameter, an error will be returned"); `add` lets any number of plugins stack. Optional `weight` (0 to 1) blends with the user's real face tracking; `faceFound: true` controls the "tracking lost" animation.
- Keep-alive: "You have to re-send data for a parameter ... at least once every second", otherwise it reverts to the previous controller or default.

**Hotkeys and expressions:**
- Hotkey list and trigger requests for the current model. "One specific hotkey can only be triggered once every 5 frames" and "the queue holds 32 hotkeys".
- Expressions: `ExpressionStateRequest` lists them and their active state; `ExpressionActivationRequest` turns one on or off, with `fadeTime` clamped to 0 to 2 s (default 0.25). The README itself recommends hotkeys, since users can be left with an expression they cannot switch off.

**Model info:** `CurrentModelRequest`, `AvailableModelsRequest`, `StatisticsRequest` (framerate, VTS version) and event subscriptions replace polling.

**Other limits that touch us:** model load has a global 2 s cooldown; sending `InputParameterListRequest` at 60+ FPS is discouraged for performance; discovery is a UDP broadcast on port 47779 every 2 s ("even if the user has turned off the API").

**Rate and latency, working assumptions** (not documented, to measure in CN-13's first task):
- There is no stated cap on injection requests. Send one batched message per tracking frame (one request can carry many parameters), at most the render frame rate, and not more than needed (30 to 60 per second).
- Loopback WebSocket plus one VTS frame of processing is expected to give a few milliseconds plus up to one VTS frame (about 8 to 17 ms at 60 to 120 fps). Treat this as an estimate. Lip sync should be sent early (audio-lead) rather than assume zero latency, and CN-13 should publish a measured number once the loopback test exists.

## 5. Risks

1. **Branding and trademark.** Largest legal-adjacent risk; mitigated by the section 2 rules. There is no trademark licence, only a "don't imply affiliation" request, so no written guarantee protects the name. Low likelihood.
2. **Policy drift.** The EULA is versioned (v5.2 of 2024-04-15; the 2021 versions are softer on paid-version rules). The plugin clauses held across versions I could open, but DenchiSoft could change them. Record the EULA version and date in CN-13 docs and re-check at each release. The API is versioned (`apiVersion` 1.0), and new fields are additive.
3. **Single-vendor dependency.** The API belongs to one developer. If DenchiSoft changes or removes it, CN-13 breaks. This does not affect the rest of Omuzaki; keep the bridge behind a connector interface.
4. **Parameter ownership conflicts.** `set` mode is exclusive. If the user has another plugin (VBridger, VNyan) driving the same parameter, our injection errors. Use `add` or custom parameters where possible, and show a clear "parameter owned by another plugin" message.
5. **Auth identity.** The token is tied to `pluginName` and `pluginDeveloper`; changing either invalidates users' tokens and reshows the popup. Fix them at release.
6. **User trust and revocation.** Plugin API access can be revoked at any time; the connector must degrade cleanly. The EULA disclaims DenchiSoft's responsibility for plugin damage ("Please only use plugins from developers you trust"), so our permission copy should be honest.
7. **Live2D layer is not ours.** The bridge never touches the Cubism SDK or `.moc3` files, which keeps CN-13 consistent with the "Live2D stays out" decision (spec v3 §114). Do not add features that read or export model files, and do not use VTS's bundled Live2D sample models in marketing (§9, Live2D Inc. terms).
8. **Latency and sync.** Not guaranteed; see section 4.
9. **Licence register.** Using the API adds no code dependency. If we choose a WebSocket client crate it follows the usual register rule. Adding VTS as a "service" row to `docs/licensing/register.md` with verdict `conditions` (naming and EULA notes above) is a follow-up for the maintainers.

## 6. Restrictions to carry into CN-13

- Depend only on the public protocol and documented requests; never VTS files, memory, or private ports.
- Request only authentication; ask for no optional permissions unless a feature needs one, and then explain why in our own UI.
- Stable plugin identity; one batched injection message per frame; re-send every parameter under 1 s; fall back gracefully on revoke or `set` conflict.
- Prefer hotkeys over direct expression toggles.
- Naming rules from section 2, plus the non-affiliation line.
- Re-check this page against the then-current EULA before release and note the EULA version in the CN-13 docs.

## 7. Spec sources (clean-room list per the licence policy)

- Protocol and requests: [DenchiSoft/VTubeStudio README](https://github.com/DenchiSoft/VTubeStudio) (MIT), the [Permissions page](https://github.com/DenchiSoft/VTubeStudio/tree/master/Permissions), `Files/ErrorID.cs` in the same repo, and the [wiki](https://github.com/DenchiSoft/VTubeStudio/wiki).
- Terms: [VTS EULA v5.2](https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf) and the [branding page](https://denchisoft.com/branding).
- Behavioural reference only (do not copy code): the plugin list entries above, e.g. [VTubeStudioJS](https://github.com/Hawkbat/VTubeStudioJS) (library by Hawkbar; check its own licence before reading source).

## Open items

- Latency numbers need a measurement (loopback test against a real VTS); I could not measure here (no VTS install).
- Optional written confirmation from DenchiSoft (support@denchisoft.com) on logo use in product UI, if we want the logo.
- VNyan's and Streamer.bot's own wording and licence terms were not read in detail.
