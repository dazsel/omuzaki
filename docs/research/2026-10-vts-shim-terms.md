# Answering the VTube Studio API as a server: terms, wire facts, tools, naming (2026-10-13)

> Research, under PL-05 (spec v3 §30), next to CN-13 (§14). `docs/research/2026-10-vts-plugin-api-terms.md` (the CN-13 note) covers Omuzaki as a plugin of VTube Studio (VTS). This note covers the other direction: a server that answers the same documented API so that stream tools can talk to Omuzaki. An engineering reading, not legal advice; open legal points.
> Sources were read on **2026-10-13** unless a row says otherwise. Marks: `verified` = I read the primary source; `secondary` = read in another document or a derived source; `unverified` = not read.

## Verification status (2026-10-13)

The first pass could not reach the EULA, the branding page or the GitHub API (blocked by the research environment's proxy). A second pass read them on 2026-10-13: the EULA PDF (https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf, "Version 5.2", dated April 15th 2024; read in full, text extracted locally), the [branding page](https://denchisoft.com/branding), `README.md`, `Files/ErrorID.cs` and `LICENSE` on `master`, and the GitHub commits API. Section 1 rows 5 to 10 are now `verified`. Still `unverified`: the docs of Mix It Up, Touch Portal, Stream Deck and TITS (section 3 "Not checked") and the Streamer.bot/Heartrate details marked there; none of these is a licence or protocol claim.

## Verdict

**The shim may be built, with conditions. Not a clean GO on the text alone.**

- Nothing I could read restricts a server that answers the documented API. The README's grant is written for plugins ("write, publish and sell any plugin you want for VTube Studio"), which a shim is not, so it does not grant the shim anything by itself; it also does not forbid it.
- The README, the `ErrorID.cs` list and the `Events/` and `Permissions/` pages are MIT-licensed (Copyright (c) 2019 DenchiSoft, `LICENSE`). The wire format they document is the interface. We implement it from our own words; we copy no text or code, so the MIT notice does not apply (and if any is copied, the notice must travel with it).
- The EULA clauses that could bite (derived works, reverse engineering, branding) are now read in the primary text (checked 2026-10-13, https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf). §5 says the licensee "may not modify, decompile or reverse engineer the software" and may not "create derivative works based upon the software". A shim contains no VTS code and was written without looking at VTS, so it is not a derivative work of the software on the plain reading. The whole EULA (v5.2) contains no clause about servers, emulation or compatible implementations. It binds a *licensee of VTS*; a developer who never installs VTS is not party to it (legal review to confirm, section 8).
- Naming is the real constraint: a shim must never claim to be VTS (section 2 and 7).

Condition list: (1) done on 2026-10-13 (EULA and branding read; nothing found that forbids the shim); (2) legal review settles the open points before any public release; (3) the naming rules in section 7 hold in UI, docs and on the wire; (4) the clean-room record below stays true.

## 1. The terms, clause by clause

| # | Source and clause | Says | Verdict for a server shim | Mark |
|---|---|---|---|---|
| 1 | [README "Legal Info"](https://github.com/DenchiSoft/VTubeStudio#legal-info), retrieved 2026-10-13 | "You are free to write, publish and sell any plugin you want for VTube Studio. No permission is required and there are no licensing fees or royalties." | `conditions`: covers plugins; a shim is a server, so it is outside this grant, not inside it. Silent on servers. | verified |
| 2 | [README "General Info"](https://github.com/DenchiSoft/VTubeStudio#general-info), 2026-10-13 | "If you develop a plugin or any app that can be used with VTube Studio, please open a PR and I will add it to the list." | `yes`: "any app that can be used with VTube Studio" is wider than plugins, but a shim is used *instead of* VTS, not with it. Read as: the author welcomes tools in the ecosystem. | verified |
| 3 | [`LICENSE`](https://github.com/DenchiSoft/VTubeStudio/blob/master/LICENSE), 2026-10-13 | MIT, Copyright (c) 2019 DenchiSoft. Covers the repo contents (docs, example files). | `yes`: no restriction on implementing the described format; notice needed only if text or code is copied. We copy neither. | verified |
| 4 | [Wiki `Plugins` page](https://github.com/DenchiSoft/VTubeStudio/wiki/Plugins) (raw markdown), 2026-10-13 | "Anyone can develop plugins!! It's free and there are no restrictions! ... There are no royalties or licensing payments required." Listed plugins "are UNOFFICIAL and are not developed by DenchiSoft." | `yes` for plugins; the same limit as row 1. Confirms the "unofficial" labelling DenchiSoft expects. | verified |
| 5 | [EULA v5.2](https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf) §5 "You acknowledge to" | "Unless it is not allowed by applicable law, you may not modify, decompile or reverse engineer the software." Also no derivative works "based upon the software", no bypassing licence verification, no use to advertise cryptocurrencies, NFTs or AI "art" projects. Matches the CN-13 summary. | `conditions`: a shim derives nothing from VTS (clean-room record below). Note the last bullet is about VTS and its assets, not about Omuzaki, but docs should not use VTS assets in such promotion anyway. | verified (checked 2026-10-13) |
| 6 | EULA v5.2 §10 "VTube Studio Plugins" ([PDF](https://denchisoft.com/wp-content/uploads/2024/04/denchisoft_eula_vts_2024_04_15.pdf)) | "Developers may distribute and sell these plugins however they want. No permission, license or payment to DenchiSoft is required." §5 also says: "If you are interested in developing extensions/plugins for VTube Studio, please use the VTube Studio Public API ... You may freely distribute and sell plugins". | `conditions`: plugins again; silent on servers. | verified (checked 2026-10-13) |
| 7 | EULA v5.2 §12 "Branding" and the [branding page](https://denchisoft.com/branding) | EULA: use of the logo and branding is allowed unmodified, "but please only do so in a way that does not make it appear as if your project/stream is officially related to or partnered with VTube Studio". Branding page: "do not use them in a way that makes it appear as if your project/stream is officially affiliated with VTube Studio ... unless you have explicit permission". The page says nothing on third-party apps or compatibility claims. | `conditions`: a shim must not look like VTS; section 7 rules stand. Descriptive "compatible with" wording is not addressed by either text; legal review question 4. | verified (checked 2026-10-13: https://denchisoft.com/branding) |
| 8 | EULA v5.2 §9 (included models) | Live2D models in the app, except Akari, belong to Live2D Inc. ("Agreement for Free Material"). Akari is DenchiSoft's: usable only inside VTube Studio, not commercially without written permission, not as a main model, and not "in any way that could make it appear as if you are officially affiliated". Other included assets: free to use "but only within VTube Studio". | `yes`: the shim ships no VTS or Live2D assets. Correction to the earlier wording: Akari is a DenchiSoft model, not a Live2D mark. | verified (checked 2026-10-13) |
| 9 | EULA v5.2 §2.1, §2.2, §13 | Commercial use of VTS needs a paid version or DLC; a company with income above $200 000 needs a Company Licence; a Live2D Publication Licence may be needed above 20 million JPY annual sales (Live2D statement in §13). | `yes`: obligations of the *user of VTS*, not ours; docs must not imply that using Omuzaki satisfies them. | verified (checked 2026-10-13) |
| 10 | Any EULA clause on "emulating", "impersonating" or "compatible implementations" | Searched the full v5.2 text (§1 to §13): none. The only API sentence is the plugin invitation in §5. | `conditions`: absence in v5.2; a later EULA could change it. Question 1 for legal review stays. | verified (absence in v5.2, checked 2026-10-13) |

Not found in the README, the wiki page, the licence or the full EULA v5.2: a clause that says a server may or may not answer the API. That is an absence, not a permission; legal review decides.

## 2. Identity on the wire

All from the [README "API Details"](https://github.com/DenchiSoft/VTubeStudio#api-details) and "API Server Discovery (UDP)" sections, retrieved 2026-10-13, `verified`.

**Envelope.** Every request has `apiName` = `"VTubeStudioPublicAPI"`, `apiVersion` = `"1.0"`, optional `requestID` (1 to 64 ASCII chars), `messageType`, optional `data`. Responses carry the same `apiName` and `apiVersion`, a UNIX-ms `timestamp`, the echoed `requestID` (a random UUID if the request had none), `messageType`, and a `data` object (empty `{}` if none). The README says the version stays `"1.0"` until incompatible changes, and new fields may appear without a bump, so clients must ignore unknown fields.

**What the client side checks.** README text only says *requests* must contain `apiName`/`apiVersion` and that `ErrorID` `APINameInvalid` (3) and `APIVersionInvalid` (4) exist (`ErrorID.cs`). Nothing says what a client checks in a *response*.
- Evidence from a public client: Karasubonk (`typeou/karasubonk`, MIT, `bonker.js`) never reads `apiName` or `apiVersion` from responses. It parses `JSON.parse(event.data)`, then reads `response.data.authenticated`, `response.messageType` (`AuthenticationTokenResponse`, `APIError`) and `response.data.errorID == 50`. Mark: `secondary` (a tool's own source, one tool).
- Libraries (VTS-Sharp inside `FomTarro/vts-heartrate`, MIT) model errors by `ErrorID`. Not read for response-side checks. `unverified`.
- A compatible reply therefore has to: be a text frame, echo `requestID`, set `messageType` to the matching `...Response`, and return `data` in the documented shape. Always send `apiName` and `apiVersion` exactly as VTS does, as real clients may check them (acceptance run item A below).

**Identity fields and what the shim says.**

| Field | Where | VTS value | Shim rule |
|---|---|---|---|
| `apiName` | every message | `VTubeStudioPublicAPI` | Echo exactly. It names the *protocol*, not the product; tools reject anything else. |
| `apiVersion` | every message | `1.0` | Echo `1.0`. |
| `vTubeStudioVersion` | `APIStateResponse.data` | e.g. `1.9.0` (README example) | **Do not** send a VTS-looking version. Send a Omuzaki version string, e.g. `Omuzaki 0.x`. Risk: a tool that parses it as semver. See acceptance item B. |
| `windowTitle` | UDP broadcast | `VTube Studio` | Never `VTube Studio`. Use `Omuzaki`. |
| `instanceID` | UDP broadcast | random id per VTS run | Own random id per Omuzaki run. |
| `pluginName`/`pluginDeveloper` | client auth | 3 to 32 chars | Client-chosen; the shim stores whatever it is given and must match them on re-auth (section 5). |

What must not be claimed: that the app is VTube Studio, made by DenchiSoft, endorsed by DenchiSoft, or "VTS" in any user-visible or wire field.

**UDP discovery.** VTS broadcasts `VTubeStudioAPIStateBroadcast` on UDP port 47779 every two seconds, even if the API is off. Fields: `data.active`, `data.port`, `data.instanceID`, `data.windowTitle`; envelope fields as above (`requestID` is the literal string `VTubeStudioAPIStateBroadcast`). **Default: the shim does not broadcast.** Reasons: a real VTS on the same LAN would be indistinguishable to a tool picking the first answer; broadcasting is a network-wide, not loopback-only, side effect; and no tool I could read depends on it (Karasubonk and Streamer.bot take a host and a port from the user). If a user opts in, the broadcast carries `windowTitle: "Omuzaki"` and the shim's own port. Mark: broadcast fields `verified`; "no tool depends on it" `secondary`/`unverified` beyond the two tools read.

## 3. What popular stream tools send

Sources: the tool's own public docs or source on GitHub, retrieved 2026-10-13. A request name appears only if the source shows it. Tools whose docs were unreachable are listed under "Not checked".

Evidence base:
- **Streamer.bot**: its docs repo `Streamerbot/docs` at commit `5902930` (2026-09-11), pages `streamerbot/2.guide/06.integrations/vtube-studio.md`, `3.api/1.sub-actions/integrations/vtube-studio/*.md`, `3.api/2.triggers/integrations/vtube-studio/*.md`. These name features and C# methods, not wire messages, so wire names below are the matching documented VTS request for each feature: `secondary`.
- **Karasubonk**: `bonker.js`, shallow clone 2026-10-13 (MIT, Copyright (c) 2022 typeou). Wire names read directly: `verified` for what it sends.
- **VTube Studio Heartrate** (`FomTarro/vts-heartrate`, MIT, Copyright (c) 2022 Tom Farro): bundles VTS-Sharp (`Assets/Core/VTS`), a full client library with every request type, so it proves nothing about which requests it *uses*. Hotkey triggering is wired in `HotkeyModule.cs`. `secondary`.

Marks: **observed** = the tool's own source or docs show it sends or offers this; **library** = only a bundled client library contains the request type, which proves support, not use; **not seen** = absent from what I read. All sources retrieved 2026-10-13. Link keys: **SB** = `Streamerbot/docs` at commit `5902930`, folder [`streamerbot/3.api/1.sub-actions/integrations/vtube-studio`](https://github.com/Streamerbot/docs/tree/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio) (triggers in the sibling `2.triggers` folder); **KB** = [Karasubonk `bonker.js`](https://github.com/typeou/karasubonk/blob/HEAD/bonker.js); **HR** = [vts-heartrate](https://github.com/FomTarro/vts-heartrate) (`Assets/Core/VTS/Unity/UnityVTSPlugin.cs` is the library, `HotkeyModule.cs` the app); **README** = [DenchiSoft/VTubeStudio README](https://github.com/DenchiSoft/VTubeStudio#api-details).

| Request type | Streamer.bot | Karasubonk | Heartrate | Source (primary) |
|---|---|---|---|---|
| `APIStateRequest` | not seen | not seen | not seen | [README, "API Details"](https://github.com/DenchiSoft/VTubeStudio#api-details) suggests it as the first call; no tool read sends it |
| `AuthenticationTokenRequest` | observed (connect flow, allow popup) | observed (`bonker.js` line 517) | not seen | [SB guide `vtube-studio.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/2.guide/06.integrations/vtube-studio.md); KB l.517 |
| `AuthenticationRequest` | observed (auto-reconnect option) | observed (l.247, stored token; falls back to the token request when `authenticated` is false) | library | SB guide; KB l.247 |
| `CurrentModelRequest` | observed ("Get Current Model") | observed (l.124, 166, 209, 591) | not seen | [SB `get-current-model.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/get-current-model.md); KB |
| `AvailableModelsRequest` + `ModelLoadRequest` | observed ("Load Model", "Load Model by Name") | not seen | library | [SB `load-model.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/load-model.md), `load-model-by-name.md`; README: 2 s global cooldown |
| `HotkeysInCurrentModelRequest` | observed (hotkey pickers per model) | not seen | observed (`HotkeyModule.cs`) | [SB `trigger-hotkey.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/trigger-hotkey.md); HR `HotkeyModule.cs` |
| `HotkeyTriggerRequest` | observed (by id or by name) | not seen | observed (`UnityVTSPlugin.TriggerHotkey`, l.126) | SB `trigger-hotkey.md`, `trigger-hotkey-by-name.md`; HR |
| `ExpressionStateRequest` / `ExpressionActivationRequest` | observed ("Set Expression State": toggle, enable, disable, fade time) | not seen | library | [SB `set-expression-state.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/set-expression-state.md) |
| `ColorTintRequest` | observed ("Color Tint", "Reset all color tints") | not seen | library | [SB `color-tint.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/color-tint.md) |
| `MoveModelRequest` | observed ("Move Model") | observed (l.26, 147, 190) | library | [SB `move-model.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/move-model.md); KB |
| `InputParameterListRequest` | not seen | observed (l.279, calibration reads `defaultParameters` and `modelID`) | library | KB l.279; README: avoid at 60+ FPS |
| `InjectParameterDataRequest` | not seen (no injection feature in its VTS pages) | observed (l.820, 868) | library | KB l.820, 868 |
| `ParameterCreationRequest` | not seen | not seen | library | [README, "Adding new tracking parameters"](https://github.com/DenchiSoft/VTubeStudio#adding-new-tracking-parameters-custom-parameters) |
| `EventSubscriptionRequest` | observed (triggers: model loaded/unloaded, hotkey triggered, background changed, model config changed, tracking status, model animation, model clicked, item event) | not seen | not seen | SB `2.triggers/integrations/vtube-studio/*.md`; event names in the [README `Events/` page](https://github.com/DenchiSoft/VTubeStudio/blob/master/Events/README.md) |
| `StatisticsRequest`, `VTSFolderInfoRequest` | not seen | not seen | library | README, "Getting current VTS statistics" |
| Raw request | observed ("Send Raw Request": any `messageType` and JSON) | n/a | n/a | [SB `send-raw-request.md`](https://github.com/Streamerbot/docs/blob/5902930/streamerbot/3.api/1.sub-actions/integrations/vtube-studio/send-raw-request.md); users can send requests we refuse |

**Not checked (unreachable or no public wire docs).** Mix It Up (docs site not reachable; wiki lists hotkey activation per the VTS plugin page, `secondary`), the Stream Deck plugin (store page not reachable; the VTS wiki says it covers hotkeys and model changes, `secondary`), Touch Portal TP_VTS (docs on `wiccy.net`, not reachable; the VTS wiki says hotkeys, load and move model, ArtMesh colouring, `secondary`), TITS and VTS React (itch.io and a web app, not reachable), VNyan (its own platform, not a VTS plugin client for every feature). Needs the tool itself: an acceptance run (section 9).

**Reading the table.** Four request groups cover nearly everything these tools do on connect and in use: auth (state, token, authenticate), current model, hotkeys (list and trigger) and expressions, and event subscriptions. Parameter injection and the parameter-list request are used by tracking-style tools (Karasubonk, VBridger-type plugins), not by redeem bots.

## 4. Conflicts and safety

1. **Port 8001 versus a real VTS.** VTS defaults to `ws://localhost:8001` and the user can change it ("You should also make sure your plugin supports configuring the port"). If both run, whichever binds first wins and the other fails to bind. The shim defaults to a *different* port (proposal: 8011) and tells the user what to type into the tool. Port 8001 stays off by default; the user may choose it when VTS is not running. The shim never kills or probes a process on 8001.
2. **Running both.** A tool is told one host/port. The shim does not forward to VTS and does not merge state with it. UDP discovery stays off, so a tool that auto-discovers only ever finds the real VTS.
3. **Loopback only.** Bind `127.0.0.1` (and `::1`). The README says nothing about non-loopback binding, but the token model assumes a local user popup. The only exposure option is an explicit user setting with a warning. Reject any `Origin` that is not loopback unless allowed in settings (a web page the user opens must not be able to reach the shim). This is design guidance, not from a source.
4. **Tokens a tool stores.** The token is at most 64 ASCII characters (README). Tools store it (Karasubonk keeps it in its data file). The shim issues random tokens from the OS CSPRNG, stores only a hash plus `pluginName`/`pluginDeveloper`, lets the user revoke any token, and on revoke follows the README's two documented states: any request from a revoked plugin gets `APIError` 50 (`TokenRequestDenied`; README, "Authentication": "If they do, you will also get this error when trying to send any requests. You may try to re-authenticate in this case"), and `AuthenticationRequest` with the revoked token gets `authenticated: false`. Error 8 (`RequestRequiresAuthetication`) is for a session that has not authenticated at all (section 5). Tokens never appear in logs. A token request pops up a Omuzaki approval (never auto-approve); only one at a time (`TokenRequestCurrentlyOngoing` = 51).
5. **Every-frame control requests.** `InjectParameterDataRequest` is meant to be sent every frame (re-send at least once per second, README). The shim must keep a per-connection token bucket and drop-oldest queue so a tool at 120 requests per second cannot starve the render loop. `HotkeyTriggerRequest` is rate-limited by VTS (README: a hotkey triggers once every 5 frames, a queue of 32; `ErrorID.cs` comment says "5 second cooldown": the sources disagree, a discrepancy to settle in the acceptance run). The shim applies its own limits and answers `HotkeyCooldownNotOver` (203) or `HotkeyQueueFull` (200).
6. **Parameter ownership.** "set" mode lets one plugin own a parameter, answered with `InjectDataParamControlledByOtherPlugin` (454); the shim copies this behaviour so tools degrade the way they already do.
7. **Revocation and disable.** When the user turns the shim off, answer `APIAccessDeactivated` (1) while the socket is still open, then close.
8. **Clean errors.** Malformed JSON, wrong name or version, missing or unknown `messageType` have their own ids (2, 3, 4, 5, 6, 7). Never panic on input.
9. **Binary frames.** The README says VTS replies with text frames and accepts UTF-8 binary frames; the shim does the same.

## 5. Which requests the shim answers and refuses

Error ids are from `Files/ErrorID.cs` (`verified`, 2026-10-13). Mapping:

**Answer (phase 1).** The tools in section 3 all work with these.
- `APIStateRequest` (active, a Omuzaki version string, `currentSessionAuthenticated`).
- `AuthenticationTokenRequest`, `AuthenticationRequest` (with the pluginName/pluginDeveloper match rule; false `authenticated` on a bad token).
- `CurrentModelRequest`, `AvailableModelsRequest`.
- `HotkeysInCurrentModelRequest`, `HotkeyTriggerRequest` (mapped to Omuzaki hotkeys/expressions).
- `ExpressionStateRequest`, `ExpressionActivationRequest` (fade time clamped 0 to 2 s per README).
- `EventSubscriptionRequest` and the events Omuzaki can truly produce (model loaded/unloaded, hotkey triggered, tracking status). Unknown event: `EventSubscriptionRequestEventTypeUnknown` (950).
- `StatisticsRequest`, `InputParameterListRequest`, `ParameterCreationRequest`, `ParameterDeletionRequest`, `InjectParameterDataRequest` (tracking-style tools; mapping to Omuzaki's own input model is a follow-up design).
- `PermissionRequest` (returns the list; the two documented permissions are refused, below).

**Answer with a state-limited result.** `MoveModelRequest`, `ColorTintRequest` and `ModelLoadRequest` map to Omuzaki features if they exist; else refuse with the id below. `ModelLoadRequest` honours the 2 s cooldown with `ModelLoadCooldownNotOver` (153).

**Refuse or answer empty.** `ErrorID.cs` has no "unsupported" id, so each row below picks the documented id whose *meaning* is true for Omuzaki's state, or answers successfully with an empty result where VTS would for an empty scene. These are design choices from the `ErrorID.cs` comments and the README; how real VTS answers for the same states is acceptance item C.

| Request | Response | Why this one |
|---|---|---|
| Unknown `messageType` | 7 `RequestTypeUnknown` | Never guess. |
| `ItemLoadRequest` with `customDataBase64` | 9 `RequestRequiresPermission` | `LoadCustomImagesAsItems` is never granted: arbitrary image injection is a risk. |
| `NDIConfigRequest` | 9 `RequestRequiresPermission` | `ControlNDISettings` is never granted; Omuzaki has its own output. |
| `ItemListRequest` | success, empty lists | Omuzaki has no VTS-style items, so "no items loaded or available" is true. Response shape to confirm in the acceptance run. |
| `ItemLoadRequest` (no custom data) | 753 `CannotCurrentlyLoadItem` | Closest documented "cannot load right now" id; the comment says menus are open, which is not literally true, so the `message` text says items are unsupported. |
| `ItemUnloadRequest` | 800 `CannotCurrentlyUnloadItem` | Same reasoning as 753. |
| `ItemMoveRequest` | 900 `ItemMoveRequestInstanceIDNotFound` | No items exist, so every instance id is unknown. True. |
| `ItemAnimationControlRequest` | 850 `ItemAnimationControlInstanceIDNotFound` | Same. |
| `ItemPinRequest` | 1050 `ItemPinRequestGivenItemNotLoaded` | Same. |
| `ItemSortRequest` | 1250 `ItemSortRequestInstanceIDNotFound` | Same. |
| `PostProcessingListRequest` | success, empty list | No effects in Omuzaki; an invalid filter (over 512 entries) is the only case for 1150, so 1150 is returned only for that. |
| `PostProcessingUpdateRequest` | 1200 `PostProcessingUpdateReqestCannotUpdateRightNow` | Closest documented "cannot update now" id; `message` says effects are unsupported. |
| `SetCurrentModelPhysicsRequest` | 700 `...NoModelLoaded` when no character is loaded, else 701 `...ModelHasNoPhysics` | Omuzaki's physics are not VTS physics groups, so "has no physics" is true from the client's view. |
| `ArtMeshListRequest` | success, empty list | Omuzaki characters are not Live2D, so there are no ArtMeshes. |
| `ArtMeshSelectionRequest` | 1000 `ArtMeshSelectionRequestNoModelLoaded` when no character is loaded; 1002 `ArtMeshSelectionRequestModelDoesNotHaveArtMesh` when `activeArtMeshes` names any id (none can exist); otherwise (character loaded, no preselection) 1001 `ArtMeshSelectionRequestOtherWindowsOpen` | 1000 and 1002 are used for exactly their documented meanings (`ErrorID.cs`). The last case has no true id: the request asks the user to pick meshes in a VTS window Omuzaki does not have, so the nearest "cannot start the request now" id is used and `message` says ArtMesh selection is unsupported. Design choice; real VTS behaviour is acceptance item C. |
| `ColorTintRequest` | 250 `ColorTintRequestNoModelLoaded` when none; 251/252 for missing match or bad colour (as VTS); otherwise success matching zero meshes | ColorTint has its own 250 to 252 group; a valid request that matches nothing is not an error. |
| Any request other than `APIStateRequest`, `AuthenticationTokenRequest` and `AuthenticationRequest`, from a session that has not authenticated | 8 `RequestRequiresAuthetication` | The handshake requests must be answered before authentication, or no client could connect. |
| Any request when the shim is disabled | 1 `APIAccessDeactivated` | Matches VTS. |

Refusing with a documented id rather than silently ignoring lets a tool show its own error. A request class we have not enumerated is refused with 7, not guessed.

## 6. Clean-room record

- **Who wrote this spec:** the spec author (an AI-assisted session), on 2026-10-13.
- **From what:** the public `DenchiSoft/VTubeStudio` repository (README, `Files/ErrorID.cs`, `Events/README.md`, `Permissions/README.md`, `LICENSE`), the repository's wiki `Plugins` page, the CN-13 note, the Streamer.bot docs repository, and the public source of Karasubonk and VTube Studio Heartrate (for which requests they send).
- **Not used:** any VTS code, binary or decompiled output; any traffic capture or network log from a real VTS session; any private test material; any non-public information about VTS. The author has no VTS install.
- **Implementers** (PL-05 tasks) use this note and the CN-13 note only. They must not open VTS binaries, other VTS-compatible server implementations' source, or capture VTS traffic. Karasubonk and Heartrate are cited for *which requests tools send*, not for code; their licences are MIT, so reading is allowed, but no code is copied.

## 7. Naming rules (UI, docs, wire)

1. **UI and docs.** Call the feature "VTube Studio-compatible API" or "VTS API compatibility", never "VTube Studio" as the product, and never "VTS" alone as a product name. Say what it does: "lets tools made for VTube Studio talk to Omuzaki".
2. **Disclaimer line**, in the UI panel and docs: "Not affiliated with or endorsed by DenchiSoft or Live2D Inc. VTube Studio is a product of DenchiSoft. Tools may behave differently with Omuzaki."
3. **Wire.** `apiName` and `apiVersion` echo the protocol (required to work). Everything that names the *product* says Omuzaki: `windowTitle`, the version string in `APIStateResponse`, log lines, error `message` strings.
4. **No logos.** No VTS or Live2D logo or logo-like mark; text only (same rule as CN-13).
5. **Tool names.** Say "works with tools such as Streamer.bot" only for tools actually tested in the acceptance run; each tool's trademark remains its owner's.
6. **No "drop-in replacement for VTube Studio"** or similar claim.
7. **Port default 8011**, not 8001 (section 4).

## 9. Needs the tools or accounts (acceptance run)

- **A.** Does each tool accept a reply when `apiName`/`apiVersion` are right but the `windowTitle`, version string and UDP are Omuzaki's own? (Streamer.bot, Mix It Up, Stream Deck plugin, Touch Portal, TITS.)
- **B.** Does any tool parse `vTubeStudioVersion` as semver or compare it to a minimum?
- **C.** Real VTS: the hotkey cooldown (5 frames or 5 seconds), response times, exact error ids returned for refused item and NDI requests and for `ArtMeshSelectionRequest` (to validate the intentional 1001 fallback in section 5).
- **D.** Which requests Mix It Up, Touch Portal and the Stream Deck plugin send on connect (their docs were unreachable). A tool with a packet log against the *shim*, not against real VTS, is the clean-room-safe way to see it.
- **E.** Done: EULA and branding page read 2026-10-13.
- **F.** README last changed 2026-09-12 (GitHub commits API, sha 6dd6f90, checked 2026-10-13).

## 10. Findings that change other work

- `ErrorID.cs` comment on `HotkeyCooldownNotOver` says "global 5 second cooldown"; the README says "once every 5 frames". Treat as unresolved (section 4, item 5).
- `ErrorID.cs` keeps the typo `RequestRequiresAuthetication` (8) and `PostProcessingListReqest` names. Use the documented *numbers* in docs; our own names should be spelled correctly.
- The CN-13 note and the README (still, 2026-10-13, line on supporting users) name the VTS toggle "Allow Plugin API access"; Streamer.bot's docs (2026-09) call it `Start API`. The setting may have been renamed; docs for users should say "turn on the API in VTube Studio settings" without quoting the label.
